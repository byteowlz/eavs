//! Advisory OSC 7501 emission. Never an authorization or activity transport for HTTP.
use base64::{engine::general_purpose::STANDARD, Engine as _};
use std::io::Write;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum State {
    Idle,
    Working,
    Done,
    Error,
    Question,
    Permission,
}

impl State {
    const fn fields(self) -> &'static str {
        match self {
            Self::Idle => "state=idle",
            Self::Working => "state=working",
            Self::Done => "state=done",
            Self::Error => "state=error",
            Self::Question => "state=blocked:kind=question",
            Self::Permission => "state=blocked:kind=permission",
        }
    }
}

fn encode(state: State, message: &str) -> Vec<u8> {
    let mut text = String::new();
    for character in message.chars() {
        // Do not expose control or direction/zero-width formatting outside the grid.
        let safe = if character.is_control() {
            ' '
        } else {
            character
        };
        if matches!(safe, '\u{061c}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2069}' | '\u{feff}')
        {
            continue;
        }
        if text.len() + safe.len_utf8() > 2048 {
            break;
        }
        text.push(safe);
    }
    format!(
        "\x1b]7501;{}:app=eavs:msg={}\x1b\\",
        state.fields(),
        STANDARD.encode(text)
    )
    .into_bytes()
}

pub fn enabled_for_terminal(terminal: bool) -> bool {
    terminal && std::env::var("EAVS_PROGRAM_STATUS").as_deref() != Ok("0")
}

pub struct Reporter<W> {
    writer: W,
    enabled: bool,
    last: Option<(State, &'static str)>,
}

impl<W: Write> Reporter<W> {
    pub(crate) const fn new(writer: W, enabled: bool) -> Self {
        Self {
            writer,
            enabled,
            last: None,
        }
    }

    // Callers use fixed summaries, never form contents, remote errors or credentials.
    pub(crate) fn set(&mut self, state: State, message: &'static str) {
        if !self.enabled || self.last == Some((state, message)) {
            return;
        }
        if self
            .writer
            .write_all(&encode(state, message))
            .and_then(|()| self.writer.flush())
            .is_err()
        {
            // Advisory output failure must not cause retries or alter a mutation's outcome.
            self.enabled = false;
            return;
        }
        self.last = Some((state, message));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_records_repeat_app_and_deduplicate() {
        let mut reporter = Reporter::new(Vec::new(), true);
        reporter.set(State::Working, "Applying plan");
        reporter.set(State::Working, "Applying plan");
        reporter.set(State::Permission, "Confirm plan");
        assert_eq!(
            reporter.writer,
            [
                encode(State::Working, "Applying plan"),
                encode(State::Permission, "Confirm plan")
            ]
            .concat()
        );
    }

    #[test]
    fn disabled_output_is_empty() {
        let mut reporter = Reporter::new(Vec::new(), false);
        reporter.set(State::Working, "Applying plan");
        assert!(reporter.writer.is_empty());
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "Assertions verify safe bounded wire text; decoding fixture errors propagate"
    )]
    fn text_is_sanitized_and_utf8_bounded() -> anyhow::Result<()> {
        let sequence = encode(
            State::Error,
            &format!("first\n\x1b\u{009b}\u{202e}{}", "é".repeat(2000)),
        );
        let raw = std::str::from_utf8(&sequence)?;
        let encoded = raw
            .split(":msg=")
            .nth(1)
            .ok_or_else(|| anyhow::anyhow!("Missing message"))?
            .trim_end_matches("\x1b\\");
        let decoded = String::from_utf8(STANDARD.decode(encoded)?)?;
        assert!(sequence.len() <= 4096);
        assert!(encoded.len() <= 2732);
        assert!(decoded.len() <= 2048);
        assert!(!decoded.chars().any(char::is_control));
        assert!(!decoded.contains('\u{202e}'));
        Ok(())
    }
}
