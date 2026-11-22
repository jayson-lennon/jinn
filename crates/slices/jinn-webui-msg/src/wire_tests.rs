//! Wire-contract tests: the cursor grammar, the SSE envelope names,
//! and the session-list row shape. Pure data — no server, no bus.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

use crate::wire::{
    CancelForm, ModelForm, SendForm, SessionListItem, SessionsQuery, SseEnvelope, SseKind,
    TranscriptQuery,
};
use jinn_core_types::SessionId;

#[rstest::rstest]
#[test]
fn sessions_query_absent_q_reads_as_no_filter() {
    // Given the query shape an unfiltered pull maps from (no `q` key).
    let query: SessionsQuery = serde_json::from_str("{}").expect("empty object parses");

    // Then no filter is in force.
    assert_eq!(query.filter_text(), None);
    assert_eq!(query, SessionsQuery::default());
}

#[rstest::rstest]
#[test]
fn sessions_query_empty_q_reads_as_no_filter() {
    // Given a query whose `q` is the empty string (the input was
    // cleared but still sent).
    let query = SessionsQuery {
        q: Some(String::new()),
        viewed: None,
    };

    // Then no filter is in force — the matcher's empty-pattern
    // behavior never defines the unfiltered case.
    assert_eq!(query.filter_text(), None);
}

#[rstest::rstest]
#[test]
fn sessions_query_text_names_the_filter() {
    // Given a query carrying filter text.
    let query = SessionsQuery {
        q: Some("fix".to_owned()),
        viewed: None,
    };

    // When reading the filter.
    // Then the text rides through verbatim.
    assert_eq!(query.filter_text(), Some("fix"));
}

#[rstest::rstest]
#[test]
fn sessions_query_absent_viewed_reads_as_no_viewer() {
    // Given the landing page's pull shape (no `viewed` key at all).
    let query: SessionsQuery = serde_json::from_str("{}").expect("empty object parses");

    // Then nothing is viewed — the landing page's no-viewer render.
    assert_eq!(query.viewed_session(), None);
}

#[rstest::rstest]
#[test]
fn sessions_query_empty_viewed_reads_as_no_viewer() {
    // Given a pull whose viewer signal is bound but empty (the
    // landing page's `$viewedSession` serializes as "").
    let query = SessionsQuery {
        q: None,
        viewed: Some(String::new()),
    };

    // Then nothing is viewed — an empty viewer string is the landing
    // page's shape, never a phantom active row.
    assert_eq!(query.viewed_session(), None);
}

#[rstest::rstest]
#[test]
fn sessions_query_a_well_formed_viewed_id_parses() {
    // Given a pull carrying the viewed session's id.
    let id = SessionId::new();
    let query = SessionsQuery {
        q: None,
        viewed: Some(id.to_string()),
    };

    // When reading the viewer.
    // Then the id parses through to the typed value.
    assert_eq!(query.viewed_session(), Some(id));
}

#[rstest::rstest]
#[test]
fn sessions_query_a_stale_viewed_string_reads_as_no_viewer() {
    // Given a pull whose viewer string is not a UUID (a page pulled
    // with a signal value from a previous page shape).
    let query = SessionsQuery {
        q: None,
        viewed: Some("not-a-uuid".to_owned()),
    };

    // Then the pull degrades to no viewer rather than failing — the
    // pull renders a list, not a validation surface.
    assert_eq!(query.viewed_session(), None);
}

#[rstest::rstest]
#[test]
fn session_page_opened_carries_its_session() {
    // Given the arrival event for one session.
    let id = SessionId::new();
    let event = crate::wire::SessionPageOpened {
        session_id: id.clone(),
    };

    // Then the event round-trips — the fan-out handler receives the
    // same session id the page handler published.
    let json = serde_json::to_string(&event).expect("the event serializes");
    let parsed: crate::wire::SessionPageOpened =
        serde_json::from_str(&json).expect("the event parses back");
    assert_eq!(parsed.session_id, id);
}

#[rstest::rstest]
#[test]
fn transcript_since_cursor_within_history_starts_there() {
    // Given a query naming a cursor inside a 7-entry history.
    let query = TranscriptQuery { since: Some(3) };

    // When resolving the cursor.
    let cursor = query.cursor_within(7);

    // Then the pull starts at the named index.
    assert_eq!(cursor, Some(3));
}

#[rstest::rstest]
#[test]
fn transcript_cursor_beyond_the_length_rereads_everything() {
    // Given a query whose cursor names the end of a shrunken history.
    let query = TranscriptQuery { since: Some(7) };

    // When resolving against a history that now holds 5 entries.
    let cursor = query.cursor_within(5);

    // Then the pull re-syncs with a full render — a rewind must not
    // leave the page silently stale.
    assert_eq!(cursor, None);
}

#[rstest::rstest]
#[test]
fn transcript_query_absent_since_deserializes_to_none() {
    // Given the JSON shape the query deserializer maps from
    // (an absent `since` key).
    let query: TranscriptQuery = serde_json::from_str("{}").expect("empty object parses");

    // Then the query reads as a full pull.
    assert_eq!(query.since, None);
    assert_eq!(query, TranscriptQuery::default());
}

#[rstest::rstest]
#[test]
fn action_forms_deserialize_their_fields() {
    // Given the JSON each form's field set maps through (the same
    // serde shape axum's Form extractor drives).
    let send: SendForm = serde_json::from_str("{\"session_id\":\"abc\",\"text\":\"hello world\"}")
        .expect("send parses");
    let cancel: CancelForm =
        serde_json::from_str("{\"session_id\":\"abc\"}").expect("cancel parses");
    let model: ModelForm =
        serde_json::from_str("{\"session_id\":\"abc\",\"provider_id\":\"openai/gpt-4o\"}")
            .expect("model parses");

    // Then every field lands.
    assert_eq!(send.session_id, "abc");
    assert_eq!(send.text, "hello world");
    assert_eq!(cancel.session_id, "abc");
    assert_eq!(model.provider_id, "openai/gpt-4o");
}

#[rstest::rstest]
#[test]
fn sse_envelope_serializes_its_kind_for_the_wire() {
    // Given one envelope of each kind.
    let id = SessionId::new();
    let envelopes = (
        SseEnvelope {
            kind: SseKind::Chat,
            session_id: id.clone(),
            cursor: 3,
            html: "<span>x</span>".to_owned(),
        },
        SseEnvelope {
            kind: SseKind::Controls,
            session_id: id.clone(),
            cursor: 0,
            html: String::new(),
        },
        SseEnvelope {
            kind: SseKind::Sessions,
            session_id: id.clone(),
            cursor: 0,
            html: String::new(),
        },
        SseEnvelope {
            kind: SseKind::TranscriptFull,
            session_id: id.clone(),
            cursor: 0,
            html: String::new(),
        },
    );

    // When serializing (the wire direction is server→client only).
    let json = serde_json::to_string(&envelopes.0).expect("serialize");

    // Then the kind serializes under its kebab-case name and the
    // cursor rides along; the other kinds name themselves distinctly.
    assert!(json.contains("\"kind\":\"chat\""), "{json}");
    assert!(json.contains("\"cursor\":3"), "{json}");
    assert_ne!(SseKind::Chat.event_name(), SseKind::Controls.event_name());
    assert_ne!(
        SseKind::Sessions.event_name(),
        SseKind::TranscriptFull.event_name()
    );
}

#[rstest::rstest]
#[test]
fn session_list_row_roundtrips_its_flags() {
    // Given one loaded active row.
    let row = SessionListItem {
        session_id: SessionId::new(),
        title: "Fix the thing".to_owned(),
        active: true,
        is_bot: false,
        loaded: true,
        unread: false,
        working: true,
        depth: 0,
    };

    // When serializing (the wire direction is server→client only).
    let json = serde_json::to_string(&row).expect("serialize");

    // Then the flags and title ride the wire.
    assert!(json.contains("\"title\":\"Fix the thing\""), "{json}");
    assert!(json.contains("\"active\":true"), "{json}");
    assert!(json.contains("\"loaded\":true"), "{json}");
    assert!(json.contains("\"is_bot\":false"), "{json}");
    assert!(json.contains("\"unread\":false"), "{json}");
    assert!(json.contains("\"working\":true"), "{json}");
    assert!(json.contains("\"depth\":0"), "{json}");
}
