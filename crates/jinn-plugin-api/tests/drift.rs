#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::unreachable,
    reason = "test code"
)]

//! Drift tests — the hand-maintained schema must agree with the wire types.
//!
//! The schema file (`plugin-api.schema.json`) is the published artifact
//! third parties generate bindings from. These tests serialize representative
//! instances of every wire message and validate them against the schema, so
//! editing the types without the schema (or vice versa) fails the build.
//!
//! They also pin the forward-compatibility behavior: unknown `type` tags
//! deserialize to `Unknown` instead of erroring.

use jinn_plugin_api::{
    CancelStream, Envelope, HostToPlugin, InsertSystemEntry, PluginCitation, PluginToHost,
    PluginToHostOrHostToPlugin, PushCitations, RestartStalledStream, StreamEndEvent,
    StreamEndReason, StreamEventPing, StreamStartEvent, TickEvent, ToolCallEvent, ToolResultEvent,
    TurnEndEvent, Welcome,
};

/// Compiles the committed schema file for validation.
fn schema() -> jsonschema::Validator {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/plugin-api.schema.json");
    let schema_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("schema file readable"))
            .expect("schema file is valid JSON");
    jsonschema::validator_for(&schema_json).expect("schema compiles")
}

/// Serializes an envelope and validates it against the schema.
fn assert_valid(envelope: &Envelope) {
    let json = serde_json::to_value(envelope).expect("serialize");
    let errors: Vec<_> = schema().iter_errors(&json).map(|e| e.to_string()).collect();
    assert!(errors.is_empty(), "schema drift:\n{json}\n{errors:#?}");
}

/// Builds a fully-populated theme for fixtures.
#[rstest::rstest]
#[test]
fn hello_envelope_validates_against_schema() {
    // Given a Hello envelope.
    let envelope = Envelope::for_plugin(
        PluginToHost::Hello(jinn_plugin_api::Hello {
            protocol_version: 1,
            name: "themes".to_owned(),
            subscriptions: vec![],
        }),
        0,
        0,
    );

    // Then it validates against the committed schema.
    assert_valid(&envelope);
}

#[rstest::rstest]
#[test]
fn welcome_envelope_validates_against_schema() {
    // Given a Welcome envelope with grants and config.
    let envelope = Envelope::for_host(
        HostToPlugin::Welcome(Welcome {
            protocol_version: 1,
            plugin_id: "themes".to_owned(),
            read_dirs: vec!["/home/u/.config/jinn/themes".to_owned()],
            write_dirs: vec!["/home/u/.local/share/jinn/plugins/themes".to_owned()],
            http_allowed: false,
            config: serde_json::json!({ "refresh_seconds": 30 }),
        }),
        0,
        0,
    );

    // Then it validates against the committed schema.
    assert_valid(&envelope);
}

#[rstest::rstest]
#[test]
fn unknown_plugin_tag_deserializes_to_unknown() {
    // Given a wire line with a tag this build does not know, carrying data.
    let line = r#"{"v":1,"seq":7,"ts":0,"type":"future_message","payload":{"x":1}}"#;

    // When deserializing as a plugin→host envelope.
    let envelope: Envelope = serde_json::from_str(line).expect("tolerant deserialize");

    // Then the message is Unknown (payload dropped, no error).
    assert_eq!(
        envelope.msg,
        jinn_plugin_api::PluginToHostOrHostToPlugin::Unknown
    );
}

#[rstest::rstest]
#[test]
fn unknown_host_tag_deserializes_to_unknown() {
    // Given a host→plugin wire line with an unknown tag.
    let line = r#"{"v":1,"seq":0,"ts":0,"type":"future_event","detail":"hi"}"#;

    // When deserializing.
    let envelope: Envelope = serde_json::from_str(line).expect("tolerant deserialize");

    // Then the message is Unknown (direction is unknowable from an
    // unknown tag; the observable contract is "no message we understand").
    assert_eq!(envelope.msg, PluginToHostOrHostToPlugin::Unknown);
}

#[rstest::rstest]
#[test]
fn tool_call_event_envelope_validates_against_schema() {
    // Given a ToolCallEvent envelope.
    let envelope = Envelope::for_host(
        HostToPlugin::ToolCallEvent(ToolCallEvent {
            session_id: "s-1".to_owned(),
            tool_call_id: "call_1".to_owned(),
            name: "mcp__parallel__web_search".to_owned(),
            arguments: r#"{"objective":"find rust docs"}"#.to_owned(),
        }),
        0,
        0,
    );

    // Then it validates against the committed schema.
    assert_valid(&envelope);
}

#[rstest::rstest]
#[test]
fn tool_result_event_envelope_validates_against_schema() {
    // Given a ToolResultEvent envelope.
    let envelope = Envelope::for_host(
        HostToPlugin::ToolResultEvent(ToolResultEvent {
            session_id: "s-1".to_owned(),
            tool_call_id: "call_1".to_owned(),
            name: "mcp__parallel__web_fetch".to_owned(),
            content: r#"{"results":[{"url":"https://a","title":"A"}]}"#.to_owned(),
            success: true,
        }),
        1,
        0,
    );

    // Then it validates against the committed schema.
    assert_valid(&envelope);
}

#[rstest::rstest]
#[test]
fn turn_end_event_envelope_validates_against_schema() {
    // Given a TurnEndEvent envelope.
    let envelope = Envelope::for_host(
        HostToPlugin::TurnEndEvent(TurnEndEvent {
            session_id: "s-1".to_owned(),
            final_answer: true,
        }),
        2,
        0,
    );

    // Then it validates against the committed schema.
    assert_valid(&envelope);
}

#[rstest::rstest]
#[test]
fn push_citations_envelope_validates_against_schema() {
    // Given a PushCitations envelope with populated citations.
    let envelope = Envelope::for_plugin(
        PluginToHost::PushCitations(PushCitations {
            session_id: "s-1".to_owned(),
            citations: vec![
                PluginCitation {
                    url: "https://example.com/a".to_owned(),
                    title: "Example A".to_owned(),
                    content: Some("An excerpt.".to_owned()),
                },
                PluginCitation {
                    url: "https://example.com/b".to_owned(),
                    title: "https://example.com/b".to_owned(),
                    content: None,
                },
            ],
        }),
        1,
        0,
    );

    // Then it validates against the committed schema.
    assert_valid(&envelope);
}

#[rstest::rstest]
#[test]
fn push_citations_without_content_validates_against_schema() {
    // Given a PushCitations envelope whose citation has no content field.
    let line = r#"{"v":1,"seq":1,"ts":0,"type":"push_citations","session_id":"s-1","citations":[{"url":"https://example.com","title":"Example"}]}"#;

    // When deserializing.
    let envelope: Envelope = serde_json::from_str(line).expect("content is optional");

    // Then the citation deserialized with content absent.
    let PluginToHostOrHostToPlugin::Plugin(PluginToHost::PushCitations(msg)) = envelope.msg else {
        unreachable!("expected PushCitations");
    };
    assert_eq!(msg.citations.len(), 1);
    assert_eq!(msg.citations[0].content, None);
}

#[rstest::rstest]
#[test]
fn cancel_stream_envelope_validates_against_schema() {
    // Given a mirrored CancelStream envelope.
    let envelope = Envelope::for_plugin(
        PluginToHost::CancelStream(CancelStream {
            session_id: "s-1".to_owned(),
        }),
        3,
        0,
    );

    // Then it validates against the committed schema.
    assert_valid(&envelope);
}

#[rstest::rstest]
#[test]
fn insert_system_entry_envelope_validates_against_schema() {
    // Given a mirrored InsertSystemEntry envelope.
    let envelope = Envelope::for_plugin(
        PluginToHost::InsertSystemEntry(InsertSystemEntry {
            session_id: "s-1".to_owned(),
            text: "tool-call-watchdog: cancelling the stream".to_owned(),
        }),
        2,
        0,
    );

    // Then it validates against the committed schema.
    assert_valid(&envelope);
}

#[rstest::rstest]
#[test]
fn raw_line_with_cancel_stream_tag_parses_as_plugin_direction() {
    // Given a raw wire line carrying the cancel_stream tag.
    let line = r#"{"v":1,"seq":3,"ts":0,"type":"cancel_stream","session_id":"s-1"}"#;

    // When deserializing it as an envelope.
    let envelope: Envelope = serde_json::from_str(line).expect("known tag parses");

    // Then it routes to the Plugin direction with the payload intact —
    // proving the envelope tag-dispatch list was extended (an omitted tag
    // would silently degrade this to Unknown).
    assert_eq!(
        envelope.msg,
        PluginToHostOrHostToPlugin::Plugin(PluginToHost::CancelStream(CancelStream {
            session_id: "s-1".to_owned(),
        }))
    );
}

#[rstest::rstest]
#[test]
fn raw_line_with_insert_system_entry_tag_parses_as_plugin_direction() {
    // Given a raw wire line carrying the insert_system_entry tag.
    let line = r#"{"v":1,"seq":2,"ts":0,"type":"insert_system_entry","session_id":"s-1","text":"watchdog"}"#;

    // When deserializing it as an envelope.
    let envelope: Envelope = serde_json::from_str(line).expect("known tag parses");

    // Then it routes to the Plugin direction with the payload intact.
    assert_eq!(
        envelope.msg,
        PluginToHostOrHostToPlugin::Plugin(PluginToHost::InsertSystemEntry(InsertSystemEntry {
            session_id: "s-1".to_owned(),
            text: "watchdog".to_owned(),
        }))
    );
}

#[rstest::rstest]
#[test]
fn stream_start_event_envelope_validates_against_schema() {
    // Given a StreamStartEvent envelope.
    let envelope = Envelope::for_host(
        HostToPlugin::StreamStartEvent(StreamStartEvent {
            session_id: "s-1".to_owned(),
        }),
        0,
        0,
    );

    // Then it validates against the committed schema.
    assert_valid(&envelope);
}

#[rstest::rstest]
#[test]
fn stream_event_ping_envelope_validates_against_schema() {
    // Given a StreamEventPing envelope.
    let envelope = Envelope::for_host(
        HostToPlugin::StreamEventPing(StreamEventPing {
            session_id: "s-1".to_owned(),
        }),
        1,
        0,
    );

    // Then it validates against the committed schema.
    assert_valid(&envelope);
}

#[rstest::rstest]
#[test]
fn stream_end_event_envelope_validates_against_schema() {
    // Given a StreamEndEvent envelope.
    let envelope = Envelope::for_host(
        HostToPlugin::StreamEndEvent(StreamEndEvent {
            session_id: "s-1".to_owned(),
            reason: StreamEndReason::ToolUse,
        }),
        2,
        0,
    );

    // Then it validates against the committed schema.
    assert_valid(&envelope);
}

#[rstest::rstest]
#[test]
fn tick_event_envelope_validates_against_schema() {
    // Given a TickEvent envelope.
    let envelope = Envelope::for_host(
        HostToPlugin::Tick(TickEvent {
            now_ms: 1_700_000_000_000,
        }),
        3,
        0,
    );

    // Then it validates against the committed schema.
    assert_valid(&envelope);
}

#[rstest::rstest]
#[test]
fn restart_stalled_stream_envelope_validates_against_schema() {
    // Given a RestartStalledStream envelope.
    let envelope = Envelope::for_plugin(
        PluginToHost::RestartStalledStream(RestartStalledStream {
            session_id: "s-1".to_owned(),
            attempt: 1,
            max_restarts: 3,
        }),
        4,
        0,
    );

    // Then it validates against the committed schema.
    assert_valid(&envelope);
}

#[rstest::rstest]
#[test]
fn restart_stalled_stream_without_attempt_fields_defaults_to_first_attempt() {
    // Given a raw restart_stalled_stream line from a legacy peer that omits
    // the attempt fields.
    let line = r#"{"v":1,"seq":4,"ts":0,"type":"restart_stalled_stream","session_id":"s-1"}"#;

    // When it is parsed as an envelope.
    let parsed = serde_json::from_str::<Envelope>(line).expect("legacy line parses");

    // Then the attempt fields fall back to their defaults.
    assert_eq!(
        parsed.msg,
        PluginToHostOrHostToPlugin::Plugin(PluginToHost::RestartStalledStream(
            RestartStalledStream {
                session_id: "s-1".to_owned(),
                attempt: 1,
                max_restarts: 3,
            }
        ))
    );
}

#[rstest::rstest]
#[test]
fn raw_line_with_stream_event_tag_parses_as_host_direction() {
    // Given a raw wire line carrying the stream_event tag.
    let line = r#"{"v":1,"seq":0,"ts":0,"type":"stream_event","session_id":"s-1"}"#;

    // When deserializing it as an envelope.
    let envelope: Envelope = serde_json::from_str(line).expect("known tag parses");

    // Then it routes to the Host direction with the payload intact —
    // proving the envelope host-side tag-dispatch list was extended (an
    // omitted tag would silently degrade this to Unknown and the plugin
    // would never wake).
    assert_eq!(
        envelope.msg,
        PluginToHostOrHostToPlugin::Host(HostToPlugin::StreamEventPing(StreamEventPing {
            session_id: "s-1".to_owned(),
        }))
    );
}

#[rstest::rstest]
#[test]
fn raw_line_with_restart_stalled_stream_tag_parses_as_plugin_direction() {
    // Given a raw wire line carrying the restart_stalled_stream tag.
    let line = r#"{"v":1,"seq":9,"ts":0,"type":"restart_stalled_stream","session_id":"s-1"}"#;

    // When deserializing it as an envelope.
    let envelope: Envelope = serde_json::from_str(line).expect("known tag parses");

    // Then it routes to the Plugin direction with the payload intact — the
    // omitted attempt fields fall back to their defaults.
    assert_eq!(
        envelope.msg,
        PluginToHostOrHostToPlugin::Plugin(PluginToHost::RestartStalledStream(
            RestartStalledStream {
                session_id: "s-1".to_owned(),
                attempt: 1,
                max_restarts: 3,
            }
        ))
    );
}

#[rstest::rstest]
#[test]
fn stream_end_reason_round_trips_snake_case() {
    // Given every declared stream end reason.
    for (reason, wire) in [
        (StreamEndReason::Finished, "\"finished\""),
        (StreamEndReason::Canceled, "\"canceled\""),
        (StreamEndReason::ToolUse, "\"tool_use\""),
        (StreamEndReason::Error, "\"error\""),
    ] {
        // When round-tripping it through its wire string.
        let json = serde_json::to_string(&reason).expect("serialize");
        let back: StreamEndReason = serde_json::from_str(&json).expect("deserialize");

        // Then the wire form is snake_case and the round-trip is lossless.
        assert_eq!(json, wire);
        assert_eq!(back, reason);
    }
}

#[rstest::rstest]
#[test]
fn new_stream_envelopes_round_trip() {
    // Given each new wire message.
    let envelopes = vec![
        Envelope::for_host(
            HostToPlugin::StreamStartEvent(StreamStartEvent {
                session_id: "s".to_owned(),
            }),
            1,
            1,
        ),
        Envelope::for_host(
            HostToPlugin::StreamEventPing(StreamEventPing {
                session_id: "s".to_owned(),
            }),
            2,
            1,
        ),
        Envelope::for_host(
            HostToPlugin::StreamEndEvent(StreamEndEvent {
                session_id: "s".to_owned(),
                reason: StreamEndReason::Finished,
            }),
            3,
            1,
        ),
        Envelope::for_host(
            HostToPlugin::Tick(TickEvent {
                now_ms: 1_700_000_000_000,
            }),
            4,
            1,
        ),
        Envelope::for_plugin(
            PluginToHost::RestartStalledStream(RestartStalledStream {
                session_id: "s".to_owned(),
                attempt: 2,
                max_restarts: 3,
            }),
            5,
            1,
        ),
    ];

    // When round-tripping each through a JSON string.
    for envelope in envelopes {
        let json = serde_json::to_string(&envelope).expect("serialize");
        let back: Envelope = serde_json::from_str(&json).expect("deserialize");

        // Then it is unchanged.
        assert_eq!(envelope, back);
    }
}

#[rstest::rstest]
#[test]
fn new_event_and_contribution_envelopes_round_trip() {
    // Given each new wire message.
    let envelopes = vec![
        Envelope::for_host(
            HostToPlugin::ToolCallEvent(ToolCallEvent {
                session_id: "s".to_owned(),
                tool_call_id: "c".to_owned(),
                name: "n".to_owned(),
                arguments: "{}".to_owned(),
            }),
            1,
            1,
        ),
        Envelope::for_host(
            HostToPlugin::ToolResultEvent(ToolResultEvent {
                session_id: "s".to_owned(),
                tool_call_id: "c".to_owned(),
                name: "n".to_owned(),
                content: "out".to_owned(),
                success: false,
            }),
            2,
            1,
        ),
        Envelope::for_host(
            HostToPlugin::TurnEndEvent(TurnEndEvent {
                session_id: "s".to_owned(),
                final_answer: false,
            }),
            3,
            1,
        ),
        Envelope::for_plugin(
            PluginToHost::PushCitations(PushCitations {
                session_id: "s".to_owned(),
                citations: vec![PluginCitation {
                    url: "https://x".to_owned(),
                    title: "X".to_owned(),
                    content: None,
                }],
            }),
            4,
            1,
        ),
    ];

    // When round-tripping each through a JSON string.
    for envelope in envelopes {
        let json = serde_json::to_string(&envelope).expect("serialize");
        let back: Envelope = serde_json::from_str(&json).expect("deserialize");

        // Then it is unchanged.
        assert_eq!(envelope, back);
    }
}

#[rstest::rstest]
#[test]
fn envelope_round_trips_through_json() {
    // Given a populated envelope.
    let envelope = Envelope::for_plugin(
        PluginToHost::PushCitations(jinn_plugin_api::PushCitations {
            session_id: "00000000-0000-0000-0000-000000000000".to_owned(),
            citations: vec![],
        }),
        42,
        1_700_000_000_000,
    );

    // When round-tripping through a JSON string.
    let json = serde_json::to_string(&envelope).expect("serialize");
    let back: Envelope = serde_json::from_str(&json).expect("deserialize");

    // Then it is unchanged.
    assert_eq!(envelope, back);
}
