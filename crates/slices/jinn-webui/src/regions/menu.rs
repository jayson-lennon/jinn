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
//! (the panel's `data-attr:hidden` bindings) toggles visibility, closes on
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

/// The menu's static fragment — the hamburger button and its item
/// panel. Infallible: the fragment is all contract constants, so a
/// render failure is a template bug that must be a compile error (askama
/// checks the fields), not a runtime path.
///
/// `viewed_session` is the page's viewed session id: empty on the
/// landing page, which omits the persona item (persona binds
/// per-session, so a page viewing nothing has no persona to open).
#[must_use]
pub fn menu_fragment(viewed_session: &str) -> String {
    crate::templates::render(&crate::templates::MenuTemplate::with_contract(viewed_session))
        .unwrap_or_else(|error| {
            tracing::warn!(%error, what = "menu-fragment", "webui template render failed; \
                 rendering an empty menu");
            String::new()
        })
}

/// The menu region's stylesheet: the slide-in panel's geometry and its
/// scrim — full-height, left-anchored, 280px (the sidebar's width),
/// translateX transition, stacked above the drawer stack (the menu is
/// the entry surface) — plus the interaction affordances (hover/focus
/// feedback) and the phone media query's touch-target bumps. The panel
/// geometry is width-independent; only the touch bumps are
/// width-specific, decided by a media query. The scrim's stack level
/// also rides here (mirrored in `sessions.rs`'s sheet for the
/// drawer-side of the A6 sandwich).
pub const MENU_CSS: &str = r#"
#menu-scrim { position: fixed; inset: 0; background: rgba(0,0,0,.55); z-index: 60; }
#menu-scrim[hidden] { display: none; }
#menu-panel { position: fixed; top: 0; bottom: 0; left: 0; width: 280px;
              background: var(--panel); border-right: 1px solid var(--border);
              padding: 12px 8px; display: flex; flex-direction: column;
              gap: 4px; z-index: 70;
              transform: translateX(-100%);
              transition: transform .25s ease; }
#menu-panel[hidden] { transform: translateX(-100%); visibility: hidden;
                      transition: none; }
#menu-panel:not([hidden]) { transform: translateX(0); }
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

    /// The fragment as a session page renders it (a session viewed,
    /// persona item present).
    fn fragment() -> String {
        super::menu_fragment("00000000-0000-0000-0000-000000000001")
    }

    /// The fragment as the landing page renders it (nothing viewed).
    fn landing_fragment() -> String {
        super::menu_fragment("")
    }

    #[rstest::rstest]
    #[test]
    fn the_fragment_carries_the_button_and_the_three_items() {
        // Given the session-page menu fragment.
        let html = fragment();

        // Then it renders the hamburger button, the hidden panel, and
        // all three items — the new-session item, the persona item
        // (a session is viewed), and the sessions item — the region's
        // whole page presence.
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
        assert!(
            html.contains(&format!("id=\"{}\"", contract::MENU_ITEM_NEW_SESSION_ID)),
            "the New session item is missing: {html}"
        );
        assert!(
            html.contains(&format!("id=\"{}\"", contract::MENU_ITEM_PERSONA_ID)),
            "the Persona item is missing: {html}"
        );
        let items = html.matches("menu-item").count();
        assert_eq!(items, 3, "the panel lists all three items: {html}");
        assert!(
            html.contains("Sessions"),
            "the item labels Sessions: {html}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn the_persona_item_pulls_into_the_persona_modal_host_with_the_viewed_session() {
        // Given the session-page menu fragment.
        let html = fragment();

        // Then the persona item's pull targets the persona-picker route
        // for the viewed session and swaps into the persona modal's own
        // host span — never the menu's container (the host-not-container
        // rule).
        let item = html
            .split_once(&format!("id=\"{}\"", contract::MENU_ITEM_PERSONA_ID))
            .and_then(|(_, rest)| rest.split_once('>').map(|(tag, _)| tag.to_owned()))
            .expect("the persona item renders");
        assert!(
            item.contains(&format!(
                "{}00000000-0000-0000-0000-000000000001",
                contract::PERSONA_PICKER_PREFIX
            )),
            "the pull names the viewed session: {item}"
        );
        assert!(
            item.contains(contract::PERSONA_MODAL_HOST_SELECTOR),
            "the pull swaps into the persona modal host: {item}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn the_landing_menu_omits_the_persona_item_entirely() {
        // Given the landing fragment (no viewed session).
        let html = landing_fragment();

        // Then the persona item is absent from the markup — the server
        // omits it, rather than shipping a hidden control the runtime
        // must know to disable.
        assert!(
            !html.contains(&contract::MENU_ITEM_PERSONA_ID),
            "the landing menu carries no persona item: {html}"
        );
        assert!(
            !html.contains(contract::PERSONA_PICKER_PREFIX),
            "the landing menu spells no persona URL: {html}"
        );
        // And the other two items still render.
        assert!(
            html.contains(&format!("id=\"{}\"", contract::MENU_ITEM_SESSIONS_ID)),
            "the Sessions item survives: {html}"
        );
        assert!(
            html.contains(&format!("id=\"{}\"", contract::MENU_ITEM_NEW_SESSION_ID)),
            "the New session item survives: {html}"
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

        // Then the item exists with its contract id, and the drawer
        // toggle rides its click handler (the action-vocabulary part
        // of the old attribute now lives in the declarative binding).
        let item = html
            .split_once(&format!("id=\"{}\"", contract::MENU_ITEM_SESSIONS_ID))
            .and_then(|(_, rest)| rest.split_once('>').map(|(tag, _)| tag.to_owned()))
            .expect("the sessions item renders");
        assert!(
            item.contains("$drawerOpen"),
            "the item's drawer toggle is missing: {html}"
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

    #[rstest::rstest]
    #[test]
    fn the_menu_css_pins_the_slide_in_panel_over_the_drawer_stack() {
        // Given the menu region's stylesheet.
        let css = super::MENU_CSS;

        // Then it carries the slide-in geometry (fixed, left, 280px,
        // translateX transition), the scrim's stack sandwich, and the
        // panel parked off-canvas by default — the Addendum 1 panel at
        // every width, above the drawer's z-order.
        assert!(
            css.contains("position: fixed") && css.contains("width: 280px"),
            "the panel is a fixed 280px slide-in: {css}"
        );
        assert!(
            css.contains("transform: translateX(-100%)") && css.contains("transition: transform"),
            "the panel slides on a transform transition: {css}"
        );
        assert!(
            css.contains("z-index: 70") && css.contains("z-index: 60"),
            "the panel (70) and its scrim (60) stack above the drawer: {css}"
        );
        assert!(
            css.contains("#menu-scrim[hidden]"),
            "the scrim's hidden rule keeps it inert at boot: {css}"
        );
    }
}
