//! The stream-rule picker — the `<leader>sx` picker over the enforced
//! stream rules.
//!
//! Lists every rule the matcher enforces — the embedded shipped set, the
//! user `stream_rules.toml` set, the jinn.toml entries, and the
//! legacy-converted rules, deduped by name — with its armed/disabled mark,
//! toggles the highlighted rule's pending state with `<tab>`, persists the
//! pending set with `<enter>`, and closes with `<esc>` writing nothing.
//! Applies globally: one keybind, no per-session state.
//!
//! The picker is fully slice-owned: its state is a cell, its keys are route
//! rows attached at activation, and its renderer reads the cell. Portable
//! values and the crossing contracts live in `jinn-stream-rule-picker-msg`.
//!
//! # What the confirm reaches
//!
//! The confirm publishes
//! [`jinn_stream_rule_picker_msg::PersistStreamRuleDisabled`] — the
//! session, the preferences path, and the pending changes — and closes.
//! The slice's persist actor
//! ([`stream_rule_persist_actor::StreamRulePersistActor`]) consumes it:
//! the writes loop through the preferences-config storage boundary on a
//! blocking worker (a key handler must not stat a disk), and only when
//! every write landed does the actor publish
//! [`jinn_stream_rules_msg::StreamRulesReloaded`] — the stream-rules
//! slice's matcher actor answers it by recompiling the rules and
//! reinstalling the live matcher, so the confirmed set reaches the next
//! stream without a restart. A failed write pushes an `Error` chat entry
//! naming the reason and publishes no refresh; the confirm's own refusal
//! (no highlighted rule) stays on the status line and publishes nothing.

pub mod stream_rule_persist_actor;
pub mod stream_rule_picker_actions;
pub mod stream_rule_picker_render;
pub mod stream_rule_picker_routes;

pub use stream_rule_persist_actor::StreamRulePersistActor;
pub use stream_rule_picker_actions::{highlighted_name, open};
pub use stream_rule_picker_render::{render_stream_rule_picker, stream_rule_picker_overlay_rect};
pub use stream_rule_picker_routes::{
    STREAM_RULE_PICKER_BINDINGS, attach_stream_rule_picker_rows,
    register_stream_rule_picker_input_hook,
};
// The picker's identity lives in the `-msg` crate beside the state it
// names; re-exported so slice consumers import it from here like the
// workflows slice's `workflow_picker_scope`.
pub use jinn_stream_rule_picker_msg::stream_rule_picker_scope;

use marked_path::{Absolute, MarkedPathBuf};

/// Activates the slice: registers the picker's overlay, attaches its route
/// rows, and spawns the persist actor.
///
/// `preferences_path` is where this run's `jinn.toml` lives — captured here
/// and handed to every route action's closure, because the open assembles
/// the rule list from it (the user `stream_rules.toml` is its sibling), the
/// confirm publishes it for the persist actor, and the actor writes through
/// it. It is the same path the stream-rules slice activates with, so the
/// list the picker shows and the set the matcher installs are read from one
/// place.
///
/// `config` is the same `jinn.toml` layer every slice receives — the actor
/// writes through it, and it is cheap to clone.
///
/// The picker's cell is not minted here: the shared cell catalog
/// (`jinn_cell_catalog::register_all_cells`) registers every slice cell in
/// one place, before any slice activates, so this resolves the handle the
/// same way every other consumer does.
///
/// # Panics
///
/// Panics if the catalog has not run — the overlay would resolve no state
/// and render nothing, and the input hook would swallow keystrokes.
#[expect(
    clippy::expect_used,
    reason = "bootstrap assertion: broken slice wiring must abort launch, not continue degraded"
)]
pub fn activate(
    host: &mut jinn_slices::SliceHost<'_, jinn_slices::RenderFacts>,
    preferences_path: &MarkedPathBuf<Absolute>,
    config: jinn_config::ConfigLayer,
) {
    let cell = host
        .slices()
        .reader::<jinn_stream_rule_picker_msg::StreamRulePickerState>(
            &jinn_stream_rule_picker_msg::stream_rule_picker_slot(),
        )
        .expect(
            "the cell catalog registers the stream-rule picker slot before any slice activates",
        );
    let scope = stream_rule_picker_scope();
    host.register_overlay(
        scope.clone(),
        std::sync::Arc::new(stream_rule_picker_overlay_rect),
    );
    host.register_overlay_selectable(&scope);
    host.register_overlay_slot(
        scope.clone(),
        jinn_stream_rule_picker_msg::stream_rule_picker_slot(),
    );
    host.register_overlay_view(scope, std::sync::Arc::new(render_stream_rule_picker));

    // The picker's keys, and the filter's input hook, are the slice's own.
    attach_stream_rule_picker_rows(host.key_routes(), &cell, preferences_path);
    register_stream_rule_picker_input_hook(host.key_routes(), &cell);

    // The persist actor consumes the confirm's command; it must be
    // subscribed before the picker can confirm, and it cannot confirm
    // before any slice is interactive, so spawning here — after the rows
    // exist — covers every ordering the runtime can produce.
    StreamRulePersistActor::spawn(host.system(), config);
}

#[cfg(test)]
mod stream_rule_picker_tests;
