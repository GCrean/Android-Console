//! ADB integration: device discovery, `adb connect`, and an interactive
//! `adb shell -tt` session with non-blocking I/O.
//!
//! The session is a child `adb` process. Reader threads forward its output to
//! an async channel; a writer thread owns stdin so UI code never blocks.

use std::fmt;
use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdbError {
    /// The `adb` executable could not be launched.
    NotFound,
    Io(String),
    /// `adb` ran but reported a failure.
    Failed(String),
}

impl fmt::Display for AdbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AdbError::NotFound => write!(
                f,
                "`adb` was not found. Install Android platform-tools and add it to PATH (or set ADB_PATH)."
            ),
            AdbError::Io(e) => write!(f, "I/O error: {e}"),
            AdbError::Failed(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for AdbError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Usb,
    TcpIp,
    Emulator,
}

impl Transport {
    pub fn label(self) -> &'static str {
        match self {
            Transport::Usb => "usb",
            Transport::TcpIp => "tcpip",
            Transport::Emulator => "emulator",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub serial: String,
    /// `device`, `unauthorized`, `offline`, ...
    pub state: String,
    pub model: Option<String>,
    pub transport: Transport,
}

impl Device {
    pub fn is_ready(&self) -> bool {
        self.state == "device"
    }
    pub fn display_name(&self) -> String {
        match &self.model {
            Some(m) => format!("{} ({})", m, self.serial),
            None => self.serial.clone(),
        }
    }
}

fn transport_of(serial: &str) -> Transport {
    if serial.starts_with("emulator-") {
        Transport::Emulator
    } else if serial.contains(':') {
        Transport::TcpIp
    } else {
        Transport::Usb
    }
}

/// Parse the output of `adb devices -l`.
pub fn parse_devices(output: &str) -> Vec<Device> {
    output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("List of devices") && !l.starts_with('*'))
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let serial = parts.next()?.to_string();
            let state = parts.next()?.to_string();
            let model = parts
                .find_map(|p| p.strip_prefix("model:"))
                .map(|m| m.replace('_', " "));
            let transport = transport_of(&serial);
            Some(Device { serial, state, model, transport })
        })
        .collect()
}

fn adb_command() -> Command {
    let program = std::env::var("ADB_PATH").unwrap_or_else(|_| "adb".to_string());
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd
}

fn map_spawn_err(e: std::io::Error) -> AdbError {
    if e.kind() == std::io::ErrorKind::NotFound {
        AdbError::NotFound
    } else {
        AdbError::Io(e.to_string())
    }
}

fn run(args: &[&str]) -> Result<String, AdbError> {
    let out = adb_command()
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(map_spawn_err)?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if out.status.success() {
        Ok(stdout)
    } else {
        Err(AdbError::Failed(if stderr.is_empty() { stdout } else { stderr }))
    }
}

/// Blocking: run from a background thread.
pub fn list_devices() -> Result<Vec<Device>, AdbError> {
    run(&["devices", "-l"]).map(|s| parse_devices(&s))
}

/// Blocking: `adb connect host[:port]` (default port 5555).
pub fn connect_tcp(addr: &str) -> Result<String, AdbError> {
    let addr = addr.trim();
    if addr.is_empty() {
        return Err(AdbError::Failed("empty address".into()));
    }
    let addr = if addr.contains(':') { addr.to_string() } else { format!("{addr}:5555") };
    let out = run(&["connect", &addr])?;
    // adb exits 0 even when the connection is refused.
    if out.contains("connected to") || out.contains("already connected") {
        Ok(out)
    } else {
        Err(AdbError::Failed(out))
    }
}

#[derive(Debug)]
pub enum Event {
    /// Bytes from the remote pty.
    Data(Vec<u8>),
    /// Text from the local `adb` client's stderr (connection problems etc.).
    Stderr(String),
    /// The shell exited; the code is the `adb` process exit status if known.
    Exited(Option<i32>),
    /// A transport I/O failure.
    Error(String),
}

pub struct Session {
    child: Arc<Mutex<Child>>,
    input: mpsc::Sender<Vec<u8>>,
}

impl Session {
    /// Start `adb [-s serial] shell -tt`. Events arrive on the returned
    /// receiver; it closes after `Exited`.
    pub fn start(
        serial: Option<&str>,
    ) -> Result<(Session, async_channel::Receiver<Event>), AdbError> {
        let mut cmd = adb_command();
        if let Some(s) = serial {
            cmd.args(["-s", s]);
        }
        let mut child = cmd
            .args(["shell", "-tt"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(map_spawn_err)?;

        let stdin = child.stdin.take().ok_or_else(|| AdbError::Io("no stdin".into()))?;
        let stdout = child.stdout.take().ok_or_else(|| AdbError::Io("no stdout".into()))?;
        let stderr = child.stderr.take().ok_or_else(|| AdbError::Io("no stderr".into()))?;
        let child = Arc::new(Mutex::new(child));
        let (tx, rx) = async_channel::unbounded();
        let (in_tx, in_rx) = mpsc::channel::<Vec<u8>>();

        // Writer thread: owns stdin.
        {
            let tx = tx.clone();
            let mut stdin = stdin;
            std::thread::spawn(move || {
                while let Ok(bytes) = in_rx.recv() {
                    if let Err(e) = stdin.write_all(&bytes).and_then(|_| stdin.flush()) {
                        let _ = tx.send_blocking(Event::Error(format!("write failed: {e}")));
                        break;
                    }
                }
            });
        }

        // stderr reader.
        {
            let tx = tx.clone();
            let mut stderr = stderr;
            std::thread::spawn(move || {
                let mut buf = [0u8; 1024];
                while let Ok(n) = stderr.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    let text = String::from_utf8_lossy(&buf[..n]).into_owned();
                    if tx.send_blocking(Event::Stderr(text)).is_err() {
                        break;
                    }
                }
            });
        }

        // stdout reader; on EOF reap the child and report the exit.
        {
            let child = Arc::clone(&child);
            let mut stdout = stdout;
            std::thread::spawn(move || {
                let mut buf = [0u8; 16 * 1024];
                loop {
                    match stdout.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            if tx.send_blocking(Event::Data(buf[..n].to_vec())).is_err() {
                                return;
                            }
                        }
                        Err(e) => {
                            let _ = tx.send_blocking(Event::Error(format!("read failed: {e}")));
                            break;
                        }
                    }
                }
                let code = loop {
                    let status = match child.lock() {
                        Ok(mut c) => c.try_wait(),
                        Err(_) => break None,
                    };
                    match status {
                        Ok(Some(s)) => break s.code(),
                        Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                        Err(_) => break None,
                    }
                };
                let _ = tx.send_blocking(Event::Exited(code));
            });
        }

        let session = Session { child, input: in_tx };
        Ok((session, rx))
    }

    pub fn send(&self, bytes: impl Into<Vec<u8>>) {
        let _ = self.input.send(bytes.into());
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Kill and reap so no zombie `adb` process is left behind.
        if let Ok(mut c) = self.child.lock() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_device_list() {
        let out = "List of devices attached\n\
            * daemon started successfully\n\
            R58M123ABC             device usb:1-2 product:oriole model:Pixel_6 device:oriole transport_id:1\n\
            192.168.1.20:5555      unauthorized transport_id:2\n\
            emulator-5554          offline transport_id:3\n\n";
        let d = parse_devices(out);
        assert_eq!(d.len(), 3);
        assert_eq!(d[0].serial, "R58M123ABC");
        assert_eq!(d[0].model.as_deref(), Some("Pixel 6"));
        assert_eq!(d[0].transport, Transport::Usb);
        assert!(d[0].is_ready());
        assert_eq!(d[1].transport, Transport::TcpIp);
        assert!(!d[1].is_ready());
        assert_eq!(d[2].transport, Transport::Emulator);
    }

    #[test]
    fn empty_list() {
        assert!(parse_devices("List of devices attached\n\n").is_empty());
    }

    /// Needs a connected, authorized device: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn live_shell_roundtrip_and_teardown() {
        let dev = list_devices().unwrap().into_iter().find(|d| d.is_ready()).expect("no ready device");
        let (session, rx) = Session::start(Some(&dev.serial)).unwrap();
        let mut out = Vec::new();
        // Let the shell print its prompt first;
        // typeahead is lost when the shell switches the tty to raw mode.
        let settle = std::time::Instant::now() + Duration::from_millis(2000);
        while std::time::Instant::now() < settle {
            match rx.try_recv() {
                Ok(Event::Data(d)) => out.extend(d),
                _ => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        session.send([&br"echo marker$((6*7)); printf '\033[38;2;1;2;3mrgb\033[0m\n'; exit"[..], b"\n"].concat());
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        let exited = loop {
            assert!(std::time::Instant::now() < deadline, "timeout; got so far: {:?}", String::from_utf8_lossy(&out));
            match rx.try_recv() {
                Ok(Event::Data(d)) => out.extend(d),
                Ok(Event::Exited(c)) => break c,
                Ok(other) => panic!("unexpected {other:?}"),
                Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        };
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("marker42"), "{text}");
        assert!(text.contains("\x1b[38;2;1;2;3mrgb"), "{text}");
        assert_eq!(exited, Some(0));
        drop(session);
    }

    #[test]
    #[ignore]
    fn drop_kills_child() {
        let dev = list_devices().unwrap().into_iter().find(|d| d.is_ready()).expect("no ready device");
        let (session, rx) = Session::start(Some(&dev.serial)).unwrap();
        drop(session);
        // Receiver must terminate with an Exited event once the child is killed.
        loop {
            match rx.recv_blocking() {
                Ok(Event::Exited(_)) | Err(_) => break,
                Ok(_) => {}
            }
        }
    }
}
