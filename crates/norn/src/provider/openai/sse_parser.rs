//! Incremental, fail-closed SSE framing for OpenAI-compatible transports.

use thiserror::Error;

const UTF8_BOM: &[u8; 3] = b"\xEF\xBB\xBF";

/// An intermediate SSE event parsed from the byte stream.
#[derive(Clone, Debug)]
pub struct SseEvent {
    /// The SSE `event:` field.
    pub event_type: String,
    /// Parsed JSON from the frame's joined `data:` fields.
    pub data: serde_json::Value,
}

/// Structural failure while framing or decoding an SSE stream.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum SseParseError {
    /// A complete SSE line was not valid UTF-8.
    #[error("SSE stream contained invalid UTF-8")]
    InvalidUtf8,
    /// A non-sentinel SSE frame did not contain valid JSON data.
    #[error("SSE stream contained an invalid JSON frame")]
    InvalidJson,
}

/// Stateful incremental SSE parser.
///
/// Complete events are returned in wire order. The first malformed line or
/// non-sentinel JSON frame poisons the parser; callers retrieve the typed
/// failure through [`Self::error`] after draining events that preceded
/// it in the same byte chunk.
#[derive(Debug, Default)]
pub struct SseParser {
    buffer: Vec<u8>,
    bom_checked: bool,
    swallow_leading_lf: bool,
    current_event_type: String,
    current_data: String,
    saw_data: bool,
    error: Option<SseParseError>,
}

impl SseParser {
    /// Create an empty parser.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed arbitrary stream bytes and return every complete preceding event.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        if chunk.is_empty() || self.error.is_some() {
            return Vec::new();
        }
        let chunk = if self.swallow_leading_lf {
            self.swallow_leading_lf = false;
            chunk.strip_prefix(b"\n").unwrap_or(chunk)
        } else {
            chunk
        };
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        if self.prepare_initial_bytes() {
            self.drain_complete_lines(&mut events);
        }
        events
    }

    /// Discard pending EOF data without synthesizing a dispatch delimiter.
    ///
    /// EOF is not an SSE dispatch delimiter. A frame without its terminating
    /// blank line remains incomplete and is deliberately discarded.
    pub fn finish(&mut self) -> Vec<SseEvent> {
        if self.error.is_some() {
            return Vec::new();
        }
        let mut events = Vec::new();
        if self.prepare_initial_bytes() {
            self.drain_complete_lines(&mut events);
        }
        self.buffer.clear();
        self.reset_frame();
        events
    }

    /// Return the first structural parse failure, if one occurred.
    #[must_use]
    pub const fn error(&self) -> Option<SseParseError> {
        self.error
    }

    fn prepare_initial_bytes(&mut self) -> bool {
        if self.bom_checked {
            return true;
        }
        let prefix_len = self.buffer.len().min(UTF8_BOM.len());
        if self.buffer[..prefix_len] == UTF8_BOM[..prefix_len] {
            if self.buffer.len() < UTF8_BOM.len() {
                return false;
            }
            self.buffer.drain(..UTF8_BOM.len());
        }
        self.bom_checked = true;
        true
    }

    fn drain_complete_lines(&mut self, events: &mut Vec<SseEvent>) {
        while let Some((line_end, drain_end, swallow_leading_lf)) = self.next_line() {
            let line = if let Ok(line) = std::str::from_utf8(&self.buffer[..line_end]) {
                line.to_owned()
            } else {
                self.fail(SseParseError::InvalidUtf8);
                break;
            };
            self.buffer.drain(..drain_end);
            self.swallow_leading_lf = swallow_leading_lf;
            self.process_line(&line, events);
            if self.error.is_some() {
                break;
            }
        }
    }

    fn next_line(&self) -> Option<(usize, usize, bool)> {
        for (position, byte) in self.buffer.iter().copied().enumerate() {
            match byte {
                b'\n' => return Some((position, position.saturating_add(1), false)),
                b'\r' if position.saturating_add(1) < self.buffer.len() => {
                    let drain_end = if self.buffer[position + 1] == b'\n' {
                        position.saturating_add(2)
                    } else {
                        position.saturating_add(1)
                    };
                    return Some((position, drain_end, false));
                }
                b'\r' => return Some((position, position.saturating_add(1), true)),
                _ => {}
            }
        }
        None
    }

    fn process_line(&mut self, line: &str, events: &mut Vec<SseEvent>) {
        if line.starts_with(':') {
            return;
        }
        if line == "event" {
            self.current_event_type.clear();
        } else if let Some(event_type) = line.strip_prefix("event:") {
            event_type
                .strip_prefix(' ')
                .unwrap_or(event_type)
                .clone_into(&mut self.current_event_type);
        } else if line == "data" {
            self.append_data("");
        } else if let Some(data) = line.strip_prefix("data:") {
            let data = data.strip_prefix(' ').unwrap_or(data);
            self.append_data(data);
        } else if line.is_empty() {
            self.dispatch_frame(events);
        }
    }

    fn append_data(&mut self, data: &str) {
        if self.saw_data {
            self.current_data.push('\n');
        }
        self.current_data.push_str(data);
        self.saw_data = true;
    }

    fn dispatch_frame(&mut self, events: &mut Vec<SseEvent>) {
        if !self.saw_data {
            self.reset_frame();
            return;
        }
        if self.current_data == "[DONE]" {
            self.reset_frame();
            return;
        }
        match serde_json::from_str(&self.current_data) {
            Ok(data) => {
                events.push(SseEvent {
                    event_type: self.current_event_type.clone(),
                    data,
                });
                self.reset_frame();
            }
            Err(_) => self.fail(SseParseError::InvalidJson),
        }
    }

    fn reset_frame(&mut self) {
        self.current_event_type.clear();
        self.current_data.clear();
        self.saw_data = false;
    }

    fn fail(&mut self, error: SseParseError) {
        self.error = Some(error);
        self.buffer.clear();
        self.reset_frame();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_json_is_typed_and_poisoning() {
        let mut parser = SseParser::new();
        let events = parser.feed(b"event: broken\ndata: {\n\n");
        assert!(events.is_empty());
        assert_eq!(parser.error(), Some(SseParseError::InvalidJson));
        assert!(parser.feed(b"event: later\ndata: {}\n\n").is_empty());
    }

    #[test]
    fn invalid_utf8_is_typed_without_lossy_substitution() {
        let mut parser = SseParser::new();
        let events = parser.feed(b"event: broken\ndata: \xff\n\n");
        assert!(events.is_empty());
        assert_eq!(parser.error(), Some(SseParseError::InvalidUtf8));
    }

    #[test]
    fn done_sentinel_is_not_a_parse_failure() {
        let mut parser = SseParser::new();
        assert!(parser.feed(b"data: [DONE]\n\n").is_empty());
        assert_eq!(parser.error(), None);
    }

    #[test]
    fn explicit_empty_data_is_invalid_and_poisoning() {
        let mut parser = SseParser::new();
        let events =
            parser.feed(b"event: empty\ndata:\n\nevent: later\ndata: {\"accepted\": true}\n\n");
        assert!(events.is_empty());
        assert_eq!(parser.error(), Some(SseParseError::InvalidJson));

        let mut colonless = SseParser::new();
        assert!(colonless.feed(b"event: empty\ndata\n\n").is_empty());
        assert_eq!(colonless.error(), Some(SseParseError::InvalidJson));
    }

    #[test]
    fn leading_and_successive_empty_data_lines_preserve_joining() {
        let mut leading = SseParser::new();
        let events = leading.feed(b"event: leading\ndata:\ndata: {\"value\": 1}\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, serde_json::json!({"value": 1}));
        assert_eq!(leading.error(), None);

        let mut successive = SseParser::new();
        assert!(
            successive
                .feed(b"event: empty\ndata:\ndata:\n\n")
                .is_empty()
        );
        assert_eq!(successive.error(), Some(SseParseError::InvalidJson));
    }

    #[test]
    fn event_field_removes_only_one_optional_space() {
        let mut parser = SseParser::new();
        let events = parser.feed(
            b"event:without-space\ndata: null\n\n\
              event: with-one-space\ndata: null\n\n\
              event:  with-two-spaces  \ndata: null\n\n",
        );
        let names = events
            .iter()
            .map(|event| event.event_type.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            ["without-space", "with-one-space", " with-two-spaces  "]
        );

        let mut colonless = SseParser::new();
        let events = colonless.feed(b"event: stale\nevent\ndata: null\n\n");
        assert_eq!(events.len(), 1);
        assert!(events[0].event_type.is_empty());
    }

    #[test]
    fn cr_only_and_split_crlf_are_line_endings() {
        let mut cr_only = SseParser::new();
        let events = cr_only.feed(b"event: cr\rdata: {\"value\": 1}\r\r");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "cr");
        assert!(cr_only.finish().is_empty());

        let mut split_crlf = SseParser::new();
        assert!(split_crlf.feed(b"event: split\r").is_empty());
        assert!(split_crlf.feed(b"\ndata: null\r").is_empty());
        let events = split_crlf.feed(b"\n\r");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "split");
        assert!(split_crlf.feed(b"\n").is_empty());
    }

    #[test]
    fn leading_bom_is_stripped_even_across_chunks() {
        let mut parser = SseParser::new();
        assert!(parser.feed(b"\xEF").is_empty());
        assert!(parser.feed(b"\xBB").is_empty());
        let events = parser.feed(b"\xBFevent: bom\ndata: null\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "bom");
    }

    #[test]
    fn eof_discards_unterminated_frame() {
        let mut parser = SseParser::new();
        assert!(
            parser
                .feed(b"event: response.completed\ndata: {\"type\":\"response.completed\"}\n")
                .is_empty()
        );
        assert!(parser.finish().is_empty());
        assert_eq!(parser.error(), None);
    }
}
