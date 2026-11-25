use super::*;

fn row_text(t: &Term, row: usize) -> String {
    t.viewport_line(row).iter().map(|c| c.ch).collect::<String>().trim_end().to_string()
}

fn cell(t: &Term, x: usize, y: usize) -> Cell {
    t.viewport_line(y)[x]
}

#[test]
fn plain_text_and_crlf() {
    let mut t = Term::new(10, 3, 100);
    t.feed(b"abc\r\ndef");
    assert_eq!(row_text(&t, 0), "abc");
    assert_eq!(row_text(&t, 1), "def");
    assert_eq!(t.cursor(), (3, 1));
}

#[test]
fn sgr_basic_16_colors_and_attrs() {
    let mut t = Term::new(20, 2, 10);
    t.feed(b"\x1b[1;3;4;31;42mX\x1b[0mY\x1b[91;104mZ");
    let x = cell(&t, 0, 0);
    assert_eq!(x.fg, Color::Indexed(1));
    assert_eq!(x.bg, Color::Indexed(2));
    assert!(x.attrs.has(Attrs::BOLD) && x.attrs.has(Attrs::ITALIC) && x.attrs.has(Attrs::UNDERLINE));
    let y = cell(&t, 1, 0);
    assert_eq!((y.fg, y.bg, y.attrs), (Color::Default, Color::Default, Attrs::default()));
    let z = cell(&t, 2, 0);
    assert_eq!((z.fg, z.bg), (Color::Indexed(9), Color::Indexed(12)));
}

#[test]
fn sgr_256_and_truecolor() {
    let mut t = Term::new(20, 2, 10);
    t.feed(b"\x1b[38;5;200;48;5;17mA\x1b[38;2;10;20;30;48;2;250;251;252mB");
    assert_eq!(cell(&t, 0, 0).fg, Color::Indexed(200));
    assert_eq!(cell(&t, 0, 0).bg, Color::Indexed(17));
    assert_eq!(cell(&t, 1, 0).fg, Color::Rgb(10, 20, 30));
    assert_eq!(cell(&t, 1, 0).bg, Color::Rgb(250, 251, 252));
}

#[test]
fn sgr_inverse_and_reset_individual() {
    let mut t = Term::new(20, 2, 10);
    t.feed(b"\x1b[7;1mA\x1b[27;22mB\x1b[mC\x1b[39;49mD");
    assert!(cell(&t, 0, 0).attrs.has(Attrs::INVERSE | Attrs::BOLD));
    assert_eq!(cell(&t, 1, 0).attrs, Attrs::default());
}

#[test]
fn palette_values() {
    assert_eq!(indexed_rgb(16), (0, 0, 0));
    assert_eq!(indexed_rgb(231), (255, 255, 255));
    assert_eq!(indexed_rgb(196), (255, 0, 0));
    assert_eq!(indexed_rgb(232), (8, 8, 8));
    assert_eq!(indexed_rgb(255), (238, 238, 238));
}

#[test]
fn cursor_movement() {
    let mut t = Term::new(10, 5, 10);
    t.feed(b"\x1b[3;4H");
    assert_eq!(t.cursor(), (3, 2));
    t.feed(b"\x1b[2A\x1b[3C");
    assert_eq!(t.cursor(), (6, 0));
    t.feed(b"\x1b[100B\x1b[100D");
    assert_eq!(t.cursor(), (0, 4));
    t.feed(b"\x1b[2;2H\x1b7\x1b[H\x1b8");
    assert_eq!(t.cursor(), (1, 1));
    t.feed(b"\x1b[5G");
    assert_eq!(t.cursor(), (4, 1));
}

#[test]
fn erase_line_and_display() {
    let mut t = Term::new(6, 3, 10);
    t.feed(b"abcdef\r\nghijkl\r\nmnopqr");
    t.feed(b"\x1b[2;3H\x1b[K");
    assert_eq!(row_text(&t, 1), "gh");
    t.feed(b"\x1b[1;3H\x1b[1K");
    assert_eq!(row_text(&t, 0), "   def");
    t.feed(b"\x1b[2J");
    assert_eq!(row_text(&t, 0), "");
    assert_eq!(row_text(&t, 2), "");
}

#[test]
fn line_wrapping() {
    let mut t = Term::new(4, 3, 10);
    t.feed(b"abcdefg");
    assert_eq!(row_text(&t, 0), "abcd");
    assert_eq!(row_text(&t, 1), "efg");
    assert_eq!(t.cursor(), (3, 1));
}

#[test]
fn wrap_is_deferred_until_next_char() {
    let mut t = Term::new(4, 3, 10);
    t.feed(b"abcd");
    assert_eq!(t.cursor(), (3, 0));
    t.feed(b"\r\n");
    assert_eq!(t.cursor(), (0, 1));
    assert_eq!(row_text(&t, 1), "");
}

#[test]
fn backspace_tab_and_delete_chars() {
    let mut t = Term::new(20, 2, 10);
    t.feed(b"ab\x08c\td");
    assert_eq!(row_text(&t, 0), "ac      d");
    t.feed(b"\r\x1b[2P");
    assert_eq!(row_text(&t, 0), "      d");
    t.feed(b"\x1b[2@");
    assert_eq!(row_text(&t, 0), "        d");
}

#[test]
fn utf8_split_across_feeds() {
    let mut t = Term::new(10, 2, 10);
    let s = "é€😀".as_bytes();
    t.feed(&s[..1]);
    t.feed(&s[1..4]);
    t.feed(&s[4..]);
    assert_eq!(row_text(&t, 0), "é€😀");
    t.feed(b"\xff\xc3(");
    assert_eq!(row_text(&t, 0), "é€😀\u{FFFD}\u{FFFD}(");
}

#[test]
fn osc_and_charset_sequences_are_swallowed() {
    let mut t = Term::new(20, 2, 10);
    t.feed(b"\x1b]0;title\x07a\x1b]2;x\x1b\\b\x1b(Bc");
    assert_eq!(row_text(&t, 0), "abc");
}

#[test]
fn malformed_csi_does_not_hang_or_panic() {
    let mut t = Term::new(20, 2, 10);
    let mut junk = b"\x1b[".to_vec();
    junk.extend(std::iter::repeat(b'1').take(500));
    junk.extend_from_slice(b"Zok");
    t.feed(&junk);
    t.feed(b"\x1b[999999999999;0;;;H\x1b[38;2;1mq");
    assert!(t.cursor().0 < 20);
}

#[test]
fn scrollback_collects_scrolled_lines() {
    let mut t = Term::new(5, 3, 100);
    for i in 0..6 {
        t.feed(format!("l{i}\r\n").as_bytes());
    }
    // 6 lines + cursor line on a 3-row screen => 4 lines scrolled off.
    assert_eq!(t.scrollback_len(), 4);
    t.scroll_view(2);
    assert_eq!(t.scroll_offset(), 2);
    assert_eq!(row_text(&t, 0), "l2");
    t.scroll_to_bottom();
    assert_eq!(row_text(&t, 0), "l4");
}

#[test]
fn scrollback_is_bounded_and_view_clamped() {
    let mut t = Term::new(5, 2, 10);
    for i in 0..100 {
        t.feed(format!("{i}\r\n").as_bytes());
    }
    assert_eq!(t.scrollback_len(), 10);
    t.scroll_view(1000);
    assert_eq!(t.scroll_offset(), 10);
    t.scroll_view(-1000);
    assert_eq!(t.scroll_offset(), 0);
}

#[test]
fn zero_scrollback_keeps_nothing() {
    let mut t = Term::new(5, 2, 0);
    t.feed(b"a\r\nb\r\nc\r\nd\r\n");
    assert_eq!(t.scrollback_len(), 0);
}

#[test]
fn pinned_to_bottom_follows_output_but_scrolled_view_stays_anchored() {
    let mut t = Term::new(5, 2, 100);
    for i in 0..10 {
        t.feed(format!("{i}\r\n").as_bytes());
    }
    assert_eq!(t.scroll_offset(), 0);
    t.scroll_view(3);
    let top_before = row_text(&t, 0);
    t.feed(b"x\r\ny\r\nz\r\n");
    assert_eq!(row_text(&t, 0), top_before);
    assert_eq!(t.scroll_offset(), 6);
}

#[test]
fn scroll_region_does_not_feed_scrollback() {
    let mut t = Term::new(5, 4, 100);
    t.feed(b"\x1b[2;3r\x1b[3;1Ha\r\nb\r\nc");
    assert_eq!(t.scrollback_len(), 0);
}

#[test]
fn alt_screen_restores_main() {
    let mut t = Term::new(6, 3, 100);
    t.feed(b"main");
    t.feed(b"\x1b[?1049hALT\r\n\r\n\r\n\r\n");
    assert_eq!(t.scrollback_len(), 0);
    assert!(t.in_alt_screen());
    t.feed(b"\x1b[?1049l");
    assert_eq!(row_text(&t, 0), "main");
    assert_eq!(t.cursor(), (4, 0));
}

#[test]
fn resize_keeps_content_and_clamps_cursor() {
    let mut t = Term::new(10, 4, 100);
    t.feed(b"a\r\nb\r\nc\r\nd");
    t.resize(5, 2);
    assert_eq!((t.cols(), t.rows()), (5, 2));
    assert_eq!(row_text(&t, 1), "d");
    assert_eq!(t.scrollback_len(), 2);
    t.resize(20, 6);
    assert!(t.cursor().1 < 6);
}

#[test]
fn cursor_position_report() {
    let mut t = Term::new(10, 4, 10);
    t.feed(b"\x1b[3;5H\x1b[6n");
    assert_eq!(t.take_responses(), b"\x1b[3;5R");
    assert!(t.take_responses().is_empty());
}

#[test]
fn selection_text_across_scrollback() {
    let mut t = Term::new(8, 2, 100);
    for w in ["alpha", "beta", "gamma", "delta"] {
        t.feed(format!("{w}\r\n").as_bytes());
    }
    t.scroll_view(100);
    let a = Pos { line: t.abs_line(0), col: 2 };
    let b = Pos { line: t.abs_line(1), col: 1 };
    assert_eq!(t.text_range(b, a), "pha\nbe");
}

#[test]
fn selection_survives_scrollback_drop() {
    let mut t = Term::new(8, 2, 3);
    t.feed(b"one\r\n");
    let p = Pos { line: t.abs_line(0), col: 0 };
    for i in 0..20 {
        t.feed(format!("{i}\r\n").as_bytes());
    }
    // The selected line has been dropped; must not panic.
    assert_eq!(t.text_range(p, p), "");
}

#[test]
fn clear_all_resets_everything() {
    let mut t = Term::new(5, 2, 10);
    t.feed(b"a\r\nb\r\nc\r\nd");
    t.clear_all();
    assert_eq!(t.scrollback_len(), 0);
    assert_eq!(row_text(&t, 0), "");
    assert_eq!(t.cursor(), (0, 0));
}
