//! Incremental VT100/xterm parser driving a `Term`.

use super::cell::{Attrs, Color};
use super::screen::Term;

#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    #[default]
    Ground,
    Escape,
    /// ESC followed by an intermediate like `(`; swallow one more byte.
    EscIntermediate,
    Csi,
    /// OSC / DCS / etc: ignored until BEL or ST.
    Str,
    StrEsc,
}

#[derive(Default)]
pub struct ParseState {
    mode: Mode,
    csi: Vec<u8>,
}

const MAX_CSI: usize = 64;

impl Term {
    /// Feed raw bytes from the remote side.
    pub fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            match self.parse.mode {
                Mode::Ground => self.ground(b),
                Mode::Escape => self.escape(b),
                Mode::EscIntermediate => self.parse.mode = Mode::Ground,
                Mode::Csi => self.csi_byte(b),
                Mode::Str => match b {
                    0x07 => self.parse.mode = Mode::Ground,
                    0x1b => self.parse.mode = Mode::StrEsc,
                    _ => {}
                },
                Mode::StrEsc => {
                    self.parse.mode = if b == b'\\' { Mode::Ground } else { Mode::Str };
                }
            }
        }
    }

    fn ground(&mut self, b: u8) {
        if !self.utf8.is_empty() {
            if b & 0xC0 == 0x80 {
                self.utf8.push(b);
                let decoded = match std::str::from_utf8(&self.utf8) {
                    Ok(s) => Some(s.chars().collect::<Vec<_>>()),
                    Err(e) if e.error_len().is_none() && self.utf8.len() < 4 => return,
                    Err(_) => Some(vec!['\u{FFFD}']),
                };
                self.utf8.clear();
                for c in decoded.into_iter().flatten() {
                    self.put_char(c);
                }
                return;
            }
            // Truncated sequence: emit replacement, then handle this byte.
            self.utf8.clear();
            self.put_char('\u{FFFD}');
        }
        match b {
            0x1b => self.parse.mode = Mode::Escape,
            0x08 => {
                self.wrap_pending = false;
                self.cx = self.cx.saturating_sub(1);
            }
            0x09 => {
                self.wrap_pending = false;
                self.cx = ((self.cx / 8 + 1) * 8).min(self.cols - 1);
            }
            0x0a..=0x0c => self.linefeed(),
            0x0d => {
                self.wrap_pending = false;
                self.cx = 0;
            }
            0x20..=0x7e => self.put_char(b as char),
            0xc2..=0xf4 => self.utf8.push(b),
            0x80..=0xff => self.put_char('\u{FFFD}'),
            _ => {}
        }
    }

    fn escape(&mut self, b: u8) {
        self.parse.mode = Mode::Ground;
        match b {
            b'[' => {
                self.parse.csi.clear();
                self.parse.mode = Mode::Csi;
            }
            b']' | b'P' | b'X' | b'^' | b'_' => self.parse.mode = Mode::Str,
            b'(' | b')' | b'*' | b'+' | b'#' => self.parse.mode = Mode::EscIntermediate,
            b'7' => self.saved_cursor = (self.cx, self.cy, self.pen),
            b'8' => {
                let (x, y, pen) = self.saved_cursor;
                self.cx = x.min(self.cols - 1);
                self.cy = y.min(self.rows - 1);
                self.pen = pen;
                self.wrap_pending = false;
            }
            b'D' => self.linefeed(),
            b'E' => {
                self.cx = 0;
                self.linefeed();
            }
            b'M' => self.reverse_index(),
            b'c' => self.reset(),
            0x1b => self.parse.mode = Mode::Escape,
            _ => {}
        }
    }

    fn csi_byte(&mut self, b: u8) {
        match b {
            0x40..=0x7e => {
                let buf = std::mem::take(&mut self.parse.csi);
                self.parse.mode = Mode::Ground;
                self.dispatch_csi(&buf, b);
            }
            0x1b => {
                self.parse.csi.clear();
                self.parse.mode = Mode::Escape;
            }
            0x20..=0x3f => {
                if self.parse.csi.len() < MAX_CSI {
                    self.parse.csi.push(b);
                } else {
                    self.parse.csi.clear();
                    self.parse.mode = Mode::Ground;
                }
            }
            _ => {} // C0 controls inside CSI are ignored
        }
    }

    fn dispatch_csi(&mut self, buf: &[u8], fin: u8) {
        let private = buf.first().is_some_and(|c| matches!(c, b'?' | b'>' | b'<' | b'='));
        let body = if private { &buf[1..] } else { buf };
        // Intermediates (0x20..=0x2f) make this a sequence we do not handle.
        if body.iter().any(|c| (0x20..=0x2f).contains(c)) {
            return;
        }
        let params: Vec<usize> = body
            .split(|c| matches!(c, b';' | b':'))
            .map(|p| {
                std::str::from_utf8(p)
                    .ok()
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(0)
                    .min(65535)
            })
            .collect();
        let p = |i: usize, default: usize| match params.get(i) {
            Some(&v) if v != 0 => v,
            _ => default,
        };

        if private {
            if buf[0] == b'?' && (fin == b'h' || fin == b'l') {
                let on = fin == b'h';
                for &mode in &params {
                    match mode {
                        7 => self.autowrap = on,
                        25 => self.cursor_visible = on,
                        47 | 1047 | 1049 => {
                            if on {
                                self.enter_alt_screen()
                            } else {
                                self.leave_alt_screen()
                            }
                        }
                        _ => {}
                    }
                }
            }
            return;
        }

        let (cols, rows) = (self.cols, self.rows);
        match fin {
            b'A' => {
                let floor = if self.cy >= self.top { self.top } else { 0 };
                self.cy = self.cy.saturating_sub(p(0, 1)).max(floor);
                self.wrap_pending = false;
            }
            b'B' | b'e' => {
                let ceil = if self.cy <= self.bot { self.bot } else { rows - 1 };
                self.cy = (self.cy + p(0, 1)).min(ceil);
                self.wrap_pending = false;
            }
            b'C' | b'a' => {
                self.cx = (self.cx + p(0, 1)).min(cols - 1);
                self.wrap_pending = false;
            }
            b'D' => {
                self.cx = self.cx.saturating_sub(p(0, 1));
                self.wrap_pending = false;
            }
            b'E' => {
                self.cy = (self.cy + p(0, 1)).min(rows - 1);
                self.cx = 0;
                self.wrap_pending = false;
            }
            b'F' => {
                self.cy = self.cy.saturating_sub(p(0, 1));
                self.cx = 0;
                self.wrap_pending = false;
            }
            b'G' | b'`' => {
                self.cx = (p(0, 1) - 1).min(cols - 1);
                self.wrap_pending = false;
            }
            b'H' | b'f' => {
                self.cy = (p(0, 1) - 1).min(rows - 1);
                self.cx = (p(1, 1) - 1).min(cols - 1);
                self.wrap_pending = false;
            }
            b'd' => {
                self.cy = (p(0, 1) - 1).min(rows - 1);
                self.wrap_pending = false;
            }
            b'J' => {
                let (x, y) = (self.cx, self.cy);
                match params.first().copied().unwrap_or(0) {
                    0 => {
                        self.erase_cells(y, x, cols);
                        for r in y + 1..rows {
                            self.erase_cells(r, 0, cols);
                        }
                    }
                    1 => {
                        for r in 0..y {
                            self.erase_cells(r, 0, cols);
                        }
                        self.erase_cells(y, 0, x + 1);
                    }
                    2 => {
                        for r in 0..rows {
                            self.erase_cells(r, 0, cols);
                        }
                    }
                    3 => self.clear_scrollback(),
                    _ => {}
                }
            }
            b'K' => {
                let (x, y) = (self.cx, self.cy);
                match params.first().copied().unwrap_or(0) {
                    0 => self.erase_cells(y, x, cols),
                    1 => self.erase_cells(y, 0, x + 1),
                    2 => self.erase_cells(y, 0, cols),
                    _ => {}
                }
            }
            b'L' | b'M' => {
                if self.cy >= self.top && self.cy <= self.bot {
                    let saved_top = self.top;
                    self.top = self.cy;
                    if fin == b'L' {
                        self.scroll_down(p(0, 1));
                    } else {
                        self.scroll_up(p(0, 1));
                    }
                    self.top = saved_top;
                    self.cx = 0;
                    self.wrap_pending = false;
                }
            }
            b'@' => {
                let n = p(0, 1).min(cols - self.cx);
                let blank = self.blank();
                let (x, y) = (self.cx, self.cy);
                let line = &mut self.screen[y];
                line.truncate(cols - n);
                for _ in 0..n {
                    line.insert(x, blank);
                }
            }
            b'P' => {
                let n = p(0, 1).min(cols - self.cx);
                let blank = self.blank();
                let (x, y) = (self.cx, self.cy);
                let line = &mut self.screen[y];
                line.drain(x..x + n);
                line.resize(cols, blank);
            }
            b'X' => {
                let (x, y) = (self.cx, self.cy);
                self.erase_cells(y, x, x + p(0, 1));
            }
            b'S' => self.scroll_up(p(0, 1)),
            b'T' => self.scroll_down(p(0, 1)),
            b'r' => {
                let top = p(0, 1) - 1;
                let bot = p(1, rows).min(rows) - 1;
                if top < bot {
                    self.top = top;
                    self.bot = bot;
                    self.cx = 0;
                    self.cy = 0;
                    self.wrap_pending = false;
                }
            }
            b's' => self.saved_cursor = (self.cx, self.cy, self.pen),
            b'u' => {
                let (x, y, _) = self.saved_cursor;
                self.cx = x.min(cols - 1);
                self.cy = y.min(rows - 1);
                self.wrap_pending = false;
            }
            b'n' => {
                if params.first() == Some(&6) {
                    let reply = format!("\x1b[{};{}R", self.cy + 1, self.cx + 1);
                    self.responses.extend_from_slice(reply.as_bytes());
                }
            }
            b'm' => self.sgr(&params),
            _ => {}
        }
    }

    fn sgr(&mut self, params: &[usize]) {
        let mut i = 0;
        while i < params.len() {
            let v = params[i];
            let attrs = &mut self.pen.attrs;
            match v {
                0 => {
                    self.pen.fg = Color::Default;
                    self.pen.bg = Color::Default;
                    self.pen.attrs = Attrs::default();
                }
                1 => attrs.set(Attrs::BOLD, true),
                2 => attrs.set(Attrs::DIM, true),
                3 => attrs.set(Attrs::ITALIC, true),
                4 => attrs.set(Attrs::UNDERLINE, true),
                7 => attrs.set(Attrs::INVERSE, true),
                22 => {
                    attrs.set(Attrs::BOLD, false);
                    attrs.set(Attrs::DIM, false);
                }
                23 => attrs.set(Attrs::ITALIC, false),
                24 => attrs.set(Attrs::UNDERLINE, false),
                27 => attrs.set(Attrs::INVERSE, false),
                30..=37 => self.pen.fg = Color::Indexed((v - 30) as u8),
                39 => self.pen.fg = Color::Default,
                40..=47 => self.pen.bg = Color::Indexed((v - 40) as u8),
                49 => self.pen.bg = Color::Default,
                90..=97 => self.pen.fg = Color::Indexed((v - 90 + 8) as u8),
                100..=107 => self.pen.bg = Color::Indexed((v - 100 + 8) as u8),
                38 | 48 => {
                    let color = match params.get(i + 1) {
                        Some(5) => params.get(i + 2).map(|&n| {
                            i += 2;
                            Color::Indexed(n.min(255) as u8)
                        }),
                        Some(2) => match (params.get(i + 2), params.get(i + 3), params.get(i + 4)) {
                            (Some(&r), Some(&g), Some(&b)) => {
                                i += 4;
                                Some(Color::Rgb(r.min(255) as u8, g.min(255) as u8, b.min(255) as u8))
                            }
                            _ => {
                                i = params.len();
                                None
                            }
                        },
                        _ => None,
                    };
                    if let Some(c) = color {
                        if v == 38 {
                            self.pen.fg = c;
                        } else {
                            self.pen.bg = c;
                        }
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }
}
