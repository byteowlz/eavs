//! Bounded byte framing: UTF-8 is decoded only after a complete SSE event.
const LIMIT: usize = 1024 * 1024;
#[derive(Default)]
pub struct Framer {
    buffer: Vec<u8>,
}
impl Framer {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>, ()> {
        let mut frames = Vec::new();
        for byte in bytes {
            self.buffer.push(*byte);
            if self.buffer.len() > LIMIT {
                return Err(());
            }
            let n = self.buffer.len();
            let complete = self.buffer.ends_with(b"\n\n")
                || self.buffer.ends_with(b"\r\n\r\n")
                || self.buffer.ends_with(b"\r\r");
            if complete {
                let text = String::from_utf8(std::mem::take(&mut self.buffer)).map_err(|_| ())?;
                frames.push(text);
            } else if n == LIMIT {
                return Err(());
            }
        }
        Ok(frames)
    }
    pub fn finish(&self) -> Result<(), ()> {
        if self.buffer.iter().all(u8::is_ascii_whitespace) {
            Ok(())
        } else {
            Err(())
        }
    }
}
pub fn event(frame: &str) -> Option<(String, String)> {
    let normalized = frame.replace("\r\n", "\n").replace('\r', "\n");
    let mut kind = String::new();
    let mut data = Vec::new();
    for line in normalized.lines() {
        if let Some(value) = line.strip_prefix("event:") {
            value.trim_start_matches(' ').clone_into(&mut kind);
        }
        if let Some(value) = line.strip_prefix("data:") {
            data.push(value.strip_prefix(' ').unwrap_or(value));
        }
    }
    if data.is_empty() {
        None
    } else {
        Some((kind, data.join("\n")))
    }
}
