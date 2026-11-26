//! The wire contract's tests: every variant of both enums round-trips
//! through JSON with its `type` tag, and every entry kind survives the
//! trip inside a snapshot.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::unreachable,
    reason = "test assertions need expect, an index into the decoded JSON is itself the claim that the field is there, and a let-else on a known variant is unreachable by construction"
)]

use jinn_core_types::chat_entry::ChatEntryKind;
use jinn_core_types::{ChatEntry, ChatEntryId, ContextOverride, SessionId, ToolResultStatus};
use jinn_slices::PhaseView;
use rstest::rstest;

use crate::payload::{
    AppletRowBody, AppletRowsBody, DeltaField, EntryDeltaBody, EntryEnvelope, EntryUpsertBody,
    ErrorBody, ImageUploadBody, LifecycleOptionBody, McpBadgeBody, McpRowBody, McpRowsBody,
    ModelBody, ModelRowBody, ModelRowsBody, NewSessionOptionsBody, PersonaRowBody, PersonaRowsBody,
    PhaseBody, PickerRowBody, PickerRowsBody, SessionRowBody, SessionsBody, SkillRowBody,
    SkillRowsBody, SnapshotBody, TextRunBody, ThemeBody, TitleBody, ToolRowBody, ToolRowsBody,
};
use crate::{ClientMsg, ServerMsg, WEBUI2_PROTOCOL_VERSION};

fn roundtrip(msg: &ClientMsg) -> ClientMsg {
    let json = serde_json::to_string(msg).expect("client msg serializes");
    serde_json::from_str(&json).expect("client msg deserializes")
}

fn roundtrip_server(msg: &ServerMsg) -> ServerMsg {
    let json = serde_json::to_string(msg).expect("server msg serializes");
    serde_json::from_str(&json).expect("server msg deserializes")
}

/// Serializes a message to its JSON value — how round-trips of
/// entry-carrying messages are compared, since `ChatEntry` has no
/// `PartialEq` and the wire form is the observable behavior.
fn json_of(msg: &ServerMsg) -> serde_json::Value {
    serde_json::to_value(msg).expect("server msg serializes")
}

fn json_of_client(msg: &ClientMsg) -> serde_json::Value {
    serde_json::to_value(msg).expect("client msg serializes")
}

fn row(title: &str) -> SessionRowBody {
    SessionRowBody {
        session_id: SessionId::new(),
        title: title.to_owned(),
        is_active: false,
        is_busy: false,
        in_library: false,
    }
}

/// Builds one stored-session picker row.
fn picker_row(title: &str) -> PickerRowBody {
    PickerRowBody {
        session_id: SessionId::new(),
        title: title.to_owned(),
        state: jinn_session_store_msg::SessionState::Archived,
        parent_id: Some(SessionId::new()),
        project: Some("jinn".to_owned()),
    }
}

/// Builds one skills-modal row: one matched run and one plain run in
/// each field, so a round trip proves the split survives.
fn skill_row(name: &str) -> SkillRowBody {
    SkillRowBody {
        name: name.to_owned(),
        description: "does a thing".to_owned(),
        enabled: true,
        loaded: false,
        is_project: true,
        name_runs: matched_name_runs(name),
        description_runs: plain_runs("does a thing"),
    }
}

/// A name's runs: `matched` highlighted, the rest plain.
fn matched_name_runs(name: &str) -> Vec<TextRunBody> {
    vec![
        TextRunBody {
            text: "web".to_owned(),
            matched: true,
        },
        TextRunBody {
            text: name.strip_prefix("web").unwrap_or(name).to_owned(),
            matched: false,
        },
    ]
}

/// One whole-text plain run.
fn plain_runs(text: &str) -> Vec<TextRunBody> {
    vec![TextRunBody {
        text: text.to_owned(),
        matched: false,
    }]
}

/// Builds one tools-modal row with a matched name run.
fn tool_row(name: &str) -> ToolRowBody {
    ToolRowBody {
        name: name.to_owned(),
        description: "does a thing".to_owned(),
        enabled: true,
        name_runs: matched_name_runs(name),
        description_runs: plain_runs("does a thing"),
    }
}

/// Builds one MCP-servers row with a matched name run and a running badge.
fn mcp_row(name: &str) -> McpRowBody {
    McpRowBody {
        name: name.to_owned(),
        description: "jinn luau serve x.lua".to_owned(),
        enabled: true,
        badge: McpBadgeBody::Running,
        name_runs: matched_name_runs(name),
        description_runs: plain_runs("jinn luau serve x.lua"),
    }
}

/// Builds one persona row with a matched name run.
fn persona_row(name: &str) -> PersonaRowBody {
    PersonaRowBody {
        name: name.to_owned(),
        description: "does a thing".to_owned(),
        is_active: true,
        name_runs: matched_name_runs(name),
        description_runs: plain_runs("does a thing"),
    }
}

/// Builds one applets row: the title as one plain run, linked when `href`
/// is `Some` and disabled when `None`.
fn applet_row(id: &str, title: &str, href: Option<&str>) -> AppletRowBody {
    AppletRowBody {
        id: id.to_owned(),
        title: title.to_owned(),
        title_runs: plain_runs(title),
        href: href.map(str::to_owned),
    }
}

/// Builds one entry of every `ChatEntryKind` variant, in the enum's order.
fn every_kind() -> Vec<EntryEnvelope> {
    let compaction = ChatEntryKind::Compaction {
        summary: "summary".to_owned(),
        tokens_before: 100,
        tokens_after: 10,
        entries_compacted: 5,
        model_used: "m".to_owned(),
    };
    let intersession = ChatEntryKind::Intersession {
        source_session: SessionId::new(),
        source_name: Some("src".to_owned()),
        target_session: SessionId::new(),
        text: "hello".to_owned(),
    };
    let literal = |kind: ChatEntryKind| ChatEntry {
        id: ChatEntryId::new(),
        timing: jinn_core_types::entry_timing::EntryTiming::instant_now(),
        kind,
        pin_position: None,
        context_override: ContextOverride::default(),
        context_history: Vec::new(),
        token_count: None,
        model_used: None,
        model_requested: None,
    };
    [
        ChatEntry::user("hi"),
        ChatEntry::system("sys"),
        ChatEntry::error("err"),
        ChatEntry::assistant("answer"),
        ChatEntry::actor("bot", "report"),
        ChatEntry::thinking("hmm"),
        ChatEntry::tool_call("c1", "bash", "{}"),
        ChatEntry::tool_result("c1", "bash", "out", ToolResultStatus::Success),
        ChatEntry::transient("note"),
        literal(compaction),
        ChatEntry::annotation(Vec::new()),
        ChatEntry::rule_interrupt("rule", "body"),
        literal(intersession),
    ]
    .map(EntryEnvelope::new)
    .to_vec()
}

#[rstest]
#[test]
fn client_hello_with_cursor_round_trips() {
    // Given a hello naming the newest entry the page holds.
    let id = ChatEntryId::new();
    let msg = ClientMsg::Hello {
        last_entry_id: Some(id),
        revision: None,
    };

    // When it round-trips.
    let back = roundtrip(&msg);

    // Then it is unchanged.
    assert_eq!(back, msg);
}

#[rstest]
#[test]
fn client_hello_without_cursor_round_trips() {
    // Given a hello from a fresh page.
    let msg = ClientMsg::Hello {
        last_entry_id: None,
        revision: None,
    };

    // When it round-trips.
    let back = roundtrip(&msg);

    // Then the cursor is still absent.
    assert_eq!(back, msg);
}

#[rstest]
#[test]
fn client_hello_without_cursor_carries_an_explicit_null_tag() {
    // Given a hello from a fresh page.
    let msg = ClientMsg::Hello {
        last_entry_id: None,
        revision: None,
    };

    // When serialized.
    let json = serde_json::to_value(&msg).expect("serializes");

    // Then the tag is `hello` and the cursor is an explicit null.
    assert_eq!(json.get("type"), Some(&serde_json::json!("hello")));
    assert_eq!(json.get("last_entry_id"), Some(&serde_json::Value::Null));
}

#[rstest]
#[test]
fn client_send_round_trips() {
    // Given a send.
    let msg = ClientMsg::Send {
        text: "hello".to_owned(),
        images: Vec::new(),
    };

    // When it round-trips.
    let back = roundtrip(&msg);

    // Then the text is unchanged.
    assert_eq!(back, msg);
}

#[rstest]
#[test]
fn client_queue_round_trips() {
    // Given a queue.
    let msg = ClientMsg::Queue {
        text: "later".to_owned(),
        images: Vec::new(),
    };

    // When it round-trips.
    let back = roundtrip(&msg);

    // Then it is unchanged.
    assert_eq!(back, msg);
}

#[rstest]
#[test]
fn client_cancel_round_trips() {
    // Given a cancel.
    let msg = ClientMsg::Cancel;

    // When it round-trips.
    let back = roundtrip(&msg);

    // Then it is unchanged.
    assert_eq!(back, msg);
}

#[rstest]
#[test]
fn client_activate_session_round_trips() {
    // Given an activate.
    let msg = ClientMsg::ActivateSession {
        session_id: SessionId::new(),
    };

    // When it round-trips.
    let back = roundtrip(&msg);

    // Then it is unchanged.
    assert_eq!(back, msg);
}

#[rstest]
#[test]
fn client_switch_model_round_trips() {
    // Given a model switch.
    let msg = ClientMsg::SwitchModel {
        provider_id: "ollama/llama3".to_owned(),
    };

    // When it round-trips.
    let back = roundtrip(&msg);

    // Then the row id is unchanged.
    assert_eq!(back, msg);
}

#[rstest]
#[test]
fn client_rename_round_trips() {
    // Given a rename.
    let msg = ClientMsg::Rename {
        title: "new name".to_owned(),
    };

    // When it round-trips.
    let back = roundtrip(&msg);

    // Then it is unchanged.
    assert_eq!(back, msg);
}

#[rstest]
#[test]
fn client_picker_rows_round_trips() {
    // Given a picker-rows request.
    let msg = ClientMsg::PickerRows {
        filter: "jinn".to_owned(),
    };

    // When it round-trips.
    let back = roundtrip(&msg);

    // Then the filter is unchanged.
    assert_eq!(back, msg);
}

#[rstest]
#[test]
fn client_model_rows_round_trips() {
    // Given a model-rows request.
    let msg = ClientMsg::ModelRows {
        filter: "llama".to_owned(),
    };

    // When it round-trips.
    let back = roundtrip(&msg);

    // Then the filter is unchanged.
    assert_eq!(back, msg);
}

#[rstest]
#[test]
fn server_snapshot_round_trips() {
    // Given a snapshot with one session and two entries.
    let session = SessionId::new();
    let msg = ServerMsg::Snapshot(Box::new(SnapshotBody {
        protocol: WEBUI2_PROTOCOL_VERSION,
        session_id: Some(session.clone()),
        phase: PhaseBody {
            session_id: session.clone(),
            phase: PhaseView::Streaming,
            queue_len: 0,
        },
        model_label: "ollama/llama3".to_owned(),
        title: "title".to_owned(),
        revision: 0,
        entries: [ChatEntry::user("hi"), ChatEntry::assistant("yo")]
            .map(EntryEnvelope::new)
            .to_vec(),
        sessions: vec![row("a"), row("b")],
        library: Vec::new(),
        theme: String::new(),
    }));

    // When it round-trips.
    let back = roundtrip_server(&msg);

    // Then it is unchanged.
    assert_eq!(json_of(&back), json_of(&msg));
}

#[rstest]
#[test]
fn server_snapshot_without_active_session_round_trips() {
    // Given a snapshot for an instance with no active session.
    let session = SessionId::new();
    let msg = ServerMsg::Snapshot(Box::new(SnapshotBody {
        protocol: WEBUI2_PROTOCOL_VERSION,
        session_id: None,
        phase: PhaseBody {
            session_id: session,
            phase: PhaseView::Idle,
            queue_len: 0,
        },
        model_label: String::new(),
        title: String::new(),
        revision: 0,
        entries: Vec::new(),
        sessions: Vec::new(),
        library: Vec::new(),
        theme: String::new(),
    }));

    // When it round-trips.
    let back = roundtrip_server(&msg);

    // Then the absent session is still absent.
    assert_eq!(json_of(&back), json_of(&msg));
}

#[rstest]
#[test]
fn server_entry_upsert_round_trips() {
    // Given an upsert of a tool result.
    let msg = ServerMsg::EntryUpsert(Box::new(EntryUpsertBody {
        session_id: SessionId::new(),
        index: 7,
        entry: EntryEnvelope::new(ChatEntry::tool_result(
            "c1",
            "bash",
            "out",
            ToolResultStatus::Pending,
        )),
    }));

    // When it round-trips.
    let back = roundtrip_server(&msg);

    // Then the position and the entry are unchanged.
    assert_eq!(json_of(&back), json_of(&msg));
}

#[rstest]
#[test]
fn server_entry_delta_round_trips() {
    // Given a delta for a tool call's arguments.
    let msg = ServerMsg::EntryDelta(EntryDeltaBody {
        session_id: SessionId::new(),
        entry_id: ChatEntryId::new(),
        field: DeltaField::Args,
        chunk: "{\"cmd\":".to_owned(),
    });

    // When it round-trips.
    let back = roundtrip_server(&msg);

    // Then it is unchanged.
    assert_eq!(json_of(&back), json_of(&msg));
}

#[rstest]
#[test]
fn server_phase_round_trips() {
    // Given a phase change to cancelling.
    let session = SessionId::new();
    let msg = ServerMsg::Phase(PhaseBody {
        session_id: session,
        phase: PhaseView::Cancelling,
        queue_len: 0,
    });

    // When it round-trips.
    let back = roundtrip_server(&msg);

    // Then it is unchanged.
    assert_eq!(json_of(&back), json_of(&msg));
}

#[rstest]
#[test]
fn server_sessions_round_trips() {
    // Given a session list.
    let msg = ServerMsg::Sessions(SessionsBody {
        rows: vec![row("one"), row("two")],
        library: vec![row("kept")],
    });

    // When it round-trips.
    let back = roundtrip_server(&msg);

    // Then the rows are unchanged.
    assert_eq!(json_of(&back), json_of(&msg));
}

#[rstest]
#[test]
fn server_model_round_trips() {
    // Given a model change.
    let msg = ServerMsg::Model(ModelBody {
        session_id: SessionId::new(),
        label: "openai/gpt".to_owned(),
    });

    // When it round-trips.
    let back = roundtrip_server(&msg);

    // Then the label is unchanged.
    assert_eq!(json_of(&back), json_of(&msg));
}

#[rstest]
#[test]
fn server_title_round_trips() {
    // Given a title change.
    let msg = ServerMsg::Title(TitleBody {
        session_id: SessionId::new(),
        title: "renamed".to_owned(),
    });

    // When it round-trips.
    let back = roundtrip_server(&msg);

    // Then the title is unchanged.
    assert_eq!(json_of(&back), json_of(&msg));
}

#[rstest]
#[test]
fn server_picker_rows_round_trips() {
    // Given picker rows answering a filter.
    let msg = ServerMsg::PickerRows(PickerRowsBody {
        filter: "ji".to_owned(),
        rows: vec![picker_row("jinn work")],
    });

    // When it round-trips.
    let back = roundtrip_server(&msg);

    // Then it is unchanged.
    assert_eq!(json_of(&back), json_of(&msg));
}

#[rstest]
#[test]
fn server_model_rows_round_trips() {
    // Given model rows answering a filter.
    let msg = ServerMsg::ModelRows(ModelRowsBody {
        session_id: SessionId::new(),
        filter: "ll".to_owned(),
        rows: vec![ModelRowBody {
            provider_id: "ollama/llama3".to_owned(),
            model: "llama3".to_owned(),
            provider_name: "ollama".to_owned(),
            is_available: true,
            is_active: true,
        }],
    });

    // When it round-trips.
    let back = roundtrip_server(&msg);

    // Then it is unchanged.
    assert_eq!(json_of(&back), json_of(&msg));
}

#[rstest]
#[test]
fn client_skill_rows_round_trips() {
    // Given a skills-rows request carrying a filter.
    let msg = ClientMsg::SkillRows {
        filter: "we".to_owned(),
    };

    // When it round-trips.
    let back = roundtrip(&msg);

    // Then it is unchanged.
    assert_eq!(json_of_client(&back), json_of_client(&msg));
}

#[rstest]
#[test]
fn client_toggle_skill_round_trips() {
    // Given a toggle naming one skill.
    let msg = ClientMsg::ToggleSkill {
        skill: "web-coder".to_owned(),
    };

    // When it round-trips.
    let back = roundtrip(&msg);

    // Then it is unchanged.
    assert_eq!(json_of_client(&back), json_of_client(&msg));
}

#[rstest]
#[test]
fn client_load_skill_round_trips() {
    // Given a load naming one skill.
    let msg = ClientMsg::LoadSkill {
        skill: "web-coder".to_owned(),
    };

    // When it round-trips.
    let back = roundtrip(&msg);

    // Then it is unchanged.
    assert_eq!(json_of_client(&back), json_of_client(&msg));
}

#[rstest]
#[test]
fn server_skill_rows_round_trips() {
    // Given skill rows answering a filter, with their matched runs.
    let msg = ServerMsg::SkillRows(SkillRowsBody {
        session_id: SessionId::new(),
        filter: "we".to_owned(),
        rows: vec![skill_row("web-coder")],
    });

    // When it round-trips.
    let back = roundtrip_server(&msg);

    // Then it is unchanged.
    assert_eq!(json_of(&back), json_of(&msg));
}

#[rstest]
#[test]
fn client_tool_rows_round_trips() {
    // Given a tools-rows request carrying a filter.
    let msg = ClientMsg::ToolRows {
        filter: "ge".to_owned(),
    };

    // When it round-trips.
    let back = roundtrip(&msg);

    // Then it is unchanged.
    assert_eq!(json_of_client(&back), json_of_client(&msg));
}

#[rstest]
#[test]
fn client_toggle_tool_round_trips() {
    // Given a toggle naming one tool.
    let msg = ClientMsg::ToggleTool {
        tool: "get_time".to_owned(),
    };

    // When it round-trips.
    let back = roundtrip(&msg);

    // Then it is unchanged.
    assert_eq!(json_of_client(&back), json_of_client(&msg));
}

#[rstest]
#[test]
fn client_mcp_rows_round_trips() {
    // Given an MCP-rows request carrying a filter.
    let msg = ClientMsg::McpRows {
        filter: "ec".to_owned(),
    };

    // When it round-trips.
    let back = roundtrip(&msg);

    // Then it is unchanged.
    assert_eq!(json_of_client(&back), json_of_client(&msg));
}

#[rstest]
#[test]
fn client_toggle_mcp_round_trips() {
    // Given a toggle naming one server.
    let msg = ClientMsg::ToggleMcp {
        server: "e2e-echo".to_owned(),
    };

    // When it round-trips.
    let back = roundtrip(&msg);

    // Then it is unchanged.
    assert_eq!(json_of_client(&back), json_of_client(&msg));
}

#[rstest]
#[test]
fn client_persona_rows_round_trips() {
    // Given a persona-rows request carrying a filter.
    let msg = ClientMsg::PersonaRows {
        filter: "br".to_owned(),
    };

    // When it round-trips.
    let back = roundtrip(&msg);

    // Then it is unchanged.
    assert_eq!(json_of_client(&back), json_of_client(&msg));
}

#[rstest]
#[test]
fn client_switch_persona_round_trips() {
    // Given a switch naming one persona.
    let msg = ClientMsg::SwitchPersona {
        persona: "brainstorm".to_owned(),
    };

    // When it round-trips.
    let back = roundtrip(&msg);

    // Then it is unchanged.
    assert_eq!(json_of_client(&back), json_of_client(&msg));
}

#[rstest]
#[test]
fn server_tool_rows_round_trips() {
    // Given tool rows answering a filter, with their matched runs.
    let msg = ServerMsg::ToolRows(ToolRowsBody {
        session_id: SessionId::new(),
        filter: "ge".to_owned(),
        rows: vec![tool_row("get_time")],
    });

    // When it round-trips.
    let back = roundtrip_server(&msg);

    // Then it is unchanged.
    assert_eq!(json_of(&back), json_of(&msg));
}

#[rstest]
#[test]
fn server_mcp_rows_round_trips() {
    // Given MCP rows answering a filter, with their matched runs and badge.
    let msg = ServerMsg::McpRows(McpRowsBody {
        session_id: SessionId::new(),
        filter: "ec".to_owned(),
        rows: vec![mcp_row("e2e-echo")],
    });

    // When it round-trips.
    let back = roundtrip_server(&msg);

    // Then it is unchanged.
    assert_eq!(json_of(&back), json_of(&msg));
}

#[rstest]
#[test]
fn server_persona_rows_round_trips() {
    // Given persona rows answering a filter, with their matched runs.
    let msg = ServerMsg::PersonaRows(PersonaRowsBody {
        session_id: SessionId::new(),
        filter: "br".to_owned(),
        rows: vec![persona_row("brainstorm")],
    });

    // When it round-trips.
    let back = roundtrip_server(&msg);

    // Then it is unchanged.
    assert_eq!(json_of(&back), json_of(&msg));
}

#[rstest]
#[test]
fn client_applet_rows_round_trips() {
    // Given a rows ask under a filter.
    let msg = ClientMsg::AppletRows {
        filter: "qui".to_owned(),
    };

    // When it round-trips.
    let back = roundtrip(&msg);

    // Then it is unchanged.
    assert_eq!(back, msg);
}

#[rstest]
#[test]
fn server_applet_rows_round_trips() {
    // Given serving rows answering a filter: one linked, one disabled.
    let msg = ServerMsg::AppletRows(AppletRowsBody {
        filter: String::new(),
        serving: true,
        bind: "127.0.0.1:41234".to_owned(),
        rows: vec![
            applet_row("quiz", "Quiz", Some("http://127.0.0.1:41234/applets/quiz/")),
            applet_row("escape", "Escape", None),
        ],
    });

    // When it round-trips.
    let back = roundtrip_server(&msg);

    // Then it is unchanged — the linked row keeps its href, the disabled
    // row keeps its explicit null, and no session rides on the frame.
    assert_eq!(json_of(&back), json_of(&msg));
    let json = json_of(&back);
    assert!(json.get("session_id").is_none(), "{json}");
}

#[rstest]
#[test]
fn mcp_badge_serializes_as_its_lowercase_word() {
    // Given one row carrying each badge state.
    let badges = [
        McpBadgeBody::Off,
        McpBadgeBody::Starting,
        McpBadgeBody::Running,
        McpBadgeBody::Dead,
    ];

    // When each is serialized on its own.
    let words = badges
        .iter()
        .map(|badge| serde_json::to_value(badge).expect("badge serializes"))
        .collect::<Vec<_>>();

    // Then each is the lowercase word the page renders.
    assert_eq!(
        words,
        [
            serde_json::json!("off"),
            serde_json::json!("starting"),
            serde_json::json!("running"),
            serde_json::json!("dead"),
        ]
    );
}

#[rstest]
#[test]
fn server_error_round_trips() {
    // Given a refusal.
    let msg = ServerMsg::Error(ErrorBody {
        message: "session is busy".to_owned(),
    });

    // When it round-trips.
    let back = roundtrip_server(&msg);

    // Then the message is unchanged.
    assert_eq!(json_of(&back), json_of(&msg));
}

#[rstest]
#[test]
fn server_ping_round_trips_as_a_bare_tag() {
    // Given a heartbeat.
    let msg = ServerMsg::Ping;

    // When it is serialized.
    let json = serde_json::to_value(&msg).expect("ping serializes");

    // Then it is the type tag alone, and reads back unchanged.
    assert_eq!(json, serde_json::json!({ "type": "ping" }));
    assert_eq!(json_of(&roundtrip_server(&msg)), json_of(&msg));
}

#[rstest]
#[test]
fn server_variant_carries_its_snake_case_tag() {
    // Given one message of every server variant.
    let session = SessionId::new();
    let phase = PhaseBody {
        session_id: session.clone(),
        phase: PhaseView::Idle,
        queue_len: 0,
    };
    let messages = [
        (
            ServerMsg::Snapshot(Box::new(SnapshotBody {
                protocol: WEBUI2_PROTOCOL_VERSION,
                session_id: None,
                phase: phase.clone(),
                model_label: String::new(),
                title: String::new(),
                revision: 0,
                entries: Vec::new(),
                sessions: Vec::new(),
                library: Vec::new(),
                theme: String::new(),
            })),
            "snapshot",
        ),
        (
            ServerMsg::EntryUpsert(Box::new(EntryUpsertBody {
                session_id: SessionId::new(),
                index: 0,
                entry: EntryEnvelope::new(ChatEntry::user("hi")),
            })),
            "entry_upsert",
        ),
        (
            ServerMsg::EntryDelta(EntryDeltaBody {
                session_id: SessionId::new(),
                entry_id: ChatEntryId::new(),
                field: DeltaField::Text,
                chunk: "x".to_owned(),
            }),
            "entry_delta",
        ),
        (ServerMsg::Phase(phase.clone()), "phase"),
        (
            ServerMsg::Sessions(SessionsBody {
                rows: Vec::new(),
                library: Vec::new(),
            }),
            "sessions",
        ),
        (
            ServerMsg::Model(ModelBody {
                session_id: session.clone(),
                label: String::new(),
            }),
            "model",
        ),
        (
            ServerMsg::Title(TitleBody {
                session_id: session.clone(),
                title: String::new(),
            }),
            "title",
        ),
        (
            ServerMsg::PickerRows(PickerRowsBody {
                filter: String::new(),
                rows: Vec::new(),
            }),
            "picker_rows",
        ),
        (
            ServerMsg::ModelRows(ModelRowsBody {
                session_id: SessionId::new(),
                filter: String::new(),
                rows: Vec::new(),
            }),
            "model_rows",
        ),
        (
            ServerMsg::SkillRows(SkillRowsBody {
                session_id: SessionId::new(),
                filter: String::new(),
                rows: Vec::new(),
            }),
            "skill_rows",
        ),
        (
            ServerMsg::ToolRows(ToolRowsBody {
                session_id: SessionId::new(),
                filter: String::new(),
                rows: Vec::new(),
            }),
            "tool_rows",
        ),
        (
            ServerMsg::McpRows(McpRowsBody {
                session_id: SessionId::new(),
                filter: String::new(),
                rows: Vec::new(),
            }),
            "mcp_rows",
        ),
        (
            ServerMsg::PersonaRows(PersonaRowsBody {
                session_id: SessionId::new(),
                filter: String::new(),
                rows: Vec::new(),
            }),
            "persona_rows",
        ),
        (
            ServerMsg::AppletRows(AppletRowsBody {
                filter: String::new(),
                serving: false,
                bind: String::new(),
                rows: Vec::new(),
            }),
            "applet_rows",
        ),
        (
            ServerMsg::NewSessionOptions(NewSessionOptionsBody {
                projects: Vec::new(),
                lifecycles: Vec::new(),
            }),
            "new_session_options",
        ),
        (
            ServerMsg::Theme(ThemeBody {
                theme: String::new(),
            }),
            "theme",
        ),
        (
            ServerMsg::Error(ErrorBody {
                message: String::new(),
            }),
            "error",
        ),
        (ServerMsg::Ping, "ping"),
    ];

    // Then every variant's wire tag matches its snake_case name.
    for (msg, tag) in messages {
        let json = serde_json::to_value(&msg).expect("serializes");
        assert_eq!(json.get("type"), Some(&serde_json::json!(tag)));
        assert_eq!(crate::server::tag_of(&msg), tag);
    }
}

#[rstest]
#[test]
fn client_variant_carries_its_snake_case_tag() {
    // Given one message of every client variant.
    let messages = [
        (
            ClientMsg::Hello {
                last_entry_id: None,
                revision: None,
            },
            "hello",
        ),
        (
            ClientMsg::Send {
                text: String::new(),
                images: Vec::new(),
            },
            "send",
        ),
        (
            ClientMsg::Queue {
                text: String::new(),
                images: Vec::new(),
            },
            "queue",
        ),
        (ClientMsg::Cancel, "cancel"),
        (
            ClientMsg::ActivateSession {
                session_id: SessionId::new(),
            },
            "activate_session",
        ),
        (
            ClientMsg::SwitchModel {
                provider_id: String::new(),
            },
            "switch_model",
        ),
        (
            ClientMsg::Rename {
                title: String::new(),
            },
            "rename",
        ),
        (
            ClientMsg::PickerRows {
                filter: String::new(),
            },
            "picker_rows",
        ),
        (
            ClientMsg::ModelRows {
                filter: String::new(),
            },
            "model_rows",
        ),
        (
            ClientMsg::SkillRows {
                filter: String::new(),
            },
            "skill_rows",
        ),
        (
            ClientMsg::ToggleSkill {
                skill: String::new(),
            },
            "toggle_skill",
        ),
        (
            ClientMsg::LoadSkill {
                skill: String::new(),
            },
            "load_skill",
        ),
        (
            ClientMsg::ToolRows {
                filter: String::new(),
            },
            "tool_rows",
        ),
        (
            ClientMsg::ToggleTool {
                tool: String::new(),
            },
            "toggle_tool",
        ),
        (
            ClientMsg::McpRows {
                filter: String::new(),
            },
            "mcp_rows",
        ),
        (
            ClientMsg::ToggleMcp {
                server: String::new(),
            },
            "toggle_mcp",
        ),
        (
            ClientMsg::PersonaRows {
                filter: String::new(),
            },
            "persona_rows",
        ),
        (
            ClientMsg::SwitchPersona {
                persona: String::new(),
            },
            "switch_persona",
        ),
        (
            ClientMsg::AppletRows {
                filter: String::new(),
            },
            "applet_rows",
        ),
        (ClientMsg::RefreshModels, "refresh_models"),
        (
            ClientMsg::SetTheme {
                theme: String::new(),
            },
            "set_theme",
        ),
        (
            ClientMsg::SetLibrary {
                session_id: SessionId::new(),
                in_library: true,
            },
            "set_library",
        ),
        (
            ClientMsg::ArchiveSession {
                session_id: SessionId::new(),
            },
            "archive_session",
        ),
        (ClientMsg::NewSessionOptions, "new_session_options"),
        (
            ClientMsg::CreateSession {
                project: String::new(),
                lifecycle: String::new(),
                args: String::new(),
            },
            "create_session",
        ),
    ];

    // Then every variant's wire tag matches its snake_case name.
    for (msg, tag) in messages {
        let json = json_of_client(&msg);
        assert_eq!(json.get("type"), Some(&serde_json::json!(tag)));
        assert_eq!(crate::client::tag_of(&msg), tag);
    }
}

#[rstest]
#[test]
fn every_entry_kind_round_trips_inside_a_snapshot() {
    // Given a snapshot carrying all thirteen entry kinds.
    let session = SessionId::new();
    let sent = every_kind();
    let msg = ServerMsg::Snapshot(Box::new(SnapshotBody {
        protocol: WEBUI2_PROTOCOL_VERSION,
        session_id: Some(session.clone()),
        phase: PhaseBody {
            session_id: session.clone(),
            phase: PhaseView::Idle,
            queue_len: 0,
        },
        model_label: String::new(),
        title: String::new(),
        revision: 0,
        entries: sent.clone(),
        sessions: Vec::new(),
        library: Vec::new(),
        theme: String::new(),
    }));

    // When it round-trips.
    let back = roundtrip_server(&msg);
    let ServerMsg::Snapshot(snapshot) = &back else {
        unreachable!("a round-tripped snapshot is a snapshot");
    };

    // Then every entry keeps its kind name and its id.
    let kept = snapshot
        .entries
        .iter()
        .map(|envelope| (envelope.kind.clone(), envelope.entry.id.clone()))
        .collect::<Vec<_>>();
    let sent = sent
        .iter()
        .map(|envelope| (envelope.kind.clone(), envelope.entry.id.clone()))
        .collect::<Vec<_>>();
    assert_eq!(kept, sent);
}

#[rstest]
#[test]
fn a_round_tripped_transient_entry_reads_back_as_a_system_entry() {
    // Given the core persisted form's one deliberate asymmetry: a
    // transient is never persisted, so its map has no deserializer arm.
    let transient = EntryEnvelope::new(ChatEntry::transient("note"));

    // When only the inner entry round-trips, without the envelope.
    let json = serde_json::to_string(&transient.entry).expect("serializes");
    let entry: ChatEntry = serde_json::from_str(&json).expect("deserializes");

    // Then the core reads it back as a system entry — the reason the
    // envelope carries the kind name the browser dispatches on.
    assert_eq!(entry.kind_str(), "system");
}

#[rstest]
#[test]
fn every_entry_kind_serializes_as_the_frozen_persisted_map_tag() {
    // Given all thirteen entry kinds.
    let entries = every_kind();

    // When each is serialized on its own.
    let tags = entries
        .iter()
        .map(|envelope| {
            let json = serde_json::to_value(&envelope.entry.kind).expect("kind serializes");
            // The persisted map is externally tagged: its single key is
            // the PascalCase variant name.
            json.as_object()
                .and_then(|map| map.keys().next())
                .cloned()
                .expect("a persisted entry map has one key")
        })
        .collect::<Vec<_>>();

    // Then each entry is the session store's frozen externally-tagged map,
    // named by its PascalCase variant name.
    assert_eq!(
        tags,
        [
            "User",
            "System",
            "Error",
            "Assistant",
            "Actor",
            "Thinking",
            "ToolCall",
            "ToolResult",
            "Transient",
            "Compaction",
            "Annotation",
            "RuleInterrupt",
            "Intersession",
        ]
    );
}

#[rstest]
#[test]
fn entry_envelope_names_every_kind_in_snake_case() {
    // Given one entry of every kind, wrapped for the wire.
    let envelopes = every_kind();

    // When each envelope's explicit kind name is read.
    let names = envelopes
        .iter()
        .map(|envelope| envelope.kind.as_str())
        .collect::<Vec<_>>();

    // Then they are the thirteen snake_case names, in order.
    assert_eq!(
        names,
        [
            "user",
            "system",
            "error",
            "assistant",
            "actor",
            "thinking",
            "tool_call",
            "tool_result",
            "transient",
            "compaction",
            "annotation",
            "rule_interrupt",
            "intersession",
        ]
    );
}

#[rstest]
#[test]
fn a_transient_entry_keeps_its_kind_name_through_the_wire() {
    // Given a transient entry — the one kind the core's persisted
    // deserializer deliberately has no arm for.
    let msg = ServerMsg::EntryUpsert(Box::new(EntryUpsertBody {
        session_id: SessionId::new(),
        index: 0,
        entry: EntryEnvelope::new(ChatEntry::transient("welcome")),
    }));

    // When it round-trips.
    let back = roundtrip_server(&msg);
    let ServerMsg::EntryUpsert(body) = &back else {
        unreachable!("a round-tripped upsert is an upsert");
    };

    // Then the envelope still names it a transient.
    assert_eq!(body.entry.kind, "transient");
}

#[rstest]
#[test]
fn delta_field_has_exactly_four_values() {
    // Given every delta field name.
    let names = [
        DeltaField::Text,
        DeltaField::Thinking,
        DeltaField::Args,
        DeltaField::Output,
    ]
    .map(|field| serde_json::to_value(field).expect("field serializes"));

    // When serialized.
    // Then they are the four streamed fields and nothing else.
    assert_eq!(
        names,
        [
            serde_json::json!("text"),
            serde_json::json!("thinking"),
            serde_json::json!("args"),
            serde_json::json!("output"),
        ]
    );
}

#[rstest]
#[test]
fn a_frame_with_an_unknown_type_tag_is_rejected() {
    // Given a frame whose tag names no client variant.
    let json = r#"{"type":"teleport"}"#;

    // When it is parsed as a client message.
    let result: Result<ClientMsg, _> = serde_json::from_str(json);

    // Then parsing fails rather than guessing a variant.
    assert!(result.is_err());
}

#[rstest]
#[test]
fn phase_serializes_in_snake_case() {
    // Given every phase.
    let phases = [
        PhaseView::Idle,
        PhaseView::Streaming,
        PhaseView::Working,
        PhaseView::Cancelling,
    ];

    // When each is serialized.
    let names = phases
        .iter()
        .map(|phase| serde_json::to_value(phase).expect("phase serializes"))
        .collect::<Vec<_>>();

    // Then the names are snake_case.
    assert_eq!(
        names,
        [
            serde_json::json!("Idle"),
            serde_json::json!("Streaming"),
            serde_json::json!("Working"),
            serde_json::json!("Cancelling"),
        ]
    );
}

#[rstest]
#[test]
fn client_send_carries_its_images() {
    // Given a send with one pasted image.
    let msg = ClientMsg::Send {
        text: String::new(),
        images: vec![ImageUploadBody {
            name: "pasted image".to_owned(),
            data_base64: "aGVsbG8=".to_owned(),
        }],
    };

    // When it round-trips.
    let back = roundtrip(&msg);

    // Then the image survives the trip.
    assert_eq!(back, msg);
}

#[rstest]
#[test]
fn client_queue_carries_its_images() {
    // Given a queue with one picked file.
    let msg = ClientMsg::Queue {
        text: "later".to_owned(),
        images: vec![ImageUploadBody {
            name: "photo.png".to_owned(),
            data_base64: "aGVsbG8=".to_owned(),
        }],
    };

    // When it round-trips.
    let back = roundtrip(&msg);

    // Then the image survives the trip.
    assert_eq!(back, msg);
}

#[rstest]
#[test]
fn client_refresh_models_round_trips() {
    // Given a refresh request.
    let msg = ClientMsg::RefreshModels;

    // When it round-trips.
    let back = roundtrip(&msg);

    // Then it is unchanged.
    assert_eq!(back, msg);
}

#[rstest]
#[test]
fn client_set_theme_round_trips() {
    // Given a theme change to light.
    let msg = ClientMsg::SetTheme {
        theme: "light".to_owned(),
    };

    // When it round-trips.
    let back = roundtrip(&msg);

    // Then the mode is unchanged.
    assert_eq!(back, msg);
}

#[rstest]
#[test]
fn client_set_library_round_trips() {
    // Given a library toggle.
    let msg = ClientMsg::SetLibrary {
        session_id: SessionId::new(),
        in_library: true,
    };

    // When it round-trips.
    let back = roundtrip(&msg);

    // Then it is unchanged.
    assert_eq!(back, msg);
}

#[rstest]
#[test]
fn client_archive_session_round_trips() {
    // Given an archive request.
    let msg = ClientMsg::ArchiveSession {
        session_id: SessionId::new(),
    };

    // When it round-trips.
    let back = roundtrip(&msg);

    // Then it is unchanged.
    assert_eq!(back, msg);
}

#[rstest]
#[test]
fn client_new_session_options_round_trips() {
    // Given a request for the form's choices.
    let msg = ClientMsg::NewSessionOptions;

    // When it round-trips.
    let back = roundtrip(&msg);

    // Then it is unchanged.
    assert_eq!(back, msg);
}

#[rstest]
#[test]
fn client_create_session_round_trips() {
    // Given a creation naming a project, a lifecycle, and args.
    let msg = ClientMsg::CreateSession {
        project: "~/proj".to_owned(),
        lifecycle: "setup".to_owned(),
        args: "a b".to_owned(),
    };

    // When it round-trips.
    let back = roundtrip(&msg);

    // Then it is unchanged.
    assert_eq!(back, msg);
}

#[rstest]
#[test]
fn server_new_session_options_round_trips() {
    // Given the form's choices: two projects and a lifecycle taking one named arg.
    let msg = ServerMsg::NewSessionOptions(NewSessionOptionsBody {
        projects: vec!["~/a".to_owned(), "~/b".to_owned()],
        lifecycles: vec![LifecycleOptionBody {
            name: "setup".to_owned(),
            param_count: 1,
            params: vec!["<name>".to_owned()],
        }],
    });

    // When it round-trips.
    let back = roundtrip_server(&msg);

    // Then the choices are unchanged.
    assert_eq!(json_of(&back), json_of(&msg));
}

#[rstest]
#[test]
fn server_theme_round_trips() {
    // Given a theme change to system.
    let msg = ServerMsg::Theme(ThemeBody {
        theme: "system".to_owned(),
    });

    // When it round-trips.
    let back = roundtrip_server(&msg);

    // Then the mode is unchanged.
    assert_eq!(json_of(&back), json_of(&msg));
}

#[rstest]
#[test]
fn snapshot_carries_its_library_rows_and_theme() {
    // Given a snapshot with one library row and the dark theme.
    let session = SessionId::new();
    let msg = ServerMsg::Snapshot(Box::new(SnapshotBody {
        protocol: WEBUI2_PROTOCOL_VERSION,
        session_id: Some(session.clone()),
        phase: PhaseBody {
            session_id: session,
            phase: PhaseView::Idle,
            queue_len: 0,
        },
        model_label: String::new(),
        title: String::new(),
        revision: 0,
        entries: Vec::new(),
        sessions: Vec::new(),
        library: vec![row("kept")],
        theme: "dark".to_owned(),
    }));

    // When it round-trips.
    let json = json_of(&roundtrip_server(&msg));

    // Then the library row and the theme are on the wire.
    assert_eq!(json["library"][0]["title"], "kept");
    assert_eq!(json["theme"], "dark");
}

#[rstest]
#[test]
fn sessions_frame_carries_its_library_rows() {
    // Given a sessions frame with a member.
    let mut member = row("kept");
    member.in_library = true;
    let msg = ServerMsg::Sessions(SessionsBody {
        rows: Vec::new(),
        library: vec![member],
    });

    // When it round-trips.
    let json = json_of(&roundtrip_server(&msg));

    // Then the member row carries its flag.
    assert_eq!(json["library"][0]["in_library"], true);
}
