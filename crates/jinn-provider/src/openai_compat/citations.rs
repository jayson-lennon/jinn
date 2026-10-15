//! Accumulation of `url_citation` annotations across streaming chunks.
//!
//! A vendor-specific feature on an otherwise generic parser. OpenRouter's
//! web-search mode spreads its source annotations one per delta chunk, each
//! arriving alongside the text it refers to. Nothing downstream can render a
//! citation until the stream is over — the set is only meaningful once every
//! chunk has been seen — so the parser collects them as they arrive and emits
//! them once, immediately before the terminal [`StreamEvent::Done`].
//!
//! Kept apart from the parser for two reasons. It is the one piece of this
//! module that is *not* part of the OpenAI-compatible protocol: a provider
//! that speaks OpenAI but is not OpenRouter never produces these, and a
//! reader of the parser should not have to hold that distinction in their
//! head while reading the generic path. And it is the only one of the four
//! state machines here with no interaction with tool calls or the deferred
//! `Done` beyond emission order, so it can be exercised on its own.

use jinn_core_types::url_citation::UrlCitation;

use crate::StreamEvent;

/// The `url_citation` annotations seen so far in this stream.
#[derive(Debug, Default)]
pub(super) struct CitationAccumulator {
    /// Annotations in arrival order. Emitted as one batch, never mid-stream.
    collected: Vec<UrlCitation>,
}

impl CitationAccumulator {
    /// Absorbs a single `delta.annotations[]` entry.
    ///
    /// Silently ignores anything that is not a `url_citation` carrying a
    /// `url` — the annotations array is a shared slot across annotation
    /// kinds, and a new provider adding one must not break the stream.
    pub(super) fn absorb(&mut self, annotation: &serde_json::Value) {
        if annotation.get("type").and_then(|t| t.as_str()) != Some("url_citation") {
            return;
        }
        let Some(citation) = annotation.get("url_citation") else {
            return;
        };
        let Some(url) = citation.get("url").and_then(|u| u.as_str()) else {
            return;
        };
        let title = citation
            .get("title")
            .and_then(|t| t.as_str())
            .unwrap_or(url)
            .to_owned();
        self.collected.push(UrlCitation {
            url: url.to_owned(),
            title,
            content: citation
                .get("content")
                .and_then(|c| c.as_str())
                .map(std::string::String::from),
            start_index: citation
                .get("start_index")
                .and_then(serde_json::Value::as_u64)
                .and_then(|n| u32::try_from(n).ok()),
            end_index: citation
                .get("end_index")
                .and_then(serde_json::Value::as_u64)
                .and_then(|n| u32::try_from(n).ok()),
        });
    }

    /// Drains everything accumulated so far into a single event, or nothing.
    ///
    /// Drains rather than borrows so a second call — which a malformed stream
    /// can provoke by sending two `[DONE]` sentinels — cannot emit the same
    /// batch twice.
    pub(super) fn drain(&mut self) -> Vec<StreamEvent> {
        if self.collected.is_empty() {
            return vec![];
        }
        vec![StreamEvent::Citations(std::mem::take(&mut self.collected))]
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, reason = "test code")]
    use super::*;

    fn annotation(kind: &str, body: &str) -> serde_json::Value {
        serde_json::json!({ "type": kind, kind: body })
    }

    #[rstest::rstest]
    fn a_url_citation_is_collected() {
        // Given an accumulator and a `url_citation` annotation.
        let mut acc = CitationAccumulator::default();

        // When it absorbs the annotation.
        acc.absorb(&serde_json::json!({
            "type": "url_citation",
            "url_citation": {
                "url": "https://example.com/a",
                "title": "Source A",
                "content": "snippet A",
                "start_index": 4,
                "end_index": 7
            }
        }));

        // Then draining yields one event carrying it.
        let events = acc.drain();
        assert_eq!(events.len(), 1);
        let StreamEvent::Citations(citations) = &events[0] else {
            panic!("expected Citations, got {:?}", events[0])
        };
        assert_eq!(citations.len(), 1);
        assert_eq!(citations[0].url, "https://example.com/a");
        assert_eq!(citations[0].title, "Source A");
        assert_eq!(citations[0].content.as_deref(), Some("snippet A"));
        assert_eq!(citations[0].start_index, Some(4));
        assert_eq!(citations[0].end_index, Some(7));
    }

    #[rstest::rstest]
    fn a_citation_without_a_title_falls_back_to_its_url() {
        // Given a citation that omits `title`.
        let mut acc = CitationAccumulator::default();

        // When it is absorbed.
        acc.absorb(&serde_json::json!({
            "type": "url_citation",
            "url_citation": {"url": "https://example.com/b"}
        }));

        // Then the url stands in for the missing title.
        let events = acc.drain();
        let StreamEvent::Citations(citations) = &events[0] else {
            panic!("expected Citations")
        };
        assert_eq!(citations[0].title, "https://example.com/b");
    }

    #[rstest::rstest]
    fn an_annotation_of_another_kind_is_ignored() {
        // Given an accumulator and a `file_citation` annotation.
        let mut acc = CitationAccumulator::default();

        // When it absorbs the annotation.
        acc.absorb(&annotation(
            "file_citation",
            &serde_json::json!({"file_id": "f"}).to_string(),
        ));

        // Then draining yields nothing.
        assert_eq!(acc.drain(), [] as [crate::stream_event::StreamEvent; 0]);
    }

    #[rstest::rstest]
    fn a_url_citation_without_a_url_is_ignored() {
        // Given a `url_citation` whose payload has no `url`.
        let mut acc = CitationAccumulator::default();

        // When it is absorbed.
        acc.absorb(&serde_json::json!({
            "type": "url_citation",
            "url_citation": {"title": "orphaned"}
        }));

        // Then draining yields nothing rather than an unusable citation.
        assert_eq!(acc.drain(), [] as [crate::stream_event::StreamEvent; 0]);
    }

    #[rstest::rstest]
    fn citations_keep_arrival_order() {
        // Given an accumulator holding two annotations, absorbed in order.
        let mut acc = CitationAccumulator::default();
        acc.absorb(&serde_json::json!({
            "type": "url_citation",
            "url_citation": {"url": "https://example.com/first"}
        }));
        acc.absorb(&serde_json::json!({
            "type": "url_citation",
            "url_citation": {"url": "https://example.com/second"}
        }));

        // When they are drained.
        let events = acc.drain();

        // Then one event holds both, in the order they arrived.
        assert_eq!(events.len(), 1);
        let StreamEvent::Citations(citations) = &events[0] else {
            panic!("expected Citations")
        };
        assert_eq!(citations.len(), 2);
        assert_eq!(citations[0].url, "https://example.com/first");
        assert_eq!(citations[1].url, "https://example.com/second");
    }

    #[rstest::rstest]
    fn draining_twice_emits_nothing_the_second_time() {
        // Given an accumulator holding one citation.
        let mut acc = CitationAccumulator::default();
        acc.absorb(&serde_json::json!({
            "type": "url_citation",
            "url_citation": {"url": "https://example.com/a"}
        }));

        // When it is drained twice.
        let first = acc.drain();
        let second = acc.drain();

        // Then only the first drain carries the batch.
        assert_eq!(first.len(), 1);
        assert_eq!(second, [] as [crate::stream_event::StreamEvent; 0]);
    }

    #[rstest::rstest]
    fn an_empty_accumulator_drains_nothing() {
        // Given an accumulator that has absorbed nothing.
        let mut acc = CitationAccumulator::default();

        // When it is drained, nothing is emitted.
        assert_eq!(acc.drain(), [] as [crate::stream_event::StreamEvent; 0]);
    }
}
