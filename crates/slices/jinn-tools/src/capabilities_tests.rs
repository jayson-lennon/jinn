//! Tests for the `capabilities_query` / `capabilities_apply` tools.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test assertions"
)]

use super::{apply_definition, apply_execute, query_definition, query_execute};
use crate::tool_types::ToolContext;
use jinn_core_types::FilterMode;
use jinn_core_types::NameFilter;
use jinn_core_types::tool_types::{ToolCall, ToolDefinition};
use jinn_kernel::common::app_state::AppState;
use jinn_kernel::common::services::bus_service::{BusAudit, BusService};
use jinn_kernel::common::state::State;
use jinn_mcp_msg::McpEnablementChanged;
use jinn_preferences_config::schemas::mcp::{McpServerConfig, McpServersConfig};
use jinn_session_state::ChatSessionState;
use jinn_session_store_msg::PersistSession;
use marked_path::{Absolute, MarkedPathBuf};

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A bare context whose cwd is a real, existing scratch directory owned by
/// the calling test: both tools must fail gracefully.
fn bare_ctx(cwd: &MarkedPathBuf<Absolute>) -> ToolContext {
    ToolContext {
        turn: crate::tool_types::test_turn(),
        cwd: cwd.clone(),
        config: jinn_config::testutil::config_layer(""),
        shells: Default::default(),
        timeout: None,
        state: None,
        session_id: None,
        app_paths: jinn_kernel::common::app_paths::AppPaths::default(),
        bus: None,
        max_output_lines: None,
        max_output_bytes: None,
        dispatched_at: jiff::Timestamp::now(),
        mcp_coordinator: None,
        canvas_server: None,
        interactive_term: None,
        task_spawns: None,
        session_store: None,
        trouper_system: None,
        model_name_resolver: None,
    }
}

/// A tool call for one of the two tools with JSON arguments.
fn call(name: &str, args: serde_json::Value) -> ToolCall {
    ToolCall {
        id: jinn_core_types::ToolCallId::from(format!("call_{name}")),
        name: name.to_owned(),
        arguments: args.to_string(),
    }
}

/// A `capabilities_query` call.
fn query_call() -> ToolCall {
    call("capabilities_query", serde_json::json!({}))
}

/// A `capabilities_apply` call carrying `mask` under `capabilities`.
fn apply_call(mask: serde_json::Value) -> ToolCall {
    call(
        "capabilities_apply",
        serde_json::json!({ "capabilities": mask }),
    )
}

/// The harness: catalog-backed state, one session seeded into it, the
/// recording bus, and a context bound to all three.
struct Harness {
    state: State,
    session_id: jinn_core_types::SessionId,
    audit: BusAudit,
    ctx: ToolContext,
    /// Held so the temp project dir outlives the seeded skill paths.
    _tmp: jinn_testutil::JinnTempDir,
}

impl Harness {
    /// Builds the harness with no configured MCP servers.
    fn new() -> Self {
        Self::with_config(&[])
    }

    /// Builds the harness with the given configured MCP servers and a
    /// catalog-registered registry cell, one session, and a recording bus.
    /// The servers are seeded through the layer (the real resolution path),
    /// not a hand-written TOML document.
    fn with_config(servers: &[&str]) -> Self {
        let tmp = jinn_testutil::tempdir();
        let state = State::new(AppState::default_with_scope_focus());
        // The default SessionMap already holds one active session; its id is
        // what every seeded edit and the tool context bind to.
        let session_id = state.read().session.active_session_id().clone();
        let config = jinn_config::testutil::config_layer("");
        if !servers.is_empty() {
            let configured = servers
                .iter()
                .map(|name| ((*name).to_owned(), McpServerConfig::default()))
                .collect();
            config
                .put::<McpServersConfig>(&McpServersConfig(configured))
                .expect("seed the mcp section");
        }
        let (bus, audit) = BusService::new_recording();
        let ctx = ToolContext {
            config,
            state: Some(state.clone()),
            session_id: Some(session_id.clone()),
            bus: Some(bus),
            ..bare_ctx(&tmp.path)
        };
        Self {
            state,
            session_id,
            audit,
            ctx,
            _tmp: tmp,
        }
    }

    /// Registers a tool definition into the global registry cell.
    fn register_tool(&self, name: &str) {
        let registry = self
            .state
            .read()
            .tool_registry()
            .expect("catalog registers the tools registry cell");
        registry.update(|r| {
            r.global.insert(
                name.to_owned(),
                ToolDefinition {
                    name: name.to_owned(),
                    description: String::new(),
                    parameters: serde_json::json!({ "type": "object" }),
                    prompt_snippet: None,
                    prompt_guidelines: vec![],
                    server_tool_type: None,
                },
            );
        });
    }

    /// Registers a per-session tool definition (an `mcp__*` tool).
    fn register_session_tool(&self, session_id: &jinn_core_types::SessionId, name: &str) {
        let registry = self
            .state
            .read()
            .tool_registry()
            .expect("catalog registers the tools registry cell");
        registry.update(|r| {
            r.session.entry(session_id.clone()).or_default().insert(
                name.to_owned(),
                ToolDefinition {
                    name: name.to_owned(),
                    description: String::new(),
                    parameters: serde_json::json!({ "type": "object" }),
                    prompt_snippet: None,
                    prompt_guidelines: vec![],
                    server_tool_type: None,
                },
            );
        });
    }

    /// Seeds two discovered skills into the session.
    fn seed_two_skills(&self, first: &str, second: &str) {
        let mut guard = self.state.write();
        let session = guard.session.get_mut(&self.session_id).expect("session");
        session.set_discovered_skills(vec![
            jinn_skills_msg::Skill {
                name: first.to_owned(),
                description: String::new(),
                body: String::new(),
                file_path: self
                    ._tmp
                    .path
                    .join_all([first, "SKILL.md"])
                    .expect("temp root joins a bare name"),
                base_dir: self
                    ._tmp
                    .path
                    .join(first)
                    .expect("temp root joins a bare name"),
                source: jinn_skills_msg::SkillSource::Global,
            },
            jinn_skills_msg::Skill {
                name: second.to_owned(),
                description: String::new(),
                body: String::new(),
                file_path: self
                    ._tmp
                    .path
                    .join_all([second, "SKILL.md"])
                    .expect("temp root joins a bare name"),
                base_dir: self
                    ._tmp
                    .path
                    .join(second)
                    .expect("temp root joins a bare name"),
                source: jinn_skills_msg::SkillSource::Global,
            },
        ]);
    }

    /// Mutates the session through `f`.
    fn edit_session(&self, f: impl FnOnce(&mut ChatSessionState)) {
        let mut guard = self.state.write();
        f(guard.session.get_mut(&self.session_id).expect("session"));
    }

    /// Reads a snapshot of the session's three capability fields.
    fn session_caps(
        &self,
    ) -> (
        Option<jinn_core_types::NameFilter>,
        Option<jinn_core_types::NameFilter>,
        std::collections::BTreeSet<String>,
    ) {
        let guard = self.state.read();
        let session = guard.session.get(&self.session_id).expect("session");
        (
            session.tool_filter().cloned(),
            session.skill_filter().cloned(),
            session.enabled_mcp_servers().clone(),
        )
    }

    /// Every recorded publish of message type `M`.
    fn published<M: jinn_slices::BusMessage>(&self) -> Vec<M> {
        self.audit.of_type::<M>()
    }
}

// ---------------------------------------------------------------------------
// query: buffer composition
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn query_lists_every_registered_tool_with_its_state() {
    // Given a session with two global tools and one session-scoped mcp tool,
    // `bash` withheld by a deny filter.
    let h = Harness::new();
    h.register_tool("bash");
    h.register_tool("read");
    h.register_session_tool(&h.session_id, "mcp__github__create_pr");
    h.edit_session(|s| s.set_tool_filter(Some(NameFilter::deny(["bash".to_owned()]))));

    // When querying capabilities.
    let result = query_execute(query_call(), h.ctx.clone()).await;

    // Then every registered tool appears exactly once, global and
    // session-scoped alike, with the filter's state on each.
    let parsed: serde_json::Value = serde_json::from_str(&result.content).expect("json");
    let tools = &parsed["tools"];
    assert_eq!(
        tools["bash"],
        serde_json::json!(false),
        "withheld by filter"
    );
    assert_eq!(tools["read"], serde_json::json!(true));
    assert_eq!(
        tools["mcp__github__create_pr"],
        serde_json::json!(true),
        "session's mcp tools are maskable entries"
    );
    assert_eq!(
        tools.as_object().expect("tools object").len(),
        3,
        "no tool listed twice or invented"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn query_lists_every_discovered_skill_with_its_state() {
    // Given a session with two discovered skills, one withheld.
    let h = Harness::new();
    h.seed_two_skills("rust-programming", "that-voice");
    h.edit_session(|s| s.set_skill_filter(Some(NameFilter::deny(["that-voice".to_owned()]))));

    // When querying capabilities.
    let result = query_execute(query_call(), h.ctx.clone()).await;

    // Then both skills appear with the filter's state on each.
    let parsed: serde_json::Value = serde_json::from_str(&result.content).expect("json");
    assert_eq!(
        parsed["skills"]["rust-programming"],
        serde_json::json!(true)
    );
    assert_eq!(parsed["skills"]["that-voice"], serde_json::json!(false));
}

#[rstest::rstest]
#[tokio::test]
async fn query_reports_server_state_from_the_session_set_not_config_presence() {
    // Given config declaring two servers, the session's set enabling only
    // one (an auto_enable-seeded set the coordinator wrote, or a picker
    // commit).
    let h = Harness::with_config(&["excalimate", "context7"]);
    h.edit_session(|s| {
        s.set_enabled_mcp_servers(std::collections::BTreeSet::from(["excalimate".to_owned()]))
    });

    // When querying capabilities.
    let result = query_execute(query_call(), h.ctx.clone()).await;

    // Then both configured servers are listed, but state reads the
    // session's set: config presence alone never reports a server as on.
    let parsed: serde_json::Value = serde_json::from_str(&result.content).expect("json");
    assert_eq!(parsed["mcp_servers"]["excalimate"], serde_json::json!(true));
    assert_eq!(
        parsed["mcp_servers"]["context7"],
        serde_json::json!(false),
        "configured but not session-enabled is off, not absent"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn an_allow_filter_session_reports_unlisted_entries_as_off() {
    // Given a session under an allow filter permitting only `read`.
    let h = Harness::new();
    h.register_tool("bash");
    h.register_tool("read");
    h.edit_session(|s| {
        s.set_tool_filter(Some(NameFilter {
            mode: FilterMode::Allow,
            names: ["read".to_owned()].into_iter().collect(),
        }))
    });

    // When querying capabilities.
    let result = query_execute(query_call(), h.ctx.clone()).await;

    // Then `read` is on and everything the allow filter withholds is off —
    // the same answer the prompt assembler and the gate give.
    let parsed: serde_json::Value = serde_json::from_str(&result.content).expect("json");
    assert_eq!(parsed["tools"]["read"], serde_json::json!(true));
    assert_eq!(parsed["tools"]["bash"], serde_json::json!(false));
}

#[rstest::rstest]
#[tokio::test]
async fn query_fails_gracefully_without_state() {
    // Given a bare context with no state and no session.
    let dir = jinn_testutil::tempdir();
    let ctx = bare_ctx(&dir.path);

    // When querying capabilities.
    let result = query_execute(query_call(), ctx).await;

    // Then the result fails with a message, not a panic.
    assert!(!result.success);
    assert!(
        result.content.contains("no active session"),
        "the failure should name the missing prerequisite, got: {}",
        result.content
    );
}

// ---------------------------------------------------------------------------
// apply: commit shape
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn apply_writes_the_tool_deny_filter() {
    // Given a session with three tools and a mask turning one off.
    let h = Harness::new();
    h.register_tool("bash");
    h.register_tool("read");
    h.register_tool("edit");
    let mask = serde_json::json!({
        "tools": {"bash": false, "read": true, "edit": true},
        "skills": {},
        "mcp_servers": {}
    });

    // When applying.
    let result = apply_execute(apply_call(mask), h.ctx.clone()).await;

    // Then the session's tool filter is a deny filter naming exactly the off
    // tool.
    assert!(result.success, "apply should succeed: {}", result.content);
    let (tool_filter, _, _) = h.session_caps();
    let filter = tool_filter.expect("a filter was committed");
    assert_eq!(filter.mode, FilterMode::Deny);
    assert_eq!(
        filter.names,
        ["bash".to_owned()].into_iter().collect(),
        "deny list carries exactly the off tools"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn apply_writes_the_skill_deny_filter() {
    // Given a session with two discovered skills and a mask turning one off.
    let h = Harness::new();
    h.seed_two_skills("rust-programming", "that-voice");
    let mask = serde_json::json!({
        "tools": {},
        "skills": {"rust-programming": true, "that-voice": false},
        "mcp_servers": {}
    });

    // When applying.
    let result = apply_execute(apply_call(mask), h.ctx.clone()).await;

    // Then the session's skill filter is a deny filter naming exactly the
    // off skill.
    assert!(result.success, "apply should succeed: {}", result.content);
    let (_, skill_filter, _) = h.session_caps();
    let filter = skill_filter.expect("a filter was committed");
    assert_eq!(filter.mode, FilterMode::Deny);
    assert_eq!(
        filter.names,
        ["that-voice".to_owned()].into_iter().collect()
    );
}

#[rstest::rstest]
#[tokio::test]
async fn apply_writes_the_enabled_server_set() {
    // Given two configured servers, one session-enabled, and a mask
    // flipping which one is on.
    let h = Harness::with_config(&["excalimate", "context7"]);
    h.edit_session(|s| {
        s.set_enabled_mcp_servers(std::collections::BTreeSet::from(["excalimate".to_owned()]))
    });
    let mask = serde_json::json!({
        "tools": {},
        "skills": {},
        "mcp_servers": {"excalimate": false, "context7": true}
    });

    // When applying.
    let result = apply_execute(apply_call(mask), h.ctx.clone()).await;

    // Then the session's enabled set is exactly the mask's on servers.
    assert!(result.success, "apply should succeed: {}", result.content);
    let (_, _, enabled) = h.session_caps();
    assert_eq!(
        enabled,
        ["context7".to_owned()].into_iter().collect(),
        "the enabled set is replaced, not merged"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_successful_apply_publishes_both_messages() {
    // Given a valid complete mask.
    let h = Harness::new();
    h.register_tool("read");
    let mask = serde_json::json!({
        "tools": {"read": true},
        "skills": {},
        "mcp_servers": {}
    });

    // When applying.
    let result = apply_execute(apply_call(mask), h.ctx.clone()).await;

    // Then McpEnablementChanged carries the full desired set and
    // PersistSession names the session — both messages, on an unchanged
    // family too.
    assert!(result.success);
    let changed = h.published::<McpEnablementChanged>();
    assert_eq!(changed.len(), 1, "exactly one enablement publish");
    assert_eq!(changed[0].session_id, h.session_id);
    assert!(changed[0].enabled.is_empty());
    let persisted = h.published::<PersistSession>();
    assert_eq!(persisted.len(), 1, "exactly one persist publish");
    assert_eq!(persisted[0].session_id, h.session_id);
}

#[rstest::rstest]
#[tokio::test]
async fn a_rejected_apply_publishes_nothing() {
    // Given a mask missing an entry.
    let h = Harness::new();
    h.register_tool("read");
    h.register_tool("edit");
    let mask = serde_json::json!({
        "tools": {"read": false},
        "skills": {},
        "mcp_servers": {}
    });

    // When applying.
    let result = apply_execute(apply_call(mask), h.ctx.clone()).await;

    // Then the bus recorded nothing: rejection is whole, not partial.
    assert!(!result.success);
    assert!(h.audit.is_empty(), "no message may ride a rejected apply");
}

// ---------------------------------------------------------------------------
// apply: atomic rejection
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn an_incomplete_mask_is_rejected_whole_with_fields_untouched() {
    // Given a session holding a pre-existing filter and enablement, and a
    // mask covering only one of its two tools.
    let h = Harness::new();
    h.register_tool("bash");
    h.register_tool("read");
    h.edit_session(|s| {
        s.set_tool_filter(Some(NameFilter::deny(["read".to_owned()])));
        s.set_enabled_mcp_servers(std::collections::BTreeSet::from(["github".to_owned()]));
    });
    let before = h.session_caps();
    let mask = serde_json::json!({
        "tools": {"bash": false},
        "skills": {},
        "mcp_servers": {}
    });

    // When applying.
    let result = apply_execute(apply_call(mask), h.ctx.clone()).await;

    // Then the apply is rejected and every field still reads as before.
    assert!(!result.success);
    assert_eq!(h.session_caps(), before, "no field may move on a rejection");
    assert!(
        result.content.contains("Current buffer"),
        "the rejection carries the current buffer for re-echo, got: {}",
        result.content
    );
}

#[rstest::rstest]
#[tokio::test]
async fn an_unknown_entry_is_rejected_whole() {
    // Given a mask naming a tool this session does not have.
    let h = Harness::new();
    h.register_tool("read");
    let before = h.session_caps();
    let mask = serde_json::json!({
        "tools": {"read": true, "ghost_tool": false},
        "skills": {},
        "mcp_servers": {}
    });

    // When applying.
    let result = apply_execute(apply_call(mask), h.ctx.clone()).await;

    // Then the apply is rejected with the stale name in the message and no
    // field moved.
    assert!(!result.success);
    assert!(result.content.contains("ghost_tool"));
    assert_eq!(h.session_caps(), before);
}

#[rstest::rstest]
#[tokio::test]
async fn an_entry_in_the_wrong_family_is_rejected() {
    // Given a session with one configured server and a mask listing that
    // server's name under tools.
    let h = Harness::with_config(&["excalimate"]);
    let before = h.session_caps();
    let mask = serde_json::json!({
        "tools": {"excalimate": false},
        "skills": {},
        "mcp_servers": {"excalimate": true}
    });

    // When applying.
    let result = apply_execute(apply_call(mask), h.ctx.clone()).await;

    // Then the mask is rejected: kinds are matched exactly, and a tool
    // named `excalimate` does not exist even though the server does.
    assert!(!result.success);
    assert_eq!(h.session_caps(), before);
}

#[rstest::rstest]
#[tokio::test]
async fn a_mask_with_a_non_boolean_state_is_rejected() {
    // Given a mask whose entry state is a string.
    let h = Harness::new();
    let before = h.session_caps();
    let mask = serde_json::json!({
        "tools": {"read": "yes"},
        "skills": {},
        "mcp_servers": {}
    });

    // When applying.
    let result = apply_execute(apply_call(mask), h.ctx.clone()).await;

    // Then the apply fails at parse time and nothing changed.
    assert!(!result.success);
    assert!(result.content.contains("true or false"));
    assert_eq!(h.session_caps(), before);
}

#[rstest::rstest]
#[tokio::test]
async fn a_mask_missing_a_section_is_rejected() {
    // Given a mask with no skills section at all.
    let h = Harness::new();
    let before = h.session_caps();
    let mask = serde_json::json!({
        "tools": {},
        "mcp_servers": {}
    });

    // When applying.
    let result = apply_execute(apply_call(mask), h.ctx.clone()).await;

    // Then the apply fails and nothing changed: the buffer is all three
    // families or it is nothing.
    assert!(!result.success);
    assert!(result.content.contains("skills"));
    assert_eq!(h.session_caps(), before);
}

// ---------------------------------------------------------------------------
// apply: allow-mode demotion + self-lockout + round trip
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn an_apply_demotes_an_allow_mode_tool_filter_to_the_mask_s_deny_list() {
    // Given a session frozen to an allow filter permitting only `read`.
    let h = Harness::new();
    h.register_tool("bash");
    h.register_tool("read");
    h.edit_session(|s| {
        s.set_tool_filter(Some(NameFilter {
            mode: FilterMode::Allow,
            names: ["read".to_owned()].into_iter().collect(),
        }))
    });
    // The mask reads the allow-mode truth: bash is off, read is on.
    let mask = serde_json::json!({
        "tools": {"bash": false, "read": true},
        "skills": {},
        "mcp_servers": {}
    });

    // When applying.
    let result = apply_execute(apply_call(mask), h.ctx.clone()).await;

    // Then the committed filter is deny-mode naming bash, replacing the
    // allow filter entirely.
    assert!(result.success, "apply should succeed: {}", result.content);
    let (tool_filter, _, _) = h.session_caps();
    let filter = tool_filter.expect("a filter was committed");
    assert_eq!(filter.mode, FilterMode::Deny, "allow mode is demoted");
    assert_eq!(filter.names, ["bash".to_owned()].into_iter().collect());
    assert!(
        filter.permits("read") && !filter.permits("bash"),
        "the demoted filter reads the same as the mask described"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_mask_may_turn_the_adjustment_tools_themselves_off() {
    // Given a mask withholding capabilities_apply and capabilities_query
    // among everything else.
    let h = Harness::new();
    h.register_tool("capabilities_query");
    h.register_tool("capabilities_apply");
    h.register_tool("read");
    let mask = serde_json::json!({
        "tools": {
            "capabilities_query": false,
            "capabilities_apply": false,
            "read": false
        },
        "skills": {},
        "mcp_servers": {}
    });

    // When applying.
    let result = apply_execute(apply_call(mask), h.ctx.clone()).await;

    // Then the mask's word is final: the apply succeeds and the filter
    // withholds all three, no special case.
    assert!(
        result.success,
        "self-lockout is permitted: {}",
        result.content
    );
    let (tool_filter, _, _) = h.session_caps();
    let filter = tool_filter.expect("a filter was committed");
    assert!(!filter.permits("capabilities_apply"));
    assert!(!filter.permits("capabilities_query"));
    assert!(!filter.permits("read"));
}

#[rstest::rstest]
#[tokio::test]
async fn entries_already_withheld_before_apply_round_trip_as_off_and_stay_off() {
    // Given a session where `bash` is already withheld by a deny filter.
    let h = Harness::new();
    h.register_tool("bash");
    h.register_tool("read");
    h.edit_session(|s| s.set_tool_filter(Some(NameFilter::deny(["bash".to_owned()]))));

    // When querying and then applying the query's own buffer unchanged.
    let query = query_execute(query_call(), h.ctx.clone()).await;
    let buffer: serde_json::Value = serde_json::from_str(&query.content).expect("json");
    let result = apply_execute(apply_call(buffer), h.ctx.clone()).await;

    // Then the round trip succeeds and bash is still withheld — the mask
    // echoed the withheld entry as off and it landed as off.
    assert!(query.success);
    assert!(
        result.success,
        "echoing the buffer must be a valid apply: {}",
        result.content
    );
    let (tool_filter, _, _) = h.session_caps();
    let filter = tool_filter.expect("a filter was committed");
    assert!(!filter.permits("bash"));
    assert!(filter.permits("read"));
}

#[rstest::rstest]
#[tokio::test]
async fn query_output_flipped_off_feeds_a_valid_full_shutdown_apply() {
    // Given a session with a tool, a skill, and a server.
    let h = Harness::with_config(&["excalimate"]);
    h.register_tool("read");
    h.seed_two_skills("rust-programming", "unused-second");
    h.edit_session(|s| {
        s.set_enabled_mcp_servers(std::collections::BTreeSet::from(["excalimate".to_owned()]))
    });

    // When querying, flipping every state off, and applying.
    let query = query_execute(query_call(), h.ctx.clone()).await;
    let mut mask: serde_json::Value = serde_json::from_str(&query.content).expect("json");
    for family in ["tools", "skills", "mcp_servers"] {
        let Some(obj) = mask[family].as_object_mut() else {
            continue;
        };
        for (_, state) in obj.iter_mut() {
            *state = serde_json::Value::Bool(false);
        }
    }
    let result = apply_execute(apply_call(mask), h.ctx.clone()).await;

    // Then everything is off: no tool or skill is permitted, the enabled
    // set is empty, and the coordinator's message carries that empty set.
    assert!(
        result.success,
        "the flipped buffer must apply: {}",
        result.content
    );
    let (tool_filter, skill_filter, enabled) = h.session_caps();
    assert!(!tool_filter.expect("filter").permits("read"));
    assert!(!skill_filter.expect("filter").permits("rust-programming"));
    assert!(enabled.is_empty());
    let changed = h.published::<McpEnablementChanged>();
    assert_eq!(changed.len(), 1);
    assert!(changed[0].enabled.is_empty());
}

#[rstest::rstest]
#[tokio::test]
async fn apply_fails_gracefully_without_the_bus() {
    // Given a context whose bus is absent (bare test dispatch).
    let h = Harness::new();
    h.register_tool("read");
    let ctx = ToolContext {
        bus: None,
        ..h.ctx.clone()
    };
    let before = h.session_caps();
    let mask = serde_json::json!({
        "tools": {"read": false},
        "skills": {},
        "mcp_servers": {}
    });

    // When applying.
    let result = apply_execute(apply_call(mask), ctx).await;

    // Then the tool fails with a message and nothing was written — a
    // capability change must never commit without its delivery path.
    assert!(!result.success);
    assert_eq!(h.session_caps(), before);
    assert!(h.audit.is_empty());
}

#[rstest::rstest]
#[tokio::test]
async fn apply_with_invalid_json_is_rejected() {
    // Given arguments that are not JSON.
    let h = Harness::new();
    let call = ToolCall {
        id: "call_bad".into(),
        name: "capabilities_apply".into(),
        arguments: "not json".to_owned(),
    };

    // When applying.
    let result = apply_execute(call, h.ctx).await;

    // Then the result fails with a parse message.
    assert!(!result.success);
    assert!(result.content.contains("invalid arguments"));
}

#[rstest::rstest]
#[test]
fn both_definitions_carry_the_tool_names() {
    // Given the two tool definitions.

    // When the definitions are listed.
    let query = query_definition();
    let apply = apply_definition();

    // Then their names match the registered nouns and apply's schema
    // requires the whole buffer.
    assert_eq!(query.name, "capabilities_query");
    assert_eq!(apply.name, "capabilities_apply");
    let required = apply
        .parameters
        .get("required")
        .and_then(|r| r.as_array())
        .expect("required list");
    assert!(required.contains(&serde_json::json!("capabilities")));
}
