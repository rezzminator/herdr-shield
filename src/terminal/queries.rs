use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::vte::ansi::Rgb;

use super::Size;

#[derive(Clone, Default)]
pub struct Events(Arc<Mutex<Vec<Event>>>);

impl EventListener for Events {
    fn send_event(&self, event: Event) {
        let mut events = self.0.lock().expect("terminal event mutex poisoned");
        if events.len() < 4096 {
            events.push(event);
        }
    }
}

impl Events {
    pub fn take(&self) -> Vec<Event> {
        std::mem::take(&mut *self.0.lock().expect("terminal event mutex poisoned"))
    }
}

pub fn color(index: usize) -> Rgb {
    const ANSI: [[u8; 3]; 16] = [
        [0, 0, 0],
        [205, 0, 0],
        [0, 205, 0],
        [205, 205, 0],
        [0, 0, 238],
        [205, 0, 205],
        [0, 205, 205],
        [229, 229, 229],
        [127, 127, 127],
        [255, 0, 0],
        [0, 255, 0],
        [255, 255, 0],
        [92, 92, 255],
        [255, 0, 255],
        [0, 255, 255],
        [255, 255, 255],
    ];
    let [r, g, b] = match index {
        0..=15 => ANSI[index],
        16..=231 => {
            let value = index - 16;
            let step = |n: usize| if n == 0 { 0 } else { (55 + n * 40) as u8 };
            [step(value / 36), step((value / 6) % 6), step(value % 6)]
        }
        232..=255 => [(8 + (index - 232) * 10) as u8; 3],
        257 => [0; 3],
        _ => [229; 3],
    };
    Rgb { r, g, b }
}

pub fn reply(
    event: &Event,
    size: Size,
    colors: &alacritty_terminal::term::color::Colors,
) -> Option<String> {
    match event {
        Event::PtyWrite(text) => Some(text.clone()),
        Event::ColorRequest(index, format) => {
            Some(format(colors[*index].unwrap_or_else(|| color(*index))))
        }
        Event::TextAreaSizeRequest(format) => Some(format(WindowSize {
            num_lines: size.rows,
            num_cols: size.cols,
            cell_width: 0,
            cell_height: 0,
        })),
        // Clipboard reads/writes are deliberately denied, including detached output.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::Terminal;

    #[test]
    fn terminal_notifications_cannot_discard_a_later_query() {
        let mut terminal = Terminal::new(Size::new(24, 80).unwrap());
        let mut bytes = vec![7; 4096];
        bytes.extend_from_slice(b"\x1b[c");
        let events = terminal.feed(&bytes);
        let replies: Vec<_> = events
            .iter()
            .filter_map(|event| reply(event, terminal.size, &terminal.palette()))
            .collect();
        assert_eq!(replies, ["\x1b[?6c"]);
    }

    #[test]
    fn color_reply_uses_palette_at_query_time_for_every_chunk_size() {
        let data = b"\x1b]4;1;?\x07\x1b]4;1;rgb:12/34/56\x07\x1b]4;1;?\x07";
        for chunk in 1..=data.len() {
            let mut terminal = Terminal::new(Size::new(24, 80).unwrap());
            let mut replies = Vec::new();
            for bytes in data.chunks(chunk) {
                for event in terminal.feed(bytes) {
                    if let Some(value) = reply(&event, terminal.size, &terminal.palette()) {
                        replies.push(value);
                    }
                }
            }
            assert_eq!(
                replies,
                [
                    "\x1b]4;1;rgb:cdcd/0000/0000\x07",
                    "\x1b]4;1;rgb:1212/3434/5656\x07"
                ],
                "chunk {chunk}"
            );
        }
    }

    #[test]
    fn changed_palette_query_returns_the_rendered_color() {
        let mut terminal = Terminal::new(Size::new(24, 80).unwrap());
        let events = terminal.feed(b"\x1b]4;1;rgb:12/34/56\x07\x1b]4;1;?\x07");
        let replies: Vec<_> = events
            .iter()
            .filter_map(|event| reply(event, terminal.size, terminal.term.colors()))
            .collect();
        assert_eq!(replies, ["\x1b]4;1;rgb:1212/3434/5656\x07"]);
    }

    #[test]
    fn split_queries_have_one_responder_and_never_reach_renderer() {
        let data = b"\x1b[c\x1b[>c\x1b[6n\x1b]10;?\x07\x1b[18t";
        for chunk in 1..data.len() {
            let mut terminal = Terminal::new(Size::new(24, 80).unwrap());
            let mut replies = Vec::new();
            for bytes in data.chunks(chunk) {
                for event in terminal.feed(bytes) {
                    if let Some(value) = reply(&event, terminal.size, terminal.term.colors()) {
                        replies.push(value);
                    }
                }
            }
            assert_eq!(replies.len(), 5, "chunk {chunk}: {replies:?}");
            assert_eq!(replies[2], "\x1b[1;1R");
            assert_eq!(replies[4], "\x1b[8;24;80t");
            let render = terminal.snapshot();
            assert!(!render.contains("\x1b[c"));
            assert!(!render.contains(";?"));
        }
    }
}
