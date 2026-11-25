//! Turns the visible part of a `Term` into GPUI quads and shaped text lines.

use gpui::{
    fill, outline, point, px, rgb, rgba, size, App, BorderStyle, Bounds, Font, FontStyle,
    FontWeight, Hsla, PaintQuad, Pixels, Point, ShapedLine, SharedString, TextRun, UnderlineStyle,
    Window,
};

use crate::term::{indexed_rgb, Attrs, Cell, Color, Pos, Term};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CursorShape {
    Block,
    Underline,
    Bar,
}

impl CursorShape {
    pub fn next(self) -> Self {
        match self {
            CursorShape::Block => CursorShape::Underline,
            CursorShape::Underline => CursorShape::Bar,
            CursorShape::Bar => CursorShape::Block,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            CursorShape::Block => "Block",
            CursorShape::Underline => "Underline",
            CursorShape::Bar => "Bar",
        }
    }
}

pub struct Theme {
    pub fg: u32,
    pub bg: u32,
    pub cursor: u32,
    pub selection: u32,
}

pub const THEME: Theme = Theme { fg: 0xd4d4d4, bg: 0x0d1117, cursor: 0xf0f6fc, selection: 0x388bfd66 };

#[cfg(target_os = "windows")]
pub const MONO_FONT: &str = "Consolas";
#[cfg(target_os = "macos")]
pub const MONO_FONT: &str = "Menlo";
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub const MONO_FONT: &str = "DejaVu Sans Mono";

pub struct RenderStyle {
    pub font_size: Pixels,
    pub cell_w: Pixels,
    pub cell_h: Pixels,
    pub cursor_shape: CursorShape,
    /// Cursor is drawn at all this frame (blink phase).
    pub cursor_on: bool,
    pub focused: bool,
}

/// Everything needed to paint one frame, built during prepaint.
pub struct Frame {
    quads: Vec<PaintQuad>,
    lines: Vec<(Point<Pixels>, ShapedLine)>,
    cell_h: Pixels,
}

impl Frame {
    pub fn paint(self, window: &mut Window, cx: &mut App) {
        for q in self.quads {
            window.paint_quad(q);
        }
        for (origin, line) in &self.lines {
            // A failed glyph paint only loses that line for one frame.
            let _ = line.paint(*origin, self.cell_h, window, cx);
        }
    }
}

fn resolve(c: Color, is_fg: bool, bold: bool) -> Hsla {
    let hex = |r: u8, g: u8, b: u8| rgb(((r as u32) << 16) | ((g as u32) << 8) | b as u32).into();
    match c {
        Color::Default => rgb(if is_fg { THEME.fg } else { THEME.bg }).into(),
        Color::Indexed(i) => {
            let i = if is_fg && bold && i < 8 { i + 8 } else { i };
            let (r, g, b) = indexed_rgb(i);
            hex(r, g, b)
        }
        Color::Rgb(r, g, b) => hex(r, g, b),
    }
}

#[derive(PartialEq, Clone, Copy)]
struct TextStyle {
    fg: Hsla,
    bold: bool,
    italic: bool,
    underline: bool,
}

struct Resolved {
    text: TextStyle,
    /// `None` = theme background, nothing to paint.
    bg: Option<Hsla>,
}

fn resolve_cell(cell: &Cell, cursor_block: bool) -> Resolved {
    let a = cell.attrs;
    let bold = a.has(Attrs::BOLD);
    let mut fg = resolve(cell.fg, true, bold);
    let mut bg = if cell.bg == Color::Default { None } else { Some(resolve(cell.bg, false, false)) };
    if a.has(Attrs::INVERSE) {
        let new_fg = bg.unwrap_or_else(|| rgb(THEME.bg).into());
        bg = Some(if cell.fg == Color::Default { rgb(THEME.fg).into() } else { fg });
        fg = new_fg;
    }
    if a.has(Attrs::DIM) {
        fg.a *= 0.6;
    }
    if cursor_block {
        bg = Some(rgb(THEME.cursor).into());
        fg = rgb(THEME.bg).into();
    }
    Resolved {
        text: TextStyle { fg, bold, italic: a.has(Attrs::ITALIC), underline: a.has(Attrs::UNDERLINE) },
        bg,
    }
}

pub fn mono_font(bold: bool, italic: bool) -> Font {
    let mut f = gpui::font(MONO_FONT);
    if bold {
        f.weight = FontWeight::BOLD;
    }
    if italic {
        f.style = FontStyle::Italic;
    }
    f
}

/// Measure the monospace cell size for a font size.
pub fn cell_metrics(window: &Window, font_size: Pixels) -> (Pixels, Pixels) {
    let ts = window.text_system();
    let id = ts.resolve_font(&mono_font(false, false));
    let w = ts
        .advance(id, font_size, 'm')
        .map(|s| s.width)
        .unwrap_or(font_size * 0.6);
    (w, (font_size * 1.3).round())
}

pub fn build_frame(
    term: &Term,
    selection: Option<(Pos, Pos)>,
    origin: Point<Pixels>,
    style: &RenderStyle,
    window: &Window,
) -> Frame {
    let mut quads = Vec::new();
    let mut lines = Vec::new();
    let (cell_w, cell_h) = (style.cell_w, style.cell_h);
    let cols = term.cols();

    let (cursor_col, cursor_row) = term.cursor();
    let cursor_here = term.scroll_offset() == 0 && term.cursor_visible();
    let sel = selection.map(|(a, b)| if a <= b { (a, b) } else { (b, a) });

    for row in 0..term.rows() {
        let line = term.viewport_line(row);
        let y = origin.y + cell_h * row as f32;
        let abs = term.abs_line(row);

        // Selection highlight for this row.
        if let Some((s, e)) = sel {
            if abs >= s.line && abs <= e.line {
                let from = if abs == s.line { s.col } else { 0 };
                let to = if abs == e.line { e.col + 1 } else { cols };
                let (from, to) = (from.min(cols), to.min(cols));
                if to > from {
                    quads.push(fill(
                        Bounds::new(
                            point(origin.x + cell_w * from as f32, y),
                            size(cell_w * (to - from) as f32, cell_h),
                        ),
                        rgba(THEME.selection),
                    ));
                }
            }
        }

        let mut text = String::with_capacity(cols);
        let mut runs: Vec<TextRun> = Vec::new();
        let mut cur: Option<(TextStyle, usize)> = None;
        let mut bg_run: Option<(Hsla, usize, usize)> = None; // color, start col, end col (excl.)
        let mut any_ink = false;

        let flush_bg = |run: &mut Option<(Hsla, usize, usize)>, quads: &mut Vec<PaintQuad>| {
            if let Some((c, s, e)) = run.take() {
                quads.push(fill(
                    Bounds::new(
                        point(origin.x + cell_w * s as f32, y),
                        size(cell_w * (e - s) as f32, cell_h),
                    ),
                    c,
                ));
            }
        };

        for col in 0..cols {
            let default = Cell::default();
            let cell = line.get(col).unwrap_or(&default);
            let is_cursor = cursor_here && row == cursor_row && col == cursor_col;
            let block = is_cursor && style.cursor_on && style.focused && style.cursor_shape == CursorShape::Block;
            let r = resolve_cell(cell, block);

            match (r.bg, bg_run.as_mut()) {
                (Some(c), Some(run)) if run.0 == c && run.2 == col => run.2 = col + 1,
                (Some(c), _) => {
                    flush_bg(&mut bg_run, &mut quads);
                    bg_run = Some((c, col, col + 1));
                }
                (None, _) => flush_bg(&mut bg_run, &mut quads),
            }

            let ch = if cell.ch.is_control() { ' ' } else { cell.ch };
            if ch != ' ' || r.text.underline {
                any_ink = true;
            }
            text.push(ch);
            let n = ch.len_utf8();
            match cur.as_mut() {
                Some((s, len)) if *s == r.text => *len += n,
                _ => {
                    if let Some((s, len)) = cur.take() {
                        runs.push(text_run(s, len));
                    }
                    cur = Some((r.text, n));
                }
            }
        }
        flush_bg(&mut bg_run, &mut quads);
        if let Some((s, len)) = cur.take() {
            runs.push(text_run(s, len));
        }
        if any_ink {
            let shaped = window.text_system().shape_line(
                SharedString::from(text),
                style.font_size,
                &runs,
                Some(cell_w),
            );
            lines.push((point(origin.x, y), shaped));
        }
    }

    // Non-block cursor shapes (and the hollow unfocused cursor).
    if cursor_here && (style.cursor_on || !style.focused) {
        let cell = Bounds::new(
            point(origin.x + cell_w * cursor_col as f32, origin.y + cell_h * cursor_row as f32),
            size(cell_w, cell_h),
        );
        let color: Hsla = rgb(THEME.cursor).into();
        if !style.focused {
            quads.push(outline(cell, color, BorderStyle::Solid));
        } else {
            match style.cursor_shape {
                CursorShape::Block => {}
                CursorShape::Underline => quads.push(fill(
                    Bounds::new(point(cell.left(), cell.bottom() - px(2.)), size(cell_w, px(2.))),
                    color,
                )),
                CursorShape::Bar => {
                    quads.push(fill(Bounds::new(cell.origin, size(px(2.), cell_h)), color))
                }
            }
        }
    }

    Frame { quads, lines, cell_h }
}

fn text_run(s: TextStyle, len: usize) -> TextRun {
    TextRun {
        len,
        font: mono_font(s.bold, s.italic),
        color: s.fg,
        background_color: None,
        underline: s.underline.then(|| UnderlineStyle {
            color: Some(s.fg),
            thickness: px(1.),
            wavy: false,
        }),
        strikethrough: None,
    }
}
