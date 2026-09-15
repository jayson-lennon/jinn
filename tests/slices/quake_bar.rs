//! Quake-bar-slice integration tests: the slice's rows inside the composed
//! system.
//!
//! These exercise the quake-bar slice's route rows **as composed** — the
//! global `` `<M-`>` `` opener, the own-scope close shadow, the input-hook
//! catch-all, and the scroll row — queried through the full-composition
//! keymap (`composed_keymap`) so toggle shadowing is proven against every
//! other slice's rows being present.

#![allow(clippy::expect_used, clippy::panic, reason = "test code")]

use crate::common::composed_keymap;
use jinn_domain::{Intent, Key, KeyEvent, Modifiers};
use jinn_quake_bar::quake_scope;
use jinn_tui::Scope;

fn alt_backtick() -> KeyEvent {
    KeyEvent {
        key: Key::Char('`'),
        modifiers: Modifiers {
            ctrl: false,
            alt: true,
            shift: false,
        },
    }
}

/// The quake `<M-\`>` toggle: open from static scopes, close inside the
/// quake's own dynamic scope (specific-scope-wins).
#[rstest::rstest]
#[test]
fn quake_backtick_toggles_open_in_normal_and_close_in_quake_scope() {
    // Given the composed keymap in the Normal scope.
    let keymap = composed_keymap();

    // When pressing <M-`> in Normal scope.
    let intent = {
        let mut wk = jinn_tui::app::WhichKeyInstance::new(keymap.clone(), Scope::Normal);
        wk.handle_key(alt_backtick())
    };

    // Then it resolves to the quake open action.
    let intent = intent.expect("<M-`> must open the quake bar from Normal");
    assert!(
        matches!(&intent, Intent::Dynamic(d) if d.action == "open"),
        "expected the quake open action, got {intent:?}"
    );

    // And when pressing <M-`> in the quake's own scope, it resolves to
    // close — making <M-`> a toggle (specific-scope-wins over the opener).
    let mut wk = jinn_tui::app::WhichKeyInstance::new(keymap, Scope::Dynamic(quake_scope()));
    let intent = wk
        .handle_key(alt_backtick())
        .expect("<M-`> must resolve in QuakeBar");
    assert!(
        matches!(&intent, Intent::Dynamic(d) if d.action == "close"),
        "expected the quake close action, got {intent:?}"
    );
}

/// ESC resolves to the quake close action inside the quake scope.
#[rstest::rstest]
#[test]
fn esc_fires_quake_close_in_quake_scope() {
    // Given the composed keymap in the quake's dynamic scope.
    let keymap = composed_keymap();
    let mut wk = jinn_tui::app::WhichKeyInstance::new(keymap, Scope::Dynamic(quake_scope()));

    // When pressing ESC.
    let esc = KeyEvent {
        key: Key::Esc,
        modifiers: Modifiers::none(),
    };
    let intent = wk.handle_key(esc);

    // Then it resolves to the quake close action (which pops the scope).
    let intent = intent.expect("ESC in QuakeBar scope must fire an intent");
    assert!(
        matches!(&intent, Intent::Dynamic(d) if d.action == "close"),
        "ESC must resolve to the quake close action; got {intent:?}"
    );
}

/// A printable char in the quake input-hook scope synthesizes InsertChar:
/// the hook only sees intents the keymap emits.
#[rstest::rstest]
#[test]
fn printable_char_synthesizes_insert_char_in_quake_hook_scope() {
    // Given the composed keymap in the quake's dynamic scope.
    let keymap = composed_keymap();
    let mut wk = jinn_tui::app::WhichKeyInstance::new(keymap, Scope::Dynamic(quake_scope()));

    // When pressing a plain printable char.
    let key_x = KeyEvent {
        key: Key::Char('x'),
        modifiers: Modifiers::none(),
    };
    let intent = wk.handle_key(key_x);

    // Then which-key synthesizes the generic editing intent for the hook
    // scopes (the handler's hook consult routes it to the slice writer).
    assert!(
        matches!(intent, Some(Intent::InsertChar { ch: 'x' })),
        "printable char must synthesize InsertChar for the slice input hook; got {intent:?}"
    );
}

/// PageUp resolves to the quake scroll-up action (so the log scrolls).
#[rstest::rstest]
#[test]
fn pgup_fires_quake_scroll_up_in_quake_scope() {
    // Given the composed keymap in the quake's dynamic scope.
    let keymap = composed_keymap();
    let mut wk = jinn_tui::app::WhichKeyInstance::new(keymap, Scope::Dynamic(quake_scope()));

    // When pressing PageUp.
    let pgup = KeyEvent {
        key: Key::PageUp,
        modifiers: Modifiers::none(),
    };
    let intent = wk.handle_key(pgup);

    // Then it resolves to the quake scroll-up action.
    let intent = intent.expect("PageUp in QuakeBar scope must fire an intent");
    assert!(
        matches!(&intent, Intent::Dynamic(d) if d.action == "scroll-up"),
        "PageUp must resolve to the quake scroll-up action; got {intent:?}"
    );
}

/// A wired app whose base focus scope is the quake bar's dynamic scope.
async fn quake_app() -> jinn_tui::TuiApp {
    let app = crate::common::test_app().await;
    app.core
        .state
        .write_test_no_cap()
        .frontend
        .scope_swap_base(jinn_domain::FocusScope::Dynamic(quake_scope()));
    app
}

/// The quake cell for a wired app.
fn quake_cell(
    app: &jinn_tui::TuiApp,
) -> jinn_domain::common::slices::TypedCell<jinn_quake_bar::QuakeBarState> {
    app.services
        .slices
        .reader(&jinn_quake_bar::quake_bar_slot())
        .expect("harness registers the quake slot")
}

/// E2E regression: Backspace deletes the last typed grapheme through the
/// hook. The slice-port dropped the trunk editing binds, so Backspace
/// fell into the char catch-all, resolved to nothing, and editing died.
#[rstest::rstest]
#[tokio::test]
async fn backspace_deletes_via_the_hook() {
    // Given a wired app focused on the quake bar with "hi" typed in.
    let mut app = quake_app().await;
    app.which_key.set_scope(Scope::Dynamic(quake_scope()));
    let cell = quake_cell(&app);
    cell.update(|s| s.input.text.set("hi".to_owned()));

    // When Backspace resolves through the composed keymap and routes
    // like the run loop.
    let backspace = KeyEvent {
        key: Key::Backspace,
        modifiers: Modifiers::none(),
    };
    let intent = app
        .which_key
        .handle_key(backspace)
        .expect("Backspace must resolve in the quake hook scope");
    app.route_intent(intent);
    crate::common::wait_for("Backspace to delete the last grapheme", || {
        cell.read().input.text.input == "h"
    })
    .await;

    // Then the input holds "h".
    assert_eq!(
        cell.read().input.text.input,
        "h",
        "Backspace must delete through the slice hook"
    );
}

/// E2E discriminator: Enter submits the typed command — the keymap must
/// resolve Enter to the submit action, the action must emit
/// SubmitQuakeBarCommand, and the quake actor must append it to the log.
#[rstest::rstest]
#[tokio::test]
async fn enter_submits_and_appends_to_the_log() {
    // Given a wired app focused on the quake bar with "hi" typed in.
    let mut app = quake_app().await;
    app.which_key.set_scope(Scope::Dynamic(quake_scope()));
    let cell = quake_cell(&app);
    cell.update(|s| s.input.text.set("hi".to_owned()));

    // When Enter resolves through the composed keymap and routes like
    // the run loop.
    let enter = KeyEvent {
        key: Key::Enter,
        modifiers: Modifiers::none(),
    };
    let intent = app
        .which_key
        .handle_key(enter)
        .expect("Enter must resolve in the quake scope");
    app.route_intent(intent);

    // Then the submit fired: the log gains "hi" and the input clears.
    crate::common::wait_for("the submitted command to land in the log", || {
        cell.read().log.len() == 1
    })
    .await;
    assert_eq!(
        cell.read()
            .log
            .visible_lines(10)
            .first()
            .map(String::as_str),
        Some("hi")
    );
    assert!(
        cell.read().input.text.input.is_empty(),
        "submit must clear the input buffer"
    );
}
