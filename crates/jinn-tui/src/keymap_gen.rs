//! Slice keybind generation — the bridge between slice route rows and
//! the which-key keymap.
//!
//! Slices declare their keybinds as [`RouteRow`]s (scope + key +
//! outcome) attached to `KeyRoutes` at activation. This module
//! materializes those rows as keymap bindings, once, after all
//! activations: static intents resolve through the `RouteId` → intent
//! table below, dynamic actions bind to `Intent::Dynamic` carrying the
//! row's `(slice, action)` identity. An unregistered slice's keys are
//! simply never bound — removability is automatic, not maintained.

use jinn_kernel::Key;
use jinn_kernel::KeyEvent;
use jinn_slices::DynamicIntent;
use jinn_slices::SliceScopeId;
use jinn_slices::route::BindSite;
use jinn_slices::route::KeyRoutes;
use jinn_slices::route::RouteOutcome;
use jinn_slices::route::RouteRow;
use ratatui_which_key::Keymap;
use ratatui_which_key::parse_key_sequence;

use crate::keymap::KeyCategory;
use crate::scope::Scope;
use jinn_kernel::KernelIntent;

/// Resolves a static row's [`RouteId`] to the composition intent it
/// binds. Slice keybind blocks used to hardcode these — the table is
/// now the single central record of slice keys that are plain static
/// intents (shared-chrome keys like `q` → quit).
fn static_intent(route_id: &str) -> Option<KernelIntent> {
    match route_id {
        "dashboard:quit" | "sidebar:quit" | "term:quit" => Some(KernelIntent::Quit),
        "dashboard:switch-tab" => Some(KernelIntent::SwitchTab),
        "dashboard:which-key" | "sidebar:which-key" | "term:which-key" => {
            Some(KernelIntent::ToggleWhichkey)
        }
        "quake-bar:ctrl-clear" | "sidebar:ctrl-clear" => Some(KernelIntent::CtrlClear),
        _ => None,
    }
}

/// Maps a row category hint onto the keymap category enum.
/// Binds a picker spec's declared rows into its static scope as
/// data-carried [`Intent::PickerAction`] bindings.
fn category(name: &str) -> KeyCategory {
    match name {
        "navigation" => KeyCategory::Navigation,
        "input" => KeyCategory::Input,
        "chat-history" => KeyCategory::ChatHistory,
        "context" => KeyCategory::Context,
        _ => KeyCategory::General,
    }
}

/// The keymap scope a row binds into.
///
/// `OwnScope` rows bind in their slice's dynamic scope; `GlobalToggle`
/// rows bind in every static scope and in other slices' dynamic scopes
/// (input-hook scopes included, key-hook scopes deliberately excluded
/// so capture stays hermetic); `StaticScopes` rows bind in the named
/// composition scopes, looked up by display name.
fn scopes_for_row<'a>(
    routes: &'a KeyRoutes,
    row: &'a RouteRow,
    tabs: &'a [SliceScopeId],
    hooks: &'a [SliceScopeId],
    key_hooks: &'a [SliceScopeId],
) -> Vec<Scope> {
    match row.site {
        BindSite::OwnScope => vec![Scope::Dynamic(row.scope.clone())],
        BindSite::StaticScopes(names) => names
            .iter()
            .filter_map(|name| match name.parse::<Scope>() {
                Ok(scope) => Some(scope),
                Err(()) => {
                    tracing::warn!(
                        route = row.route_id.as_str(),
                        scope = name,
                        "static-scope row names an unknown scope; key unbound there"
                    );
                    None
                }
            })
            .collect(),
        BindSite::GlobalToggle => {
            let mut scopes: Vec<Scope> = [Scope::Normal, Scope::Input].into_iter().collect();
            // A picker scope is a modal overlay: while one is open the global
            // toggles still have to work. Modal scopes are discovered from the
            // routes themselves — a slice declares its picker modal — so a new
            // picker needs no line here.
            for scope in routes.modal_scopes() {
                scopes.push(Scope::Dynamic(scope));
            }
            for scope in tabs {
                scopes.push(Scope::Dynamic(scope.clone()));
            }
            for scope in hooks {
                if *scope != row.scope {
                    scopes.push(Scope::Dynamic(scope.clone()));
                }
            }
            // Key-hook scopes are intentionally excluded — this is the
            // GlobalToggle pass, and those scopes carry catch-all key
            // hooks instead (capture mode hermeticity). Modal scopes
            // (declared by their slice) are excluded too: while such a
            // scope is on top, other slices' toggles do not pierce it —
            // its keys come from its own rows and hooks. Both include
            // scopes that host their own rows (term:control hosts the
            // release-control row); the owning slice still binds there.
            scopes.retain(|scope| match scope {
                Scope::Dynamic(id) => {
                    // The row's own scope always binds (the owning
                    // slice's rows are the point).
                    if id == &row.scope {
                        return true;
                    }
                    !key_hooks.contains(id) && !routes.is_modal_scope(id)
                }
                _ => true,
            });
            scopes
        }
    }
}

/// Collects every dynamic scope the route table knows about: row scopes
/// (tab scopes) plus hook scopes (input and key-hook scopes). Used to
/// spread per-scope composition chrome (the `<M-t>` toggle) across
/// slices.
#[must_use]
pub fn dynamic_scopes(routes: &KeyRoutes) -> Vec<SliceScopeId> {
    let mut scopes: Vec<SliceScopeId> = routes.rows().iter().map(|r| r.scope.clone()).collect();
    for hook in routes
        .input_hook_scopes()
        .iter()
        .chain(routes.key_hook_scopes().iter())
    {
        if !scopes.contains(hook) {
            scopes.push(hook.clone());
        }
    }
    scopes
}

/// Derives which-key group descriptions from row keys.
///
/// A multi-token sequence (e.g. `gdc` — three keys) implies a group at
/// each proper prefix (`g`, `gd`): the prefix must describe itself or
/// the which-key popup shows it as an undescribed node. Descriptions
/// come from the owning slice's `feature` label. Existing descriptions
/// win: the keymap only fills `"..."` placeholders, so hardcoded group
/// descriptions (`g` → "general") are never clobbered — and the same
/// prefix reached via two slices merges into one group.
///
/// Groups derive at keymap level (not per scope): a scoped leaf binding
/// shadows the shared branch description in its own scope, while scopes
/// without a scoped leaf keep the group visible.
fn derive_groups_from_rows(
    rows: &[RouteRow],
    keymap: &mut Keymap<KeyEvent, Scope, KernelIntent, KeyCategory>,
) {
    let mut prefixes: Vec<(String, &'static str)> = Vec::new();
    for row in rows {
        // The leader placeholder only matters for `<leader>` notation,
        // which row keys never use.
        let tokens = parse_key_sequence::<KeyEvent>(row.key, &plain_key('\\'));
        for n in 1..tokens.len() {
            // A prefix is only derivable when its display form re-parses
            // to exactly the same tokens: plain chars and `<c-x>`/`<m-x>`
            // forms round-trip; named keys (`Tab`, `Esc`) and shifted
            // forms (`S-x`) do not. Joining can also fuse tokens
            // (`<M-a>` + `b` → `<M-ab>`), so equality is checked on the
            // re-parsed sequence, not per token.
            let Some(prefix) = tokens.get(..n) else {
                break;
            };
            let notation = describe_prefix(prefix);
            let reparsed = parse_key_sequence::<KeyEvent>(&notation, &plain_key('\\'));
            if reparsed != prefix {
                break;
            }
            if let Some(existing) = prefixes.iter_mut().find(|(p, _)| *p == notation) {
                if existing.1 != row.feature {
                    existing.1 = "actions";
                }
            } else {
                prefixes.push((notation, row.feature));
            }
        }
    }
    for (prefix, label) in prefixes {
        keymap.describe_group(&prefix, label);
    }
}

/// A bare character key with no modifiers.
fn plain_key(c: char) -> KeyEvent {
    KeyEvent {
        key: Key::Char(c),
        modifiers: jinn_kernel::Modifiers::none(),
    }
}

/// Joins parsed key tokens back into display notation.
fn describe_prefix(tokens: &[KeyEvent]) -> String {
    let mut out = String::new();
    for token in tokens {
        out.push_str(&ratatui_which_key::Key::display(token));
    }
    out
}

/// Materializes every attached route row as keymap bindings.
///
/// Called once after all slice activations, before the event loop. The
/// keymap is mutated in place so the generated bindings land in the
/// same tree as the built-in scope bindings.
pub fn bind_route_rows(
    routes: &KeyRoutes,
    keymap: &mut Keymap<KeyEvent, Scope, KernelIntent, KeyCategory>,
) {
    let rows = routes.rows();
    let input_hooks = routes.input_hook_scopes();
    let key_hooks = routes.key_hook_scopes();
    derive_groups_from_rows(&rows, keymap);
    // Row scopes that host other slices' global toggles: every registered
    // scope (rows + input hooks) except the row's own, where its OwnScope
    // rows must win. Key-hook scopes are excluded: nothing from the row
    // spread may land there (capture hermeticity).
    let mut tabs: Vec<SliceScopeId> = rows.iter().map(|r| r.scope.clone()).collect();
    for hook in &input_hooks {
        if !tabs.contains(hook) {
            tabs.push(hook.clone());
        }
    }
    tabs.dedup();
    for row in &rows {
        let category = category(row.category);
        let scopes = scopes_for_row(routes, row, &tabs, &input_hooks, &key_hooks);
        match &row.outcome {
            RouteOutcome::StaticIntent(_) => {
                let Some(intent) = static_intent(row.route_id.as_str()) else {
                    tracing::warn!(
                        route = row.route_id.as_str(),
                        "static route row has no composition intent mapping; key unbound"
                    );
                    continue;
                };
                for scope in scopes {
                    keymap.bind(row.key, intent.clone(), category, scope);
                }
            }
            RouteOutcome::Action {
                action, display, ..
            } => {
                let intent = KernelIntent::Dynamic(jinn_slices::DynamicIntent::new(
                    row.scope.clone(),
                    action,
                    display,
                ));
                for scope in scopes {
                    keymap.bind(row.key, intent.clone(), category, scope);
                }
            }
        }
    }
    // Typing carve-out: a slice with a registered *input* hook captures
    // printable keystrokes in its own scope. The keymap synthesizes a
    // dynamic intent per editing key — the char catch-all for printable
    // keys, plus explicit binds for the six non-char editing keys
    // (Backspace used to fall into the catch-all, resolve to nothing, and
    // die in the which-key popup). The intent handler's hook consult (not
    // a god-match arm) routes them to the slice's sync writer via
    // `as_edit_intent`, which still speaks the `EditIntent` vocabulary —
    // the kernel's own `InsertChar`/`DeleteGrapheme` variants are gone
    // with the chat input box. Key-hook scopes are excluded: their
    // catch-all encodes keys for the slice's own consumer.
    for hook in input_hooks {
        let scope = hook.clone();
        let char_scope = scope.clone();
        keymap.scope(Scope::Dynamic(hook.clone()), move |b| {
            let scope = scope.clone();
            b.bind(
                "<backspace>",
                KernelIntent::Dynamic(edit_intent(&scope, "delete-backward")),
                KeyCategory::Input,
            )
            .bind(
                "<delete>",
                KernelIntent::Dynamic(edit_intent(&scope, "delete-forward")),
                KeyCategory::Input,
            )
            .bind(
                "<left>",
                KernelIntent::Dynamic(edit_intent(&scope, "move-cursor-left")),
                KeyCategory::Input,
            )
            .bind(
                "<right>",
                KernelIntent::Dynamic(edit_intent(&scope, "move-cursor-right")),
                KeyCategory::Input,
            )
            .bind(
                "<home>",
                KernelIntent::Dynamic(edit_intent(&scope, "move-cursor-home")),
                KeyCategory::Input,
            )
            .bind(
                "<end>",
                KernelIntent::Dynamic(edit_intent(&scope, "move-cursor-end")),
                KeyCategory::Input,
            )
            .catch_all(move |key: KeyEvent| {
                if let KeyEvent {
                    key: Key::Char(c), ..
                } = &key
                {
                    Some(KernelIntent::Dynamic(
                        edit_intent(&char_scope, "insert-char").with_char(*c),
                    ))
                } else {
                    None
                }
            });
        });
    }
    // Key-hook catch-alls: a slice key hook captures *every* unbound key
    // in its scope (terminal capture mode forwards them to the pty).
    // Bindings beat catch-alls, so rows bound in the scope (the toggle
    // handback) keep priority; there is no global or chrome spread into
    // key-hook scopes, so the hook is the only exit — capture hermetic.
    for hook in key_hooks {
        let Some(hook_fn) = routes.key_hook(&hook) else {
            continue;
        };
        keymap.scope(Scope::Dynamic(hook.clone()), move |b| {
            b.catch_all(move |key: KeyEvent| hook_fn(&key).map(KernelIntent::Dynamic));
        });
    }
}

/// The dynamic intent a slice input hook's editing key dispatches to.
///
/// The hook consult in the intent handler matches on the action name and
/// translates it into the shared `EditIntent` vocabulary, so a slice's
/// filter keeps its existing hook implementation.
fn edit_intent(scope: &jinn_slices::SliceScopeId, action: &'static str) -> DynamicIntent {
    DynamicIntent::new(scope.clone(), action, action)
}

/// Carries a printable character in an edit intent's byte payload.
trait EditIntentChar {
    /// Returns the intent with `ch` encoded as its UTF-8 payload.
    fn with_char(self, ch: char) -> Self;
}

impl EditIntentChar for DynamicIntent {
    fn with_char(mut self, ch: char) -> Self {
        let mut buf = [0_u8; 4];
        self.bytes = ch.encode_utf8(&mut buf).as_bytes().to_vec();
        self
    }
}

/// Binds the chat input's printable-character catch-all in `Scope::Input`.
///
/// The box is a slice, so a typed character is no longer a static
/// `KernelIntent::InsertChar`; it dispatches as a dynamic intent to the
/// box's `insert-char` row, with the character carried in the intent's
/// byte payload. Every other `Scope::Input` key — including all the other
/// chat input keys — arrives through that slice's route rows instead.
///
/// Ctrl- or alt-modified characters are declined, so they resolve to
/// nothing: the which-key state dismisses and the key inserts no text.
/// This is what makes a pasted newline a noop rather than a `j` — in raw
/// mode the terminal reports the newline byte (`0x0A`) as Ctrl+J, and
/// without the guard the catch-all would insert that `j`. The guard also
/// keeps unbound chords (`<c-x>`, `<M-q>`) from inserting text instead of
/// falling through, mirroring the chat-input slice's own key hook, which
/// claims exactly the chars with no ctrl or alt held. Shift alone is not
/// a chord — the glyph already carries its case — so a shifted char
/// still inserts.
pub fn bind_chat_input_catch_all(keymap: &mut Keymap<KeyEvent, Scope, KernelIntent, KeyCategory>) {
    keymap.scope(Scope::Input, |b| {
        b.catch_all(|key: KeyEvent| match key.key {
            Key::Char(c) if !key.modifiers.ctrl && !key.modifiers.alt => {
                Some(KernelIntent::Dynamic(DynamicIntent::with_bytes(
                    jinn_chat_input_msg::chat_input_scope(),
                    "insert-char",
                    "type a character",
                    c.to_string().into_bytes(),
                )))
            }
            _ => None,
        });
    });
}

/// Resolves a key through the which-key state, with a shifted-variant
/// fallback.
///
/// Terminals that report native modifier state (the Windows console;
/// kitty-protocol terminals) deliver e.g. Shift+Left as `Left` with shift
/// set. Explicit shifted rows (like `<s-enter>`) match first, and a
/// catch-all hit wins too — but a shifted event whose plain form is bound
/// would otherwise resolve to nothing and be silently swallowed. After a
/// miss, and only after a miss that left no chord sequence pending, the
/// key is retried with shift cleared so a shifted variant behaves as its
/// unshifted form.
pub fn handle_key_with_shift_fallback(
    state: &mut crate::app::WhichKeyInstance,
    key: KeyEvent,
) -> Option<KernelIntent> {
    let shift_only = key.modifiers.shift && !key.modifiers.ctrl && !key.modifiers.alt;
    let base = key.key.clone();
    if let Some(intent) = state.handle_key(key) {
        return Some(intent);
    }
    if shift_only && !state.is_pending() {
        let unshifted = KeyEvent {
            key: base,
            modifiers: jinn_kernel::Modifiers::none(),
        };
        return state.handle_key(unshifted);
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, reason = "test code")]
    use super::*;
    use ratatui_which_key::WhichKeyState;

    /// Builds a fresh keymap with the full composition (static binds,
    /// route-row spread, chat-input catch-all) and sets it to the Input
    /// scope — the same resolution path production dispatch uses.
    fn input_scope_state() -> WhichKeyState<KeyEvent, Scope, KernelIntent, KeyCategory> {
        let keymap = crate::keymap::init();
        WhichKeyState::new(keymap, Scope::Input)
    }

    #[rstest::rstest]
    fn ctrl_j_in_the_input_scope_resolves_to_nothing() {
        // Given the Input scope's keymap state.
        let mut state = input_scope_state();

        // When pressing Ctrl+J — the event a pasted newline becomes in raw mode.
        let intent = state.handle_key(KeyEvent {
            key: Key::Char('j'),
            modifiers: jinn_kernel::Modifiers::ctrl(),
        });

        // Then nothing resolves: the key inserts no text.
        assert!(intent.is_none(), "Ctrl+J must not insert anything");
    }

    #[rstest::rstest]
    fn unbound_ctrl_chars_in_the_input_scope_resolve_to_nothing() {
        // Given the Input scope's keymap state.
        let mut state = input_scope_state();

        // When pressing an unbound ctrl chord.
        let intent = state.handle_key(KeyEvent {
            key: Key::Char('x'),
            modifiers: jinn_kernel::Modifiers::ctrl(),
        });

        // Then nothing resolves: an unbound chord must not type its letter.
        assert!(intent.is_none(), "unbound ctrl chord must not insert text");
    }

    #[rstest::rstest]
    fn alt_chars_in_the_input_scope_resolve_to_nothing() {
        // Given the Input scope's keymap state.
        let mut state = input_scope_state();

        // When pressing an unbound alt chord.
        let intent = state.handle_key(KeyEvent {
            key: Key::Char('a'),
            modifiers: jinn_kernel::Modifiers::alt(),
        });

        // Then nothing resolves: the catch-all matches the slice key hook's
        // contract that only unmodified chars insert.
        assert!(intent.is_none(), "unbound alt chord must not insert text");
    }

    #[rstest::rstest]
    fn plain_chars_still_insert_through_the_catch_all() {
        // Given the Input scope's keymap state.
        let mut state = input_scope_state();

        // When typing an unmodified character.
        let intent = state.handle_key(KeyEvent {
            key: Key::Char('a'),
            modifiers: jinn_kernel::Modifiers::none(),
        });

        // Then it resolves to the box's insert-char intent carrying the char.
        let KernelIntent::Dynamic(intent) = intent.expect("plain char must insert") else {
            panic!("expected a dynamic insert-char intent");
        };
        assert_eq!(intent.action, "insert-char");
        assert_eq!(intent.bytes, b"a");
    }

    #[rstest::rstest]
    fn shift_modified_char_in_the_input_scope_resolves_to_insert_char() {
        // Given the Input scope's keymap state.
        let mut state = input_scope_state();

        // When a char arrives with shift set (terminals that do not
        // pre-shift the glyph report it this way).
        let intent = state.handle_key(KeyEvent {
            key: Key::Char('G'),
            modifiers: jinn_kernel::Modifiers::shift(),
        });

        // Then it resolves to the box's insert-char intent carrying the
        // glyph — shift is not a chord the box declines.
        let KernelIntent::Dynamic(intent) = intent.expect("shifted char must insert") else {
            panic!("expected a dynamic insert-char intent");
        };
        assert_eq!(intent.action, "insert-char");
        assert_eq!(intent.bytes, b"G");
    }
}
