//! The menu region — the titlebar's hamburger button and its item
//! panel.
//!
//! Parts declared: `page_fill` and nothing else. The menu is static
//! chrome — no pull route (nothing to re-render server-side), no SSE
//! event (the `-msg` crate is untouched by design), no settle, and no
//! guard render (the fragment cannot vary with session state). The
//! model-picker button is the precedent for a fragment-only region:
//! the region system is where page areas live, and future
//! server-backed menu items need the home.
//!
//! The fragment ships the panel `hidden`; the client half
//! (`webui/client/src/regions/menu.ts`) toggles visibility, closes on
//! the item, and toggles the drawer-open class on the page root —
//! the drawer itself is the sessions region's mobile presentation
//! (its media query lives in Unit 2's `SIDEBAR_CSS` work). No width
//! logic exists on either side: at desktop widths the item closes
//! the menu and toggles a class the desktop CSS never matches, so
//! the observable effect is "menu closes" — the CSS decides.

use crate::region::RegionParts;
use crate::region::WebRegion;

/// The menu region.
pub struct MenuRegion;

impl WebRegion for MenuRegion {
    fn id(&self) -> &'static str {
        "menu"
    }

    fn parts(&self) -> RegionParts {
        RegionParts {
            settle_render: false,
            pull_route: false,
            sse_event: false,
            page_fill: true,
        }
    }

    fn event_kinds(&self) -> &'static [jinn_webui_msg::SseKind] {
        &[]
    }

    fn pull(&self) -> Option<crate::region::RegionPull> {
        None
    }

    fn container_id(&self) -> Option<&'static str> {
        Some(crate::contract::MENU_ID)
    }

    fn css(&self) -> &'static str {
        MENU_CSS
    }

    fn render(
        &self,
        _session_id: &jinn_core_types::SessionId,
        _guard: &jinn_kernel::common::state::StateReadGuard<'_>,
    ) -> String {
        // Declared without the guard-render part: the menu is static
        // chrome and cannot vary with session state, so its fragment
        // never comes from a guard. The trait method exists for
        // registry completeness; it renders nothing.
        String::new()
    }
}

/// The menu's static fragment — the hamburger button and its one
/// "Sessions" item panel. Infallible: the fragment is all contract
/// constants, so a render failure is a template bug that must be a
/// compile error (askama checks the fields), not a runtime path.
#[must_use]
pub fn menu_fragment() -> String {
    crate::templates::render(&crate::templates::MenuTemplate::with_contract()).unwrap_or_else(
        |error| {
            tracing::warn!(%error, what = "menu-fragment", "webui template render failed; \
                 rendering an empty menu");
            String::new()
        },
    )
}

/// The menu region's stylesheet: Unit 1's functional styles live in
/// `BASE_CSS` (the button, the panel, the `[hidden]` open/closed rule
/// the whole page needs); this fragment carries the region-owned
/// interaction affordances — hover/focus feedback at every width, and
/// the phone media query's touch-target bumps (the hamburger and its
/// item reach the 40px hit height only at phone widths; desktop keeps
/// its compact titlebar sizing). The query mirrors the drawer's
/// breakpoint in `sessions.rs`.
pub const MENU_CSS: &str = r#"
#menu-button:hover { border-color: var(--accent); }
#menu-panel button:hover { background: #23272f; }
#menu-panel button:focus-visible { outline: 1px solid var(--accent); }
@media (max-width: 767px) {
  #menu-button { min-height: 40px; min-width: 40px; }
  #menu-panel button { min-height: 40px; padding: 10px 12px; }
}
"#;

#[cfg(test)]
mod menu_region_tests {
    //! Test code: assertions use expect/panic to state what went wrong.
    #![allow(
        clippy::expect_used,
        clippy::panic,
        reason = "test assertions need expect/panic to state what went wrong"
    )]

    use crate::contract;
    use crate::region::WebRegion as _;

    /// The fragment, rendered once per test (it is static).
    fn fragment() -> String {
        super::menu_fragment()
    }

    #[rstest::rstest]
    #[test]
    fn the_fragment_carries_the_button_and_the_one_sessions_item() {
        // Given the menu fragment.
        let html = fragment();

        // Then it renders the hamburger button, the hidden panel, and
        // exactly one item naming "Sessions" — the region's whole
        // page presence.
        assert!(
            html.contains(&format!("id=\"{}\"", contract::MENU_BUTTON_ID)),
            "the hamburger button is missing: {html}"
        );
        assert!(
            html.contains(&format!("id=\"{}\"", contract::MENU_PANEL_ID)),
            "the panel is missing: {html}"
        );
        assert!(
            html.contains(&format!("id=\"{}\"", contract::MENU_ITEM_SESSIONS_ID)),
            "the Sessions item is missing: {html}"
        );
        let items = html.matches("menu-item").count();
        assert_eq!(items, 1, "the panel lists exactly one item: {html}");
        assert!(
            html.contains("Sessions"),
            "the item labels Sessions: {html}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn the_panel_ships_hidden_so_a_fresh_page_has_the_menu_closed() {
        // Given the menu fragment rendered with the id the open tag
        // spells first.
        let html = fragment();

        // Then the whole open tag — the text from the panel's id
        // attribute through the tag's closing bracket — carries the
        // hidden attribute: the closed state is server-rendered, so a
        // fresh page (and every page swap, which re-renders the shell)
        // boots with the menu shut. The scan is substring-only (no
        // byte slicing): the fragment is ASCII, but the discipline
        // keeps the test honest if a label ever goes non-ASCII.
        let open_tag = html
            .split_once(&format!("id=\"{}\"", contract::MENU_PANEL_ID))
            .and_then(|(_, rest)| rest.split_once('>').map(|(tag, _)| tag.to_owned()))
            .expect("the panel's open tag renders");
        assert!(
            open_tag.contains("hidden"),
            "the panel opens hidden: {html}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn the_item_names_the_sessions_action_through_the_contract_attribute() {
        // Given the menu fragment.
        let html = fragment();

        // Then the item carries the action attribute the client's
        // delegation reads, spelled from the contract.
        assert!(
            html.contains(&format!(
                "{}=\"{}\"",
                contract::ACTION_DATA_ATTR,
                contract::ACTION_MENU_SESSIONS
            )),
            "the item's action hook is missing: {html}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn the_menu_region_conforms_with_only_page_fill_declared() {
        // Given the menu region.
        let region = super::MenuRegion;

        // Then the conformance gate finds nothing — page_fill with a
        // container and no pull/SSE/settle is an honest static region.
        let gaps = crate::region::check_region(&region);
        assert!(gaps.is_empty(), "the menu region must conform: {gaps:?}");
        assert_eq!(region.id(), "menu");
        assert_eq!(region.container_id(), Some(contract::MENU_ID));
    }

    #[rstest::rstest]
    #[test]
    fn the_state_class_is_not_a_dom_constant_of_the_fresh_render() {
        // Given the contract's DOM constants.
        // Then the drawer-open class is absent — membership would
        // assert presence in a fresh server render, and the class
        // exists only after client mutation (the referential gate
        // would fail, and rightly).
        assert!(
            !contract::DOM_CONSTANTS.contains(&contract::DRAWER_OPEN_CLASS),
            "the drawer-open class is client state, not fresh-render markup"
        );
    }
}
