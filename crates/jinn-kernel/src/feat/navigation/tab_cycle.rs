// Copyright (C) 2026 Jayson Lennon
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as
// published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Tab-scope cycling — walking the registered tabs with `<Tab>`.
//!
//! Tabs are declared by slices as tab descriptors at activation, and
//! composition keeps the ordered list on `Slices`. This module resolves the
//! next base scope in that order, wrapping from the last tab back to the chat
//! tab. With no dynamic tab registered, `<Tab>` is a no-op round-trip to
//! Normal, because the chat tab is then the only tab.
//!
//! Lives under [`super::navigation`] rather than in the intent handler: tab
//! order is composition state, and this is the navigation walk over it rather
//! than a dispatch rule.

use crate::AppState;

/// Resolves the base scope after a `<Tab>` switch, walking the
/// registered tab scopes.
///
/// Tabs are declared by slices (tab descriptors registered at
/// activation); composition keeps the ordered list on `Slices`. With no
/// dynamic tab registered, `<Tab>` is a no-op round-trip to Normal —
/// the chat tab is the only tab.
pub fn next_tab_base(state: &AppState, slices: &jinn_slices::Slices) -> jinn_slices::FocusScope {
    use jinn_slices::FocusScope;

    // The chat tab (Normal) is always first in the cycle, so the walk
    // is: Normal → tab[0] → … → tab[n-1] → Normal.
    let tabs = tab_scopes(slices);
    if tabs.is_empty() {
        return FocusScope::Normal;
    }
    let position = match state.frontend.scope_base() {
        FocusScope::Dynamic(id) => tabs.iter().position(|tab| tab == &id),
        _ => None,
    };
    match position {
        // Currently on a dynamic tab: advance, wrapping back to chat.
        Some(i) => match tabs.get(i + 1) {
            Some(next) => FocusScope::Dynamic(next.clone()),
            // Last tab: wrap to chat.
            None => FocusScope::Normal,
        },
        // On chat (or any other base): enter the first dynamic tab.
        None => match tabs.first() {
            Some(first) => FocusScope::Dynamic(first.clone()),
            None => FocusScope::Normal,
        },
    }
}

/// The registered tab scope ids, in tab order.
pub fn tab_scopes(slices: &jinn_slices::Slices) -> Vec<jinn_slices::SliceScopeId> {
    slices.tab_scopes()
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::missing_docs_in_private_items,
        reason = "test code"
    )]
    use super::*;
    use crate::protocol::KernelIntent;
    use jinn_slices::FocusScope;

    use crate::feat::intent::handler::IntentHandler;

    /// The slice registry with nothing registered: enough for dispatch tests
    /// that are not exercising composition.
    fn empty_slices() -> jinn_slices::Slices {
        jinn_slices::Slices::new()
    }

    fn empty_routes() -> jinn_slices::route::KeyRoutes {
        jinn_slices::route::KeyRoutes::new()
    }

    #[rstest::rstest]
    fn switch_tab_with_no_registered_tab_stays_normal() {
        // Given default (Normal) state and no dynamic tab registered.
        let mut state = AppState::default_with_scope_focus();

        // When switching tabs.
        IntentHandler::handle(
            &KernelIntent::SwitchTab,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            jinn_slices::empty_config_layer(),
        );

        // Then the base is Normal (chat is the only tab).
        assert_eq!(state.frontend.scope_base(), FocusScope::Normal);
    }

    #[rstest::rstest]
    fn switch_tab_activates_the_registered_tab() {
        // Given a slices registry with one dynamic tab registered.
        let slices = jinn_slices::Slices::new();
        let tab = jinn_slices::SliceScopeId::new("dashboard", "tab");
        slices.register_tab_scope(
            tab.clone(),
            jinn_slices::SlotKey::builtin("dashboard", "tab"),
        );
        let mut state = AppState::default_with_scope_focus();

        // When switching tabs.
        IntentHandler::handle(
            &KernelIntent::SwitchTab,
            &mut state,
            &slices,
            &empty_routes(),
            jinn_slices::empty_config_layer(),
        );

        // Then the base is the registered tab.
        assert_eq!(
            state.frontend.scope_base(),
            FocusScope::Dynamic(tab.clone())
        );
    }

    #[rstest::rstest]
    fn switch_tab_wraps_to_normal_after_the_last_tab() {
        // Given a state whose base is the only registered tab.
        let slices = jinn_slices::Slices::new();
        let tab = jinn_slices::SliceScopeId::new("dashboard", "tab");
        slices.register_tab_scope(
            tab.clone(),
            jinn_slices::SlotKey::builtin("dashboard", "tab"),
        );
        let mut state = AppState::default_with_scope_focus();
        state
            .frontend
            .scope_swap_base(FocusScope::Dynamic(tab.clone()));

        // When switching tabs again.
        IntentHandler::handle(
            &KernelIntent::SwitchTab,
            &mut state,
            &slices,
            &empty_routes(),
            jinn_slices::empty_config_layer(),
        );

        // Then the cycle wraps to Normal.
        assert_eq!(state.frontend.scope_base(), FocusScope::Normal);
    }

    #[rstest::rstest]
    fn switch_tab_while_overlay_open_closes_it() {
        // Given an open terminal overlay over the Normal base.
        let mut state = AppState::default_with_scope_focus();
        let chat = state.session.active_session_id().clone();
        state
            .term_tabs()
            .expect("term tabs cell")
            .update(|t| t.set_live(&chat, true));
        state.frontend.scope_swap_base(FocusScope::Normal);
        state
            .frontend
            .scope_push(FocusScope::Dynamic(jinn_term_msg::view_scope()));

        // When switching tabs.
        IntentHandler::handle(
            &KernelIntent::SwitchTab,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            jinn_slices::empty_config_layer(),
        );

        // Then the overlay closed (back to base, not a tab flip).
        assert_eq!(state.frontend.scope(), FocusScope::Normal);
        assert_eq!(state.frontend.scope_base(), FocusScope::Normal);
    }

    #[rstest::rstest]
    fn switch_tab_is_inert_while_user_holds_terminal_control() {
        // Given the terminal-control overlay open (user holds control).
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_clear_overlays();
        state
            .frontend
            .scope_push(FocusScope::Dynamic(jinn_term_msg::control_scope()));

        // When switching tabs.
        IntentHandler::handle(
            &KernelIntent::SwitchTab,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            jinn_slices::empty_config_layer(),
        );

        // Then the scope stays on term:control — handback is the only exit.
        assert_eq!(
            state.frontend.scope(),
            FocusScope::Dynamic(jinn_term_msg::control_scope())
        );
    }
}
