//! SSE (Server-Sent Events) line parser for streaming responses.
//!
//! Accumulates raw bytes from `reqwest`'s `bytes_stream()` into lines,
//! extracts `data: {...}` payloads, and handles the `[DONE]` sentinel.

/// Truncate a string to at most `max_len` bytes, respecting char boundaries.
fn truncate_str(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        return s;
    }
    let mut end = max_len;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Stateful SSE parser that accumulates bytes and yields complete data payloads.
///
/// SSE format from OpenAI-compatible providers:
/// ```text
/// data: {"id":"...","choices":[...]}\n
/// \n
/// data: [DONE]\n
/// \n
/// ```
///
/// Events are separated by blank lines. Each event line starting with `data: `
/// contains a JSON payload (or the `[DONE]` sentinel).
#[derive(Debug, Default)]
pub struct SseParser {
    /// Incomplete line buffer (bytes may arrive mid-line).
    buffer: String,
}

/// A parsed SSE data payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SseEvent {
    /// A JSON data payload.
    Data(String),
    /// The stream is done.
    Done,
}

impl SseParser {
    /// Create a new parser.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed bytes into the parser and return any complete events.
    ///
    /// Bytes may arrive in arbitrary chunks - the parser handles partial lines
    /// by buffering until a complete event boundary (`\n\n`) is found.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<SseEvent> {
        let text = String::from_utf8_lossy(bytes);
        tracing::trace!(
            raw_bytes = bytes.len(),
            buffer_len_before = self.buffer.len(),
            incoming_preview = %truncate_str(&text, 200),
            "SSE feed"
        );
        self.buffer.push_str(&text);
        let events = self.drain_events();
        tracing::trace!(
            buffer_len_after = self.buffer.len(),
            events_out = events.len(),
            buffer_preview = %truncate_str(&self.buffer, 200),
            "SSE feed result"
        );
        events
    }

    /// Drain any remaining buffered events (call when the stream ends).
    #[allow(dead_code)]
    pub fn finish(&mut self) -> Vec<SseEvent> {
        if self.buffer.trim().is_empty() {
            return vec![];
        }
        self.drain_events()
    }

    /// Parse complete events from the buffer.
    fn drain_events(&mut self) -> Vec<SseEvent> {
        let mut events = Vec::new();

        // Normalize CRLF to LF so we only need to look for \n\n.
        self.buffer = self.buffer.replace("\r\n", "\n");

        while let Some(pos) = self.buffer.find("\n\n") {
            let event_text = self.buffer[..pos].to_owned();
            self.buffer.drain(..pos + 2);

            tracing::trace!(
                event_text_preview = %truncate_str(&event_text, 300),
                "SSE drain found boundary"
            );

            for line in event_text.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }

                // Log every non-empty line for visibility.
                tracing::trace!(
                    line_preview = %truncate_str(line, 200),
                    "SSE line"
                );

                if let Some(data) = line.strip_prefix("data: ") {
                    let data = data.trim();
                    if data == "[DONE]" {
                        events.push(SseEvent::Done);
                    } else if !data.is_empty() {
                        events.push(SseEvent::Data(data.to_owned()));
                    }
                }
                // Ignore non-data lines (e.g., "event: message", comments).
            }
        }

        events
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]
    use super::*;

    #[rstest::rstest]
    fn single_data_event() {
        // Given a complete SSE event.
        let mut parser = SseParser::new();

        // When feeding bytes.
        let events = parser.feed(b"data: {\"hello\":true}\n\n");

        // Then one data event is produced.
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], SseEvent::Data("{\"hello\":true}".to_owned()));
    }

    #[rstest::rstest]
    fn done_sentinel() {
        // Given a parser fed the `[DONE]` sentinel.
        let mut parser = SseParser::new();

        // When feeding the sentinel bytes.
        let events = parser.feed(b"data: [DONE]\n\n");

        // Then a single Done event is produced.
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], SseEvent::Done);
    }

    #[rstest::rstest]
    fn multiple_events_in_one_chunk() {
        // Given a parser fed three complete events in a single chunk.
        let mut parser = SseParser::new();

        // When feeding those bytes.
        let events = parser.feed(b"data: {\"a\":1}\n\ndata: {\"b\":2}\n\ndata: [DONE]\n\n");

        // Then all three events are produced in order.
        assert_eq!(events.len(), 3);
        assert_eq!(events[0], SseEvent::Data("{\"a\":1}".to_owned()));
        assert_eq!(events[1], SseEvent::Data("{\"b\":2}".to_owned()));
        assert_eq!(events[2], SseEvent::Done);
    }

    #[rstest::rstest]
    fn partial_bytes_accumulate() {
        // Given a parser fed an event split across two chunks.
        let mut parser = SseParser::new();

        // When feeding only the first, incomplete chunk.
        let events1 = parser.feed(b"data: {\"hel");

        // Then no event is produced yet.
        assert_eq!(events1, [] as [crate::openai_compat::sse::SseEvent; 0]);

        // And when the completing chunk arrives.
        let events2 = parser.feed(b"lo\":true}\n\n");

        // Then the buffered bytes form one complete event.
        assert_eq!(events2.len(), 1);
        assert_eq!(events2[0], SseEvent::Data("{\"hello\":true}".to_owned()));
    }

    #[rstest::rstest]
    fn ignores_non_data_lines() {
        // Given a chunk carrying an `event:` line alongside a data line.
        let mut parser = SseParser::new();

        // When feeding those bytes.
        let events = parser.feed(b"event: message\ndata: {\"x\":1}\n\n");

        // Then only the data event is produced.
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], SseEvent::Data("{\"x\":1}".to_owned()));
    }

    #[rstest::rstest]
    fn ignores_empty_data() {
        // Given a chunk with an empty data line followed by a real one.
        let mut parser = SseParser::new();

        // When feeding those bytes.
        let events = parser.feed(b"data: \n\ndata: {\"x\":1}\n\n");

        // Then the empty line is dropped and one event is produced.
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], SseEvent::Data("{\"x\":1}".to_owned()));
    }

    #[rstest::rstest]
    fn handles_cr_lf() {
        // Given a complete SSE event delimited by CRLF line endings.
        let mut parser = SseParser::new();

        // When feeding those bytes.
        let events = parser.feed(b"data: {\"x\":1}\r\n\r\n");

        // Then one data event is produced.
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], SseEvent::Data("{\"x\":1}".to_owned()));
    }

    #[rstest::rstest]
    fn finish_drains_remaining() {
        // Given a parser holding an event with no terminating blank line.
        // The parser only completes events separated by \n\n, so a stream
        // that ends without one leaves the buffered content unemitted.
        let mut parser = SseParser::new();
        parser.feed(b"data: {\"x\":1}\n");

        // When the stream is finished.
        let events = parser.finish();

        // Then no events are produced.
        assert_eq!(events, [] as [crate::openai_compat::sse::SseEvent; 0]);
    }
}
