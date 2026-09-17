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

use jinn_domain::Key;
use jinn_domain::KeyEvent;
use jinn_domain::common::slices::key_routes::BindSite;
use jinn_domain::common::slices::key_routes::KeyRoutes;
use jinn_domain::common::slices::key_routes::RouteOutcome;
use jinn_domain::common::slices::key_routes::RouteRow;
use jinn_slices::SliceScopeId;
use ratatui_which_key::Keymap;
use ratatui_which_key::parse_key_sequence;

use crate::keymap::KeyCategory;
use crate::scope::Scope;
use jinn_domain::Intent;

/// Resolves a static row's [`RouteId`] to the composition intent it
/// binds. Slice keybind blocks used to hardcode these — the table is
/// now the single central record of slice keys that are plain static
/// intents (shared-chrome keys like `q` → quit).
fn static_intent(route_id: &str) -> Option<Intent> {
    match route_id {
        "dashboard:quit" | "sidebar:quit" | "term:quit" => Some(Intent::Quit),
        "dashboard:switch-tab" => Some(Intent::SwitchTab),
        "dashboard:which-key" | "sidebar:which-key" | "term:which-key" => {
            Some(Intent::ToggleWhichkey)
        }
        "quake-bar:ctrl-clear" | "sidebar:ctrl-clear" => Some(Intent::CtrlClear),
        _ => None,
    }
}

/// Maps a row category hint onto the keymap category enum.
/// Binds a picker spec's declared rows into its static scope as
/// data-carried [`Intent::PickerAction`] bindings.
///
/// The picker's open/confirm/close/nav base stays in `keymap::init`'s
/// static scope builder; this appends the spec's kind-specific rows so
/// keymap, keybind line, and geometry all derive from the same data.
pub fn bind_picker_spec_rows(
    registry: &jinn_picker::PickerRegistry,
    keymap: &mut Keymap<KeyEvent, Scope, Intent, KeyCategory>,
) {
    for spec in registry.all() {
        let Some(scope) = picker_spec_scope(spec.id()) else {
            tracing::warn!(
                picker = spec.id().as_str(),
                "no static scope for picker spec"
            );
            continue;
        };
        for row in spec.binds() {
            let intent = Intent::PickerAction {
                picker: spec.id().as_str().to_owned(),
                action: row.notation.to_owned(),
            };
            keymap.scope(scope.clone(), |b| {
                b.bind(row.notation, intent, category(row.category_hint));
            });
        }
    }
}

/// The static scope hosting `id`'s spec-derived bindings.
///
/// The adapter lives in the domain crate; jinn-tui keeps only this
/// scope-level mapping (a jinn-tui concern).
fn picker_spec_scope(id: jinn_picker::PickerId) -> Option<Scope> {
    match id.as_str() {
        "persona" => Some(Scope::PickerPersona),
        "skill" => Some(Scope::PickerSkill),
        "tool" => Some(Scope::PickerTool),
        "mcp-server" => Some(Scope::PickerMcpServer),
        "session-lifecycle" => Some(Scope::PickerLifecycle),
        "reasoning-effort" => Some(Scope::PickerReasoningEffort),
        "plugin" => Some(Scope::PickerPlugin),
        "task-list" => Some(Scope::PickerTaskList),
        "session" => Some(Scope::PickerSession),
        "provider" => Some(Scope::PickerProvider),
        "endpoint" => Some(Scope::PickerEndpoint),
        "project" => Some(Scope::PickerProject),
        _ => None,
    }
}

fn category(name: &str) -> KeyCategory {
    match name {
        "navigation" => KeyCategory::Navigation,
        "input" => KeyCategory::Input,
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
            let mut scopes: Vec<Scope> = [
                Scope::Normal,
                Scope::Input,
                Scope::ArgInput,
                Scope::TokenBudgetInput,
                Scope::RenameSessionInput,
                Scope::ProjectAddInput,
                Scope::PrunerAccumulationInput,
                Scope::PickerProvider,
                Scope::PickerSession,
                Scope::PickerPersona,
                Scope::PickerTheme,
                Scope::PickerLifecycle,
                Scope::PickerReasoningEffort,
                Scope::PickerEndpoint,
                Scope::PickerTool,
                Scope::PickerSkill,
                Scope::PickerTaskList,
                Scope::PickerProject,
                Scope::PickerMcpServer,
                Scope::PickerPlugin,
            ]
            .into_iter()
            .collect();
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
    keymap: &mut Keymap<KeyEvent, Scope, Intent, KeyCategory>,
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
        modifiers: jinn_domain::Modifiers::none(),
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
    keymap: &mut Keymap<KeyEvent, Scope, Intent, KeyCategory>,
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
                let intent = Intent::Dynamic(jinn_slices::DynamicIntent::new(
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
    // printable keystrokes in its own scope. The keymap synthesizes the
    // generic editing intents — the char catch-all for printable keys,
    // plus trunk-parity explicit binds for the six non-char editing
    // keys (Backspace used to fall into the catch-all, resolve to
    // nothing, and die in the which-key popup). The intent handler's
    // hook consult (not a god-match arm) routes them to the slice's
    // sync writer via `as_edit_intent`. Key-hook scopes are excluded:
    // their catch-all encodes keys for the slice's own consumer.
    for hook in input_hooks {
        keymap.scope(Scope::Dynamic(hook.clone()), |b| {
            b.bind("<backspace>", Intent::DeleteGrapheme, KeyCategory::Input)
                .bind(
                    "<delete>",
                    Intent::DeleteGraphemeForward,
                    KeyCategory::Input,
                )
                .bind("<left>", Intent::MoveCursorLeft, KeyCategory::Input)
                .bind("<right>", Intent::MoveCursorRight, KeyCategory::Input)
                .bind("<home>", Intent::MoveCursorToStart, KeyCategory::Input)
                .bind("<end>", Intent::MoveCursorToEnd, KeyCategory::Input)
                .catch_all(|key: KeyEvent| {
                    if let KeyEvent {
                        key: Key::Char(c), ..
                    } = &key
                    {
                        Some(Intent::InsertChar { ch: *c })
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
            b.catch_all(move |key: KeyEvent| hook_fn(&key).map(Intent::Dynamic));
        });
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]

    use super::bind_route_rows;
    use super::category;
    use super::static_intent;
    use crate::app::WhichKeyInstance;
    use crate::keymap::KeyCategory;
    use crate::scope::Scope;
    use jinn_domain::Intent;
    use jinn_domain::KeyEvent;
    use jinn_domain::common::slices::key_routes::ActionFn;
    use jinn_domain::common::slices::key_routes::BindSite;
    use jinn_domain::common::slices::key_routes::KeyRoutes;
    use jinn_domain::common::slices::key_routes::RouteId;
    use jinn_domain::common::slices::key_routes::RouteOutcome;
    use jinn_domain::common::slices::key_routes::RouteRow;
    use jinn_domain::protocol::IntentResult;
    use jinn_slices::SliceScopeId;
    use ratatui_which_key::Keymap;

    fn quake_open_row() -> RouteRow {
        RouteRow {
            route_id: RouteId::new("quake-bar:open"),
            scope: SliceScopeId::new("quake-bar", "open"),
            key: "<M-`>",
            category: "general",
            site: BindSite::GlobalToggle,
            feature: "quake-bar",
            outcome: RouteOutcome::Action {
                action: "open",
                display: "quake bar",
                run: ActionFn::new(|_ctx| IntentResult::empty()),
            },
        }
    }

    fn term_toggle_row() -> RouteRow {
        RouteRow {
            route_id: RouteId::new("term:toggle-overlay"),
            scope: SliceScopeId::new("term", "view"),
            key: "<M-t>",
            category: "general",
            site: BindSite::GlobalToggle,
            feature: "term",
            outcome: RouteOutcome::Action {
                action: "toggle-overlay",
                display: "terminal overlay",
                run: ActionFn::new(|_ctx| IntentResult::empty()),
            },
        }
    }

    fn dashboard_quit_row() -> RouteRow {
        RouteRow {
            route_id: RouteId::new("dashboard:quit"),
            scope: SliceScopeId::new("dashboard", "tab"),
            key: "q",
            category: "general",
            site: BindSite::OwnScope,
            feature: "dashboard",
            outcome: RouteOutcome::StaticIntent(RouteId::new("dashboard:quit")),
        }
    }

    #[rstest::rstest]
    #[test]
    fn dynamic_action_row_binds_dynamic_intent_in_own_scope() {
        // Given a route table with a quake-bar open row (global toggle).
        let routes = KeyRoutes::new();
        routes.attach(quake_open_row());

        // When generating bindings into a fresh keymap.
        let mut keymap = Keymap::new();
        bind_route_rows(&routes, &mut keymap);

        // Then Normal scope carries the toggle as a dynamic intent...
        let bindings = keymap.bindings_for_scope(Scope::Normal);
        assert!(
            bindings.iter().any(|group| group
                .bindings
                .iter()
                .any(|b| b.description.contains("quake bar"))),
            "Normal scope should show the open binding"
        );
        // ...and the slice's own scope also carries it: with only this
        // row attached there is no competing close row, so the toggle is
        // the binding the slice's scope resolves (a registered slice's
        // OwnScope close row then shadows it by binding the same key).
    }

    #[rstest::rstest]
    #[test]
    fn global_toggle_row_binds_in_normal_scope() {
        // Given a route table with a quake-bar open row (global toggle).
        let routes = KeyRoutes::new();
        routes.attach(quake_open_row());

        // When generating bindings into a fresh keymap.
        let mut keymap = Keymap::new();
        bind_route_rows(&routes, &mut keymap);

        // Then Normal scope gained the <M-`> binding.
        let bindings = keymap.bindings_for_scope(Scope::Normal);
        assert!(
            bindings.iter().any(|group| group
                .bindings
                .iter()
                .any(|b| b.description.contains("quake bar"))),
            "Normal scope should show the quake toggle"
        );
    }

    #[rstest::rstest]
    #[test]
    fn static_row_resolves_through_the_intent_table() {
        // Given a route table with the dashboard quit row.
        let routes = KeyRoutes::new();
        routes.attach(dashboard_quit_row());

        // When resolving the row's route id through the mapping.
        let intent = static_intent("dashboard:quit");

        // Then it resolves to the shared-chrome Quit intent.
        assert_eq!(intent, Some(Intent::Quit));
        // And an unknown route id resolves to nothing (unbound, not guessed).
        assert_eq!(static_intent("dashboard:unknown"), None);
    }

    #[rstest::rstest]
    #[test]
    fn category_hints_map_onto_keymap_categories() {
        // Given the three category hints in use.
        // When mapping them.
        // Then they land on the matching keymap categories.
        assert_eq!(category("navigation"), KeyCategory::Navigation);
        assert_eq!(category("input"), KeyCategory::Input);
        assert_eq!(category("general"), KeyCategory::General);
        // And an unknown hint falls back to General.
        assert_eq!(category("whatever"), KeyCategory::General);
    }

    fn key(notation: &str) -> KeyEvent {
        KeyEvent::parse_notation(notation).expect("notation should parse")
    }

    fn leaf_at(
        keymap: &Keymap<KeyEvent, Scope, Intent, KeyCategory>,
        keys: &[KeyEvent],
        scope: &Scope,
    ) -> Option<Intent> {
        match keymap.navigate(keys, scope) {
            Some(ratatui_which_key::NodeResult::Leaf { action }) => Some(action),
            _ => None,
        }
    }

    fn at_path(
        keymap: &Keymap<KeyEvent, Scope, Intent, KeyCategory>,
        keys: &[KeyEvent],
        scope: &Scope,
    ) -> Vec<(KeyEvent, String)> {
        keymap
            .children_at_path(keys, scope)
            .unwrap_or_default()
            .into_iter()
            .map(|b| (b.key, b.description))
            .collect()
    }

    #[rstest::rstest]
    #[test]
    fn static_scope_row_binds_only_in_listed_scopes() {
        // Given a route table with a row bound to the Normal scope only.
        let routes = KeyRoutes::new();
        routes.attach(RouteRow {
            route_id: RouteId::new("test:act"),
            scope: SliceScopeId::new("test-slice", "main"),
            key: "zq",
            category: "general",
            site: BindSite::StaticScopes(&["Normal"]),
            feature: "test-slice",
            outcome: RouteOutcome::Action {
                action: "act",
                display: "test action",
                run: ActionFn::new(|_ctx| IntentResult::empty()),
            },
        });

        // When generating bindings into a fresh keymap.
        let mut keymap = Keymap::new();
        bind_route_rows(&routes, &mut keymap);

        // Then Normal resolves the row's dynamic intent.
        let normal = leaf_at(&keymap, &[key("z"), key("q")], &Scope::Normal);
        assert!(normal.is_some(), "Normal should bind the zq sequence");
        // And Input does not: the row named only Normal.
        let input = leaf_at(&keymap, &[key("z"), key("q")], &Scope::Input);
        assert!(input.is_none(), "Input should not bind the zq sequence");
    }

    #[rstest::rstest]
    #[test]
    fn static_scope_row_skips_unknown_scope_names() {
        // Given a route table with a row naming a nonexistent scope.
        let routes = KeyRoutes::new();
        routes.attach(RouteRow {
            route_id: RouteId::new("test:act"),
            scope: SliceScopeId::new("test-slice", "main"),
            key: "zq",
            category: "general",
            site: BindSite::StaticScopes(&["NoSuchScope"]),
            feature: "test-slice",
            outcome: RouteOutcome::Action {
                action: "act",
                display: "test action",
                run: ActionFn::new(|_ctx| IntentResult::empty()),
            },
        });

        // When generating bindings into a fresh keymap.
        let mut keymap = Keymap::new();
        bind_route_rows(&routes, &mut keymap);

        // Then no scope gained the binding.
        assert!(at_path(&keymap, &[key("z"), key("q")], &Scope::Normal).is_empty());
        // And the dynamic scope didn't silently inherit it either.
        let dynamic = Scope::Dynamic(SliceScopeId::new("test-slice", "main"));
        assert!(at_path(&keymap, &[key("z"), key("q")], &dynamic).is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn multi_key_row_describes_prefix_groups() {
        // Given a route table with a three-key row (`zqc`).
        let routes = KeyRoutes::new();
        routes.attach(RouteRow {
            route_id: RouteId::new("test:act"),
            scope: SliceScopeId::new("test-slice", "main"),
            key: "zqc",
            category: "general",
            site: BindSite::StaticScopes(&["Normal"]),
            feature: "test-slice",
            outcome: RouteOutcome::Action {
                action: "act",
                display: "test action",
                run: ActionFn::new(|_ctx| IntentResult::empty()),
            },
        });

        // When generating bindings into a fresh keymap.
        let mut keymap = Keymap::new();
        bind_route_rows(&routes, &mut keymap);

        // Then the root shows `z` as a group named for the owning slice,
        let root = at_path(&keymap, &[], &Scope::Normal);
        assert!(
            root.iter()
                .any(|(k, d)| *k == key("z") && d == "test-slice"),
            "root should describe z as a group, got {root:?}"
        );
        // And the `z` group shows `q` as a group too.
        let zg = at_path(&keymap, &[key("z")], &Scope::Normal);
        assert!(
            zg.iter().any(|(k, d)| *k == key("q") && d == "test-slice"),
            "z group should describe q as a group, got {zg:?}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn derived_groups_never_clobber_hardcoded_descriptions() {
        // Given a keymap with a hardcoded `z` group ("builtin") and a
        // route table whose row would derive `z` ("test-slice").
        let mut keymap = Keymap::new();
        keymap.describe_group_with_category("z", "builtin", KeyCategory::General);
        let routes = KeyRoutes::new();
        routes.attach(RouteRow {
            route_id: RouteId::new("test:act"),
            scope: SliceScopeId::new("test-slice", "main"),
            key: "zq",
            category: "general",
            site: BindSite::StaticScopes(&["Normal"]),
            feature: "test-slice",
            outcome: RouteOutcome::Action {
                action: "act",
                display: "test action",
                run: ActionFn::new(|_ctx| IntentResult::empty()),
            },
        });

        // When generating bindings into that keymap.
        bind_route_rows(&routes, &mut keymap);

        // Then the hardcoded description survives.
        let root = at_path(&keymap, &[], &Scope::Normal);
        assert!(
            root.iter().any(|(k, d)| *k == key("z") && d == "builtin"),
            "hardcoded group description should win, got {root:?}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn single_token_keys_never_derive_groups() {
        // Given a route table with only single-token rows (the `<M-\`>` quake
        // toggle and a plain `k`).
        let routes = KeyRoutes::new();
        routes.attach(quake_open_row());
        routes.attach(RouteRow {
            route_id: RouteId::new("test:act"),
            scope: SliceScopeId::new("test-slice", "main"),
            key: "k",
            category: "navigation",
            site: BindSite::StaticScopes(&["Normal"]),
            feature: "test-slice",
            outcome: RouteOutcome::Action {
                action: "act",
                display: "test action",
                run: ActionFn::new(|_ctx| IntentResult::empty()),
            },
        });

        // When generating bindings into a fresh keymap.
        let mut keymap = Keymap::new();
        bind_route_rows(&routes, &mut keymap);

        // Then no group descriptions were derived: the root's `M-\`>`
        // binding keeps its leaf description, and no stray descriptions
        // appear for any key.
        let root = at_path(&keymap, &[], &Scope::Normal);
        assert!(
            root.iter()
                .all(|(_, d)| d != "test-slice" && d != "quake-bar"),
            "no derived group descriptions should exist, got {root:?}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn multi_key_synthetic_row_derives_a_group_label_and_resolves() {
        // Given a route table with a three-key synthetic row (`gdc` shape:
        // a StaticScopes leaf deep enough to imply two prefixes).
        let routes = KeyRoutes::new();
        routes.attach(RouteRow {
            route_id: RouteId::new("test:to-thread"),
            scope: SliceScopeId::new("test-slice", "main"),
            key: "gdc",
            category: "general",
            site: BindSite::StaticScopes(&["Normal"]),
            feature: "test-slice",
            outcome: RouteOutcome::Action {
                action: "to-thread",
                display: "test thread action",
                run: ActionFn::new(|_ctx| IntentResult::empty()),
            },
        });

        // When generating bindings into a fresh keymap.
        let mut keymap = Keymap::new();
        bind_route_rows(&routes, &mut keymap);

        // Then the full sequence resolves to the row's dynamic intent.
        let leaf = leaf_at(&keymap, &[key("g"), key("d"), key("c")], &Scope::Normal);
        assert!(
            matches!(&leaf, Some(Intent::Dynamic(d)) if d.action == "to-thread"),
            "gdc should resolve to the synthetic action, got {leaf:?}"
        );
        // And the `g` prefix derives a group labeled for the owning slice,
        // with `gd` beneath it (each proper prefix describes itself).
        let root = at_path(&keymap, &[], &Scope::Normal);
        assert!(
            root.iter()
                .any(|(k, d)| *k == key("g") && d == "test-slice"),
            "root should describe g as the test-slice group, got {root:?}"
        );
        let gd = at_path(&keymap, &[key("g")], &Scope::Normal);
        assert!(
            gd.iter().any(|(k, d)| *k == key("d") && d == "test-slice"),
            "g group should describe d as the test-slice group, got {gd:?}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn global_toggle_row_binds_in_its_own_scope_too() {
        // Given the term toggle-overlay row (a GlobalToggle whose scope
        // is `term:view`).
        let routes = KeyRoutes::new();
        routes.attach(RouteRow {
            route_id: RouteId::new("term:toggle-overlay"),
            scope: SliceScopeId::new("term", "view"),
            key: "<M-t>",
            category: "general",
            site: BindSite::GlobalToggle,
            feature: "term",
            outcome: RouteOutcome::Action {
                action: "toggle-overlay",
                display: "terminal overlay",
                run: ActionFn::new(|_ctx| IntentResult::empty()),
            },
        });

        // When generating bindings.
        let mut keymap = Keymap::new();
        bind_route_rows(&routes, &mut keymap);

        // Then the toggle binds inside the row's own dynamic scope too
        // (scopes_for_row spreads GlobalToggle rows through all row
        // scopes, own scope included — open and close are one action).
        let dynamic = Scope::Dynamic(SliceScopeId::new("term", "view"));
        let leaf = leaf_at(
            &keymap,
            &[KeyEvent {
                key: jinn_domain::Key::Char('t'),
                modifiers: jinn_domain::Modifiers {
                    ctrl: false,
                    alt: true,
                    shift: false,
                },
            }],
            &dynamic,
        );
        assert!(
            matches!(
                &leaf,
                Some(Intent::Dynamic(d))
                    if d.slice == SliceScopeId::new("term", "view")
                        && d.action == "toggle-overlay"
            ),
            "the GlobalToggle row's own scope must carry the toggle, got {leaf:?}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn own_scope_rows_bind_in_the_dynamic_scope() {
        // Given an OwnScope row in a slice's dynamic scope.
        let routes = KeyRoutes::new();
        let aliased = SliceScopeId::navigation("sidebar", "pins");
        routes.attach(RouteRow {
            route_id: RouteId::new("sidebar:move-down"),
            scope: aliased,
            key: "j",
            category: "navigation",
            site: BindSite::OwnScope,
            feature: "sidebar",
            outcome: RouteOutcome::Action {
                action: "move-down",
                display: "cursor down",
                run: ActionFn::new(|_ctx| IntentResult::empty()),
            },
        });

        // When generating bindings.
        let mut keymap = Keymap::new();
        bind_route_rows(&routes, &mut keymap);

        let key = key("j");

        // Then `j` resolves in the dynamic scope.
        let in_dynamic = leaf_at(
            &keymap,
            &[key.clone()],
            &Scope::Dynamic(SliceScopeId::navigation("sidebar", "pins")),
        );
        assert!(in_dynamic.is_some(), "row key binds in the dynamic scope");
    }

    #[rstest::rstest]
    #[test]
    fn hook_scopes_bind_the_six_editing_keys() {
        // Given a route table whose slice registers an input hook.
        let routes = KeyRoutes::new();
        let hook_scope = SliceScopeId::new("quake-bar", "bar");
        routes.register_input_hook(
            &hook_scope,
            std::sync::Arc::new(|_: &jinn_slices::route::EditIntent| None),
        );

        // When generating bindings into a fresh keymap.
        let mut keymap = Keymap::new();
        bind_route_rows(&routes, &mut keymap);

        // Then all six editing keys resolve to the kernel editing
        // intents in the hook scope (trunk parity: Backspace et al.
        // bound explicitly, not left to the char catch-all).
        let dynamic = Scope::Dynamic(hook_scope);
        let expected: [(&str, Intent); 6] = [
            ("backspace", Intent::DeleteGrapheme),
            ("delete", Intent::DeleteGraphemeForward),
            ("left", Intent::MoveCursorLeft),
            ("right", Intent::MoveCursorRight),
            ("home", Intent::MoveCursorToStart),
            ("end", Intent::MoveCursorToEnd),
        ];
        for (notation, intent) in expected {
            let leaf = leaf_at(&keymap, &[key(notation)], &dynamic);
            assert_eq!(
                leaf,
                Some(intent),
                "{notation} must bind the editing intent in hook scopes"
            );
        }
    }

    #[rstest::rstest]
    #[test]
    fn global_toggle_spreads_into_other_slices_input_hook_scopes() {
        // Given a route table with the term toggle row and another
        // slice's input-hook scope.
        let routes = KeyRoutes::new();
        let hook_scope = SliceScopeId::new("quake-bar", "bar");
        routes.register_input_hook(
            &hook_scope,
            std::sync::Arc::new(|_: &jinn_slices::route::EditIntent| None),
        );
        routes.attach(term_toggle_row());

        // When generating bindings into a fresh keymap.
        let mut keymap = Keymap::new();
        bind_route_rows(&routes, &mut keymap);

        // Then the input-hook scope resolves <M-t> (the GlobalToggle
        // spread covers other slices' dynamic scopes, input hooks
        // included — typing there still allows opening the overlay).
        let leaf = leaf_at(
            &keymap,
            &[KeyEvent {
                key: jinn_domain::Key::Char('t'),
                modifiers: jinn_domain::Modifiers {
                    ctrl: false,
                    alt: true,
                    shift: false,
                },
            }],
            &Scope::Dynamic(hook_scope),
        );
        assert!(
            matches!(
                &leaf,
                Some(Intent::Dynamic(d))
                    if d.slice == SliceScopeId::new("term", "view")
                        && d.action == "toggle-overlay"
            ),
            "input-hook scope must carry the <M-t> toggle, got {leaf:?}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn global_toggle_never_pierces_key_hook_scopes() {
        // Given a route table with the term toggle row and a key-hook
        // scope (terminal capture).
        let routes = KeyRoutes::new();
        let key_hook_scope = SliceScopeId::navigation("term", "control");
        routes.register_key_hook(&key_hook_scope, std::sync::Arc::new(|_: &KeyEvent| None));
        routes.attach(term_toggle_row());

        // When generating bindings and pressing <M-t> in the capture
        // scope.
        let mut keymap = Keymap::new();
        bind_route_rows(&routes, &mut keymap);
        let mut wk = WhichKeyInstance::new(keymap, Scope::Dynamic(key_hook_scope));
        let intent = wk.handle_key(KeyEvent {
            key: jinn_domain::Key::Char('t'),
            modifiers: jinn_domain::Modifiers {
                ctrl: false,
                alt: true,
                shift: false,
            },
        });

        // Then nothing resolves (capture hermeticity: no global toggle
        // pierces the key-hook scope; the hook itself declines the key).
        assert!(
            intent.is_none(),
            "key-hook scope must stay hermetic, got {intent:?}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn key_hook_catchall_resolves_on_a_navigation_scope() {
        // Given a key hook registered under a navigation scope (the
        // regression for the string-roundtrip blocker: the hook must
        // resolve by its exact scope id, captures_input intact).
        let routes = KeyRoutes::new();
        let key_hook_scope = SliceScopeId::navigation("term", "control");
        let hook_target = key_hook_scope.clone();
        routes.register_key_hook(
            &key_hook_scope,
            std::sync::Arc::new(move |key: &KeyEvent| {
                (key.key == jinn_domain::Key::Char('x')).then(|| {
                    jinn_slices::DynamicIntent::new(hook_target.clone(), "send-key", "send key")
                })
            }),
        );

        // When generating bindings and pressing an unbound key in the
        // hook's scope.
        let mut keymap = Keymap::new();
        bind_route_rows(&routes, &mut keymap);
        let mut wk = WhichKeyInstance::new(
            keymap,
            Scope::Dynamic(SliceScopeId::navigation("term", "control")),
        );
        let intent = wk.handle_key(key("x"));

        // Then the hook fires (the catch-all was synthesized on the
        // exact scope id).
        assert!(
            matches!(
                &intent,
                Some(Intent::Dynamic(d))
                    if d.slice == SliceScopeId::navigation("term", "control")
                        && d.action == "send-key"
            ),
            "key hook must resolve on its navigation scope, got {intent:?}"
        );
    }
}

#[cfg(test)]
mod picker_spec_row_tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        reason = "test code, panics are acceptable"
    )]

    use super::bind_picker_spec_rows;
    use crate::app::WhichKeyInstance;
    use crate::keymap::init;
    use crate::scope::Scope;
    use jinn_domain::{Key, KeyEvent, Modifiers};

    /// A persona stub spec registers no binds; the skill stub spec
    /// registers none either — so `bind_picker_spec_rows` must be a
    /// no-op for the pilot stubs and must land rows once specs declare
    /// binds. This test pins the mechanism with a throwaway spec.
    #[rstest::rstest]
    #[test]
    fn spec_rows_land_in_the_picker_scope_as_picker_actions() {
        // Given a registry with a spec that declares one general and one
        // navigation bind, under a throwaway id mapped to a static scope.
        let mut registry = jinn_picker::PickerRegistry::new();
        registry.register(
            jinn_picker::PickerSpec::<jinn_domain::feat::picker::skill_spec::SkillEntry>::new(
                jinn_picker::PickerId::new("skill"),
            )
            .bind("<tab>", "toggle", |_| jinn_picker::PickerOutcome::empty())
            .bind_navigation("<c-u>", "page up", |_| jinn_picker::PickerOutcome::empty()),
        );

        // When binding the spec rows into a keymap.
        let mut keymap = init();
        bind_picker_spec_rows(&registry, &mut keymap);
        let mut wk = WhichKeyInstance::new(keymap, Scope::PickerSkill);

        // Then Tab resolves to the spec's picker action.
        let tab = KeyEvent {
            key: Key::Tab,
            modifiers: Modifiers::none(),
        };
        let intent = wk.handle_key(tab);
        assert!(
            matches!(
                &intent,
                Some(jinn_domain::Intent::PickerAction { picker, action })
                    if picker == "skill" && action == "<tab>"
            ),
            "<Tab> must land as the spec's picker action; got {intent:?}",
        );
    }

    #[rstest::rstest]
    #[test]
    fn spec_navigation_rows_resolve_without_shadowing_base_binds() {
        // Given a registry with a navigation-hinted bind.
        let mut registry = jinn_picker::PickerRegistry::new();
        registry.register(
            jinn_picker::PickerSpec::<jinn_domain::feat::picker::skill_spec::SkillEntry>::new(
                jinn_picker::PickerId::new("skill"),
            )
            .bind_navigation("<c-u>", "page up", |_| jinn_picker::PickerOutcome::empty()),
        );

        // When binding spec rows over the base keymap.
        let mut keymap = init();
        bind_picker_spec_rows(&registry, &mut keymap);
        let mut wk = WhichKeyInstance::new(keymap, Scope::PickerSkill);

        // Then the <c-u> binding resolves to the spec action.
        let c_u = KeyEvent {
            key: Key::Char('u'),
            modifiers: Modifiers::ctrl(),
        };
        let intent = wk.handle_key(c_u);
        assert!(
            matches!(
                &intent,
                Some(jinn_domain::Intent::PickerAction { picker, action })
                    if picker == "skill" && action == "<c-u>"
            ),
            "<c-u> must land as the spec's navigation action; got {intent:?}",
        );
    }
}

#[cfg(test)]
mod real_registry_spec_rows {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        reason = "test code, panics are acceptable"
    )]

    use super::bind_picker_spec_rows;
    use crate::app::WhichKeyInstance;
    use crate::keymap::init;
    use crate::scope::Scope;
    use jinn_domain::{Key, KeyEvent, Modifiers};

    #[rstest::rstest]
    #[test]
    fn project_spec_rows_resolve_in_its_scope() {
        // Given the real domain registry (whose project spec declares
        // <c-enter>/<c-n>/<c-d> rows) bound into a fresh keymap.
        let registry = jinn_domain::feat::picker::registry::build_picker_registry();
        let mut keymap = init();
        bind_picker_spec_rows(&registry, &mut keymap);
        let mut wk = WhichKeyInstance::new(keymap, Scope::PickerProject);

        // When pressing each of the spec's keys.
        let c_enter = KeyEvent {
            key: Key::Enter,
            modifiers: Modifiers::ctrl(),
        };
        let c_n = KeyEvent {
            key: Key::Char('n'),
            modifiers: Modifiers::ctrl(),
        };
        let c_d = KeyEvent {
            key: Key::Char('d'),
            modifiers: Modifiers::ctrl(),
        };
        let enter_intent = wk.handle_key(c_enter);
        let n_intent = wk.handle_key(c_n);
        let d_intent = wk.handle_key(c_d);

        // Then each resolves to the project spec's action.
        let expected = [
            ("<c-enter>", enter_intent),
            ("<c-n>", n_intent),
            ("<c-d>", d_intent),
        ];
        for (notation, intent) in expected {
            assert!(
                matches!(
                    &intent,
                    Some(jinn_domain::Intent::PickerAction { picker, action })
                        if picker == "project" && action == notation
                ),
                "{notation} must land as the project spec's action; got {intent:?}",
            );
        }
    }

    #[rstest::rstest]
    #[test]
    fn endpoint_spec_refresh_row_resolves_in_its_scope() {
        // Given the real domain registry (whose endpoint spec declares a <c-r>
        // refresh row) bound into a fresh keymap.
        let registry = jinn_domain::feat::picker::registry::build_picker_registry();
        let mut keymap = init();
        bind_picker_spec_rows(&registry, &mut keymap);
        let mut wk = WhichKeyInstance::new(keymap, Scope::PickerEndpoint);

        // When pressing Ctrl+R.
        let c_r = KeyEvent {
            key: Key::Char('r'),
            modifiers: Modifiers::ctrl(),
        };
        let intent = wk.handle_key(c_r);

        // Then it resolves to the endpoint spec's refresh picker action.
        assert!(
            matches!(
                &intent,
                Some(jinn_domain::Intent::PickerAction { picker, action })
                    if picker == "endpoint" && action == "<c-r>"
            ),
            "<c-r> must land as the endpoint spec's refresh action; got {intent:?}",
        );
    }

    #[rstest::rstest]
    #[rstest::rstest]
    #[test]
    fn provider_spec_rows_resolve_in_their_scope() {
        // Given the real domain registry (whose provider spec declares
        // <tab>/<c-a>/<c-r> rows) bound into a fresh keymap.
        let registry = jinn_domain::feat::picker::registry::build_picker_registry();
        let mut keymap = init();
        bind_picker_spec_rows(&registry, &mut keymap);
        let mut wk = WhichKeyInstance::new(keymap, Scope::PickerProvider);

        // When pressing each of the spec's keys.
        let tab = KeyEvent {
            key: Key::Tab,
            modifiers: Modifiers::none(),
        };
        let c_a = KeyEvent {
            key: Key::Char('a'),
            modifiers: Modifiers::ctrl(),
        };
        let tab_intent = wk.handle_key(tab);
        let a_intent = wk.handle_key(c_a);

        // Then both resolve to the provider spec's picker actions.
        assert!(
            matches!(
                &tab_intent,
                Some(jinn_domain::Intent::PickerAction { picker, action })
                    if picker == "provider" && action == "<tab>"
            ),
            "<Tab> must land as the provider spec's toggle; got {tab_intent:?}",
        );
        assert!(
            matches!(
                &a_intent,
                Some(jinn_domain::Intent::PickerAction { picker, action })
                    if picker == "provider" && action == "<c-a>"
            ),
            "<c-a> must land as the provider spec's alloy toggle; got {a_intent:?}",
        );
    }
}
