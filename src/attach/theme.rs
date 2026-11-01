#[derive(Default)]
pub struct Probe {
    pending: Vec<u8>,
    pub received: usize,
    paste: bool,
    seen: std::collections::BTreeSet<usize>,
}
impl Probe {
    pub fn queries() -> String {
        let mut result = String::from("\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b]12;?\x1b\\");
        for index in 0..256 {
            result.push_str(&format!("\x1b]4;{index};?\x1b\\"));
        }
        result
    }
    pub fn flush(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending)
    }
    pub fn feed(&mut self, bytes: &[u8]) -> (Vec<u8>, Vec<(usize, [u8; 3])>) {
        let mut input = Vec::new();
        let mut colors = Vec::new();
        for byte in bytes {
            if self.pending.is_empty() && *byte != 27 {
                input.push(*byte);
                continue;
            }
            self.pending.push(*byte);
            if self.pending.starts_with(b"\x1b[") {
                if self.pending.len() > 2 && (0x40..=0x7e).contains(byte) {
                    if self.pending == b"\x1b[200~" {
                        self.paste = true;
                    }
                    if self.pending == b"\x1b[201~" {
                        self.paste = false;
                    }
                    input.extend(self.flush());
                } else if self.pending.len() > 128 {
                    input.extend(self.flush());
                }
                continue;
            }
            if self.pending.len() == 2 && self.pending[1] == b']' && self.paste {
                input.extend(self.flush());
                continue;
            }
            if (self.pending.len() == 2 && self.pending[1] != b']') || self.pending.len() > 128 {
                input.extend(self.flush());
                continue;
            }
            if self.pending.len() > 2 && (*byte == 7 || self.pending.ends_with(b"\x1b\\")) {
                let response = self.flush();
                if let Some(color) = parse(&response) {
                    if self.seen.insert(color.0) {
                        colors.push(color);
                        self.received += 1;
                    } else {
                        input.extend(response);
                    }
                } else {
                    input.extend(response);
                }
            }
        }
        (input, colors)
    }
}
fn parse(bytes: &[u8]) -> Option<(usize, [u8; 3])> {
    let text = std::str::from_utf8(
        bytes
            .strip_prefix(b"\x1b]")?
            .strip_suffix(b"\x07")
            .or_else(|| bytes.strip_prefix(b"\x1b]")?.strip_suffix(b"\x1b\\"))?,
    )
    .ok()?;
    let mut parts = text.split(';');
    let code = parts.next()?;
    let index = match code {
        "4" => {
            let index = parts.next()?.parse::<usize>().ok()?;
            if index > 255 {
                return None;
            }
            index
        }
        "10" => 256,
        "11" => 257,
        "12" => 258,
        _ => return None,
    };
    let value = parts.next()?.strip_prefix("rgb:")?;
    if parts.next().is_some() {
        return None;
    }
    let channels = value.split('/').collect::<Vec<_>>();
    if channels.len() != 3 {
        return None;
    }
    let mut rgb = [0; 3];
    for (slot, text) in rgb.iter_mut().zip(channels) {
        if text.is_empty() || text.len() > 4 {
            return None;
        }
        let value = u32::from_str_radix(text, 16).ok()?;
        *slot = (value * 255 / ((1u32 << (text.len() * 4)) - 1)) as u8;
    }
    Some((index, rgb))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn split_theme_responses_are_validated_and_other_input_roundtrips() {
        let bytes = b"x\x1b[A\x1b]10;rgb:1212/3434/5656\x1b\\\x1b[200~paste\x1b]4;1;rgb:ff/ff/ff\x07\x1b[201~";
        for chunk in 1..bytes.len() {
            let mut probe = Probe::default();
            let mut input = Vec::new();
            let mut colors = Vec::new();
            for bytes in bytes.chunks(chunk) {
                let (i, c) = probe.feed(bytes);
                input.extend(i);
                colors.extend(c);
            }
            input.extend(probe.flush());
            assert_eq!(
                input,
                b"x\x1b[A\x1b[200~paste\x1b]4;1;rgb:ff/ff/ff\x07\x1b[201~"
            );
            assert_eq!(colors, [(256, [18, 52, 86])]);
        }
        assert!(parse(b"\x1b]4;300;rgb:ff/ff/ff\x07").is_none());
        assert!(parse(b"\x1b]10;rgb:fffff/0/0\x07").is_none());
    }
}
