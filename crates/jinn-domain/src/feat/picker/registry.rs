//! Composition-side picker registry — where specs are built and registered.
//!
//! This module is the adapter between the kernel's legacy `PickerKind`
//! world and the generic `jinn-picker` spec world: specs register here at
//! composition time, and [`spec_id_for_kind`] maps the static per-kind
//! scopes/intents onto the registry until every picker has migrated.
//!
//! When the last picker migrates, the adapter (and eventually `PickerKind`
//! itself) is deleted; `registered_ids` then *is* the picker vocabulary.

use jinn_picker::PickerRegistry;

/// The id of the persona picker's spec.
pub const PERSONA_ID: &str = "persona";
/// The id of the skill picker's spec.
pub const SKILL_ID: &str = "skill";
/// The id of the theme picker's spec.
pub const THEME_ID: &str = "theme";
/// The id of the tool picker's spec.
pub const TOOL_ID: &str = "tool";
/// The id of the MCP server picker's spec.
pub const MCP_SERVER_ID: &str = "mcp-server";
/// The id of the session-lifecycle picker's spec.
pub const SESSION_LIFECYCLE_ID: &str = "session-lifecycle";
/// The id of the plugin picker's spec.
pub const PLUGIN_ID: &str = "plugin";
/// The id of the task-list picker's spec.
pub const TASK_LIST_ID: &str = "task-list";
/// The id of the session picker's spec.
pub const SESSION_ID: &str = "session";
/// The id of the reasoning-effort picker's spec.
pub const REASONING_EFFORT_ID: &str = "reasoning-effort";
/// The id of the provider picker's spec.
pub const PROVIDER_ID: &str = "provider";
/// The id of the endpoint picker's spec.
pub const ENDPOINT_ID: &str = "endpoint";

/// The id of the project picker's spec.
pub const PROJECT_ID: &str = "project";

/// Maps a `PickerKind` onto its spec id. Every kind has a spec; `None`
/// therefore means the caller is holding a kind this version of the code
/// does not know (forward-compat guard only).
#[must_use]
pub fn spec_id_for_kind(kind: &crate::feat::picker::PickerKind) -> Option<&'static str> {
    match kind {
        crate::feat::picker::PickerKind::Persona => Some(PERSONA_ID),
        crate::feat::picker::PickerKind::Skill => Some(SKILL_ID),
        crate::feat::picker::PickerKind::Theme => Some(THEME_ID),
        crate::feat::picker::PickerKind::Tool => Some(TOOL_ID),
        crate::feat::picker::PickerKind::McpServer => Some(MCP_SERVER_ID),
        crate::feat::picker::PickerKind::SessionLifecycle => Some(SESSION_LIFECYCLE_ID),
        crate::feat::picker::PickerKind::ReasoningEffort => Some(REASONING_EFFORT_ID),
        crate::feat::picker::PickerKind::Plugin => Some(PLUGIN_ID),
        crate::feat::picker::PickerKind::TaskList => Some(TASK_LIST_ID),
        crate::feat::picker::PickerKind::Session => Some(SESSION_ID),
        crate::feat::picker::PickerKind::Provider => Some(PROVIDER_ID),
        crate::feat::picker::PickerKind::Endpoint => Some(ENDPOINT_ID),
        crate::feat::picker::PickerKind::Project => Some(PROJECT_ID),
        // CompactionModel is not spec-migrated yet; its legacy open/confirm
        // path in `intent.rs` still owns it.
        crate::feat::picker::PickerKind::CompactionModel => None,
    }
}

/// Builds the domain's picker registry: every migrated picker registers its
/// spec here once at composition.
#[must_use]
pub fn build_picker_registry() -> PickerRegistry {
    let mut registry = PickerRegistry::new();
    registry.register(super::persona_spec::persona_spec());
    registry.register(super::skill_spec::skill_spec());
    registry.register(super::theme_spec::theme_spec());
    registry.register(super::tool_spec::tool_spec());
    registry.register(super::mcp_server_spec::mcp_server_spec());
    registry.register(super::session_lifecycle_spec::session_lifecycle_spec());
    registry.register(super::reasoning_effort_spec::reasoning_effort_spec());
    registry.register(super::plugin_spec::plugin_spec());
    registry.register(super::task_list_spec::task_list_spec());
    registry.register(super::session_spec::session_spec());
    registry.register(super::provider_spec::provider_spec());
    registry.register(super::endpoint_spec::endpoint_spec());
    registry.register(super::project_spec::project_spec());
    registry
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        reason = "test module, panics are acceptable"
    )]
    use super::*;
    use crate::feat::picker::PickerKind;

    #[rstest::rstest]
    #[test]
    fn migrated_kinds_map_onto_registered_specs_and_vice_versa() {
        // Given the domain's picker registry and the kind→id adapter.
        let registry = build_picker_registry();
        let migrated = [
            PickerKind::Persona,
            PickerKind::Skill,
            PickerKind::Theme,
            PickerKind::Tool,
            PickerKind::McpServer,
            PickerKind::SessionLifecycle,
            PickerKind::ReasoningEffort,
            PickerKind::Plugin,
            PickerKind::TaskList,
            PickerKind::Session,
            PickerKind::Provider,
            PickerKind::Endpoint,
            PickerKind::Project,
        ];

        // When mapping each migrated kind and listing registered ids.
        let mapped_ids: Vec<&str> = migrated.iter().filter_map(spec_id_for_kind).collect();
        let mut registered_ids = registry.ids();
        registered_ids.sort_unstable();

        // Then the two sets are exactly equal — a kind with a spec id but
        // no registered spec (or the reverse) is a wiring bug.
        let mut expected = mapped_ids.clone();
        expected.sort_unstable();
        assert_eq!(registered_ids, expected);
        assert_eq!(mapped_ids.len(), migrated.len());
    }
}
