//! Tool, MCP-server, and skill enablement gates plus model-selection access.

use super::*;

impl ChatSessionState {
    /// Set the model selection for this session.
    ///
    /// No endpoint bookkeeping is needed here: a routing pin is a per-model
    /// default in `providers.toml`, so switching models switches which pin
    /// applies by construction rather than by clearing a session field.
    pub fn set_model(&mut self, model: ModelSelection) {
        self.core.integrations.profile.model = model;
    }

    /// Whether this session may use the tool called `tool_name`.
    ///
    /// One predicate for the tool gate, called by both the prompt assembler
    /// and the tool dispatcher. Deciding at separate call sites is what let
    /// the picker list tools the prompt hid, and — worse — what let a tool
    /// absent from the prompt run anyway when the model named it directly.
    ///
    /// This gate is independent of the provider gate (`ToolDefinition::
    /// available_for_provider`), which needs the provider name and stays
    /// where it is. A bot's filter composes as filter ∧ provider ∧
    /// the bot-only gate.
    #[must_use]
    pub fn is_tool_enabled(&self, tool_name: &str) -> bool {
        permits_or_inherits(
            self.core.integrations.profile.tool_filter.as_ref(),
            tool_name,
        )
    }

    /// Read-only access to this session's tool filter.
    ///
    /// `None` means the session has no tool filter and inherits whatever it
    /// would have had. Callers that only need to answer a gate read
    /// [`Self::is_tool_enabled`] instead; this one is for the callers that
    /// seed a picker or hand the filter on.
    #[must_use]
    pub fn tool_filter(&self) -> Option<&NameFilter> {
        self.core.integrations.profile.tool_filter.as_ref()
    }

    /// Replace the tool filter for this session.
    ///
    /// `None` releases it, sending the session back to inheriting. Used by
    /// the tool picker to commit toggle state and by the bot panel to
    /// thaw a frozen set.
    pub fn set_tool_filter(&mut self, filter: Option<NameFilter>) {
        self.core.integrations.profile.tool_filter = filter;
    }

    /// Read-only access to this session's enabled MCP server names.
    ///
    /// **Frozen**: stamped at session creation and never re-seeded — a
    /// live config edit does not move this set, and the MCP picker is
    /// the live-session control.
    ///
    /// Opt-in model: only servers in this set are active for the session.
    #[must_use]
    pub fn enabled_mcp_servers(&self) -> &std::collections::BTreeSet<String> {
        &self.core.integrations.enabled_mcp_servers
    }

    /// Returns `true` if the named MCP server is enabled for this session.
    #[must_use]
    pub fn is_mcp_server_enabled(&self, server: &str) -> bool {
        self.core.integrations.enabled_mcp_servers.contains(server)
    }

    /// Enables an MCP server for this session.
    ///
    /// Returns `true` if the server was not previously enabled (i.e. this call
    /// changed state).
    pub fn enable_mcp_server(&mut self, server: &str) -> bool {
        self.core
            .integrations
            .enabled_mcp_servers
            .insert(server.to_owned())
    }

    /// Disables an MCP server for this session.
    ///
    /// Returns `true` if the server was previously enabled (i.e. this call
    /// changed state).
    pub fn disable_mcp_server(&mut self, server: &str) -> bool {
        self.core.integrations.enabled_mcp_servers.remove(server)
    }

    /// Replaces the entire enabled MCP server set for this session.
    ///
    /// Used by the MCP picker to commit toggle state.
    pub fn set_enabled_mcp_servers(&mut self, servers: std::collections::BTreeSet<String>) {
        self.core.integrations.enabled_mcp_servers = servers;
    }

    /// Whether this session may load the skill called `skill_name`.
    ///
    /// The skill counterpart of [`Self::is_tool_enabled`], sharing its one
    /// predicate so the prompt assembler and the `skill` tool's refusal
    /// path cannot disagree about what the session can load.
    #[must_use]
    pub fn is_skill_enabled(&self, skill_name: &str) -> bool {
        permits_or_inherits(
            self.core.integrations.profile.skill_filter.as_ref(),
            skill_name,
        )
    }

    /// Read-only access to this session's skill filter.
    ///
    /// `None` means the session has no skill filter and inherits whatever it
    /// would have had; see [`Self::tool_filter`].
    #[must_use]
    pub fn skill_filter(&self) -> Option<&NameFilter> {
        self.core.integrations.profile.skill_filter.as_ref()
    }

    /// Replace the skill filter for this session.
    ///
    /// `None` releases it, sending the session back to inheriting. Used by
    /// the skill picker to commit toggle state and by the bot panel to
    /// thaw a frozen set.
    pub fn set_skill_filter(&mut self, filter: Option<NameFilter>) {
        self.core.integrations.profile.skill_filter = filter;
    }

    /// Compute the set of skill names that are currently loaded in this session.
    ///
    /// A skill is considered loaded if its body is present in history as a pinned
    /// ToolResult from the `skill` tool whose content begins with `<skill name="X"`.
    pub fn loaded_skills(&self) -> HashSet<String> {
        use ChatEntryKind;
        use parse_loaded_skill_name;

        let mut out = HashSet::new();
        for entry in self.history() {
            if !entry.is_pinned() {
                continue;
            }
            let ChatEntryKind::ToolResult {
                name: tool_name,
                content,
                ..
            } = &entry.kind
            else {
                continue;
            };
            if tool_name != "skill" {
                continue;
            }
            // Skill bodies are pinned as `<skill name="X" ...>` — extract X.
            let Some(skill_name) = parse_loaded_skill_name(content) else {
                continue;
            };
            out.insert(skill_name.to_owned());
        }
        out
    }
}

/// The one place an absent filter is answered at a gate.
///
/// An absent filter inherits, and an inherited filter withholds nothing, so
/// the answer is the same as an empty deny filter's — but the decision is
/// made here, from the field's `None`, rather than inside the filter, whose
/// emptiness means nothing at all.
fn permits_or_inherits(filter: Option<&NameFilter>, name: &str) -> bool {
    filter.as_ref().is_none_or(|filter| filter.permits(name))
}
