use base64::{
    Engine, alphabet,
    engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig},
};
use serde_json::{Value, json};
use std::collections::VecDeque;

#[derive(Default)]
pub struct Strings {
    pending: Vec<u8>,
    kind: u8,
    escaped: bool,
    overflow: bool,
    length: usize,
    /// The last string dropped for its length, shown in holder status.
    pub discarded: Option<String>,
}
impl Strings {
    pub fn byte(&mut self, byte: u8) -> Option<Vec<u8>> {
        if self.kind > 1 {
            self.length += 1;
        }
        if self.kind == 0 {
            if byte == 27 {
                self.kind = 1;
                self.pending.push(byte);
                return None;
            }
            return Some(vec![byte]);
        }
        if self.kind == 1 {
            self.pending.push(byte);
            if matches!(byte, b']' | b'P' | b'_' | b'^' | b'X') {
                self.kind = byte;
                self.length = 2;
                return None;
            }
            self.kind = 0;
            return Some(std::mem::take(&mut self.pending));
        }
        let done = (self.kind == b']' && byte == 7) || (self.escaped && byte == b'\\');
        if matches!(byte, 24 | 26) {
            self.pending.clear();
            self.kind = 0;
            self.overflow = false;
            self.escaped = false;
            return None;
        }
        if !self.overflow {
            let limit = if self.pending.starts_with(CLIPBOARD) {
                CLIPBOARD_MAX
            } else {
                4096
            };
            if self.pending.len() < limit {
                self.pending.push(byte);
            } else {
                self.pending.clear();
                self.overflow = true;
            }
        }
        self.escaped = byte == 27;
        if done {
            if self.overflow {
                let kind = match self.kind {
                    b']' => "OSC",
                    b'P' => "DCS",
                    b'_' => "APC",
                    b'^' => "PM",
                    _ => "SOS",
                };
                self.discarded = Some(format!(
                    "discarded an over-long {kind} string of {} bytes",
                    self.length
                ));
            }
            self.kind = 0;
            self.escaped = false;
            let result = (!self.overflow).then(|| std::mem::take(&mut self.pending));
            self.overflow = false;
            return result;
        }
        None
    }
}

const CLIPBOARD: &[u8] = b"\x1b]52;";
/// Bounds one clipboard write, the one event a snapshot frame carries this large.
const CLIPBOARD_MAX: usize = 64 * 1024;

/// Standard base64, padded or not and with any trailing bits, as terminals
/// accept it in OSC 52.
const CLIPBOARD_BASE64: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new()
        .with_decode_padding_mode(DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true),
);

/// An OSC 52 clipboard write (never a `?` read): selection targets, empty
/// meaning xterm's `s0`, and a non-empty payload that decodes as base64.
fn clipboard_write(value: &str) -> bool {
    value.split_once(';').is_some_and(|(targets, data)| {
        targets.len() <= 8
            && targets.bytes().all(|byte| b"cpqs01234567".contains(&byte))
            && !data.is_empty()
            && CLIPBOARD_BASE64.decode(data).is_ok()
    })
}

#[derive(Default)]
pub struct Sideband {
    pub sequence: u64,
    events: VecDeque<(u64, String)>,
    cwd: Option<String>,
    title: Option<String>,
    /// The newest clipboard write and its sequence, replayed only across a gap inside one attachment.
    clipboard: Option<(u64, String)>,
}
pub fn safe_text(text: &str, max: usize) -> bool {
    text.len() <= max && !text.chars().any(char::is_control)
}
pub fn safe_uri(text: &str) -> bool {
    safe_text(text, 2048)
        && !text.as_bytes().windows(3).any(|part| {
            part[0] == b'%'
                && std::str::from_utf8(&part[1..])
                    .ok()
                    .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                    .is_some_and(|value| value < 32 || value == 127)
        })
        && ["https://", "http://", "file://", "mailto:"]
            .iter()
            .any(|prefix| text.starts_with(prefix))
}
impl Sideband {
    pub fn reset(&mut self) {
        self.cwd = None;
        self.clipboard = None;
        self.title = Some(String::new());
        self.sequence += 1;
        self.events.clear();
        self.events
            .push_back((self.sequence, "\x1b]2;\x1b\\".into()));
    }
    pub fn control(&mut self, bytes: &[u8], row: i32, col: usize) {
        let Some(body) = bytes.strip_prefix(b"\x1b]").and_then(|bytes| {
            bytes
                .strip_suffix(b"\x07")
                .or_else(|| bytes.strip_suffix(b"\x1b\\"))
        }) else {
            return;
        };
        let Ok(body) = std::str::from_utf8(body) else {
            return;
        };
        let Some((code, value)) = body.split_once(';') else {
            return;
        };
        let approved = match code {
            "0" | "2" if safe_text(value, 1024) => {
                self.title = Some(value.into());
                Some(format!("\x1b]2;{value}\x1b\\"))
            }
            "7" if value.starts_with("file://") && safe_uri(value) => {
                self.cwd = Some(value.into());
                Some(format!("\x1b]7;{value}\x1b\\"))
            }
            "133"
                if matches!(value, "A" | "B" | "C" | "D")
                    || value
                        .strip_prefix("D;")
                        .is_some_and(|code| code.len() <= 11 && code.parse::<i32>().is_ok()) =>
            {
                Some(format!(
                    "\x1b[{};{}H\x1b]133;{value}\x1b\\",
                    row.max(0) + 1,
                    col + 1
                ))
            }
            // A live write reaches the attached pane, as it would unwrapped. since()
            // replays the newest write only across a gap inside the attachment that
            // has not yet received it, never on reconnect, so detached output cannot
            // reach the outer clipboard. A newer write supersedes one not yet delivered;
            // the older event stays, emptied, so its sequence never reads as a gap.
            "52" if clipboard_write(value) => {
                for (_, event) in &mut self.events {
                    if event.as_bytes().starts_with(CLIPBOARD) {
                        event.clear();
                    }
                }
                Some(format!("\x1b]52;{value}\x1b\\"))
            }
            _ => None,
        };
        if let Some(approved) = approved {
            self.sequence += 1;
            if approved.as_bytes().starts_with(CLIPBOARD) {
                self.clipboard = Some((self.sequence, approved.clone()));
            }
            self.events.push_back((self.sequence, approved));
            if self.events.len() > 256 {
                self.events.pop_front();
            }
        }
    }
    pub fn since(&self, after: Option<u64>) -> Value {
        let gap = after.is_some_and(|after| {
            self.events
                .front()
                .is_some_and(|(sequence, _)| after.saturating_add(1) < *sequence)
                || after > self.sequence
        });
        let data = if after.is_none() || gap {
            [
                self.cwd
                    .as_ref()
                    .map(|value| format!("\x1b]7;{value}\x1b\\")),
                self.title
                    .as_ref()
                    .map(|value| format!("\x1b]2;{value}\x1b\\")),
                // A write this attachment has not received survives a gap; a reconnect
                // (no `after`) never replays one.
                self.clipboard
                    .as_ref()
                    .filter(|(sequence, _)| after.is_some_and(|after| *sequence > after))
                    .map(|(_, value)| value.clone()),
            ]
            .into_iter()
            .flatten()
            .collect::<String>()
        } else {
            self.events
                .iter()
                .filter(|(sequence, _)| Some(*sequence) > after)
                .map(|(_, value)| value.as_str())
                .collect()
        };
        json!({"sequence":self.sequence,"gap":gap,"data":data})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn audited_order_and_reconnect_do_not_replay_historic_prompt_markers() {
        let mut sideband = Sideband::default();
        for bytes in [
            b"\x1b]7;file:///work\x07".as_slice(),
            b"\x1b]133;A\x07",
            b"\x1b]2;title\x1b\\",
            b"\x1b]133;C\x07",
            b"\x1b]52;c;T0xE\x07",
            b"\x1b]52;c;?\x07",
            b"\x1b]52;c;not base64!\x07",
            b"\x1b]52;c;U0VDUkVU\x1b\\",
        ] {
            sideband.control(bytes, 1, 2);
        }
        let live = sideband.since(Some(0));
        let text = live["data"].as_str().unwrap();
        assert!(text.find("133;A").unwrap() < text.find("133;C").unwrap());
        // A live clipboard write reaches the attached pane once; only the newest is kept.
        assert_eq!(
            text,
            "\x1b]7;file:///work\x1b\\\x1b[2;3H\x1b]133;A\x1b\\\x1b]2;title\x1b\\\x1b[2;3H\x1b]133;C\x1b\\\x1b]52;c;U0VDUkVU\x1b\\"
        );
        let reconnect = sideband.since(None);
        assert_eq!(
            reconnect["data"], "\x1b]7;file:///work\x1b\\\x1b]2;title\x1b\\",
            "reconnect replays no prompt marker or clipboard write"
        );
        let behind = sideband.since(Some(4));
        assert_eq!(behind["gap"], false);
        assert_eq!(behind["data"], "\x1b]52;c;U0VDUkVU\x1b\\");
        assert_eq!(sideband.since(Some(6))["data"], "");
        for _ in 0..300 {
            sideband.control(b"\x1b]133;A\x07", 1, 2);
        }
        let behind = sideband.since(Some(4));
        assert_eq!(behind["gap"], true);
        assert_eq!(
            behind["data"], "\x1b]7;file:///work\x1b\\\x1b]2;title\x1b\\\x1b]52;c;U0VDUkVU\x1b\\",
            "a gap keeps the undelivered clipboard write"
        );
        assert_eq!(
            sideband.since(Some(6))["data"],
            "\x1b]7;file:///work\x1b\\\x1b]2;title\x1b\\",
            "a delivered clipboard write is never replayed"
        );
        assert_eq!(
            sideband.since(None)["data"],
            "\x1b]7;file:///work\x1b\\\x1b]2;title\x1b\\",
            "reconnect replays no clipboard write"
        );
    }
    #[test]
    fn clipboard_write_needs_decodable_base64_and_xterm_targets() {
        for (value, write) in [
            ("c;T0xE", true),
            (";T0xE", true),
            ("c;U0VDUkVU", true),
            ("c;SECRET", true),
            ("c;TElWRQ==", true),
            ("pqs01234;T0xE", true),
            ("c;?", false),
            ("c;", false),
            ("c;not base64!", false),
            ("c;U0VDUkVU=", false),
            ("c;=T0xE", false),
            ("x;T0xE", false),
            ("cpqs01234;T0xE", false),
            ("T0xE", false),
        ] {
            assert_eq!(clipboard_write(value), write, "{value:?}");
        }
    }
    #[test]
    fn unterminated_strings_are_bounded_and_chunks_do_not_change_semantics() {
        let mut filter = Strings::default();
        for byte in b"\x1b]2;"
            .iter()
            .copied()
            .chain(std::iter::repeat_n(b'a', 100000))
        {
            assert!(filter.byte(byte).is_none());
            assert!(filter.pending.len() <= 4096);
        }
        assert!(filter.byte(7).is_none());
        assert_eq!(
            filter.discarded.as_deref(),
            Some("discarded an over-long OSC string of 100005 bytes")
        );
        assert_eq!(filter.byte(b'X'), Some(vec![b'X']));
        for (length, kept) in [(CLIPBOARD_MAX - 8, true), (CLIPBOARD_MAX - 7, false)] {
            let mut write = b"\x1b]52;c;".to_vec();
            write.extend(std::iter::repeat_n(b'A', length));
            write.push(7);
            let total = write.len();
            let out: Vec<_> = write
                .into_iter()
                .filter_map(|byte| filter.byte(byte))
                .collect();
            assert_eq!(out.len(), usize::from(kept), "{total}-byte clipboard write");
        }
    }

    #[test]
    fn chunked_audited_sideband_gaps_and_injection_are_independent_of_redraw() {
        use crate::terminal::{Size, Terminal};
        let corpus=b"\x1b]7;file:///work\x07\x1b]133;A\x1b\\\x1b]2;safe title\x07\x1b]133;B\x07\x1b]133;C\x07\x1b]133;D;0\x07\x1b]52;c;SECRET\x07\x1b]133;D;bad\x07";
        for chunk in 1..corpus.len() {
            let mut terminal = Terminal::new(Size::new(4, 20).unwrap());
            for bytes in corpus.chunks(chunk) {
                terminal.feed(bytes);
            }
            let live = terminal.sideband.since(Some(0));
            assert_eq!(live["sequence"], 7);
            let text = live["data"].as_str().unwrap();
            assert!(text.contains("file:///work"));
            assert!(text.contains("safe title"));
            assert!(
                text.ends_with("\x1b]133;D;0\x1b\\\x1b]52;c;SECRET\x1b\\"),
                "{text:?}"
            );
            assert!(!text.contains("bad"));
            let mut previous = 0;
            for marker in ["133;A", "133;B", "133;C", "133;D;0"] {
                let position = text.find(marker).unwrap();
                assert!(position > previous);
                previous = position;
            }
            for _ in 0..300 {
                terminal.feed(b"\x1b]133;A\x07");
            }
            let gap = terminal.sideband.since(Some(1));
            assert_eq!(gap["gap"], true);
            assert!(!gap["data"].as_str().unwrap().contains("133;"));
            assert!(terminal.sideband.events.len() <= 256);
            assert_eq!(terminal.sideband.since(Some(u64::MAX))["gap"], true);
        }
        assert!(!safe_uri("file:///work%1b]52"));
        assert!(!safe_uri("https://x/%00"));
        assert!(!safe_uri("javascript:alert(1)"));
    }
}
