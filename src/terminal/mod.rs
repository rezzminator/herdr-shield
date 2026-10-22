pub mod queries;
pub mod render;
pub mod sideband;

use alacritty_terminal::event::Event;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::{Config, Osc52, Term};
use alacritty_terminal::vte::ansi;

use crate::{Result, fail};
use queries::Events;

#[derive(Clone, Copy)]
pub struct Size {
    pub rows: u16,
    pub cols: u16,
}

impl Size {
    pub fn new(rows: u64, cols: u64) -> Result<Self> {
        if rows == 0 || cols == 0 || rows > 160 || cols > 320 {
            return fail("invalid terminal size (1..160 rows, 1..320 columns)");
        }
        Ok(Self {
            rows: rows as u16,
            cols: cols as u16,
        })
    }
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        usize::from(self.rows)
    }
    fn screen_lines(&self) -> usize {
        usize::from(self.rows)
    }
    fn columns(&self) -> usize {
        usize::from(self.cols)
    }
}

pub struct Terminal {
    pub term: Term<Events>,
    processor: ansi::Processor,
    pub size: Size,
    pub revision: u64,
    pub events: Events,
    pub sideband: sideband::Sideband,
    strings: sideband::Strings,
    pub theme: alacritty_terminal::term::color::Colors,
    pub theme_policy: String,
    pub query_error: Option<String>,
}

impl Terminal {
    pub fn new(size: Size) -> Self {
        let events = Events::default();
        let config = Config {
            scrolling_history: 1000,
            osc52: Osc52::Disabled,
            kitty_keyboard: true,
            ..Config::default()
        };
        Self {
            term: Term::new(config, &size, events.clone()),
            processor: ansi::Processor::new(),
            size,
            revision: 1,
            events,
            sideband: sideband::Sideband::default(),
            strings: sideband::Strings::default(),
            theme: Default::default(),
            theme_policy: "last/default palette; outer colors not yet observed".into(),
            query_error: None,
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Event> {
        let mut events = Vec::new();
        for byte in bytes {
            if let Some(cooked) = self.strings.byte(*byte) {
                self.processor.advance(&mut self.term, &cooked);
                if cooked == b"\x1bc" {
                    self.sideband.reset();
                }
                let point = self.term.grid().cursor.point;
                self.sideband.control(&cooked, point.line.0, point.column.0);
                for event in self.events.take() {
                    // Resolve colors before later bytes can change the query's palette.
                    if matches!(event, Event::ColorRequest(..)) {
                        if let Some(reply) = queries::reply(&event, self.size, &self.palette()) {
                            events.push(Event::PtyWrite(reply));
                        }
                    } else if !matches!(event, Event::Bell | Event::Wakeup) {
                        events.push(event);
                    }
                }
            }
        }
        self.revision = self.revision.wrapping_add(1);
        events
    }

    pub fn resize(&mut self, size: Size) {
        self.term.resize(size);
        self.size = size;
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn snapshot(&self) -> String {
        render::screen(&self.term, self.size, &self.palette())
    }

    pub fn palette(&self) -> alacritty_terminal::term::color::Colors {
        let mut palette = self.theme;
        for index in 0..269 {
            if self.term.colors()[index].is_some() {
                palette[index] = self.term.colors()[index];
            }
        }
        palette
    }
    pub fn set_theme(&mut self, colors: Vec<(usize, [u8; 3])>) -> Result<()> {
        if colors.is_empty() {
            if self.theme_policy.starts_with("last/default") {
                self.theme_policy =
                    "outer theme query unavailable; retaining last/default palette".into();
            }
            return Ok(());
        }
        if colors.len() > 269 || colors.iter().any(|(index, _)| *index >= 269) {
            return fail("invalid theme color bounds");
        }
        for (index, [r, g, b]) in colors {
            self.theme[index] = Some(ansi::Rgb { r, g, b });
        }
        self.theme_policy = "validated attachment colors merged with retained/default palette; retained while detached".into();
        self.revision += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::index::{Column, Line, Point};
    use alacritty_terminal::term::TermMode;

    #[test]
    fn terminal_unicode_alternate_reset_resize_and_modes() {
        let mut term = Terminal::new(Size::new(4, 12).unwrap());
        term.feed("ab\rZ\n界e\u{301}".as_bytes());
        assert_eq!(term.term.grid()[Point::new(Line(0), Column(0))].c, 'Z');
        assert_eq!(term.term.grid()[Point::new(Line(1), Column(1))].c, '界');
        term.feed(b"\x1b[?1049hALT\x1b[?2004h\x1b[?1006h\x1b[?1002h");
        assert!(term.term.mode().contains(
            TermMode::ALT_SCREEN
                | TermMode::BRACKETED_PASTE
                | TermMode::SGR_MOUSE
                | TermMode::MOUSE_DRAG
        ));
        term.feed(b"\x1b[?1049l");
        assert_eq!(term.term.grid()[Point::new(Line(0), Column(0))].c, 'Z');
        term.resize(Size::new(5, 20).unwrap());
        term.feed(b"\x1bc");
        assert_eq!(term.term.grid()[Point::new(Line(0), Column(0))].c, ' ');
    }

    #[test]
    fn arbitrary_byte_splitting_and_flood_have_bounded_history() {
        let corpus = "\x1b[31m界e\u{301}\x1b[0m\r\n\x1b[2;3HXY\x1b[1@Z".as_bytes();
        let mut whole = Terminal::new(Size::new(8, 20).unwrap());
        whole.feed(corpus);
        for chunk in 1..=corpus.len() {
            let mut split = Terminal::new(Size::new(8, 20).unwrap());
            for bytes in corpus.chunks(chunk) {
                split.feed(bytes);
            }
            assert_eq!(whole.snapshot(), split.snapshot());
        }
        for _ in 0..10000 {
            whole.feed(b"line\r\n");
        }
        assert!(whole.term.grid().total_lines() <= 1008);
        assert!(whole.snapshot().len() < 200000);
    }

    #[test]
    fn reset_clears_reconnect_title_and_theme_is_one_query_render_palette() {
        let mut terminal = Terminal::new(Size::new(4, 20).unwrap());
        terminal.feed(b"\x1b]2;obsolete-title\x07");
        terminal.feed(b"\x1bc");
        assert!(
            !terminal.sideband.since(None)["data"]
                .as_str()
                .unwrap()
                .contains("obsolete-title")
        );
        terminal
            .set_theme(vec![(1, [18, 52, 86]), (256, [11, 22, 33])])
            .unwrap();
        let events = terminal.feed(b"\x1b[31mX\x1b]4;1;?\x07\x1b]10;?\x07");
        let replies: Vec<_> = events
            .iter()
            .filter_map(|event| queries::reply(event, terminal.size, &terminal.palette()))
            .collect();
        assert_eq!(
            replies,
            [
                "\x1b]4;1;rgb:1212/3434/5656\x07",
                "\x1b]10;rgb:0b0b/1616/2121\x07"
            ]
        );
        assert!(terminal.snapshot().contains("38;2;18;52;86"));
        terminal.feed(b"\x1b]4;1;rgb:ff/ee/dd\x07");
        terminal.set_theme(vec![(1, [0, 0, 0])]).unwrap();
        assert!(
            terminal.snapshot().contains("38;2;255;238;221"),
            "application palette override must survive theme refresh"
        );
    }

    #[test]
    fn fixed_vt_edit_scroll_origin_wrap_tab_and_saved_cursor_cells() {
        let cases: &[(&[u8], &[&str])] = &[
            (b"abc\rZ\nQ\x08R", &["Zbc", " R"]),
            (b"abcdef\r\x1b[2C\x1b[2@XY\x1b[P", &["abXYdef"]),
            (b"abcdef\r\x1b[2C\x1b[3X", &["ab   f"]),
            (b"ab\tZ", &["ab      Z"]),
            (b"\x1b[2;3HX\x1b7\x1b[Htop\x1b8Y", &["top", "  XY"]),
            (
                b"\x1b[1;1HA\x1b[2;1HB\x1b[3;1HC\x1b[4;1HD\x1b[2;3r\x1b[3;1H\n",
                &["A", "C", "", "D"],
            ),
            (b"\x1b[2;4r\x1b[?6h\x1b[1;1HX", &["", "X"]),
            (b"\x1b[?7l\x1b[1;11HXYZ", &["          XZ"]),
        ];
        for (bytes, lines) in cases {
            let mut inner = Terminal::new(Size::new(4, 12).unwrap());
            inner.feed(bytes);
            let mut outer = Terminal::new(inner.size);
            outer.feed(b"\x1b[4h\x1b[2;3r\x1b[?6h");
            outer.feed(inner.snapshot().as_bytes());
            for row in 0..4 {
                let expected = format!("{:<12}", lines.get(row).unwrap_or(&""));
                let actual = (0..12)
                    .map(|col| outer.term.grid()[Point::new(Line(row as i32), Column(col))].c)
                    .collect::<String>();
                assert_eq!(actual, expected, "input {bytes:?}, row {row}");
            }
        }
    }
}
