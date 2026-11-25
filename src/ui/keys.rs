//! Translate GPUI keystrokes into the byte sequences a terminal sends.

use gpui::Keystroke;

/// xterm modifier parameter: 1 + shift(1) + alt(2) + ctrl(4).
fn modifier_param(k: &Keystroke) -> u8 {
    1 + k.modifiers.shift as u8 + 2 * k.modifiers.alt as u8 + 4 * k.modifiers.control as u8
}

fn csi_with_mod(k: &Keystroke, final_byte: char) -> Vec<u8> {
    match modifier_param(k) {
        1 => format!("\x1b[{final_byte}").into_bytes(),
        m => format!("\x1b[1;{m}{final_byte}").into_bytes(),
    }
}

fn tilde(k: &Keystroke, code: u8) -> Vec<u8> {
    match modifier_param(k) {
        1 => format!("\x1b[{code}~").into_bytes(),
        m => format!("\x1b[{code};{m}~").into_bytes(),
    }
}

/// Returns the bytes to send, or `None` if the keystroke produces no input.
pub fn encode_key(k: &Keystroke) -> Option<Vec<u8>> {
    let m = &k.modifiers;
    let named = match k.key.as_str() {
        // `adb shell -tt` leaves the remote pty without ICRNL, so a bare CR
        // never ends a line; LF works whether or not ICRNL is set.
        "enter" => Some(if m.alt { b"\x1b\n".to_vec() } else { b"\n".to_vec() }),
        "backspace" => Some(if m.alt { b"\x1b\x7f".to_vec() } else if m.control { vec![0x08] } else { vec![0x7f] }),
        "tab" => Some(if m.shift { b"\x1b[Z".to_vec() } else { b"\t".to_vec() }),
        "escape" => Some(vec![0x1b]),
        "up" => Some(csi_with_mod(k, 'A')),
        "down" => Some(csi_with_mod(k, 'B')),
        "right" => Some(csi_with_mod(k, 'C')),
        "left" => Some(csi_with_mod(k, 'D')),
        "home" => Some(csi_with_mod(k, 'H')),
        "end" => Some(csi_with_mod(k, 'F')),
        "insert" => Some(tilde(k, 2)),
        "delete" => Some(tilde(k, 3)),
        "pageup" => Some(tilde(k, 5)),
        "pagedown" => Some(tilde(k, 6)),
        "f1" => Some(b"\x1bOP".to_vec()),
        "f2" => Some(b"\x1bOQ".to_vec()),
        "f3" => Some(b"\x1bOR".to_vec()),
        "f4" => Some(b"\x1bOS".to_vec()),
        "f5" => Some(tilde(k, 15)),
        "f6" => Some(tilde(k, 17)),
        "f7" => Some(tilde(k, 18)),
        "f8" => Some(tilde(k, 19)),
        "f9" => Some(tilde(k, 20)),
        "f10" => Some(tilde(k, 21)),
        "f11" => Some(tilde(k, 23)),
        "f12" => Some(tilde(k, 24)),
        _ => None,
    };
    if named.is_some() {
        return named;
    }

    if m.platform || m.function {
        return None;
    }

    // Ctrl+<char> -> C0 control code.
    if m.control {
        let c = k.key.chars().next().filter(|_| k.key.chars().count() == 1)?;
        let code = match c.to_ascii_lowercase() {
            c @ 'a'..='z' => Some(c as u8 - b'a' + 1),
            '@' | ' ' | '2' => Some(0),
            '[' | '3' => Some(0x1b),
            '\\' | '4' => Some(0x1c),
            ']' | '5' => Some(0x1d),
            '^' | '6' => Some(0x1e),
            '_' | '7' | '/' => Some(0x1f),
            '8' | '?' => Some(0x7f),
            _ => None,
        }?;
        return Some(if m.alt { vec![0x1b, code] } else { vec![code] });
    }

    let text = k.key_char.as_deref().filter(|s| !s.is_empty()).or(if k.key == "space" { Some(" ") } else { None })?;
    let mut out = Vec::with_capacity(text.len() + 1);
    if m.alt {
        out.push(0x1b);
    }
    out.extend_from_slice(text.as_bytes());
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(s: &str) -> Option<Vec<u8>> {
        encode_key(&Keystroke::parse(s).expect("valid keystroke"))
    }

    #[test]
    fn special_keys() {
        assert_eq!(enc("enter").unwrap(), b"\n");
        assert_eq!(enc("backspace").unwrap(), [0x7f]);
        assert_eq!(enc("tab").unwrap(), b"\t");
        assert_eq!(enc("shift-tab").unwrap(), b"\x1b[Z");
        assert_eq!(enc("escape").unwrap(), [0x1b]);
        assert_eq!(enc("up").unwrap(), b"\x1b[A");
        assert_eq!(enc("left").unwrap(), b"\x1b[D");
        assert_eq!(enc("home").unwrap(), b"\x1b[H");
        assert_eq!(enc("end").unwrap(), b"\x1b[F");
        assert_eq!(enc("pageup").unwrap(), b"\x1b[5~");
        assert_eq!(enc("pagedown").unwrap(), b"\x1b[6~");
        assert_eq!(enc("delete").unwrap(), b"\x1b[3~");
        assert_eq!(enc("f5").unwrap(), b"\x1b[15~");
    }

    #[test]
    fn control_combos() {
        assert_eq!(enc("ctrl-c").unwrap(), [3]);
        assert_eq!(enc("ctrl-d").unwrap(), [4]);
        assert_eq!(enc("ctrl-z").unwrap(), [26]);
        assert_eq!(enc("ctrl-[").unwrap(), [0x1b]);
        assert_eq!(enc("alt-ctrl-a").unwrap(), [0x1b, 1]);
        assert_eq!(enc("ctrl-up").unwrap(), b"\x1b[1;5A");
        assert_eq!(enc("shift-ctrl-left").unwrap(), b"\x1b[1;6D");
    }

    #[test]
    fn printable_text() {
        let mut k = Keystroke::parse("a").unwrap();
        k.key_char = Some("a".into());
        assert_eq!(encode_key(&k).unwrap(), b"a");
        k.modifiers.shift = true;
        k.key_char = Some("A".into());
        assert_eq!(encode_key(&k).unwrap(), b"A");
        k.modifiers.shift = false;
        k.modifiers.alt = true;
        k.key_char = Some("a".into());
        assert_eq!(encode_key(&k).unwrap(), b"\x1ba");
    }

    #[test]
    fn modifier_only_and_platform_keys_send_nothing() {
        assert!(enc("cmd-a").is_none());
        assert!(enc("shift").is_none());
    }
}
