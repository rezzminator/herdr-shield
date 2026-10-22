use std::fmt::Write;

use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, CursorShape};

use super::{Size, queries};

fn color(palette: &alacritty_terminal::term::color::Colors, value: Color, prefix: u8) -> String {
    let index = match value {
        Color::Spec(rgb) => {
            return format!("{};2;{};{};{}", prefix, rgb.r, rgb.g, rgb.b);
        }
        Color::Indexed(index) => usize::from(index),
        Color::Named(index) => index as usize,
    };
    let rgb = palette[index].unwrap_or_else(|| queries::color(index));
    format!("{};2;{};{};{}", prefix, rgb.r, rgb.g, rgb.b)
}

pub fn screen(
    term: &Term<queries::Events>,
    size: Size,
    palette: &alacritty_terminal::term::color::Colors,
) -> String {
    // Output-only origin/wrap/insert semantics stay inside the holder model;
    // physical redraw must not inherit them from a previous outer application.
    let mut out = String::from("\x1b[?2026h\x1b[?25l\x1b[?6l\x1b[?7l\x1b[4l\x1b[r\x1b[0m\x1b[H");
    let mut previous = String::new();
    let mut hyperlink = None;
    for row in 0..size.rows {
        let _ = write!(out, "\x1b[{};1H", row + 1);
        for col in 0..size.cols {
            let cell = &term.grid()[Point::new(Line(i32::from(row)), Column(usize::from(col)))];
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            let mut style = format!(
                "0;{};{}",
                color(palette, cell.fg, 38),
                color(palette, cell.bg, 48)
            );
            for (flag, code) in [
                (Flags::BOLD, 1),
                (Flags::DIM, 2),
                (Flags::ITALIC, 3),
                (Flags::INVERSE, 7),
                (Flags::HIDDEN, 8),
                (Flags::STRIKEOUT, 9),
            ] {
                if cell.flags.contains(flag) {
                    let _ = write!(style, ";{code}");
                }
            }
            for (flag, code) in [
                (Flags::UNDERCURL, "4:3"),
                (Flags::DOTTED_UNDERLINE, "4:4"),
                (Flags::DASHED_UNDERLINE, "4:5"),
                (Flags::DOUBLE_UNDERLINE, "4:2"),
                (Flags::UNDERLINE, "4:1"),
            ] {
                if cell.flags.contains(flag) {
                    let _ = write!(style, ";{code}");
                    break;
                }
            }
            if let Some(underline) = cell.underline_color() {
                let _ = write!(style, ";{}", color(palette, underline, 58));
            }
            let link = cell
                .hyperlink()
                .filter(|link| {
                    super::sideband::safe_uri(link.uri())
                        && super::sideband::safe_text(link.id(), 64)
                        && !link.id().contains([';', ':', '='])
                })
                .map(|link| (link.id().to_owned(), link.uri().to_owned()));
            if link != hyperlink {
                match &link {
                    Some((id, uri)) => {
                        let _ = write!(out, "\x1b]8;id={id};{uri}\x1b\\");
                    }
                    None => out.push_str("\x1b]8;;\x1b\\"),
                };
                hyperlink = link;
            }
            if style != previous {
                let _ = write!(out, "\x1b[{style}m");
                previous = style;
            }
            out.push(
                if cell.c.is_control() || cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) {
                    ' '
                } else {
                    cell.c
                },
            );
            if let Some(chars) = cell.zerowidth() {
                for ch in chars {
                    if !ch.is_control() {
                        out.push(*ch);
                    }
                }
            }
        }
    }
    out.push_str("\x1b]8;;\x1b\\\x1b[0m");
    let mode = *term.mode();
    for (flag, code) in [
        (TermMode::APP_CURSOR, 1),
        (TermMode::BRACKETED_PASTE, 2004),
        (TermMode::MOUSE_REPORT_CLICK, 1000),
        (TermMode::MOUSE_DRAG, 1002),
        (TermMode::MOUSE_MOTION, 1003),
        (TermMode::FOCUS_IN_OUT, 1004),
        (TermMode::UTF8_MOUSE, 1005),
        (TermMode::SGR_MOUSE, 1006),
    ] {
        let _ = write!(
            out,
            "\x1b[?{code}{}",
            if mode.contains(flag) { 'h' } else { 'l' }
        );
    }
    let _ = write!(
        out,
        "\x1b[20{}",
        if mode.contains(TermMode::LINE_FEED_NEW_LINE) {
            'h'
        } else {
            'l'
        }
    );
    out.push_str(if mode.contains(TermMode::APP_KEYPAD) {
        "\x1b="
    } else {
        "\x1b>"
    });
    let mut keyboard = 0;
    for (flag, bit) in [
        (TermMode::DISAMBIGUATE_ESC_CODES, 1),
        (TermMode::REPORT_EVENT_TYPES, 2),
        (TermMode::REPORT_ALTERNATE_KEYS, 4),
        (TermMode::REPORT_ALL_KEYS_AS_ESC, 8),
        (TermMode::REPORT_ASSOCIATED_TEXT, 16),
    ] {
        if mode.contains(flag) {
            keyboard |= bit;
        }
    }
    let _ = write!(out, "\x1b[={keyboard}u");
    let cursor = term.renderable_content().cursor;
    let shape = match cursor.shape {
        CursorShape::Beam => 6,
        CursorShape::Underline => 4,
        _ => 2,
    } - u8::from(term.cursor_style().blinking);
    let _ = write!(
        out,
        "\x1b[{shape} q\x1b[{};{}H",
        cursor.point.line.0 + 1,
        cursor.point.column.0 + 1
    );
    if mode.contains(TermMode::SHOW_CURSOR) {
        out.push_str("\x1b[?25h");
    }
    out.push_str("\x1b[?2026l");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::Terminal;
    use alacritty_terminal::index::{Column, Line, Point};

    #[test]
    fn hyperlink_and_advanced_underline_survive_redraw() {
        let mut inner = Terminal::new(Size::new(3, 20).unwrap());
        inner.feed(b"\x1b]8;id=link;https://example.invalid/a\x1b\\\x1b[4:3;58:2::18:52:86mX\x1b]8;;\x1b\\");
        let screen = inner.snapshot();
        assert!(screen.contains("\x1b]8;id=link;https://example.invalid/a\x1b\\"));
        assert!(screen.contains("4:3"));
        assert!(screen.contains("58;2;18;52;86"));
        let mut outer = Terminal::new(inner.size);
        outer.feed(screen.as_bytes());
        let cell = &outer.term.grid()[Point::new(Line(0), Column(0))];
        assert_eq!(cell.c, 'X');
        assert!(cell.flags.contains(Flags::UNDERCURL));
        assert_eq!(
            cell.underline_color(),
            Some(Color::Spec(alacritty_terminal::vte::ansi::Rgb {
                r: 18,
                g: 52,
                b: 86
            }))
        );
        assert_eq!(cell.hyperlink().unwrap().uri(), "https://example.invalid/a");
    }

    #[test]
    fn reconnect_restores_independently_expected_interaction_modes_and_cursor() {
        for mouse in [1000, 1002, 1003] {
            let mut inner = Terminal::new(Size::new(4, 20).unwrap());
            inner.feed(format!("\x1b[?1h\x1b=\x1b[20h\x1b[?2004h\x1b[?{mouse}h\x1b[?1004h\x1b[?1005h\x1b[?1006h\x1b[=31u\x1b[5 q\x1b[3;7H").as_bytes());
            let mut outer = Terminal::new(inner.size);
            outer.feed(inner.snapshot().as_bytes());
            let expected = TermMode::APP_CURSOR
                | TermMode::APP_KEYPAD
                | TermMode::LINE_FEED_NEW_LINE
                | TermMode::BRACKETED_PASTE
                | TermMode::FOCUS_IN_OUT
                | TermMode::SGR_MOUSE
                | TermMode::DISAMBIGUATE_ESC_CODES
                | TermMode::REPORT_EVENT_TYPES
                | TermMode::REPORT_ALTERNATE_KEYS
                | TermMode::REPORT_ALL_KEYS_AS_ESC
                | TermMode::REPORT_ASSOCIATED_TEXT;
            assert!(
                outer.term.mode().contains(expected),
                "mouse {mouse}: {:?}",
                outer.term.mode()
            );
            assert!(outer.term.mode().contains(match mouse {
                1000 => TermMode::MOUSE_REPORT_CLICK,
                1002 => TermMode::MOUSE_DRAG,
                _ => TermMode::MOUSE_MOTION,
            }));
            assert_eq!(
                outer.term.grid().cursor.point,
                Point::new(Line(2), Column(6))
            );
            assert_eq!(outer.term.cursor_style().shape, CursorShape::Beam);
            assert!(outer.term.cursor_style().blinking);
            inner.feed(b"\x1bc");
            outer.feed(inner.snapshot().as_bytes());
            assert!(!outer.term.mode().intersects(expected));
        }
        let mut inner = Terminal::new(Size::new(4, 20).unwrap());
        inner.feed(b"\x1b[?1003h\x1b[?1005h\x1b[?25l");
        let mut outer = Terminal::new(inner.size);
        outer.feed(inner.snapshot().as_bytes());
        assert!(
            outer
                .term
                .mode()
                .contains(TermMode::MOUSE_MOTION | TermMode::UTF8_MOUSE)
        );
        assert!(
            !outer
                .term
                .mode()
                .intersects(TermMode::SGR_MOUSE | TermMode::SHOW_CURSOR)
        );
    }

    #[test]
    fn independently_expected_cells_survive_redraw() {
        let mut inner = Terminal::new(Size::new(4, 12).unwrap());
        inner.feed(b"\x1b[2;3H\x1b[31mHELLO\x1b[?2004h");
        let mut outer = Terminal::new(inner.size);
        outer.feed(inner.snapshot().as_bytes());
        for (offset, expected) in "HELLO".chars().enumerate() {
            assert_eq!(
                outer.term.grid()[Point::new(Line(1), Column(2 + offset))].c,
                expected
            );
        }
        assert!(outer.term.mode().contains(TermMode::BRACKETED_PASTE));
        assert_eq!(
            outer.term.grid().cursor.point,
            Point::new(Line(1), Column(7))
        );
    }

    #[test]
    fn wide_wrap_clears_stale_last_column_on_redraw() {
        let mut inner = Terminal::new(Size::new(3, 4).unwrap());
        inner.feed("abc界".as_bytes());
        let mut outer = Terminal::new(inner.size);
        outer.feed(b"XXXX\r\nYYYY");
        outer.feed(inner.snapshot().as_bytes());
        assert_eq!(outer.term.grid()[Point::new(Line(0), Column(3))].c, ' ');
        assert_eq!(outer.term.grid()[Point::new(Line(1), Column(0))].c, '界');
    }
}
