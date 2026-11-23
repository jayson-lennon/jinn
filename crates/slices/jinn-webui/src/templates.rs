//! The askama page and fragment templates.
//!
//! Compile-time-checked: a template naming a field the model lacks
//! fails the build, which is the reason this stack was chosen. The
//! page shell is inert markup — data attributes and ids from
//! [`crate::contract`]; every behavior rides the declarative Datastar
//! attributes the templates carry (stream subscription, fragment
//! swaps, action posts) against the committed browser bundle. Every
//! dynamic region is a fragment template the handlers render and a
//! Datastar action post swaps into the page.
//!
//! Escaping: askama escapes by extension — `.html` templates escape
//! every interpolation — so a session title carrying markup cannot
//! break the page. Pre-rendered HTML (the markdown bodies) rides
//! through askama's `|safe` filter *only* on fields the transcript
//! module produced, and that module is where the escaping contract
//! lives.

use askama::Template;
use jinn_webui_msg::SessionListItem;

/// The contract constants the templates interpolate. Kept as a local
/// re-export so template expressions stay short.
use crate::contract;

/// The page shell's base stylesheet: the design tokens (all three
/// palettes — the whole stylesheet's only color-literal home) and the
/// cross-region chrome (flex layout, titlebar; transcript entry
/// styling is the transcript region's; see [`crate::region`]'s `css`
/// join). Region styles live on the region and arrive through
/// [`page_style`], so a region change never edits this constant.
///
/// Palette sources: dark is lifted role-for-role from the shipped
/// default TUI theme (`res/themes/default.toml` — the comment on each
/// token names its role), light from `res/themes/nord-light.toml`.
/// The default token set is dark (on `:root`); the mode attribute
/// lives on `<body>` (outside the swapped page root, so it survives
/// a session-activate swap and the body background follows the mode):
/// `body[data-theme="light"]` overrides it, and
/// `@media (prefers-color-scheme: light)` re-applies the light set
/// for `data-theme="system"` and for a body carrying no mode at all,
/// so system detection is pure CSS with no client script.
/// `color-scheme` rides each block so native form controls (the
/// session-new `<select>`, filter inputs) match the palette.
pub const BASE_CSS: &str = r#"
:root {
  /* sonokai gutter_bg [37,39,46] */
  --bg: #25272e;
  /* derived: one step above --bg on the sonokai neutral ramp — the
     theme names no popup role; tab_active_fg #2c2e34 is the theme's
     own raised-surface tone */
  --panel: #2c2e34;
  /* sonokai primary_text */
  --fg: #e2e2e3;
  /* sonokai muted_text [99,101,117] lifted to #93959b: the theme's
     #636575 falls under 3:1 on the raised surfaces (hover, active
     rows) the web paints it over; #93959b clears 3:1 on every dark
     surface */
  --muted: #93959b;
  /* sonokai border_unfocused */
  --border: #44474e;
  /* sonokai user_block_bg [48,50,58] — the user entry's own surface */
  --bg-user-entry: #30323a;
  /* derived: below --bg — the code and preformatted surface, the
     scroll_indicator_bg tone */
  --bg-code: #1e2026;
  /* sonokai picker_selected_bg — the hover step (the dimmer of the
     picker pair); the theme's picker pair drops muted below 3:1 only
     at the highlight step, so this one needs no lift */
  --bg-hover: #3b3e47;
  /* sonokai picker_highlight_bg lifted from the raw value's pair —
     the raw #44474e is already the border tone; muted clears 3:1 on
     it as-is (#93959b on #44474e) */
  --bg-active: #44474e;
  /* derived: tool_pending retinted to the badge family, one step
     above --panel on the neutral ramp */
  --bg-badge-pending: #33353d;
  /* sonokai tool_success_bg [36,50,36] darkened until the status
     text clears 4.5:1 on it */
  --bg-badge-ok: #1c2a1c;
  /* sonokai tool_failure_bg [50,30,38] darkened for the same floor */
  --bg-badge-err: #2e1b22;
  /* sonokai focus_accent — the purple, verified 6.5:1+ as text on
     every dark surface and 6.48:1 under the on-fill ink */
  --accent: #c4a7e7;
  /* sonokai error_text: 5.5:1+ on every dark surface */
  --err: #ed8796;
  /* sonokai success: 8.4:1+ on every dark surface */
  --ok: #a9d89d;
  /* the modal/overlay scrim: black at the shipped opacity */
  --scrim: rgba(0, 0, 0, .55);
  /* text on an accent or error FILL: near-black — the darker side of
     both fills is the sonokai panel tone, 5.5:1+ under both */
  --on-fill: #2c2e34;
  /* the unread mark's fill: sonokai warning, 10:1 under --on-fill */
  --mark: #ebdca0;
  /* the mark's secondary text on the gold fill */
  --on-mark-muted: #4d4527;
  /* the bot suffix's gold as text: derived from the sonokai warning
     family, deepened for the 4.5:1 text floor (6.5:1 on --bg) */
  --bot: #c9b168;
  /* on the accent fill the label reads near-black; on the page the
     send-button label rides --on-fill too (6.48:1) */
  --on-accent: #2c2e34;
  --on-err: #2c2e34;
  color-scheme: dark;
}
body[data-theme="light"] {
  /* nord-light gutter_bg Snow Storm (Nord6) */
  --bg: #eceff4;
  /* nord-light infopopup_bg (Nord4) */
  --panel: #e5e9f0;
  /* nord-light primary_text (Polar Night, Nord0) */
  --fg: #2e3440;
  /* nord-light muted_text lifted to #67768f: the theme's #7b88a1
     falls under 3:1 on the badge fills; #67768f clears 3:1 on every
     light surface */
  --muted: #67768f;
  /* nord-light border_unfocused (Snow Storm, Nord4) */
  --border: #d8dee9;
  /* nord-light user_block_bg (Nord4) — same surface as --panel */
  --bg-user-entry: #e5e9f0;
  /* nord-light gutter_context_included, a step below --bg */
  --bg-code: #e2e6ee;
  /* nord-light picker_highlight_bg (Nord) */
  --bg-hover: #e7edf5;
  /* nord-light picker_selected_bg (Nord) */
  --bg-active: #dce6f2;
  /* nord-light tool_pending_bg, retinted to the badge family */
  --bg-badge-pending: #dfe0ee;
  /* nord-light tool_success_bg, darkened for the 4.5:1 status floor */
  --bg-badge-ok: #dde8d8;
  /* nord-light tool_failure_bg, darkened for the same floor */
  --bg-badge-err: #f2dcdc;
  /* nord-light focus_accent (Frost, Nord10) deepened to #4a6e95: the
     theme value carries 3.5:1 as text, under the 4.5:1 text floor */
  --accent: #4a6e95;
  /* nord-light error_text (Aurora red, Nord11) deepened to #a8404a
     for the text floor (the theme value carries 3.55:1) */
  --err: #a8404a;
  /* nord-light success (Aurora green, Nord14) deepened to #3d6b2a
     for the text floor (the theme value carries 1.8:1 as text) */
  --ok: #3d6b2a;
  /* the scrim keeps its dark wash over the pale palette */
  --scrim: rgba(0, 0, 0, .55);
  /* on fills: white on the mid-tone accent (5.3:1), near-black on
     the pale badge fills */
  --on-fill: #2e3440;
  --on-accent: #ffffff;
  --on-err: #ffffff;
  /* the unread mark: nord-light warning (Aurora yellow, Nord13) */
  --mark: #ebcb8b;
  --on-mark-muted: #63511d;
  /* the bot suffix's gold, deepened for the text floor (theme-family
     amber at 4.9:1 on --bg) */
  --bot: #7f6400;
  color-scheme: light;
}
@media (prefers-color-scheme: light) {
  body[data-theme="system"], body:not([data-theme]) {
    --bg: #eceff4;
    --panel: #e5e9f0;
    --fg: #2e3440;
    --muted: #67768f;
    --border: #d8dee9;
    --bg-user-entry: #e5e9f0;
    --bg-code: #e2e6ee;
    --bg-hover: #e7edf5;
    --bg-active: #dce6f2;
    --bg-badge-pending: #dfe0ee;
    --bg-badge-ok: #dde8d8;
    --bg-badge-err: #f2dcdc;
    --accent: #4a6e95;
    --err: #a8404a;
    --ok: #3d6b2a;
    --scrim: rgba(0, 0, 0, .55);
    --on-fill: #2e3440;
    --on-accent: #ffffff;
    --on-err: #ffffff;
    --mark: #ebcb8b;
    --on-mark-muted: #63511d;
    --bot: #7f6400;
    color-scheme: light;
  }
}
* { box-sizing: border-box; }
body { margin: 0; background: var(--bg); color: var(--fg);
       font: 15px/1.55 system-ui, sans-serif; }
#page-root { display: flex; height: 100vh; height: 100dvh; }
#main { flex: 1; display: flex; flex-direction: column; min-width: 0; }
#titlebar { padding: 10px 18px; padding-left: calc(18px + env(safe-area-inset-left));
            padding-right: calc(18px + env(safe-area-inset-right));
            padding-top: calc(10px + env(safe-area-inset-top));
            border-bottom: 1px solid var(--border);
            display: flex; align-items: baseline; gap: 12px; }
#titlebar .t { font-weight: 600; min-width: 0; overflow: hidden;
               text-overflow: ellipsis; white-space: nowrap; }
#menu { position: relative; display: inline-flex; }
#menu-button { background: var(--panel); color: var(--fg);
               border: 1px solid var(--border); border-radius: 6px;
               padding: 6px 10px; font: inherit; line-height: 1;
               cursor: pointer; }
#menu-button:hover { border-color: var(--accent); }
#menu-panel button { background: transparent; color: var(--fg); text-align: left;
                     border: 0; border-radius: 5px; padding: 8px 12px;
                     font: inherit; cursor: pointer; white-space: nowrap; }
#menu-panel button:hover { background: var(--bg-hover); }
"#;

/// The `<meta name="theme-color">` content for a color mode — the
/// mode's background token. `system` (or anything unrecognized)
/// renders the dark value: the OS decides at paint time, and the
/// meta cannot express a media query, so the page names the palette
/// its `:root` defaults to.
#[must_use]
pub fn theme_color_for(mode: &str) -> &'static str {
    match mode {
        "light" => "#eceff4",
        _ => "#25272e",
    }
}

/// The stylesheet the page shell inlines: the base chrome, then every
/// registered region's fragment in registry order (registration order
/// is the equal-specificity tie-breaker; regions are appended at the
/// tail, so an established region's rules never lose to a later one).
#[must_use]
pub fn page_style() -> String {
    let mut css = String::from(BASE_CSS);
    for region in crate::region::regions() {
        css.push('\n');
        css.push_str(region.css());
    }
    css
}

/// The entry module's cache-busting URL — `main.<hash>.js`, the hash
/// being the manifest row's sha256 prefix.
///
/// # Panics
///
/// Panics when the entry module is not a manifest row — a contract
/// break caught at the first page render, in tests, not in the wild.
#[must_use]
#[expect(
    clippy::expect_used,
    reason = "a missing entry row is a contract break that must panic loudly at the \
              first page render, not a fallible path to thread through"
)]
pub fn app_js_versioned_path() -> String {
    crate::vendor::versioned_path(contract::VENDOR_APP_JS)
        .expect("the client entry module is a manifest row")
}

/// The image-attach helper's `/vendor/` URL — `paste.<hash>.js`.
///
/// # Panics
///
/// Panics when the helper is not a manifest row — a contract break
/// caught at the first page render, in tests, not in the wild.
#[must_use]
#[expect(
    clippy::expect_used,
    reason = "a missing row is a contract break that must panic loudly at the \
              first page render, not a fallible path to thread through"
)]
pub fn paste_js_versioned_path() -> String {
    crate::vendor::versioned_path(contract::VENDOR_PASTE_JS)
        .expect("the image-attach helper is a manifest row")
}

/// The full page shell. Rendered by `GET /web` and by
/// `GET /web/session/{id}` (a session page is the same shell with the
/// regions filled for that session).
#[derive(Template)]
#[template(path = "page.html")]
pub struct PageTemplate {
    /// The stylesheet text, inlined into the page.
    pub style: &'static str,
    /// The viewed session's id (empty on the no-session landing page).
    pub session_id: String,
    /// The session title.
    pub title: String,
    /// The titlebar's model-picker button fragment (bare button; the
    /// page shell owns the `#model-picker` host span), rendered inline.
    pub button_html: String,
    /// The button's host span id (`model-picker`) — the shell owns
    /// the span; the fragment fills it.
    pub picker_host_id: &'static str,
    /// The rendered-entry cursor the page's inline transcript already
    /// carries — the stream's starting `Last-Event-ID`, so catch-up
    /// never re-appends what the page render included.
    pub cursor: usize,
    /// The sessions-list fragment, rendered inline.
    pub sessions_html: String,
    /// The transcript fragment, rendered inline.
    pub transcript_html: String,
    /// The composer-controls fragment, rendered inline.
    pub controls_html: String,
    /// The menu fragment (hamburger + panel), rendered inline.
    pub menu_html: String,
    /// The render-time unread-mark count — the menu's sessions
    /// button badge's seeded value.
    pub sessions_attention: usize,
    /// `/vendor/` — the script tag's URL prefix.
    pub vendor_prefix: &'static str,
    /// The client bundle's cache-busting URL inside `/vendor/`
    /// (`main.<hash>.js`) — the hash is the manifest row's sha256
    /// prefix, so an upgraded binary serves a URL no old page cached.
    pub app_js_versioned: String,
    /// The image-attach helper's cache-busting URL inside `/vendor/`
    /// (`paste.<hash>.js`), versioned the same way as the bundle.
    pub paste_js_versioned: String,
    /// The page-root id — the element a session switch replaces.
    pub page_root_id: &'static str,
    /// The attribute carrying the viewed session's id.
    pub page_session_attr: &'static str,
    /// The sessions-list container id.
    pub sessions_id: &'static str,
    /// The sessions filter input's id — in the page shell, outside the
    /// `#sessions` swap target.
    pub sessions_filter_input_id: &'static str,
    /// The model-picker modal's host id — the empty span sibling the
    /// modal fragment swaps into.
    pub modal_host_id: &'static str,
    /// The new-session modal's host id — the second empty span
    /// sibling; each modal swaps into its own host, so opening one
    /// can never destroy the other.
    pub session_new_host_id: &'static str,
    /// The persona modal's host id — the third empty span sibling
    /// (the same host-not-container rule).
    pub persona_modal_host_id: &'static str,
    /// The MCP modal's host id — the fourth empty span sibling (the
    /// same host-not-container rule).
    pub mcp_modal_host_id: &'static str,
    /// The tools modal's host id — the fifth empty span sibling (the
    /// same host-not-container rule).
    pub tools_modal_host_id: &'static str,
    /// The skills modal's host id — the sixth empty span sibling (the
    /// same host-not-container rule).
    pub skills_modal_host_id: &'static str,
    /// The transcript scroll-wrap id.
    pub transcript_wrap_id: &'static str,
    /// The transcript entry container id.
    pub transcript_id: &'static str,
    /// The attribute carrying the inline transcript's cursor.
    pub cursor_attr: &'static str,
    /// The composer-controls container id.
    pub controls_id: &'static str,
    /// The titlebar title span's id — present on session pages (the
    /// inline rename editor's host), empty on the landing page whose
    /// "jinn" title is inert.
    pub session_title_id: &'static str,
    /// The menu region's host container id — the hamburger + panel
    /// render inside it in the titlebar.
    pub menu_id: &'static str,
    /// The drawer scrim's id — the static shell overlay behind the
    /// off-canvas sidebar when the drawer is open.
    pub drawer_scrim_id: &'static str,
    /// The menu scrim's id — the static shell overlay behind the
    /// slide-in menu panel (stacked above the drawer scrim).
    pub menu_scrim_id: &'static str,
    /// The session-switch interstitial's id — the loading overlay a
    /// session-row click raises; static shell markup like the scrims.
    pub switch_overlay_id: &'static str,
    /// The error-notice banner's id — the failed-activate surface the
    /// fetch-error listener fills; static shell markup like the scrims.
    pub error_notice_id: &'static str,
    /// The viewed session's stream URL, empty on the landing page —
    /// the `data-init` `@get` binds to it; the runtime keeps the
    /// fetch-stream open (with retries) for the page's lifetime and
    /// re-reads it after a page-root replace (the activation path).
    pub stream_url: String,
    /// The sessions pull's route — the list's filter/tick effect
    /// re-pulls it.
    pub sessions_route: &'static str,
    /// The sessions pull's filter query parameter — the effect builds
    /// `?q=<text>` from it when a filter is in force.
    pub filter_query_param: &'static str,
    /// The activate POST's route prefix — the rename editor's `@post`
    /// target builds `<prefix><id>/rename` from it.
    pub session_page_path: &'static str,
    /// The persisted color mode seeding the `webTheme` signal —
    /// `light`, `dark`, `system`, or empty (system is the default;
    /// the body's `data-attr` binding treats an empty signal as
    /// system).
    pub web_theme: String,
    /// The `<meta name="theme-color">` content for the active mode —
    /// the mode's `--bg` value, rendered per render and patched by
    /// the theme POST (the browser chrome reads it from `<head>`,
    /// outside the swapped page root).
    pub theme_color: &'static str,
}

impl PageTemplate {
    /// The no-session landing page: sessions list only, every other
    /// region empty, cursor zero.
    #[must_use]
    pub fn landing(
        sessions_html: String,
        menu_html: String,
        attention_count: usize,
        web_theme: String,
    ) -> Self {
        Self {
            style: Box::leak(page_style().into_boxed_str()),
            session_id: String::new(),
            title: "jinn".to_owned(),
            cursor: 0,
            sessions_html,
            transcript_html: String::new(),
            controls_html: String::new(),
            button_html: String::new(),
            picker_host_id: contract::MODEL_PICKER_ID,
            menu_html,
            sessions_attention: attention_count,
            vendor_prefix: contract::VENDOR_PREFIX,
            app_js_versioned: app_js_versioned_path(),
            paste_js_versioned: paste_js_versioned_path(),
            page_root_id: contract::PAGE_ROOT_ID,
            page_session_attr: contract::PAGE_SESSION_DATA_ATTR,
            sessions_id: contract::SESSIONS_ID,
            sessions_filter_input_id: contract::SESSIONS_FILTER_INPUT_ID,
            modal_host_id: contract::MODEL_PICKER_MODAL_HOST_ID,
            session_new_host_id: contract::SESSION_NEW_MODAL_HOST_ID,
            persona_modal_host_id: contract::PERSONA_MODAL_HOST_ID,
            mcp_modal_host_id: contract::MCP_MODAL_HOST_ID,
            tools_modal_host_id: contract::TOOLS_MODAL_HOST_ID,
            skills_modal_host_id: contract::SKILL_MODAL_HOST_ID,
            transcript_wrap_id: contract::TRANSCRIPT_WRAP_ID,
            transcript_id: contract::TRANSCRIPT_ID,
            cursor_attr: contract::TRANSCRIPT_CURSOR_DATA_ATTR,
            controls_id: contract::CONTROLS_ID,
            session_title_id: "",
            menu_id: contract::MENU_ID,
            drawer_scrim_id: contract::DRAWER_SCRIM_ID,
            menu_scrim_id: contract::MENU_SCRIM_ID,
            switch_overlay_id: contract::SWITCH_OVERLAY_ID,
            error_notice_id: contract::ERROR_NOTICE_ID,
            stream_url: String::new(),
            sessions_route: contract::SESSIONS_ROUTE,
            filter_query_param: contract::SESSIONS_FILTER_QUERY_PARAM,
            session_page_path: contract::SESSION_PAGE_PREFIX,
            theme_color: theme_color_for(&web_theme),
            web_theme,
        }
    }

    /// A session page: every region filled for one viewed session at
    /// the cursor its inline transcript already carries. The stream
    /// URL targets the same session; its `?since=` names the page's
    /// rendered cursor so the first catch-up never re-appends what
    /// the page already rendered.
    #[must_use]
    #[expect(
        clippy::too_many_arguments,
        reason = "the page render's full fragment set is one composite value in practice"
    )]
    pub fn session(
        session_id: &str,
        title: String,
        cursor: usize,
        sessions_html: String,
        transcript_html: String,
        controls_html: String,
        button_html: String,
        menu_html: String,
        attention_count: usize,
        web_theme: String,
    ) -> Self {
        Self {
            style: Box::leak(page_style().into_boxed_str()),
            session_id: session_id.to_owned(),
            title,
            cursor,
            sessions_html,
            transcript_html,
            controls_html,
            button_html,
            picker_host_id: contract::MODEL_PICKER_ID,
            menu_html,
            sessions_attention: attention_count,
            vendor_prefix: contract::VENDOR_PREFIX,
            app_js_versioned: app_js_versioned_path(),
            paste_js_versioned: paste_js_versioned_path(),
            page_root_id: contract::PAGE_ROOT_ID,
            page_session_attr: contract::PAGE_SESSION_DATA_ATTR,
            sessions_id: contract::SESSIONS_ID,
            sessions_filter_input_id: contract::SESSIONS_FILTER_INPUT_ID,
            modal_host_id: contract::MODEL_PICKER_MODAL_HOST_ID,
            session_new_host_id: contract::SESSION_NEW_MODAL_HOST_ID,
            persona_modal_host_id: contract::PERSONA_MODAL_HOST_ID,
            mcp_modal_host_id: contract::MCP_MODAL_HOST_ID,
            tools_modal_host_id: contract::TOOLS_MODAL_HOST_ID,
            skills_modal_host_id: contract::SKILL_MODAL_HOST_ID,
            transcript_wrap_id: contract::TRANSCRIPT_WRAP_ID,
            transcript_id: contract::TRANSCRIPT_ID,
            cursor_attr: contract::TRANSCRIPT_CURSOR_DATA_ATTR,
            controls_id: contract::CONTROLS_ID,
            session_title_id: contract::SESSION_TITLE_ID,
            menu_id: contract::MENU_ID,
            drawer_scrim_id: contract::DRAWER_SCRIM_ID,
            menu_scrim_id: contract::MENU_SCRIM_ID,
            switch_overlay_id: contract::SWITCH_OVERLAY_ID,
            error_notice_id: contract::ERROR_NOTICE_ID,
            stream_url: format!("{}{session_id}?since={cursor}", contract::STREAM_PREFIX),
            sessions_route: contract::SESSIONS_ROUTE,
            filter_query_param: contract::SESSIONS_FILTER_QUERY_PARAM,
            session_page_path: contract::SESSION_PAGE_PREFIX,
            theme_color: theme_color_for(&web_theme),
            web_theme,
        }
    }
}

/// The page-root fragment: exactly the shell's root `div` — the shape
/// a navigation patch (`POST activate` / `POST session-new`) replaces
/// the live root with. Chrome only, by the navigation-shape contract:
/// the sessions container renders empty (the swapped root's tick
/// effect is the one list fetch) and the transcript renders empty at
/// cursor 0 (the new stream's catch-up is the one history transfer),
/// so the response is bounded by chrome size regardless of session
/// history. The full [`PageTemplate`] renders this same markup
/// through the shared `page_root.html` include; this struct exists so
/// the overlay patch carries only the root subtree (never a whole
/// HTML document — the browser runtime morphs element patches, and a
/// document-shaped patch would fight the page it lands in).
#[derive(Template)]
#[template(path = "page_root.html")]
pub struct PageRootTemplate {
    /// The viewed session's id (empty on the landing page).
    pub session_id: String,
    /// The session title.
    pub title: String,
    /// The sessions-list fragment (empty on the navigation shapes).
    pub sessions_html: String,
    /// The transcript fragment (the empty template — `#streaming`
    /// present, zero entries — on the navigation shapes).
    pub transcript_html: String,
    /// The composer-controls fragment.
    pub controls_html: String,
    /// The titlebar's model-picker button fragment.
    pub button_html: String,
    /// The button's host span id (`model-picker`) — the shell owns
    /// the span; the fragment fills it.
    pub picker_host_id: &'static str,
    /// The menu fragment.
    pub menu_html: String,
    /// The rendered-entry cursor the inline transcript carries —
    /// always 0 on the navigation shapes (the catch-up's starting
    /// cursor; the stream URL carries `?since=0` to match).
    pub cursor: usize,
    /// The page-root id.
    pub page_root_id: &'static str,
    /// The attribute carrying the viewed session's id.
    pub page_session_attr: &'static str,
    /// The sessions-list container id.
    pub sessions_id: &'static str,
    /// The sessions filter input's id.
    pub sessions_filter_input_id: &'static str,
    /// The model-picker modal's host id.
    pub modal_host_id: &'static str,
    /// The new-session modal's host id.
    pub session_new_host_id: &'static str,
    /// The persona modal's host id.
    pub persona_modal_host_id: &'static str,
    /// The MCP modal's host id.
    pub mcp_modal_host_id: &'static str,
    /// The tools modal's host id.
    pub tools_modal_host_id: &'static str,
    /// The skills modal's host id.
    pub skills_modal_host_id: &'static str,
    /// The transcript scroll-wrap id.
    pub transcript_wrap_id: &'static str,
    /// The transcript entry container id.
    pub transcript_id: &'static str,
    /// The attribute carrying the inline transcript's cursor.
    pub cursor_attr: &'static str,
    /// The composer-controls container id.
    pub controls_id: &'static str,
    /// The titlebar title span's id (empty on the landing page).
    pub session_title_id: &'static str,
    /// The menu region's host container id.
    pub menu_id: &'static str,
    /// The drawer scrim's id.
    pub drawer_scrim_id: &'static str,
    /// The menu scrim's id.
    pub menu_scrim_id: &'static str,
    /// The session-switch interstitial's id.
    pub switch_overlay_id: &'static str,
    /// The error-notice banner's id.
    pub error_notice_id: &'static str,
    /// The sessions pull's route — the list's effect re-pulls it.
    pub sessions_route: &'static str,
    /// The sessions pull's filter query parameter — the effect builds
    /// `?q=<text>` from it when a filter is in force.
    pub filter_query_param: &'static str,
    /// The activate POST's route prefix — the rename editor's target.
    pub session_page_path: &'static str,
    /// The viewed session's stream URL (empty on the landing page) —
    /// the `data-init` `@get` binding the root carries on session
    /// pages, so a navigation patch re-opens the stream by itself.
    pub stream_url: String,
    /// The render-time unread-mark count — the menu's sessions
    /// button badge's seeded value in the root's signal seed.
    pub sessions_attention: usize,
    /// The persisted color mode seeding the `webTheme` signal — the
    /// same value the full page render seeds.
    pub web_theme: String,
}

impl PageRootTemplate {
    /// Builds the root fragment for one viewed session — the
    /// navigation shape: chrome with the transcript the caller
    /// rendered (the empty template — `#streaming` present, zero
    /// entries) and the sessions container empty. The constructor
    /// accepts no sessions-HTML parameter by design: a navigation
    /// response carries no rows, and a signature that still took the
    /// fragment would invite a future caller to refill it (the list
    /// travels only through the GET pull and the swapped root's one
    /// tick-effect fetch).
    #[must_use]
    #[expect(
        clippy::too_many_arguments,
        reason = "the page render's full fragment set is one composite value in practice"
    )]
    pub fn session(
        session_id: &str,
        title: String,
        transcript_html: String,
        controls_html: String,
        button_html: String,
        menu_html: String,
        attention_count: usize,
        web_theme: String,
    ) -> Self {
        Self {
            session_id: session_id.to_owned(),
            title,
            cursor: 0,
            sessions_html: String::new(),
            transcript_html,
            controls_html,
            button_html,
            picker_host_id: contract::MODEL_PICKER_ID,
            menu_html,
            sessions_attention: attention_count,
            page_root_id: contract::PAGE_ROOT_ID,
            page_session_attr: contract::PAGE_SESSION_DATA_ATTR,
            sessions_id: contract::SESSIONS_ID,
            sessions_filter_input_id: contract::SESSIONS_FILTER_INPUT_ID,
            modal_host_id: contract::MODEL_PICKER_MODAL_HOST_ID,
            session_new_host_id: contract::SESSION_NEW_MODAL_HOST_ID,
            persona_modal_host_id: contract::PERSONA_MODAL_HOST_ID,
            mcp_modal_host_id: contract::MCP_MODAL_HOST_ID,
            tools_modal_host_id: contract::TOOLS_MODAL_HOST_ID,
            skills_modal_host_id: contract::SKILL_MODAL_HOST_ID,
            transcript_wrap_id: contract::TRANSCRIPT_WRAP_ID,
            transcript_id: contract::TRANSCRIPT_ID,
            cursor_attr: contract::TRANSCRIPT_CURSOR_DATA_ATTR,
            controls_id: contract::CONTROLS_ID,
            session_title_id: contract::SESSION_TITLE_ID,
            menu_id: contract::MENU_ID,
            drawer_scrim_id: contract::DRAWER_SCRIM_ID,
            menu_scrim_id: contract::MENU_SCRIM_ID,
            switch_overlay_id: contract::SWITCH_OVERLAY_ID,
            error_notice_id: contract::ERROR_NOTICE_ID,
            sessions_route: contract::SESSIONS_ROUTE,
            filter_query_param: contract::SESSIONS_FILTER_QUERY_PARAM,
            session_page_path: contract::SESSION_PAGE_PREFIX,
            stream_url: format!("{}{session_id}?since=0", contract::STREAM_PREFIX),
            web_theme,
        }
    }
}

/// One transcript entry of the transcript fragment.
#[derive(Template)]
#[template(path = "entry.html")]
pub struct EntryTemplate {
    /// The rendered entry.
    pub entry: crate::transcript::RenderedEntry,
    /// The entry's index — the cursor coordinate, rendered as the
    /// element id.
    pub index: usize,
}

/// The transcript fragment: the entries at `cursor..` (or all), then
/// the streaming container — always the fragment's last child, so a
/// `transcript-full` inner replace re-creates it empty and settled
/// entries never interleave with the streamed tail.
#[derive(Template)]
#[template(path = "transcript.html")]
pub struct TranscriptTemplate {
    /// The rendered entries.
    pub entries: Vec<crate::transcript::RenderedEntry>,
    /// The index the slice starts at (the first entry's cursor
    /// position).
    pub start: usize,
    /// The streaming container's id, from the contract.
    pub streaming_id: &'static str,
}

impl TranscriptTemplate {
    /// Builds the fragment with the contract's container id filled.
    #[must_use]
    pub fn new(entries: Vec<crate::transcript::RenderedEntry>, start: usize) -> Self {
        Self {
            entries,
            start,
            streaming_id: contract::STREAMING_ID,
        }
    }
}

/// One row of the sessions fragment.
#[derive(Template)]
#[template(path = "session_row.html")]
pub struct SessionRowTemplate {
    /// The row.
    pub item: SessionListItem,
    /// The row's class hook.
    pub row_class: &'static str,
    /// The attribute carrying the row's session id.
    pub session_id_attr: &'static str,
    /// The activate POST's route prefix — the row's `@post` target is
    /// `<prefix><id>/activate`, and the response replaces the page
    /// root (the activation navigation).
    pub session_page_path: &'static str,
    /// The page-root id — the activate response's swap target.
    pub page_root_id: &'static str,
    /// The working row's class and its spinner span's class — one
    /// hook for both, the span living inside the row.
    pub working_class: &'static str,
}

impl SessionRowTemplate {
    /// Builds the row template with the contract's hooks filled.
    #[must_use]
    pub fn with_contract(item: SessionListItem) -> Self {
        Self {
            item,
            row_class: contract::SESSION_ROW_CLASS,
            session_id_attr: contract::SESSION_ID_DATA_ATTR,
            session_page_path: contract::SESSION_PAGE_PREFIX,
            page_root_id: contract::PAGE_ROOT_ID,
            working_class: contract::WORKING_CLASS,
        }
    }
}

/// The sessions fragment: the sidebar list. The include of
/// `session_row.html` shares this struct's context, so the row hooks
/// ride as fields here.
#[derive(Template)]
#[template(path = "sessions.html")]
pub struct SessionsTemplate {
    /// One row per session, active first.
    pub rows: Vec<SessionListItem>,
    /// The row's class hook, visible to the row include.
    pub row_class: &'static str,
    /// The attribute carrying each row's session id.
    pub session_id_attr: &'static str,
    /// The activate POST's route prefix, visible to the row include.
    pub session_page_path: &'static str,
    /// The page-root id, visible to the row include.
    pub page_root_id: &'static str,
    /// The working row's class, visible to the row include.
    pub working_class: &'static str,
}

impl SessionsTemplate {
    /// Builds the fragment with the contract's row hooks filled.
    #[must_use]
    pub fn with_contract(rows: Vec<SessionListItem>) -> Self {
        Self {
            rows,
            row_class: contract::SESSION_ROW_CLASS,
            session_id_attr: contract::SESSION_ID_DATA_ATTR,
            session_page_path: contract::SESSION_PAGE_PREFIX,
            page_root_id: contract::PAGE_ROOT_ID,
            working_class: contract::WORKING_CLASS,
        }
    }
}

/// The composer-controls fragment: the send↔cancel button.
#[derive(Template)]
#[template(path = "controls.html")]
pub struct ControlsTemplate {
    /// The session the composer targets.
    pub session_id: String,
    /// Whether the session is busy (true renders the cancel button).
    pub busy: bool,
    /// The streaming spinner's id (rendered only while busy).
    pub spinner_id: &'static str,
    /// The form container's id.
    pub composer_id: &'static str,
    /// The textarea's id.
    pub input_id: &'static str,
    /// The attribute a button names its action with — rendered on
    /// non-interactive descendants where a hook is still useful
    /// (kept for the contract gate's vocabulary assertion).
    pub action_attr: &'static str,
    /// The send action's contract name.
    pub send_action: &'static str,
    /// The cancel action's contract name.
    pub cancel_action: &'static str,
    /// The queue action's contract name — the busy fragment's second
    /// button, posting the send endpoint (a busy-phase enqueue queues).
    pub queue_action: &'static str,
    /// The send endpoint — the composer's `@post` target.
    pub send_path: &'static str,
    /// The cancel endpoint — the cancel button's `@post` target.
    pub cancel_path: &'static str,
    /// The send button's label.
    pub send_label: &'static str,
    /// The cancel button's label.
    pub cancel_label: &'static str,
    /// The pending-image chips strip's id.
    pub chips_id: &'static str,
    /// The hidden image file input's id.
    pub file_input_id: &'static str,
    /// The visible attach label's id (the file input's click proxy).
    pub attach_button_id: &'static str,
    /// The queue button's label (busy fragment only).
    pub queue_label: &'static str,
}

impl ControlsTemplate {
    /// Builds the template with the contract's hooks filled.
    #[must_use]
    pub fn with_contract(session_id: String, busy: bool) -> Self {
        Self {
            session_id,
            busy,
            spinner_id: contract::BUSY_SPINNER_ID,
            composer_id: contract::COMPOSER_ID,
            input_id: contract::INPUT_ID,
            action_attr: contract::ACTION_DATA_ATTR,
            send_action: contract::ACTION_SEND,
            cancel_action: contract::ACTION_CANCEL,
            queue_action: contract::ACTION_QUEUE,
            send_path: contract::SEND_PATH,
            cancel_path: contract::CANCEL_PATH,
            send_label: "Send",
            cancel_label: "Cancel",
            chips_id: contract::IMAGE_CHIPS_ID,
            file_input_id: contract::IMAGE_FILE_INPUT_ID,
            attach_button_id: contract::IMAGE_ATTACH_BUTTON_ID,
            queue_label: "Q",
        }
    }
}

/// The menu fragment: the titlebar's hamburger button and its item
/// panel, rendered as one static fragment (the model-picker button's
/// precedent — a region renders its page-fill through a fragment even
/// when the content is chrome). The panel ships `hidden`; the client
/// toggles visibility and, on the item, the drawer-open class on the
/// page root.
#[derive(Template)]
#[template(path = "menu.html")]
pub struct MenuTemplate {
    /// The menu region's host container id.
    pub menu_id: &'static str,
    /// The titlebar sessions button's id.
    pub sessions_button_id: &'static str,
    /// The sessions button's count badge id.
    pub sessions_button_count_id: &'static str,
    /// The hamburger button's id.
    pub menu_button_id: &'static str,
    /// The item panel's id.
    pub menu_panel_id: &'static str,
    /// The one item's id ("Sessions").
    pub menu_item_sessions_id: &'static str,
    /// The new-session item's id ("New session").
    pub menu_item_new_session_id: &'static str,
    /// The persona item's id ("Persona"); rendered only when a session
    /// is viewed.
    pub menu_item_persona_id: &'static str,
    /// Whether a session is viewed — the persona item's render gate.
    /// Persona binds per-session and the landing page views nothing,
    /// so the server omits the item rather than hiding it client-side.
    pub has_session: bool,
    /// The attribute a button names its action with.
    pub action_attr: &'static str,
    /// The menu item's action name.
    pub menu_action: &'static str,
    /// The new-session item's action name.
    pub menu_new_session_action: &'static str,
    /// The new-session form pull's URL — the item's `@get` target.
    pub session_new_path: &'static str,
    /// The new-session modal host's id — the pull's swap target.
    pub session_new_host_id: &'static str,
    /// The persona-picker pull's URL prefix — the persona item's
    /// `@get` target is `<prefix><viewed session id>`.
    pub persona_picker_url: String,
    /// The persona modal host's CSS selector — the pull's swap target.
    pub persona_modal_host_selector: &'static str,
    /// The MCP item's id ("MCP servers"); rendered only when a session
    /// is viewed (enablement binds per-session — the persona item's
    /// rule).
    pub menu_item_mcp_id: &'static str,
    /// The MCP pull's URL prefix — the MCP item's `@get` target is
    /// `<prefix><viewed session id>`.
    pub mcp_picker_url: String,
    /// The MCP modal host's CSS selector — the pull's swap target.
    pub mcp_modal_host_selector: &'static str,
    /// The tools item's id ("Tools"); rendered only when a session is
    /// viewed (the tool filter binds per-session — the persona item's
    /// rule).
    pub menu_item_tools_id: &'static str,
    /// The tools pull's URL prefix — the tools item's `@get` target is
    /// `<prefix><viewed session id>`.
    pub tools_picker_url: String,
    /// The tools modal host's CSS selector — the pull's swap target.
    pub tools_modal_host_selector: &'static str,
    /// The skills item's id ("Skills"); rendered only when a session
    /// is viewed (enablement binds per-session — the persona item's
    /// rule).
    pub menu_item_skills_id: &'static str,
    /// The skills pull's URL prefix — the skills item's `@get` target
    /// is `<prefix><viewed session id>`.
    pub skills_picker_url: String,
    /// The skills modal host's CSS selector — the pull's swap target.
    pub skills_modal_host_selector: &'static str,
    /// The render-time unread-mark count — the badge's seeded value.
    pub attention_count: usize,
    /// The theme control's host id — the segmented Light/Dark/System
    /// control at the panel's tail (mode binds app-wide, not
    /// per-session, so it renders on the landing page too).
    pub menu_item_theme_id: &'static str,
    /// The theme control's Light option id.
    pub menu_item_theme_light_id: &'static str,
    /// The theme control's Dark option id.
    pub menu_item_theme_dark_id: &'static str,
    /// The theme control's System option id.
    pub menu_item_theme_system_id: &'static str,
    /// The theme pick POST's endpoint — each option's `@post` target.
    pub theme_path: &'static str,
    /// The active-mode signal's name — the options' active-mark class
    /// binding reads it.
    pub signal_web_theme: &'static str,
    /// The active option's class — applied by the signal binding.
    pub theme_active_class: &'static str,
}

impl MenuTemplate {
    /// Builds the template with the contract's hooks filled.
    ///
    /// `viewed_session` is the page's viewed session id — empty on the
    /// landing page, which omits the persona item entirely.
    /// `attention_count` seeds the sessions button's unread count —
    /// the render-time fact the live signal updates between renders.
    #[must_use]
    pub fn with_contract(viewed_session: &str, attention_count: usize) -> Self {
        Self {
            menu_id: contract::MENU_ID,
            sessions_button_id: contract::SESSIONS_BUTTON_ID,
            sessions_button_count_id: contract::SESSIONS_BUTTON_COUNT_ID,
            menu_button_id: contract::MENU_BUTTON_ID,
            attention_count,
            menu_panel_id: contract::MENU_PANEL_ID,
            menu_item_sessions_id: contract::MENU_ITEM_SESSIONS_ID,
            menu_item_new_session_id: contract::MENU_ITEM_NEW_SESSION_ID,
            menu_item_persona_id: contract::MENU_ITEM_PERSONA_ID,
            has_session: !viewed_session.is_empty(),
            action_attr: contract::ACTION_DATA_ATTR,
            menu_action: contract::ACTION_MENU_SESSIONS,
            menu_new_session_action: contract::ACTION_MENU_NEW_SESSION,
            session_new_path: contract::SESSION_NEW_ROUTE,
            session_new_host_id: contract::SESSION_NEW_MODAL_HOST_ID,
            persona_picker_url: format!("{}{viewed_session}", contract::PERSONA_PICKER_PREFIX),
            persona_modal_host_selector: contract::PERSONA_MODAL_HOST_SELECTOR,
            menu_item_mcp_id: contract::MENU_ITEM_MCP_ID,
            mcp_picker_url: format!("{}{viewed_session}", contract::MCP_PICKER_PREFIX),
            mcp_modal_host_selector: contract::MCP_MODAL_HOST_SELECTOR,
            menu_item_tools_id: contract::MENU_ITEM_TOOLS_ID,
            tools_picker_url: format!("{}{viewed_session}", contract::TOOLS_PICKER_PREFIX),
            tools_modal_host_selector: contract::TOOLS_MODAL_HOST_SELECTOR,
            menu_item_skills_id: contract::MENU_ITEM_SKILLS_ID,
            skills_picker_url: format!("{}{viewed_session}", contract::SKILL_PICKER_PREFIX),
            skills_modal_host_selector: contract::SKILL_MODAL_HOST_SELECTOR,
            menu_item_theme_id: contract::MENU_ITEM_THEME_ID,
            menu_item_theme_light_id: contract::MENU_ITEM_THEME_LIGHT_ID,
            menu_item_theme_dark_id: contract::MENU_ITEM_THEME_DARK_ID,
            menu_item_theme_system_id: contract::MENU_ITEM_THEME_SYSTEM_ID,
            theme_path: contract::THEME_PATH,
            signal_web_theme: contract::SIGNAL_WEB_THEME,
            theme_active_class: contract::THEME_ACTIVE_CLASS,
        }
    }
}

/// The model-picker's titlebar button fragment: the current model as a
/// clickable control — the **bare button**, no host span (the page
/// shell owns `#model-picker`; the switch and refresh POSTs patch this
/// same render into that host's interior). The label carries the
/// viewed selection; the client opens the modal on its click.
#[derive(Template)]
#[template(path = "model_button.html")]
pub struct ModelButtonTemplate {
    /// The session the picker targets.
    pub session_id: String,
    /// The current selection's label.
    pub current: String,
    /// The button's id.
    pub button_id: &'static str,
    /// The modal-open pull's URL (unfiltered) — the button's `@get`.
    pub picker_url: String,
    /// The modal host span's id — the pull's swap target.
    pub modal_host_id: &'static str,
}

impl ModelButtonTemplate {
    /// Builds the template with the contract's hooks filled.
    #[must_use]
    pub fn with_contract(session_id: String, current: String) -> Self {
        Self {
            picker_url: format!("{}{session_id}", contract::MODEL_PICKER_PREFIX),
            button_id: contract::MODEL_PICKER_BUTTON_ID,
            modal_host_id: contract::MODEL_PICKER_MODAL_HOST_ID,
            session_id,
            current,
        }
    }
}

/// One row of the model-picker modal — the shared builder's
/// [`ProviderPickerEntry`][entry] data stamped into markup. Rendered
/// standalone; the modal embeds the pre-rendered rows.
///
/// [entry]: jinn_provider_selection_msg::ProviderPickerEntry
#[derive(Template)]
#[template(path = "model_row.html")]
pub struct ModelRowTemplate {
    /// The row's entry data (presentation-free).
    pub entry: jinn_provider_selection_msg::ProviderPickerEntry,
    /// The model field split into matched/plain runs from the fuzzy
    /// filter — matched runs render in a highlight span. One plain run
    /// of the full text when there is no filter.
    pub model_segments: Vec<jinn_provider_selection_msg::provider_rows::TextSegment>,
    /// The provider-name field, split like `model_segments`.
    pub provider_segments: Vec<jinn_provider_selection_msg::provider_rows::TextSegment>,
    /// The session the switch targets.
    pub session_id: String,
    /// Whether this row is the session's current model (bold + marked).
    pub is_active: bool,
    /// The attribute a row names its model with — the click handler
    /// reads it when stamping `$providerId`.
    pub model_attr: &'static str,
    /// The attribute a row names its marker with.
    pub marker_attr: &'static str,
    /// The attribute an unavailable row carries.
    pub unavailable_attr: &'static str,
    /// The attribute a row names its action with (contract
    /// vocabulary; the Datastar click handler drives the switch).
    pub action_attr: &'static str,
    /// The switch action's contract name.
    pub model_action: &'static str,
    /// The switch POST's endpoint.
    pub model_path: &'static str,
}

impl ModelRowTemplate {
    /// Builds the template from a filtered entry: the entry data plus
    /// its filter-highlight segments.
    #[must_use]
    pub fn from_filtered(
        filtered: jinn_provider_selection_msg::FilteredEntry,
        session_id: String,
        is_active: bool,
    ) -> Self {
        let segments = filtered.segments();
        Self {
            entry: filtered.entry,
            model_segments: segments.model,
            provider_segments: segments.provider,
            session_id,
            is_active,
            model_attr: contract::MODEL_ID_DATA_ATTR,
            marker_attr: contract::MODEL_MARKER_ATTR,
            unavailable_attr: contract::MODEL_UNAVAILABLE_ATTR,
            action_attr: contract::ACTION_DATA_ATTR,
            model_action: contract::ACTION_MODEL,
            model_path: contract::MODEL_PATH,
        }
    }
}

/// The model-picker modal: the overlay, the filter input, the row
/// container, and the refresh button. Rendered into the picker
/// container by the GET pull (with the filter applied server-side);
/// the client toggles its visibility. The rows arrive pre-rendered
/// (each [`ModelRowTemplate`] renders first — the same shape the
/// sessions list uses for its rows).
#[derive(Template)]
#[template(path = "model_modal.html")]
pub struct ModelModalTemplate {
    /// The session the picker targets.
    pub session_id: String,
    /// The pre-rendered row fragments, already sorted/filtered by the
    /// shared builder.
    pub rows: Vec<String>,
    /// The current filter text (echoed back into the input).
    pub filter: String,
    /// The modal overlay's id.
    pub modal_id: &'static str,
    /// The filter input's id.
    pub filter_id: &'static str,
    /// The row container's id.
    pub rows_id: &'static str,
    /// The refresh button's id.
    pub refresh_id: &'static str,
    /// The attribute a button names its action with.
    pub action_attr: &'static str,
    /// The refresh action's contract name.
    pub refresh_action: &'static str,
    /// The refresh POST's endpoint.
    pub refresh_path: &'static str,
    /// The picker pull's route pattern (the modelsTick re-pull's base).
    pub picker_route: &'static str,
    /// The switch POST's endpoint.
    pub model_path: &'static str,
    /// The modal host span's id — the filter/tick re-pull's swap
    /// target.
    pub modal_host_id: &'static str,
}

impl ModelModalTemplate {
    /// Builds the template with the contract's hooks filled.
    #[must_use]
    pub fn with_contract(session_id: String, rows: Vec<String>, filter: String) -> Self {
        Self {
            session_id,
            rows,
            filter,
            modal_id: contract::MODEL_PICKER_MODAL_ID,
            filter_id: contract::MODEL_PICKER_FILTER_ID,
            rows_id: contract::MODEL_PICKER_ROWS_ID,
            refresh_id: contract::MODEL_PICKER_REFRESH_ID,
            action_attr: contract::ACTION_DATA_ATTR,
            refresh_action: contract::ACTION_REFRESH_MODELS,
            refresh_path: contract::MODELS_REFRESH_PATH,
            picker_route: contract::MODEL_PICKER_PREFIX,
            model_path: contract::MODEL_PATH,
            modal_host_id: contract::MODEL_PICKER_MODAL_HOST_ID,
        }
    }
}

/// One `<option>` row of the new-session form's selects — a raw
/// value string plus a label, presentation-free.
#[derive(Debug, Clone)]
pub struct SessionNewOption {
    /// The value the option submits (the configured path string for
    /// projects; the lifecycle name for lifecycles; empty for the
    /// blank row).
    pub value: String,
    /// The label the option shows.
    pub label: String,
    /// The selected lifecycle's parameter count, carried by lifecycle
    /// options so the client can show/hide the args input; projects
    /// leave this `None`.
    pub param_count: Option<usize>,
}

/// The new-session modal: project select, lifecycle select, args
/// input (only when the selected lifecycle declares parameters), and
/// submit. Rendered by the GET pull into
/// [`contract::SESSION_NEW_MODAL_HOST_ID`]; a lifecycle re-pull
/// replaces it wholesale. When no project entries are configured the
/// template renders the explanatory refusal wrapper instead of the
/// usable form (`empty` state, one string swap — no option-driven
/// branching in the template body).
#[derive(Template)]
#[template(path = "session_new.html")]
pub struct SessionNewTemplate {
    /// The project options, in configured order. Empty → refusal
    /// state.
    pub projects: Vec<SessionNewOption>,
    /// The lifecycle options, blank row first.
    pub lifecycles: Vec<SessionNewOption>,
    /// The lifecycle the form renders selected (empty string = the
    /// blank row).
    pub selected_lifecycle: String,
    /// Whether the selected lifecycle's setup declares parameters —
    /// the args input renders only when it does.
    pub show_args: bool,
    /// The selected lifecycle's replacement tokens, display-form and
    /// space-joined (`<1> <branch> <args>`) — what the user is being
    /// asked to type, the Discord prompt's token list.
    pub arg_tokens: String,
    /// The modal overlay's id.
    pub modal_id: &'static str,
    /// The project select's id.
    pub project_id: &'static str,
    /// The lifecycle select's id.
    pub lifecycle_id: &'static str,
    /// The args input's id.
    pub args_id: &'static str,
    /// The submit button's id.
    pub submit_id: &'static str,
    /// The refusal wrapper's id.
    pub empty_id: &'static str,
    /// The lifecycle option attribute carrying the param count.
    pub param_count_attr: &'static str,
    /// The submit action's contract name.
    pub create_action: &'static str,
    /// The create POST's endpoint.
    pub create_path: &'static str,
    /// The lifecycle re-pull's route.
    pub session_new_route: &'static str,
    /// The page-root id — the create response's swap target.
    pub page_root_id: &'static str,
    /// The modal host span's id — the lifecycle re-pull's swap
    /// target.
    pub session_new_host_id: &'static str,
}

impl SessionNewTemplate {
    /// Builds the template with the contract's hooks filled.
    #[must_use]
    pub fn with_contract(
        projects: Vec<SessionNewOption>,
        lifecycles: Vec<SessionNewOption>,
        selected_lifecycle: String,
        show_args: bool,
        arg_tokens: String,
    ) -> Self {
        Self {
            projects,
            lifecycles,
            selected_lifecycle,
            show_args,
            arg_tokens,
            modal_id: contract::SESSION_NEW_MODAL_ID,
            project_id: contract::SESSION_NEW_PROJECT_SELECT_ID,
            lifecycle_id: contract::SESSION_NEW_LIFECYCLE_SELECT_ID,
            args_id: contract::SESSION_NEW_ARGS_INPUT_ID,
            submit_id: contract::SESSION_NEW_SUBMIT_ID,
            empty_id: contract::SESSION_NEW_EMPTY_ID,
            param_count_attr: contract::SESSION_NEW_PARAM_COUNT_ATTR,
            create_action: contract::ACTION_SESSION_CREATE,
            create_path: contract::SESSION_CREATE_PATH,
            session_new_route: contract::SESSION_NEW_ROUTE,
            page_root_id: contract::PAGE_ROOT_ID,
            session_new_host_id: contract::SESSION_NEW_MODAL_HOST_ID,
        }
    }
}

/// One row of the persona modal — the shared row vocabulary's
/// [`PersonaRow`][row] data stamped into markup. Rendered standalone;
/// the modal embeds the pre-rendered rows.
///
/// [row]: jinn_persona_msg::PersonaRow
#[derive(Template)]
#[template(path = "persona_row.html")]
pub struct PersonaRowTemplate {
    /// The row's entry data (presentation-free).
    pub entry: jinn_persona_msg::PersonaRow,
    /// The name field split into matched/plain runs from the fuzzy
    /// filter — matched runs render in a highlight span. One plain run
    /// of the full text when there is no filter.
    pub name_segments: Vec<jinn_persona_msg::TextSegment>,
    /// The description field, split like `name_segments`.
    pub description_segments: Vec<jinn_persona_msg::TextSegment>,
    /// The attribute a row names its persona with — the switch POST's
    /// payload field.
    pub persona_attr: &'static str,
    /// The attribute marking the active row.
    pub active_attr: &'static str,
    /// The attribute a row names its action with (contract
    /// vocabulary; the Datastar click handler drives the switch).
    pub action_attr: &'static str,
    /// The switch action's contract name.
    pub persona_action: &'static str,
    /// The switch POST's endpoint.
    pub persona_path: &'static str,
}

impl PersonaRowTemplate {
    /// Builds the template from a filtered row: the row data plus its
    /// filter-highlight segments.
    #[must_use]
    pub fn from_filtered(filtered: jinn_persona_msg::FilteredPersonaRow) -> Self {
        let segments = filtered.segments();
        Self {
            entry: filtered.row,
            name_segments: segments.name,
            description_segments: segments.description,
            persona_attr: contract::PERSONA_NAME_DATA_ATTR,
            active_attr: contract::PERSONA_ACTIVE_ATTR,
            action_attr: contract::ACTION_DATA_ATTR,
            persona_action: contract::ACTION_PERSONA,
            persona_path: contract::PERSONA_PATH,
        }
    }
}

/// The persona modal: the overlay with filter input and the filtered
/// TUI-parity rows. Rendered into the persona modal host by the GET
/// pull (with the filter applied server-side); the client toggles its
/// visibility. The rows arrive pre-rendered (each
/// [`PersonaRowTemplate`] renders first — the same shape the model
/// picker's rows use).
#[derive(Template)]
#[template(path = "persona_modal.html")]
pub struct PersonaModalTemplate {
    /// The pre-rendered rows.
    pub rows: Vec<String>,
    /// The filter text the rows were rendered with (the input's
    /// initial value, so a re-pull keeps what the user typed).
    pub filter: String,
    /// The modal overlay's id.
    pub modal_id: &'static str,
    /// The filter input's id.
    pub filter_id: &'static str,
    /// The rows container's id.
    pub rows_id: &'static str,
    /// The modal host span's id — the pull's swap target.
    pub modal_host_id: &'static str,
    /// The persona-picker pull's route prefix — the filter effect's
    /// `@get` target.
    pub picker_route: &'static str,
}

impl PersonaModalTemplate {
    /// Builds the template with the contract's hooks filled.
    #[must_use]
    pub fn with_contract(rows: Vec<String>, filter: String) -> Self {
        Self {
            rows,
            filter,
            modal_id: contract::PERSONA_MODAL_ID,
            filter_id: contract::PERSONA_FILTER_ID,
            rows_id: contract::PERSONA_ROWS_ID,
            modal_host_id: contract::PERSONA_MODAL_HOST_ID,
            picker_route: contract::PERSONA_PICKER_PREFIX,
        }
    }
}

/// One MCP server row of the MCP servers modal: the shared row
/// builder's output (the TUI inspector consumes the same builder)
/// plus the contract hooks the toggle POST needs and the
/// filter-highlight segments. The status badge's class and label
/// derive from the row's connection status — the TUI's
/// `ServerRowState::derive` semantics verbatim (`None`/starting →
/// "starting", running → "running", dead → "dead"), never re-derived
/// here.
#[derive(Template)]
#[template(path = "mcp_row.html")]
pub struct McpRowTemplate {
    /// The shared builder's row data.
    pub row: jinn_mcp_msg::McpServerRow,
    /// The name field split into matched/plain runs from the fuzzy
    /// filter — matched runs render in `<mark>`. One plain run of the
    /// full text when there is no filter.
    pub name_segments: Vec<jinn_selection_widget::TextRun>,
    /// The description field, split like `name_segments`.
    pub description_segments: Vec<jinn_selection_widget::TextRun>,
    /// The attribute a row names its server with — the toggle POST's
    /// payload field.
    pub server_attr: &'static str,
    /// The attribute a row names its action with.
    pub action_attr: &'static str,
    /// The toggle action's contract name.
    pub toggle_action: &'static str,
    /// The toggle POST's endpoint.
    pub toggle_path: &'static str,
    /// The status badge's class suffix (`off`/`starting`/`running`/`dead`
    /// — the shared badge's label verbatim).
    pub status_class: &'static str,
    /// The status badge's label.
    pub status_label: &'static str,
    /// The filter text signal's name — the row's toggle POST echoes it
    /// so the response renders the filtered view.
    pub filter_signal: &'static str,
}

impl McpRowTemplate {
    /// Builds the template from a filtered shared-builder row: the
    /// badge is the msg crate's shared derivation — enablement first
    /// (a disabled server reads `off` even when the runtime cell still
    /// holds the killed connection's last report), then the connection
    /// status with no-report rendered as `starting`.
    #[must_use]
    pub fn from_filtered(filtered: jinn_mcp_msg::FilteredMcpRow) -> Self {
        let segments = filtered.row.segments(&filtered.match_indices);
        let badge = jinn_mcp_msg::McpRowBadge::derive(filtered.row.enabled, filtered.row.status);
        let status_label = badge.label();
        let status_class = status_label;
        Self {
            row: filtered.row,
            name_segments: segments.name,
            description_segments: segments.description,
            filter_signal: contract::SIGNAL_MCP_FILTER,
            server_attr: contract::MCP_SERVER_DATA_ATTR,
            action_attr: contract::ACTION_DATA_ATTR,
            toggle_action: contract::ACTION_MCP_TOGGLE,
            toggle_path: contract::MCP_TOGGLE_PATH,
            status_class,
            status_label,
        }
    }
}

/// The MCP modal: the overlay with the filter input and the filtered
/// TUI-parity rows. Rendered into the MCP modal host by the GET pull
/// (with the filter applied server-side); the client toggles its
/// visibility, and the `mcpTick` effect re-pulls the rows when the
/// coordinator's stored event lands (the modelsTick pattern). The
/// rows arrive pre-rendered (each [`McpRowTemplate`] renders first —
/// the shape the persona modal's rows use).
#[derive(Template)]
#[template(path = "mcp_modal.html")]
pub struct McpModalTemplate {
    /// The pre-rendered rows.
    pub rows: Vec<String>,
    /// The filter text the rows were rendered with (the input's
    /// initial value, so a re-pull keeps what the user typed).
    pub filter: String,
    /// The modal overlay's id.
    pub modal_id: &'static str,
    /// The filter input's id.
    pub filter_id: &'static str,
    /// The filter text signal's name — the input binds it; the rows
    /// effect reads it.
    pub filter_signal: &'static str,
    /// The rows container's id.
    pub rows_id: &'static str,
    /// The modal host span's id — the open pull's swap target.
    pub modal_host_id: &'static str,
    /// The MCP pull's route prefix — the menu item's `@get` target.
    pub picker_route: &'static str,
    /// The rows-fragment pull's URL prefix — the modal effect's `@get`
    /// target is `<prefix><viewed session id>`.
    pub rows_url: String,
    /// The rows container's CSS selector — the modal effect's swap
    /// target (never the modal host: the input must not re-render).
    pub rows_selector: &'static str,
}

impl McpModalTemplate {
    /// Builds the template with the contract's hooks filled.
    #[must_use]
    pub fn with_contract(rows: Vec<String>, filter: String, viewed_session: &str) -> Self {
        Self {
            rows,
            filter,
            modal_id: contract::MCP_MODAL_ID,
            filter_id: contract::MCP_FILTER_ID,
            filter_signal: contract::SIGNAL_MCP_FILTER,
            rows_id: contract::MCP_ROWS_ID,
            modal_host_id: contract::MCP_MODAL_HOST_ID,
            picker_route: contract::MCP_PICKER_PREFIX,
            rows_url: format!("{}{viewed_session}", contract::MCP_ROWS_PREFIX),
            rows_selector: contract::MCP_ROWS_SELECTOR,
        }
    }
}

/// The MCP modal's rows fragment: the filtered rows alone, rendered
/// into the rows container by the modal effect's re-pull and the
/// toggle POST — never the skeleton, so the filter input is never
/// re-rendered (a re-render would reset it to the pull's query
/// mid-typing).
#[derive(Template)]
#[template(path = "mcp_rows.html")]
pub struct McpRowsTemplate {
    /// The pre-rendered rows.
    pub rows: Vec<String>,
    /// The rows container's id.
    pub rows_id: &'static str,
}

impl McpRowsTemplate {
    /// Builds the template: the contract's rows id filled, the rows
    /// pre-rendered by the region.
    #[must_use]
    pub fn with_contract(rows: Vec<String>) -> Self {
        Self {
            rows,
            rows_id: contract::MCP_ROWS_ID,
        }
    }
}

/// One row of the tools modal: the TUI picker's ✓/✓ enablement mark
/// rendered as HTML. The row renders the shared vocabulary's
/// [`jinn_tools_msg::WebToolRow`] with its filter-highlight segments
/// — no badge, no status: the tool list has no runtime half to report
/// on.
#[derive(Template)]
#[template(path = "tools_row.html")]
pub struct ToolsRowTemplate {
    /// The shared vocabulary's row data.
    pub row: jinn_tools_msg::WebToolRow,
    /// The name field split into matched/plain runs from the fuzzy
    /// filter — matched runs render in `<mark>`. One plain run of the
    /// full text when there is no filter.
    pub name_segments: Vec<jinn_selection_widget::TextRun>,
    /// The description field, split like `name_segments`.
    pub description_segments: Vec<jinn_selection_widget::TextRun>,
    /// The filter text signal's name — the row's toggle POST echoes it
    /// so the response renders the filtered view.
    pub filter_signal: &'static str,
    /// The attribute a row names its tool with — the toggle POST's
    /// payload field.
    pub tool_attr: &'static str,
    /// The attribute a row names its action with.
    pub action_attr: &'static str,
    /// The toggle action's contract name.
    pub toggle_action: &'static str,
    /// The toggle POST's endpoint.
    pub toggle_path: &'static str,
}

impl ToolsRowTemplate {
    /// Builds the template from a filtered shared-vocabulary row.
    #[must_use]
    pub fn from_filtered(filtered: jinn_tools_msg::FilteredWebToolRow) -> Self {
        let segments = filtered.row.segments(&filtered.match_indices);
        Self {
            row: filtered.row,
            name_segments: segments.name,
            description_segments: segments.description,
            filter_signal: contract::SIGNAL_TOOLS_FILTER,
            tool_attr: contract::TOOL_NAME_DATA_ATTR,
            action_attr: contract::ACTION_DATA_ATTR,
            toggle_action: contract::ACTION_TOOLS_TOGGLE,
            toggle_path: contract::TOOLS_TOGGLE_PATH,
        }
    }
}

/// The tools rows container: the toggle responses' swap target. The
/// modal skeleton and the revert stamp live outside it — a toggle
/// re-render replaces the marks only, never re-executing the stamp's
/// `data-init` (the open-time snapshot must survive toggles) and
/// never nesting a second modal inside the open one.
#[derive(Template)]
#[template(path = "tools_rows.html")]
pub struct ToolsRowsTemplate {
    /// The pre-rendered rows.
    pub rows: Vec<String>,
    /// The rows container's id.
    pub rows_id: &'static str,
}

impl ToolsRowsTemplate {
    /// Builds the template: the contract's rows id filled, the rows
    /// pre-rendered by the region.
    #[must_use]
    pub fn with_contract(rows: Vec<String>) -> Self {
        Self {
            rows,
            rows_id: contract::TOOLS_ROWS_ID,
        }
    }
}

/// The tools modal: the overlay with the TUI-parity rows, plus the
/// revert snapshot's stamp element riding beside it in the same host.
/// Rendered into the tools modal host by the GET pull — the open pull
/// is also the snapshot's only writer (the stamp's `data-init` patches
/// the signal once per open; a re-open morphs the same-id stamp and
/// re-runs it). The toggle POST answers with this same fragment, whose
/// stamp is deliberately identical to the open pull's — morphing an
/// unchanged attribute re-runs nothing, and a changed snapshot must
/// never overwrite the open-time one, so the stamp carries the
/// open-time value on every render that produces it.
///
/// The dismissal expressions are guarded by `$toolsOpen` (the
/// title-editing handler's boolean-guard shape): the pulled modal
/// element persists hidden after close, and an unguarded document
/// escape would re-post a stale snapshot over newer TUI-side changes.
///
/// The tick effect lives on this skeleton (an element the rows-only
/// re-pull never re-renders): when a `tools` push bumps `$toolsTick`,
/// the open modal re-pulls the **rows route**, swapping `#tools-rows`
/// only — the stamp's `data-init` never re-runs mid-open, so the
/// open-time snapshot survives.
#[derive(Template)]
#[template(path = "tools_modal.html")]
pub struct ToolsModalTemplate {
    /// The pre-rendered rows.
    pub rows: Vec<String>,
    /// The filter text the rows were rendered with (the input's
    /// initial value, so a re-pull keeps what the user typed).
    pub filter: String,
    /// The modal overlay's id.
    pub modal_id: &'static str,
    /// The filter input's id.
    pub filter_id: &'static str,
    /// The filter text signal's name — the input binds it; the rows
    /// effect reads it.
    pub filter_signal: &'static str,
    /// The rows container's id.
    pub rows_id: &'static str,
    /// The revert snapshot's stamp element id.
    pub revert_stamp_id: &'static str,
    /// The open-time snapshot as a JSON literal — the stamp's
    /// `data-init` patches it into the signal verbatim (`null` when
    /// the session held no filter).
    pub revert_snapshot: String,
    /// The revert POST's endpoint — the dismissal handlers' target.
    pub revert_path: &'static str,
    /// The rows-fragment pull's URL prefix — the tick effect's `@get`
    /// target is `<prefix><viewed session id>`.
    pub rows_url: String,
    /// The rows container's CSS selector — the tick re-pull's swap
    /// target (never the modal host: the stamp must not re-init).
    pub rows_selector: &'static str,
}

impl ToolsModalTemplate {
    /// Builds the template: the contract hooks filled and the snapshot
    /// serialized through [`NameFilter`]'s own serde (mode and absence
    /// included — the wire form the revert restores verbatim).
    #[must_use]
    pub fn with_contract(
        rows: Vec<String>,
        filter: String,
        snapshot: Option<&jinn_core_types::NameFilter>,
        viewed_session: &str,
    ) -> Self {
        let revert_snapshot = snapshot.map_or_else(
            || "null".to_owned(),
            |filter| {
                serde_json::to_string(filter)
                    // A filter is a map and a set of plain strings; a
                    // serialization failure is unreachable. Rendering
                    // `null` degrades to "no revert on dismiss" — the
                    // safe side of the failure.
                    .map_or_else(|_| "null".to_owned(), |json| format!("'{json}'"))
            },
        );
        Self {
            rows,
            filter,
            modal_id: contract::TOOLS_MODAL_ID,
            filter_id: contract::TOOLS_FILTER_ID,
            filter_signal: contract::SIGNAL_TOOLS_FILTER,
            rows_id: contract::TOOLS_ROWS_ID,
            revert_stamp_id: contract::TOOLS_REVERT_STAMP_ID,
            revert_snapshot,
            revert_path: contract::TOOLS_REVERT_PATH,
            rows_url: format!("{}{viewed_session}", contract::TOOLS_ROWS_PREFIX),
            rows_selector: contract::TOOLS_ROWS_SELECTOR,
        }
    }
}

/// One skill row of the skills modal: the discovered [`Skill`][skill]
/// stamped with the contract hooks the toggle and load POSTs need,
/// plus the enablement mark, filter-highlight segments, source badge,
/// and loaded state the TUI picker's row vocabulary mirrors. Built
/// from the msg crate's filtered skill rows — never the TUI's
/// `SkillEntry` (that carries the theme and feeds ratatui line
/// rendering).
///
/// [skill]: jinn_skills_msg::Skill
#[derive(Template)]
#[template(path = "skill_row.html")]
pub struct SkillRowTemplate {
    /// The discovered skill's data.
    pub skill: jinn_skills_msg::Skill,
    /// Whether the skill passes the session's current filter — the
    /// enabled mark (● vs ○, the TUI marker vocabulary).
    pub enabled: bool,
    /// The name field split into matched/plain runs from the fuzzy
    /// filter — matched runs render in `<mark>`. One plain run of the
    /// full text when there is no filter.
    pub name_segments: Vec<jinn_selection_widget::TextRun>,
    /// The description field, split like `name_segments`.
    pub description_segments: Vec<jinn_selection_widget::TextRun>,
    /// The source badge's class suffix (`project`/`global`); global
    /// skills render no badge, the TUI badge rule.
    pub source_class: &'static str,
    /// The source badge's label; empty for global skills.
    pub source_label: &'static str,
    /// Whether the row renders a source badge — global skills render
    /// none (the TUI badge rule), project skills badge.
    pub has_source_badge: bool,
    /// The filter text signal's name — the row's toggle and load
    /// POSTs echo it so the responses render the filtered view.
    pub filter_signal: &'static str,
    /// The attribute a row names its skill with — the toggle and load
    /// POSTs' payload field.
    pub skill_attr: &'static str,
    /// The attribute a row names its action with.
    pub action_attr: &'static str,
    /// The toggle action's contract name — the row's own click.
    pub toggle_action: &'static str,
    /// The toggle POST's endpoint.
    pub toggle_path: &'static str,
    /// The force-load action's contract name — the row's `+` button.
    pub load_action: &'static str,
    /// The load POST's endpoint.
    pub load_path: &'static str,
    /// The force-load button's class hook.
    pub load_class: &'static str,
}

impl SkillRowTemplate {
    /// Builds the template from a filtered skill and the session's
    /// enablement read: the source badge follows the TUI badge rule
    /// (project-scoped skills badge, global skills do not).
    #[must_use]
    pub fn from_filtered(filtered: jinn_skills_msg::FilteredSkillRow, enabled: bool) -> Self {
        let segments = filtered.segments();
        let skill = filtered.skill;
        let (source_class, source_label, has_source_badge) = match &skill.source {
            jinn_skills_msg::SkillSource::Project { .. } => ("project", "project", true),
            jinn_skills_msg::SkillSource::Global => ("global", "", false),
        };
        Self {
            skill,
            enabled,
            name_segments: segments.name,
            description_segments: segments.description,
            filter_signal: contract::SIGNAL_SKILLS_FILTER,
            source_class,
            source_label,
            has_source_badge,
            skill_attr: contract::SKILL_NAME_DATA_ATTR,
            action_attr: contract::ACTION_DATA_ATTR,
            toggle_action: contract::ACTION_SKILL_TOGGLE,
            toggle_path: contract::SKILL_TOGGLE_PATH,
            load_action: contract::ACTION_SKILL_LOAD,
            load_path: contract::SKILL_LOAD_PATH,
            load_class: contract::SKILL_LOAD_CLASS,
        }
    }
}

/// The skills modal: the overlay with the filter input and the
/// filtered TUI-parity rows. Rendered into the skills modal host by
/// the GET pull (with the filter applied server-side); the client
/// toggles its visibility, and the `skillsTick` effect re-pulls the
/// rows when a discovery scan reports (the mcpTick pattern —
/// discovery lands async after the modal can be opened). The rows
/// arrive pre-rendered (each [`SkillRowTemplate`] renders first —
/// the shape the MCP modal's rows use).
#[derive(Template)]
#[template(path = "skill_modal.html")]
pub struct SkillModalTemplate {
    /// The pre-rendered rows.
    pub rows: Vec<String>,
    /// The filter text the rows were rendered with (the input's
    /// initial value, so a re-pull keeps what the user typed).
    pub filter: String,
    /// The modal overlay's id.
    pub modal_id: &'static str,
    /// The filter input's id.
    pub filter_id: &'static str,
    /// The filter text signal's name — the input binds it; the rows
    /// effect reads it.
    pub filter_signal: &'static str,
    /// The rows container's id.
    pub rows_id: &'static str,
    /// The modal host span's id — the open pull's swap target.
    pub modal_host_id: &'static str,
    /// The skills pull's route prefix — the menu item's `@get`
    /// target.
    pub picker_route: &'static str,
    /// The rows-fragment pull's URL prefix — the modal effect's `@get`
    /// target is `<prefix><viewed session id>`.
    pub rows_url: String,
    /// The rows container's CSS selector — the modal effect's swap
    /// target (never the modal host: the input must not re-render).
    pub rows_selector: &'static str,
}

impl SkillModalTemplate {
    /// Builds the template with the contract's hooks filled.
    #[must_use]
    pub fn with_contract(rows: Vec<String>, filter: String, viewed_session: &str) -> Self {
        Self {
            rows,
            filter,
            modal_id: contract::SKILL_MODAL_ID,
            filter_id: contract::SKILL_FILTER_ID,
            filter_signal: contract::SIGNAL_SKILLS_FILTER,
            rows_id: contract::SKILL_ROWS_ID,
            modal_host_id: contract::SKILL_MODAL_HOST_ID,
            picker_route: contract::SKILL_PICKER_PREFIX,
            rows_url: format!("{}{viewed_session}", contract::SKILL_ROWS_PREFIX),
            rows_selector: contract::SKILL_ROWS_SELECTOR,
        }
    }
}

/// The skills modal's rows fragment: the filtered rows alone, rendered
/// into the rows container by the modal effect's re-pull and the
/// toggle/load POSTs — never the skeleton, so the filter input is
/// never re-rendered (a re-render would reset it to the pull's query
/// mid-typing).
#[derive(Template)]
#[template(path = "skill_rows.html")]
pub struct SkillRowsTemplate {
    /// The pre-rendered rows.
    pub rows: Vec<String>,
    /// The rows container's id.
    pub rows_id: &'static str,
}

impl SkillRowsTemplate {
    /// Builds the template: the contract's rows id filled, the rows
    /// pre-rendered by the region.
    #[must_use]
    pub fn with_contract(rows: Vec<String>) -> Self {
        Self {
            rows,
            rows_id: contract::SKILL_ROWS_ID,
        }
    }
}

/// Renders a template to its HTML string.
///
/// # Errors
///
/// Returns the render error; askama render failures are template
/// bugs, so the caller maps them to a 500 with the sentence.
pub fn render<T: Template>(template: &T) -> Result<String, askama::Error> {
    template.render()
}
