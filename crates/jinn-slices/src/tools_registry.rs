//! Shared tool-registry cell vocabulary.
//!
//! The global + per-session tool definition registries were formerly
//! fields on the kernel's `ContextAssemblyState`; they are genuinely
//! multi-party — kernel tools handlers write them, the queue/session
//! dispatch snapshots, the TUI, and MCP picker refresh read them — so
//! the cell lives in `jinn-slices` under the decomposition policy.
//! When the tools family migrates, this module graduates to its crate.

use std::collections::{BTreeMap, HashSet};

use jinn_core_types::SessionId;
use jinn_provider::ToolDefinition;

use crate::SlotKey;

/// The tools family's registry payload: global definitions plus
/// per-session overrides (session tools shadow global tools by name).
#[derive(Debug, Default, Clone)]
pub struct ToolRegistry {
    /// Global tools available to all sessions.
    pub global: BTreeMap<String, ToolDefinition>,
    /// Per-session overrides keyed by session id.
    pub session: BTreeMap<SessionId, BTreeMap<String, ToolDefinition>>,
}

impl ToolRegistry {
    /// Merged tools for one session: global tools with that session's
    /// overrides replacing same-name entries (identical semantics to
    /// the former `ContextAssemblyState::tools_for_session`).
    #[must_use]
    pub fn tools_for_session(&self, session_id: &SessionId) -> Vec<ToolDefinition> {
        let mut tools: Vec<ToolDefinition> = self.global.values().cloned().collect();
        if let Some(session_tools) = self.session.get(session_id) {
            let global_names: HashSet<String> = self.global.keys().cloned().collect();
            for (name, def) in session_tools {
                if global_names.contains(name)
                    && let Some(pos) = tools.iter().position(|t| &t.name == name)
                    && let Some(slot) = tools.get_mut(pos)
                {
                    *slot = def.clone();
                } else {
                    tools.push(def.clone());
                }
            }
        }
        tools
    }
}

/// The slot key the tools registry cell lives under.
#[must_use]
pub fn tools_registry_slot() -> SlotKey {
    SlotKey::builtin("tools", "registry")
}
