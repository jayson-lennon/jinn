//! `capabilities_query` / `capabilities_apply` — the agent's handle on its own
//! session's tools, skills, and MCP servers.
//!
//! The pickers write the same three pieces of session state (`tool_filter`,
//! `skill_filter`, `enabled_mcp_servers`) from the keyboard side; these two
//! tools are the agent-side counterpart. `capabilities_query` composes the
//! session's complete on/off buffer — one entry per tool, skill, and MCP
//! server, as this session's state reads at call time — and
//! `capabilities_apply` accepts that whole buffer back with a state on every
//! entry, validating it against the same composition before touching
//! anything.
//!
//! The buffer is the atomicity device: an apply that misses an entry, or
//! names one the session no longer has, is rejected in full before the first
//! write, with the current buffer in the rejection so the caller can re-echo
//! it. A successful apply commits through the same `ChatSessionState` setters
//! the pickers use and publishes the same two messages the picker paths
//! publish (`McpEnablementChanged` to the coordinator, `PersistSession` to
//! the store) — there is no second writer path here.

use std::collections::BTreeSet;

use error_stack::Report;
use jinn_core_types::NameFilter;
use jinn_core_types::tool_types::{ToolCall, ToolDefinition, ToolResult};
use jinn_mcp_msg::McpEnablementChanged;
use jinn_preferences_config::schemas::mcp::McpServersConfig;
use jinn_session_store_msg::PersistSession;

use super::BoxedToolFuture;
use crate::tool_types::{ToolContext, failed};

/// Why a capabilities call could not run.
///
/// The call boundary renders the message into tool-result prose; a `Report`'s
/// `Display` prints only the outermost context, so each variant's message
/// stands alone.
#[derive(Debug, wherror::Error)]
pub enum CapabilitiesError {
    /// The call's JSON arguments were malformed.
    #[error("invalid arguments: {detail}")]
    InvalidArguments {
        /// The parser's complaint.
        detail: String,
    },
    /// The arguments were not a JSON object.
    #[error("arguments must be a JSON object")]
    NotAnObject,
    /// The mask omitted one of the three buffer sections.
    #[error("missing `{section}` section")]
    MissingSection {
        /// The missing section's wire token.
        section: &'static str,
    },
    /// A buffer entry's state was not a boolean.
    #[error("`{name}` state must be true or false")]
    InvalidState {
        /// The offending entry's name.
        name: String,
    },
    /// The apply named no `capabilities` argument.
    #[error("missing `capabilities` argument")]
    MissingCapabilities,
    /// The mask named an entry the current buffer does not carry.
    #[error(
        "unknown or stale {kind} `{name}`: re-run `capabilities_query` and \
             echo its whole buffer back with states flipped"
    )]
    UnknownEntry {
        /// The entry's kind token.
        kind: &'static str,
        /// The refused name.
        name: String,
    },
    /// The mask's entry count differed from the current buffer's.
    #[error(
        "mask has {mask_len} {mask_plural} but this session has {current_len}: the mask \
             must cover every entry `capabilities_query` returns, changed or not"
    )]
    EntryCountMismatch {
        /// Entries in the submitted mask.
        mask_len: usize,
        /// "y" when one, "ies" otherwise.
        mask_plural: &'static str,
        /// Entries in the current buffer.
        current_len: usize,
    },
}

/// The kind of capability a buffer entry describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CapabilityKind {
    /// A tool: a global built-in or this session's `mcp__<server>__<tool>`.
    Tool,
    /// A discovered skill.
    Skill,
    /// A configured MCP server.
    McpServer,
}

impl CapabilityKind {
    /// The wire token naming this kind in the buffer.
    fn as_str(self) -> &'static str {
        match self {
            CapabilityKind::Tool => "tool",
            CapabilityKind::Skill => "skill",
            CapabilityKind::McpServer => "mcp_server",
        }
    }
}

/// One capability in the on/off buffer: its name, kind, and current state.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CapabilityEntry {
    kind: CapabilityKind,
    name: String,
    enabled: bool,
}

/// The session's complete capability buffer: every tool, skill, and MCP
/// server this session has, each with its current on/off state.
///
/// Ordering is deterministic (tools, then skills, then servers, each sorted)
/// so a query's output is stable and an apply's validation is exact-match.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct CapabilityBuffer {
    entries: Vec<CapabilityEntry>,
}

impl CapabilityBuffer {
    /// Renders the buffer as the JSON the model exchanges:
    /// `{"tools": {...}, "skills": {...}, "mcp_servers": {...}}` with
    /// `true` = enabled and `false` = off.
    fn to_json(&self) -> serde_json::Value {
        let mut tools = serde_json::Map::new();
        let mut skills = serde_json::Map::new();
        let mut servers = serde_json::Map::new();
        for entry in &self.entries {
            let map = match entry.kind {
                CapabilityKind::Tool => &mut tools,
                CapabilityKind::Skill => &mut skills,
                CapabilityKind::McpServer => &mut servers,
            };
            map.insert(entry.name.clone(), serde_json::Value::Bool(entry.enabled));
        }
        serde_json::json!({
            "tools": tools,
            "skills": skills,
            "mcp_servers": servers,
        })
    }

    /// Parses a buffer back from its JSON form, preserving entry order.
    ///
    /// `serde_json::Map` is a `BTreeMap` under the `preserve_order = false`
    /// default, so each family's names arrive sorted; the family order below
    /// is the one `to_json` writes.
    ///
    /// # Errors
    ///
    /// Returns an error when a declared capability family's section is
    /// absent or malformed.
    fn from_json(value: &serde_json::Value) -> Result<Self, Report<CapabilitiesError>> {
        let mut entries = Vec::new();
        let mut push_all = |kind: CapabilityKind,
                            map: Option<&serde_json::Map<String, serde_json::Value>>|
         -> Result<(), Report<CapabilitiesError>> {
            let Some(map) = map else {
                return Err(CapabilitiesError::MissingSection {
                    section: match kind {
                        CapabilityKind::Tool => "tools",
                        CapabilityKind::Skill => "skills",
                        CapabilityKind::McpServer => "mcp_servers",
                    },
                }
                .into());
            };
            for (name, state) in map {
                let Some(enabled) = state.as_bool() else {
                    return Err(CapabilitiesError::InvalidState { name: name.clone() }.into());
                };
                entries.push(CapabilityEntry {
                    kind,
                    name: name.clone(),
                    enabled,
                });
            }
            Ok(())
        };
        let obj = value.as_object().ok_or(CapabilitiesError::NotAnObject)?;
        push_all(
            CapabilityKind::Tool,
            obj.get("tools").and_then(|v| v.as_object()),
        )?;
        push_all(
            CapabilityKind::Skill,
            obj.get("skills").and_then(|v| v.as_object()),
        )?;
        push_all(
            CapabilityKind::McpServer,
            obj.get("mcp_servers").and_then(|v| v.as_object()),
        )?;
        Ok(Self { entries })
    }
}

/// Returns the definition for the `capabilities_query` built-in tool.
pub fn query_definition() -> ToolDefinition {
    ToolDefinition {
        name: "capabilities_query".to_owned(),
        description: "List every capability of this session — tools, skills, and MCP \
            servers — each with its current on/off state. Returns the complete buffer as \
            JSON. Use this before `capabilities_apply`: build the new mask by taking this \
            exact output and flipping states, then pass the WHOLE buffer (every entry, \
            changed or not) to `capabilities_apply`."
            .to_owned(),
        prompt_snippet: None,
        prompt_guidelines: vec![],
        parameters: serde_json::json!({
            "type": "object",
            "properties": {},
            "required": []
        }),
        server_tool_type: None,
    }
}

/// Returns the definition for the `capabilities_apply` built-in tool.
pub fn apply_definition() -> ToolDefinition {
    ToolDefinition {
        name: "capabilities_apply".to_owned(),
        description: "Set this session's tools, skills, and MCP servers in one atomic \
            step. Pass the COMPLETE capability buffer from `capabilities_query` with a \
            true/false state on every entry — a mask missing any entry, or naming an \
            unknown one, is rejected whole with the current buffer attached, and nothing \
            changes. Off tools and skills are withheld from the prompt; off MCP servers \
            are disconnected. This change persists with the session."
            .to_owned(),
        prompt_snippet: None,
        prompt_guidelines: vec![],
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "capabilities": {
                    "type": "object",
                    "description": "The complete buffer from `capabilities_query` with \
                        each entry's state set: {\"tools\": {name: bool}, \"skills\": \
                        {name: bool}, \"mcp_servers\": {name: bool}}. Every entry must \
                        be present.",
                    "properties": {
                        "tools": {"type": "object", "additionalProperties": {"type": "boolean"}},
                        "skills": {"type": "object", "additionalProperties": {"type": "boolean"}},
                        "mcp_servers": {"type": "object", "additionalProperties": {"type": "boolean"}}
                    },
                    "required": ["tools", "skills", "mcp_servers"]
                }
            },
            "required": ["capabilities"]
        }),
        server_tool_type: None,
    }
}

/// Executes the `capabilities_query` built-in tool.
pub fn query_execute(call: ToolCall, ctx: ToolContext) -> BoxedToolFuture {
    Box::pin(async move {
        // Session state is what every family reads from; without it there is
        // nothing to report.
        let Some(session_id) = ctx.session_id.clone() else {
            return failed(call.id.clone(), &call.name, "no active session");
        };
        let Some(state) = ctx.state.clone() else {
            return failed(
                call.id.clone(),
                &call.name,
                "no application state available",
            );
        };

        let buffer = {
            let guard = state.read();
            let Some(session) = guard.session.get(&session_id) else {
                return failed(call.id.clone(), &call.name, "session not found");
            };
            compose_buffer(&guard, session, &ctx.config)
        };

        ToolResult {
            tool_call_id: call.id,
            name: call.name,
            content: buffer.to_json().to_string(),
            success: true,
            full_content: None,
            truncation: None,
            pin_position: None,
            attachments: Vec::new(),
        }
    })
}

/// Executes the `capabilities_apply` built-in tool.
pub fn apply_execute(call: ToolCall, ctx: ToolContext) -> BoxedToolFuture {
    let publishes = (ctx.session_id.clone(), ctx.state.clone(), ctx.bus.clone());
    Box::pin(async move {
        let (Some(session_id), Some(state), Some(bus)) = publishes else {
            return failed(
                call.id.clone(),
                &call.name,
                "capabilities_apply requires session state and the bus \
                 (unavailable in this context)",
            );
        };

        // Parse and validate the whole mask before the first write: a
        // rejected apply must leave every field and message untouched.
        let mask = match parse_mask(&call.arguments) {
            Ok(mask) => mask,
            Err(msg) => return failed(call.id.clone(), &call.name, msg.to_string()),
        };

        let (off_tools, off_skills, enabled_servers) = {
            let guard = state.read();
            let Some(session) = guard.session.get(&session_id) else {
                return failed(call.id.clone(), &call.name, "session not found");
            };
            let current = compose_buffer(&guard, session, &ctx.config);
            if let Err(msg) = validate_mask(&mask, &current) {
                // The rejection carries the current buffer so the caller can
                // re-echo it instead of making a second query call.
                return failed(
                    call.id.clone(),
                    &call.name,
                    format!("{} Current buffer: {}", msg, current.to_json()),
                );
            }
            let off_tools = off_names(&mask, CapabilityKind::Tool);
            let off_skills = off_names(&mask, CapabilityKind::Skill);
            let enabled_servers = on_names(&mask, CapabilityKind::McpServer);
            (off_tools, off_skills, enabled_servers)
        };

        // Validation passed — commit through the same setters the pickers
        // use. A session holding an allow-mode filter is demoted by design:
        // the mask is a blocklist over everything this session has, and
        // committing it as a deny filter is what the mask describes.
        {
            let mut guard = state.write();
            let Some(session) = guard.session.get_mut(&session_id) else {
                return failed(
                    call.id.clone(),
                    &call.name,
                    "session disappeared during apply",
                );
            };
            session.set_tool_filter(Some(NameFilter::deny(off_tools.clone())));
            session.set_skill_filter(Some(NameFilter::deny(off_skills.clone())));
            session.set_enabled_mcp_servers(enabled_servers.clone());
        }

        // Both publishes go out on every success, even when a family is
        // unchanged: the coordinator diffs the desired set, and persist is
        // cheap.
        bus.publish(McpEnablementChanged {
            session_id: session_id.clone(),
            enabled: enabled_servers,
        })
        .await;
        bus.publish(PersistSession { session_id }).await;

        ToolResult {
            tool_call_id: call.id,
            name: call.name,
            content: format!(
                "Capabilities applied: {} tool(s) off, {} skill(s) off, {} MCP server(s) \
                 enabled. Tools and skills are withheld from the next prompt; MCP server \
                 connections are being reconciled; the change persists with this session.",
                off_tools.len(),
                off_skills.len(),
                mask.servers_on_count(),
            ),
            success: true,
            full_content: None,
            truncation: None,
            pin_position: None,
            attachments: Vec::new(),
        }
    })
}

// ---------------------------------------------------------------------------
// Buffer composition (the one source of truth both tools read)
// ---------------------------------------------------------------------------

/// Composes the session's capability buffer: one entry per tool (from the
/// session-merged registry cell), per discovered skill, and per configured
/// MCP server, with state read from the session's own filters and set.
///
/// Tools read `is_tool_enabled` and skills read `is_skill_enabled` — the same
/// predicates the prompt assembler and the gates use — so an allow-mode
/// session's withheld names report as off without this module re-deriving
/// filter semantics. Server state reads only `enabled_mcp_servers` (the
/// coordinator seeds it); config presence never invents an entry or flips a
/// state.
fn compose_buffer(
    app: &jinn_kernel::common::app_state::AppState,
    session: &jinn_session_state::ChatSessionState,
    config: &jinn_config::ConfigLayer,
) -> CapabilityBuffer {
    let mut entries = Vec::new();

    let tool_defs = app
        .tool_registry()
        .map_or_default(|cell| cell.read().tools_for_session(session.session_id()));
    for def in &tool_defs {
        entries.push(CapabilityEntry {
            kind: CapabilityKind::Tool,
            enabled: session.is_tool_enabled(&def.name),
            name: def.name.clone(),
        });
    }

    for skill in session.discovered_skills() {
        entries.push(CapabilityEntry {
            kind: CapabilityKind::Skill,
            enabled: session.is_skill_enabled(&skill.name),
            name: skill.name.clone(),
        });
    }

    for server in configured_server_names(config) {
        entries.push(CapabilityEntry {
            kind: CapabilityKind::McpServer,
            enabled: session.is_mcp_server_enabled(&server),
            name: server,
        });
    }

    CapabilityBuffer { entries }
}

/// The configured MCP server names, from the config layer (the same read the
/// MCP picker uses for its catalog).
fn configured_server_names(config: &jinn_config::ConfigLayer) -> Vec<String> {
    config
        .get::<McpServersConfig>()
        .unwrap_or_default()
        .keys()
        .cloned()
        .collect()
}

// ---------------------------------------------------------------------------
// Mask validation + commit extraction
// ---------------------------------------------------------------------------

/// Parses the apply arguments into a `CapabilityBuffer` mask.
fn parse_mask(args: &str) -> Result<CapabilityBuffer, Report<CapabilitiesError>> {
    let value: serde_json::Value =
        serde_json::from_str(args).map_err(|e| CapabilitiesError::InvalidArguments {
            detail: e.to_string(),
        })?;
    let capabilities = value
        .get("capabilities")
        .ok_or(CapabilitiesError::MissingCapabilities)?;
    CapabilityBuffer::from_json(capabilities)
}

/// Validates the mask against the current buffer: exact same entry set, names
/// matched exactly per family. One mismatch rejects the whole apply.
fn validate_mask(
    mask: &CapabilityBuffer,
    current: &CapabilityBuffer,
) -> Result<(), Report<CapabilitiesError>> {
    for entry in &mask.entries {
        let matches = current
            .entries
            .iter()
            .any(|c| c.kind == entry.kind && c.name == entry.name);
        if !matches {
            return Err(CapabilitiesError::UnknownEntry {
                kind: entry.kind.as_str(),
                name: entry.name.clone(),
            }
            .into());
        }
    }
    if mask.entries.len() != current.entries.len() {
        return Err(CapabilitiesError::EntryCountMismatch {
            mask_len: mask.entries.len(),
            mask_plural: if mask.entries.len() == 1 { "y" } else { "ies" },
            current_len: current.entries.len(),
        }
        .into());
    }
    Ok(())
}

/// The mask's off names in one family.
fn off_names(mask: &CapabilityBuffer, kind: CapabilityKind) -> Vec<String> {
    mask.entries
        .iter()
        .filter(|e| e.kind == kind && !e.enabled)
        .map(|e| e.name.clone())
        .collect()
}

/// The mask's on names in one family.
fn on_names(mask: &CapabilityBuffer, kind: CapabilityKind) -> BTreeSet<String> {
    mask.entries
        .iter()
        .filter(|e| e.kind == kind && e.enabled)
        .map(|e| e.name.clone())
        .collect()
}

impl CapabilityBuffer {
    /// The number of MCP servers the mask leaves enabled (for the success text).
    fn servers_on_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| e.kind == CapabilityKind::McpServer && e.enabled)
            .count()
    }
}

#[cfg(test)]
#[path = "capabilities_tests.rs"]
mod tests;
