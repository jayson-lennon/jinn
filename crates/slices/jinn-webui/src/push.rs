//! The push seam — one [`SseEnvelope`] becomes one Datastar SDK event.
//!
//! The fan-out still speaks [`SseEnvelope`] (the five [`SseKind`]s are
//! the region registry's vocabulary: event ownership, settle order);
//! this module is the only place an envelope meets the wire, and the
//! wire's shape is the SDK's: every push is a typed
//! [`datastar::PatchElements`] or [`datastar::PatchSignals`]. No hand-
//! formatted SSE framing exists anywhere in the crate.
//!
//! The translation table:
//!
//! | kind            | event          | target                | mode   |
//! |-----------------|----------------|-----------------------|--------|
//! | Chat            | PatchElements  | `#transcript`         | append |
//! | TranscriptFull  | PatchElements  | `#transcript`         | inner  |
//! | Controls        | PatchElements  | `#composer-controls`  | inner  |
//! | Sessions        | PatchSignals   | `sessionsTick` bump   | —      |
//! | Models          | PatchSignals   | `modelsTick` stamp    | —      |
//!
//! Sessions and Models pushes are **signals, not HTML**: the pushed
//! list is a signal to the page's own effect, which re-pulls the
//! fragment with its active filter — the pull endpoint remains the
//! renderer (the filtered-list trap: an unfiltered pushed list would
//! un-filter the sidebar under the user's eyes). Models keeps the
//! cache-write timestamp as its payload (non-empty by construction —
//! the dispatch contract), sessions bumps a counter.
//!
//! No `id:` line rides any of these events: the browser keeps the
//! last received `id` across events, and convergence pushes stamped
//! `id: 0` under the old client, so a reconnect after a settle
//! re-caught-up from zero and re-appended the whole transcript (a
//! shipped latent bug). Only `chat` frames carry ids — real cursor
//! coordinates, so the fetch transport's `last-event-id` resume and
//! the server's catch-up agree.

use datastar::consts::ElementPatchMode;
use datastar::prelude::PatchElements;
use datastar::prelude::PatchSignals;
use jinn_webui_msg::SseEnvelope;
use jinn_webui_msg::SseKind;

use crate::contract;

/// The transcript container selector — every transcript push targets
/// it, spelled from the contract id.
fn transcript_selector() -> String {
    format!("#{}", contract::TRANSCRIPT_ID)
}

/// The composer-controls container selector.
fn controls_selector() -> String {
    format!("#{}", contract::CONTROLS_ID)
}

/// One kind of push error: a fragment kind whose payload should have
/// been HTML got an empty string, or a signal kind got nothing. Both
/// are fan-out bugs, not runtime conditions; the endpoint drops the
/// frame (an empty patch mutates nothing).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PushError {
    /// A `Chat` envelope carried empty HTML.
    EmptyChat,
}

/// Translates one envelope into at most one SDK event.
///
/// `sessions_tick` is the per-stream counter the sessions signal
/// bumps — the endpoint owns the counter so every connection counts
/// its own pushes.
///
/// # Errors
///
/// Returns [`PushError::EmptyChat`] for a `Chat` envelope with empty
/// HTML (the cursor-advance sentinel of the old protocol — Datastar
/// has no empty-append, so the sentinel dissolves into a no-op here).
pub(crate) fn sdk_event_of(
    envelope: &SseEnvelope,
    sessions_tick: u64,
) -> Result<datastar::DatastarEvent, PushError> {
    match envelope.kind {
        SseKind::Chat => {
            if envelope.html.is_empty() {
                return Err(PushError::EmptyChat);
            }
            Ok(PatchElements::new(envelope.html.clone())
                .selector(transcript_selector())
                .mode(ElementPatchMode::Append)
                .id(envelope.cursor.to_string())
                .into_datastar_event())
        }
        SseKind::TranscriptFull => Ok(PatchElements::new(envelope.html.clone())
            .selector(transcript_selector())
            .mode(ElementPatchMode::Inner)
            .into_datastar_event()),
        SseKind::Controls => Ok(PatchElements::new(envelope.html.clone())
            .selector(controls_selector())
            .mode(ElementPatchMode::Inner)
            .into_datastar_event()),
        SseKind::Sessions => Ok(PatchSignals::new(format!(
            "{{\"{}\": {sessions_tick}}}",
            contract::SIGNAL_SESSIONS_TICK
        ))
        .into_datastar_event()),
        SseKind::Models => Ok(PatchSignals::new(
            serde_json::json!({ contract::SIGNAL_MODELS_TICK: envelope.html }).to_string(),
        )
        .into_datastar_event()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

    use super::*;
    use jinn_core_types::SessionId;

    fn envelope(kind: SseKind, html: &str, cursor: usize) -> SseEnvelope {
        SseEnvelope {
            kind,
            session_id: SessionId::new(),
            cursor,
            html: html.to_owned(),
        }
    }

    #[rstest::rstest]
    #[test]
    fn chat_becomes_an_append_patch_at_the_transcript_with_the_cursor_id() {
        // Given a chat envelope with one entry's HTML and a cursor.
        let event = sdk_event_of(&envelope(SseKind::Chat, "<p>hi</p>", 7), 0)
            .expect("a non-empty chat translates");

        // Then it is a patch-elements event appending at #transcript,
        // carrying the cursor on the id line.
        let datastar::DatastarEvent {
            event: kind,
            id,
            data,
            ..
        } = event;
        assert_eq!(kind, datastar::consts::EventType::PatchElements);
        assert_eq!(id.as_deref(), Some("7"));
        let joined = data.join("\n");
        assert!(joined.contains("selector #transcript"), "{joined}");
        assert!(joined.contains("mode append"), "{joined}");
        assert!(joined.contains("elements <p>hi</p>"), "{joined}");
    }

    #[rstest::rstest]
    #[test]
    fn transcript_full_becomes_an_inner_replace_and_carries_no_id() {
        // Given a convergence envelope (cursor 0, full HTML).
        let event = sdk_event_of(&envelope(SseKind::TranscriptFull, "<p>all</p>", 0), 0)
            .expect("a transcript-full translates");

        // Then it replaces #transcript's inner HTML and carries no id
        // line — a reconnect resumes from the last real chat cursor,
        // never from this push (the re-append bug's fix).
        let datastar::DatastarEvent {
            event: kind,
            id,
            data,
            ..
        } = event;
        assert_eq!(kind, datastar::consts::EventType::PatchElements);
        assert_eq!(id, None);
        let joined = data.join("\n");
        assert!(joined.contains("selector #transcript"), "{joined}");
        assert!(joined.contains("mode inner"), "{joined}");
    }

    #[rstest::rstest]
    #[test]
    fn controls_become_an_inner_replace_of_the_controls_container() {
        // Given a controls envelope.
        let event = sdk_event_of(&envelope(SseKind::Controls, "<button>go</button>", 0), 0)
            .expect("controls translate");

        // Then they replace #composer-controls's inner HTML.
        let datastar::DatastarEvent {
            event: kind, data, ..
        } = event;
        assert_eq!(kind, datastar::consts::EventType::PatchElements);
        let joined = data.join("\n");
        assert!(joined.contains("selector #composer-controls"), "{joined}");
        assert!(joined.contains("mode inner"), "{joined}");
    }

    #[rstest::rstest]
    #[test]
    fn sessions_pushes_bump_the_tick_signal_and_drop_the_html() {
        // Given a sessions refresh envelope (the fan-out renders the
        // list into the html field; the browser must not receive it —
        // the effect re-pulls with the active filter instead).
        let event = sdk_event_of(&envelope(SseKind::Sessions, "<a>row</a>", 0), 3)
            .expect("a sessions push translates");

        // Then the event is a signal patch bumping sessionsTick, and
        // no list HTML rides it.
        let datastar::DatastarEvent {
            event: kind, data, ..
        } = event;
        assert_eq!(kind, datastar::consts::EventType::PatchSignals);
        let joined = data.join("\n");
        assert!(joined.contains("signals {"), "{joined}");
        assert!(
            joined.contains(&format!("\"{}\": 3", contract::SIGNAL_SESSIONS_TICK)),
            "{joined}"
        );
        assert!(!joined.contains("<a>row</a>"), "{joined}");
    }

    #[rstest::rstest]
    #[test]
    fn models_pushes_stamp_the_timestamp_into_the_tick_signal() {
        // Given a models-changed envelope whose payload is the cache
        // write timestamp (non-empty — the dispatch contract).
        let event = sdk_event_of(&envelope(SseKind::Models, "2026-01-01T00:00:00Z", 0), 0)
            .expect("a models push translates");

        // Then the event is a signal patch carrying the timestamp.
        let datastar::DatastarEvent {
            event: kind, data, ..
        } = event;
        assert_eq!(kind, datastar::consts::EventType::PatchSignals);
        let joined = data.join("\n");
        // serde_json writes without spaces; the timestamp must ride
        // verbatim as the tick's value (the modal's re-pull compares
        // it truthily, and it names the cache write).
        assert!(
            joined.replace(' ', "").contains(&format!(
                "\"{}\":\"2026-01-01T00:00:00Z\"",
                contract::SIGNAL_MODELS_TICK
            )),
            "{joined}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn models_payloads_are_json_escaped() {
        // Given a models payload containing quotes and backslashes
        // (defensive: a timestamp never carries them, but the seam
        // must not emit malformed JSON if one ever does).
        let event = sdk_event_of(&envelope(SseKind::Models, "a\"b\\c", 0), 0)
            .expect("a models push translates");

        // Then the payload's quotes and backslashes are escaped —
        // the dataline parses as JSON in the browser.
        let datastar::DatastarEvent { data, .. } = event;
        let joined = data.join("\n");
        assert!(joined.contains("a\\\"b\\\\c"), "{joined}");
    }

    #[rstest::rstest]
    #[test]
    fn an_empty_chat_is_the_dissolved_cursor_sentinel() {
        // Given the old protocol's trailing cursor-advance frame (empty
        // HTML, real cursor).
        let error =
            sdk_event_of(&envelope(SseKind::Chat, "", 5), 0).expect_err("an empty chat is a no-op");

        // Then the seam names it: the endpoint drops the frame.
        assert_eq!(error, PushError::EmptyChat);
    }
}
