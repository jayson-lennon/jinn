//! Identifies a sidebar section (shared vocabulary; the kernel
//! re-exports under the sidebar's `section_trait` module).

/// Identifies a sidebar section. Used for focus tracking and dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SidebarSectionId {
    /// The pinned context entries section.
    #[default]
    Pins,
    /// The active persona display section.
    Persona,
    /// The task list section (collapsible phases, expandable when focused).
    TaskList,
    /// The open sessions section.
    Sessions,
    /// The MCP servers section (per-session enabled servers + live status).
    McpServers,
}

impl std::fmt::Display for SidebarSectionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pins => write!(f, "Pins"),
            Self::Persona => write!(f, "Persona"),
            Self::TaskList => write!(f, "TaskList"),
            Self::Sessions => write!(f, "Sessions"),
            Self::McpServers => write!(f, "McpServers"),
        }
    }
}

impl SidebarSectionId {
    /// The focus scope for this section: the dynamic scope the sidebar
    /// pushes while the section is focused.
    #[must_use]
    pub fn focus_scope(self) -> jinn_slices::focus::FocusScope {
        jinn_slices::focus::FocusScope::Dynamic(self.scope_id())
    }

    /// The section for a sidebar dynamic scope's name — the inverse of
    /// the name mapping in [`Self::scope_id`]. Returns `None` for the
    /// resize scope or any unknown name.
    #[must_use]
    pub fn from_scope_name(name: &str) -> Option<Self> {
        match name {
            "pins" => Some(Self::Pins),
            "persona" => Some(Self::Persona),
            "task-list" => Some(Self::TaskList),
            "sessions" => Some(Self::Sessions),
            "mcp-servers" => Some(Self::McpServers),
            _ => None,
        }
    }

    /// The navigation scope id for this section: the dynamic scope the
    /// sidebar pushes while the section is focused. Navigation-only —
    /// the sections drive a cursor, they never capture text input.
    #[must_use]
    pub fn scope_id(self) -> jinn_slices::slice_scope::SliceScopeId {
        jinn_slices::slice_scope::SliceScopeId::navigation(
            "sidebar",
            match self {
                Self::Pins => "pins",
                Self::Persona => "persona",
                Self::TaskList => "task-list",
                Self::Sessions => "sessions",
                Self::McpServers => "mcp-servers",
            },
        )
    }

    /// The sidebar's resize-mode scope id (adjusting sidebar width with
    /// h/l keys).
    #[must_use]
    pub fn resize_scope_id() -> jinn_slices::slice_scope::SliceScopeId {
        jinn_slices::slice_scope::SliceScopeId::navigation("sidebar", "resize")
    }
}
