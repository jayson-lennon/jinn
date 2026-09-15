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
