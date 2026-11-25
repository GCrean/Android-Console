//! Tiny persisted user settings (`key = value` lines in the OS config dir).

use std::path::PathBuf;

pub const DEFAULT_FONT_SIZE: f32 = 14.0;
pub const MIN_FONT_SIZE: f32 = 8.0;
pub const MAX_FONT_SIZE: f32 = 32.0;

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub font_size: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { font_size: DEFAULT_FONT_SIZE }
    }
}

pub fn clamp_font_size(v: f32) -> f32 {
    if v.is_finite() {
        v.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE)
    } else {
        DEFAULT_FONT_SIZE
    }
}

impl Settings {
    pub fn parse(text: &str) -> Settings {
        let mut s = Settings::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            if key.trim() == "font_size" {
                if let Ok(v) = value.trim().parse::<f32>() {
                    s.font_size = clamp_font_size(v);
                }
            }
        }
        s
    }

    pub fn serialize(&self) -> String {
        format!("font_size = {}\n", self.font_size)
    }

    /// Load from disk; any problem (missing/corrupt file) yields defaults.
    pub fn load() -> Settings {
        path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|t| Settings::parse(&t))
            .unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = path().ok_or_else(|| std::io::Error::other("no config directory available"))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, self.serialize())
    }
}

fn env_dir(var: &str) -> Option<PathBuf> {
    std::env::var_os(var).filter(|v| !v.is_empty()).map(PathBuf::from)
}

fn path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    let dir = env_dir("APPDATA")?.join("AndroidConsole");
    #[cfg(target_os = "macos")]
    let dir = env_dir("HOME")?.join("Library/Application Support/AndroidConsole");
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let dir = env_dir("XDG_CONFIG_HOME")
        .or_else(|| env_dir("HOME").map(|h| h.join(".config")))?
        .join("android-console");
    Some(dir.join("settings.conf"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let s = Settings { font_size: 17.0 };
        assert_eq!(Settings::parse(&s.serialize()), s);
    }

    #[test]
    fn bad_input_falls_back_or_clamps() {
        assert_eq!(Settings::parse("").font_size, DEFAULT_FONT_SIZE);
        assert_eq!(Settings::parse("font_size = abc").font_size, DEFAULT_FONT_SIZE);
        assert_eq!(Settings::parse("font_size = 500").font_size, MAX_FONT_SIZE);
        assert_eq!(Settings::parse("font_size = 1").font_size, MIN_FONT_SIZE);
        assert_eq!(Settings::parse("font_size = NaN").font_size, DEFAULT_FONT_SIZE);
        assert_eq!(Settings::parse("junk\nfont_size=12.5\n").font_size, 12.5);
    }
}
