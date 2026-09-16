//! Generic pluggable sidebar - hosts multiple sections with hybrid navigation.
//!
//! The sidebar is a persistent panel on the right side of the chat tab.
//! It contains pluggable sections (e.g., pinned entries, future features)
//! that register via the [`SidebarSection`] trait.
//!
//! Navigation uses a hybrid model: `j`/`k` moves within the focused section,
//! and sections can signal "unhandled" to let the sidebar move focus to the
//! next/previous section.

pub mod intent;
pub mod mcp_servers_section;
pub mod persona_section;
pub mod pins;
pub mod rename_input;
pub mod resize;
pub mod section_trait;
pub mod sessions;
pub mod sidebar;
pub mod sidebar_state_actor;
pub mod task_list_section;

#[cfg(test)]
mod sessions_tests;
#[cfg(test)]
mod sidebar_tests;

pub use section_trait::{
    EnterFrom, SectionNavResult, SidebarIntent, SidebarSection, SidebarSectionId,
};
pub use sidebar::Sidebar;
pub use sidebar::jump_to_section;
pub use sidebar::navigate_sidebar;

/// Registers all built-in sidebar sections into the given sidebar.
pub fn register_sections(sidebar: &mut Sidebar) {
    sidebar.register(Box::new(persona_section::PersonaSection));
    sidebar.register(Box::new(pins::PinsSection));
    sidebar.register(Box::new(task_list_section::TaskListSection));
    sidebar.register(Box::new(mcp_servers_section::McpServersSection));
    sidebar.register(Box::new(sessions::SessionsSection::new()));
}
