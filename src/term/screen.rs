//! Terminal screen grid, cursor, scroll region and bounded scrollback.

use std::collections::VecDeque;

use super::cell::{Cell, Color};
use super::parser::ParseState;

/// Absolute position in the line history. `line` keeps pointing at the same
/// logical line even while old scrollback lines are dropped from the front.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
    pub line: u64,
    pub col: usize,
}

type Line = Vec<Cell>;

pub struct Term {
    pub(super) cols: usize,
    pub(super) rows: usize,
    pub(super) screen: VecDeque<Line>,
    saved_main: Option<VecDeque<Line>>,
    scrollback: VecDeque<Line>,
    max_scrollback: usize,
    /// Number of lines ever dropped from the front of the scrollback.
    dropped: u64,
    pub(super) cx: usize,
    pub(super) cy: usize,
    pub(super) wrap_pending: bool,
    pub(super) autowrap: bool,
    /// Current drawing pen (only fg/bg/attrs are meaningful).
    pub(super) pen: Cell,
    pub(super) saved_cursor: (usize, usize, Cell),
    pub(super) top: usize,
    pub(super) bot: usize,
    scroll_offset: usize,
    pub(super) cursor_visible: bool,
    pub(super) parse: ParseState,
    pub(super) utf8: Vec<u8>,
    pub(super) responses: Vec<u8>,
}

impl Term {
    pub fn new(cols: usize, rows: usize, max_scrollback: usize) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        Term {
            cols,
            rows,
            screen: (0..rows).map(|_| vec![Cell::default(); cols]).collect(),
            saved_main: None,
            scrollback: VecDeque::new(),
            max_scrollback,
            dropped: 0,
            cx: 0,
            cy: 0,
            wrap_pending: false,
            autowrap: true,
            pen: Cell::default(),
            saved_cursor: (0, 0, Cell::default()),
            top: 0,
            bot: rows - 1,
            scroll_offset: 0,
            cursor_visible: true,
            parse: ParseState::default(),
            utf8: Vec::new(),
            responses: Vec::new(),
        }
    }

    pub fn cols(&self) -> usize {
        self.cols
    }
    pub fn rows(&self) -> usize {
        self.rows
    }
    pub fn cursor(&self) -> (usize, usize) {
        (self.cx, self.cy)
    }
    pub fn cursor_visible(&self) -> bool {
        self.cursor_visible
    }
    pub fn in_alt_screen(&self) -> bool {
        self.saved_main.is_some()
    }
    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }
    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    /// Bytes the terminal wants to send back to the host (e.g. cursor reports).
    pub fn take_responses(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.responses)
    }

    // ---- scrollback viewing -------------------------------------------

    /// Scroll the view by `delta` lines; positive = back into history.
    pub fn scroll_view(&mut self, delta: isize) {
        let cur = self.scroll_offset as isize;
        let max = self.scrollback.len() as isize;
        self.scroll_offset = (cur + delta).clamp(0, max) as usize;
    }
    pub fn scroll_to_offset(&mut self, offset: usize) {
        self.scroll_offset = offset.min(self.scrollback.len());
    }
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
    }

    /// Line shown at viewport row `row` (0 = top of the viewport).
    pub fn viewport_line(&self, row: usize) -> &[Cell] {
        self.line_at(self.viewport_index(row))
    }

    fn viewport_index(&self, row: usize) -> usize {
        self.scrollback.len() - self.scroll_offset + row
    }

    fn line_at(&self, idx: usize) -> &[Cell] {
        let sb = self.scrollback.len();
        if idx < sb {
            &self.scrollback[idx]
        } else {
            self.screen.get(idx - sb).map(|l| l.as_slice()).unwrap_or(&[])
        }
    }

    /// Absolute line id for viewport row `row`.
    pub fn abs_line(&self, row: usize) -> u64 {
        self.dropped + self.viewport_index(row) as u64
    }

    /// Extract text between two positions (inclusive), in any order.
    pub fn text_range(&self, a: Pos, b: Pos) -> String {
        let (s, e) = if a <= b { (a, b) } else { (b, a) };
        if e.line < self.dropped {
            // Entire selection has scrolled out of history.
            return String::new();
        }
        let total = (self.scrollback.len() + self.screen.len()) as u64;
        let to_idx = |l: u64| l.saturating_sub(self.dropped).min(total.saturating_sub(1)) as usize;
        let (si, ei) = (to_idx(s.line), to_idx(e.line));
        let mut out = String::new();
        for idx in si..=ei {
            let line = self.line_at(idx);
            let from = if idx == si && s.line >= self.dropped { s.col } else { 0 };
            let to = if idx == ei { e.col.saturating_add(1) } else { line.len() };
            let to = to.min(line.len());
            let text: String = line
                .get(from.min(to)..to)
                .unwrap_or(&[])
                .iter()
                .map(|c| c.ch)
                .collect();
            out.push_str(text.trim_end());
            if idx != ei {
                out.push('\n');
            }
        }
        out
    }

    // ---- editing primitives (used by the parser) ----------------------

    pub(super) fn blank(&self) -> Cell {
        Cell { ch: ' ', fg: Color::Default, bg: self.pen.bg, attrs: Default::default() }
    }

    fn blank_line(&self) -> Line {
        vec![self.blank(); self.cols]
    }

    fn push_scrollback(&mut self, line: Line) {
        if self.max_scrollback == 0 {
            return;
        }
        self.scrollback.push_back(line);
        if self.scrollback.len() > self.max_scrollback {
            self.scrollback.pop_front();
            self.dropped += 1;
        } else if self.scroll_offset > 0 {
            // Keep the user's view anchored on the same content.
            self.scroll_offset += 1;
        }
        self.scroll_offset = self.scroll_offset.min(self.scrollback.len());
    }

    pub(super) fn scroll_up(&mut self, n: usize) {
        let n = n.min(self.bot + 1 - self.top);
        for _ in 0..n {
            if let Some(line) = self.screen.remove(self.top) {
                if self.top == 0 && !self.in_alt_screen() {
                    self.push_scrollback(line);
                }
            }
            let blank = self.blank_line();
            self.screen.insert(self.bot, blank);
        }
    }

    pub(super) fn scroll_down(&mut self, n: usize) {
        let n = n.min(self.bot + 1 - self.top);
        for _ in 0..n {
            self.screen.remove(self.bot);
            let blank = self.blank_line();
            self.screen.insert(self.top, blank);
        }
    }

    pub(super) fn linefeed(&mut self) {
        self.wrap_pending = false;
        if self.cy == self.bot {
            self.scroll_up(1);
        } else if self.cy + 1 < self.rows {
            self.cy += 1;
        }
    }

    pub(super) fn reverse_index(&mut self) {
        self.wrap_pending = false;
        if self.cy == self.top {
            self.scroll_down(1);
        } else if self.cy > 0 {
            self.cy -= 1;
        }
    }

    pub(super) fn put_char(&mut self, ch: char) {
        if self.wrap_pending {
            self.cx = 0;
            self.linefeed();
        }
        let mut cell = self.pen;
        cell.ch = ch;
        self.screen[self.cy][self.cx] = cell;
        if self.cx + 1 < self.cols {
            self.cx += 1;
        } else if self.autowrap {
            self.wrap_pending = true;
        }
    }

    pub(super) fn erase_cells(&mut self, row: usize, from: usize, to: usize) {
        let blank = self.blank();
        let to = to.min(self.cols);
        for c in from.min(to)..to {
            self.screen[row][c] = blank;
        }
    }

    pub(super) fn clear_scrollback(&mut self) {
        self.dropped += self.scrollback.len() as u64;
        self.scrollback.clear();
        self.scroll_offset = 0;
    }

    pub(super) fn enter_alt_screen(&mut self) {
        if self.saved_main.is_none() {
            let blank: VecDeque<Line> =
                (0..self.rows).map(|_| vec![Cell::default(); self.cols]).collect();
            self.saved_main = Some(std::mem::replace(&mut self.screen, blank));
            self.saved_cursor = (self.cx, self.cy, self.pen);
            self.scroll_offset = 0;
        }
    }

    pub(super) fn leave_alt_screen(&mut self) {
        if let Some(main) = self.saved_main.take() {
            self.screen = main;
            let (x, y, pen) = self.saved_cursor;
            self.cx = x.min(self.cols - 1);
            self.cy = y.min(self.rows - 1);
            self.pen = pen;
            self.wrap_pending = false;
        }
    }

    /// Full reset (RIS). Scrollback is kept.
    pub(super) fn reset(&mut self) {
        let (cols, rows, max) = (self.cols, self.rows, self.max_scrollback);
        let sb = std::mem::take(&mut self.scrollback);
        let dropped = self.dropped;
        *self = Term::new(cols, rows, max);
        self.scrollback = sb;
        self.dropped = dropped;
    }

    /// Wipe screen and scrollback (UI "clear" action).
    pub fn clear_all(&mut self) {
        self.clear_scrollback();
        for r in 0..self.screen.len() {
            self.screen[r] = vec![Cell::default(); self.cols];
        }
        self.cx = 0;
        self.cy = 0;
        self.wrap_pending = false;
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        if cols == self.cols && rows == self.rows {
            return;
        }
        let in_alt = self.in_alt_screen();
        // Shrinking rows: push top lines into history while the cursor
        // would fall off the bottom, otherwise trim from the bottom.
        while self.screen.len() > rows {
            if self.cy + 1 >= self.screen.len() && !in_alt {
                if let Some(line) = self.screen.pop_front() {
                    self.push_scrollback(line);
                }
                self.cy = self.cy.saturating_sub(1);
            } else {
                self.screen.pop_back();
            }
        }
        while self.screen.len() < rows {
            self.screen.push_back(vec![Cell::default(); cols]);
        }
        for line in self.screen.iter_mut() {
            line.resize(cols, Cell::default());
        }
        if let Some(main) = self.saved_main.as_mut() {
            main.truncate(rows);
            while main.len() < rows {
                main.push_back(vec![Cell::default(); cols]);
            }
            for line in main.iter_mut() {
                line.resize(cols, Cell::default());
            }
        }
        self.cols = cols;
        self.rows = rows;
        self.top = 0;
        self.bot = rows - 1;
        self.cx = self.cx.min(cols - 1);
        self.cy = self.cy.min(rows - 1);
        self.wrap_pending = false;
        self.scroll_offset = self.scroll_offset.min(self.scrollback.len());
    }
}
