//! Scope-stack extension for sidebar section focus.
//!
//! Lives here rather than in `jinn-slices` because it returns
//! [`SidebarSectionId`] — sidebar-family vocabulary. The mechanism
//! (`FocusScope::Dynamic` + scope names) is infra; this trait is the
//! sidebar-typed convenience over it.

use crate::sidebar_section_id::SidebarSectionId;
use jinn_slices::FocusScope;
use jinn_slices::ScopeStack;

/// Sidebar-typed conveniences over the scope stack.
pub trait SidebarScopeExt {
    /// Returns the focused sidebar section, if a sidebar scope is active.
    /// The resize scope is not a section.
    fn sidebar_section(&self) -> Option<SidebarSectionId>;

    /// Swaps the top of the scope stack to a different sidebar section.
    ///
    /// No-op if the current scope is not a sidebar section.
    fn set_sidebar_section(&mut self, section: SidebarSectionId);
}

impl SidebarScopeExt for ScopeStack {
    fn sidebar_section(&self) -> Option<SidebarSectionId> {
        match self.current() {
            FocusScope::Dynamic(id) if id.slice() == "sidebar" => {
                SidebarSectionId::from_scope_name(id.name())
            }
            _ => None,
        }
    }

    fn set_sidebar_section(&mut self, section: SidebarSectionId) {
        if self.is_sidebar() {
            self.pop();
            self.push(FocusScope::Dynamic(section.scope_id()));
        }
    }
}
