//! The push seam — one [`SseEnvelope`] becomes one Datastar SDK event.
//!
//! The fan-out still speaks [`SseEnvelope`] (the [`SseKind`]s are the
//! region registry's vocabulary: event ownership, settle order); this
//! module is the only place an envelope meets the wire, and the wire's
//! shape is the SDK's: every push is a typed
//! [`datastar::PatchElements`] or [`datastar::PatchSignals`]. No hand-
//! formatted SSE framing exists anywhere in the crate.
//!
//! The translation table:
//!
//! | kind           | event          | target                | mode   |
//! |----------------|----------------|-----------------------|--------|
//! | Streaming      | PatchElements  | `#streaming`          | inner  |
//! | TranscriptFull | PatchElements  | `#transcript`         | inner  |
//! | Controls       | PatchElements  | `#composer-controls`  | inner  |
//! | Sessions       | PatchSignals   | `sessionsTick` bump   | —      |
//! | Models         | PatchSignals   | `modelsTick` stamp    | —      |
//! | McpStatus      | PatchSignals   | `mcpTick` stamp       | —      |
//! | Skills         | PatchSignals   | `skillsTick` stamp    | —      |
//! | Tools          | PatchSignals   | `toolsTick` stamp     | —      |
//!
//! Sessions, Models, McpStatus, Skills, and Tools pushes are
//! **signals, not HTML**: the pushed list is a signal to the page's
//! own effect, which re-pulls the fragment with its active filter —
//! the pull endpoint remains the renderer (the filtered-list trap: an
//! unfiltered pushed list would un-filter the sidebar under the
//! user's eyes). Models, McpStatus, Skills, and Tools keep the write
//! timestamp as the payload (non-empty by construction — the dispatch
//! contract), sessions bumps a counter.
//!
//! The `id:` line is the reconnect-resume coordinate, and it is
//! deliberately sparse. Only the catch-up events the stream endpoint
//! emits carry real cursor ids; **no fan-out push does**. A
//! `Streaming` push stamps `id:` at its envelope's settled boundary —
//! a constant per turn, never a growing watermark — and
//! `TranscriptFull` carries none at all: the browser keeps the last
//! received id across events, so a settle-pushed id under a
//! full-replace payload would make a reconnect resume from the
//! settle's count and re-append the whole transcript (the shipped
//! latent bug the no-id rule fixed). Parking the streaming id at the
//! boundary is the same rule held for the new container: a reconnect
//! mid-turn resumes from the boundary, pulls the settled entries, and
//! the next flush tick refills the tail.
//!
//! No `id:` line rides the signal kinds, and an envelope whose payload
//! should have been HTML carrying an empty string is a fan-out bug
//! the endpoint drops (an empty patch mutates nothing).

use datastar::consts::ElementPatchMode;
use datastar::prelude::PatchElements;
use datastar::prelude::PatchSignals;
use jinn_webui_msg::SseEnvelope;
use jinn_webui_msg::SseKind;

use crate::contract;

/// The transcript container selector — the full-replace push targets
/// it, spelled from the contract id.
fn transcript_selector() -> String {
    format!("#{}", contract::TRANSCRIPT_ID)
}

/// The transcript's streaming-container selector — the in-flight
/// turn's tail replace targets it.
fn streaming_selector() -> String {
    format!("#{}", contract::STREAMING_ID)
}

/// The composer-controls container selector.
fn controls_selector() -> String {
    format!("#{}", contract::CONTROLS_ID)
}

/// One kind of push error: a fragment kind whose payload should have
/// been HTML got an empty string. That is a fan-out bug, not a runtime
/// condition; the endpoint drops the frame (an empty patch mutates
/// nothing).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PushError {
    /// A fragment envelope carried empty HTML.
    EmptyFragment,
}

/// Translates one envelope into its SSE event sequence: one element
/// patch plus a [`contract::SIGNAL_CHAT_TICK`] signal bump for the
/// three transcript-shape kinds (streaming tail replace, full
/// replace) — the browser's scroll effect keys on it, so a reader
/// watching the tail follows each batch. The bump rides after the
/// render (patch first, tick second) so the effect's scroll lands on
/// the grown DOM. Controls and the signal kinds carry no tick.
///
/// `sessions_tick` is the per-stream counter the sessions signal
/// bumps — the endpoint owns the counter so every connection counts
/// its own pushes.
///
/// # Errors
///
/// Returns [`PushError::EmptyFragment`] for an envelope whose payload
/// should have been HTML but is empty.
pub(crate) fn sdk_events_of(
    envelope: &SseEnvelope,
    sessions_tick: u64,
) -> Result<Vec<datastar::DatastarEvent>, PushError> {
    let patch = |html: String,
                 selector: String,
                 mode: datastar::consts::ElementPatchMode,
                 id: Option<String>| {
        let mut builder = PatchElements::new(html).selector(selector).mode(mode);
        if let Some(id) = id {
            builder = builder.id(id);
        }
        builder.into_datastar_event()
    };
    let tick = |n: u64| {
        PatchSignals::new(serde_json::json!({ contract::SIGNAL_CHAT_TICK: n }).to_string())
            .into_datastar_event()
    };
    match envelope.kind {
        // The in-flight tail: wholesale-replaced into the streaming
        // container. The id line parks at the envelope's settled
        // boundary — constant for the turn, so a reconnect mid-turn
        // resumes from the settled prefix and never replays the tail.
        SseKind::Streaming => {
            if envelope.html.is_empty() {
                return Err(PushError::EmptyFragment);
            }
            Ok(vec![
                patch(
                    envelope.html.clone(),
                    streaming_selector(),
                    ElementPatchMode::Inner,
                    Some(envelope.cursor.to_string()),
                ),
                tick(sessions_tick),
            ])
        }
        SseKind::TranscriptFull => {
            if envelope.html.is_empty() {
                return Err(PushError::EmptyFragment);
            }
            Ok(vec![
                patch(
                    envelope.html.clone(),
                    transcript_selector(),
                    ElementPatchMode::Inner,
                    None,
                ),
                tick(sessions_tick),
            ])
        }
        SseKind::Controls => {
            if envelope.html.is_empty() {
                return Err(PushError::EmptyFragment);
            }
            Ok(vec![patch(
                envelope.html.clone(),
                controls_selector(),
                ElementPatchMode::Inner,
                None,
            )])
        }
        SseKind::Sessions => Ok(vec![
            PatchSignals::new(format!(
                "{{\"{}\": {sessions_tick}}}",
                contract::SIGNAL_SESSIONS_TICK
            ))
            .into_datastar_event(),
        ]),
        SseKind::Models => Ok(vec![
            PatchSignals::new(
                serde_json::json!({ contract::SIGNAL_MODELS_TICK: envelope.html }).to_string(),
            )
            .into_datastar_event(),
        ]),
        SseKind::McpStatus => Ok(vec![
            PatchSignals::new(
                serde_json::json!({ contract::SIGNAL_MCP_TICK: envelope.html }).to_string(),
            )
            .into_datastar_event(),
        ]),
        SseKind::Skills => Ok(vec![
            PatchSignals::new(
                serde_json::json!({ contract::SIGNAL_SKILLS_TICK: envelope.html }).to_string(),
            )
            .into_datastar_event(),
        ]),
        SseKind::Tools => Ok(vec![
            PatchSignals::new(
                serde_json::json!({ contract::SIGNAL_TOOLS_TICK: envelope.html }).to_string(),
            )
            .into_datastar_event(),
        ]),
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
    fn streaming_becomes_an_inner_replace_at_the_streaming_container_parked_at_the_boundary() {
        // Given a streaming envelope with the tail's HTML and the
        // session's settled boundary as its cursor.
        let event = sdk_events_of(&envelope(SseKind::Streaming, "<p>tail</p>", 7), 0)
            .expect("a non-empty streaming tail translates")
            .remove(0);

        // Then it is a patch-elements event replacing #streaming's
        // inner HTML, carrying the boundary on the id line — a
        // reconnect mid-turn resumes from the settled prefix and the
        // next tick refills the tail; a growing id here would
        // reintroduce the re-append bug class.
        let datastar::DatastarEvent {
            event: kind,
            id,
            data,
            ..
        } = event;
        assert_eq!(kind, datastar::consts::EventType::PatchElements);
        assert_eq!(id.as_deref(), Some("7"));
        let joined = data.join("\n");
        assert!(
            joined.contains(&format!("selector #{}", contract::STREAMING_ID)),
            "{joined}"
        );
        assert!(joined.contains("mode inner"), "{joined}");
        assert!(joined.contains("elements <p>tail</p>"), "{joined}");
    }

    #[rstest::rstest]
    #[test]
    fn transcript_full_becomes_an_inner_replace_and_carries_no_id() {
        // Given a convergence envelope (cursor 0, full HTML).
        let event = sdk_events_of(&envelope(SseKind::TranscriptFull, "<p>all</p>", 0), 0)
            .expect("a transcript-full translates")
            .remove(0);

        // Then it replaces #transcript's inner HTML and carries no id
        // line — a reconnect resumes from the last real cursor, never
        // from this push (the re-append bug's fix).
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
        let event = sdk_events_of(&envelope(SseKind::Controls, "<button>go</button>", 0), 0)
            .expect("controls translate")
            .remove(0);

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
        let event = sdk_events_of(&envelope(SseKind::Sessions, "<a>row</a>", 0), 3)
            .expect("a sessions push translates")
            .remove(0);

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
        let event = sdk_events_of(&envelope(SseKind::Models, "2026-01-01T00:00:00Z", 0), 0)
            .expect("a models push translates")
            .remove(0);

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
    fn a_streaming_push_is_a_patch_then_a_chat_tick_bump() {
        // Given a streaming envelope (one flush tick's tail HTML).
        let events = sdk_events_of(&envelope(SseKind::Streaming, "<p>t</p>", 7), 3)
            .expect("a streaming push translates");

        // Then it is two events: the inner replace first (the DOM
        // changes), then the chatTick bump (the wrap's scroll effect
        // fires on the changed DOM — tick-after-render is the order
        // that scrolls to the NEW tail).
        assert_eq!(events.len(), 2, "{events:?}");
        let patch = events.first().expect("the patch rides first");
        let tick = events.get(1).expect("the tick rides second");
        assert_eq!(patch.event, datastar::consts::EventType::PatchElements);
        assert_eq!(tick.event, datastar::consts::EventType::PatchSignals);
        let joined = tick.data.join("\n");
        assert!(
            joined
                .replace(' ', "")
                .contains(&format!("\"{}\":3", contract::SIGNAL_CHAT_TICK)),
            "{joined}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn non_transcript_pushes_carry_no_chat_tick() {
        // Given a controls envelope (a region push that does not grow
        // the transcript).
        let events = sdk_events_of(&envelope(SseKind::Controls, "<button>b</button>", 0), 1)
            .expect("a controls push translates");

        // Then it is one event with no tick bump — a controls swap
        // must not scroll the transcript.
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(
            events.first().expect("the controls patch").event,
            datastar::consts::EventType::PatchElements
        );
    }

    #[rstest::rstest]
    #[test]
    fn skills_pushes_stamp_the_timestamp_into_the_tick_signal() {
        // Given a skills-changed envelope whose payload is the scan's
        // landing timestamp (non-empty — the dispatch contract).
        let event = sdk_events_of(&envelope(SseKind::Skills, "2026-01-01T00:00:00Z", 0), 0)
            .expect("a skills push translates")
            .remove(0);

        // Then the event is a signal patch carrying the timestamp, and
        // no HTML rides it (the pull remains the renderer).
        let datastar::DatastarEvent {
            event: kind, data, ..
        } = event;
        assert_eq!(kind, datastar::consts::EventType::PatchSignals);
        let joined = data.join("\n");
        assert!(
            joined.replace(' ', "").contains(&format!(
                "\"{}\":\"2026-01-01T00:00:00Z\"",
                contract::SIGNAL_SKILLS_TICK
            )),
            "{joined}"
        );
        assert!(!joined.contains('<'), "{joined}");
    }

    #[rstest::rstest]
    #[test]
    fn models_payloads_are_json_escaped() {
        // Given a models payload containing quotes and backslashes
        // (defensive: a timestamp never carries them, but the seam
        // must not emit malformed JSON if one ever does).
        let event = sdk_events_of(&envelope(SseKind::Models, "a\"b\\c", 0), 0)
            .expect("a models push translates")
            .remove(0);

        // Then the payload's quotes and backslashes are escaped —
        // the dataline parses as JSON in the browser.
        let datastar::DatastarEvent { data, .. } = event;
        let joined = data.join("\n");
        assert!(joined.contains("a\\\"b\\\\c"), "{joined}");
    }

    #[rstest::rstest]
    #[test]
    fn an_empty_streaming_tail_is_the_dropped_fanout_bug() {
        // Given a streaming envelope whose tail HTML is empty.
        let error = sdk_events_of(&envelope(SseKind::Streaming, "", 5), 0)
            .expect_err("an empty tail is a fan-out bug, dropped");

        // Then the seam names it: the endpoint drops the frame.
        assert_eq!(error, PushError::EmptyFragment);
    }

    #[rstest::rstest]
    #[test]
    fn an_empty_transcript_full_is_the_dropped_fanout_bug() {
        // Given a transcript-full envelope whose HTML is empty.
        let error = sdk_events_of(&envelope(SseKind::TranscriptFull, "", 0), 0)
            .expect_err("an empty full render is a fan-out bug, dropped");

        // Then the seam names it: the endpoint drops the frame.
        assert_eq!(error, PushError::EmptyFragment);
    }
}
