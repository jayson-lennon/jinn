//! Cwd popup route actions and input hook.
//!
//! Open seeds the popup from the active session's cwd and pushes the popup's
//! dynamic scope; confirm resolves the typed path with the shared resolver
//! and returns a publish closure for the kernel's `SetSessionCwd` through
//! [`SliceActionState`]; leave discards. Editing lands in the
//! cell through a route-table input hook — the same pattern as the rename
//! popup.

use jinn_common::shorten_path;
use jinn_cwd_msg::CwdInputState;
use jinn_cwd_msg::CwdResolution;
use jinn_cwd_msg::SuggestCwdCompletion;
use jinn_cwd_msg::resolve_cwd_input;
use jinn_cwd_msg::resolve_parent_dir;
use jinn_cwd_msg::split_at_cursor;
use jinn_slices::RouteResult as IntentResult;
use jinn_slices::SliceScopeId;
use jinn_slices::cell::TypedCell;
use jinn_slices::route::{
    ActionCtx, ActionFn, BindSite, EditIntent, InputHook, PublishClosure, RouteId, RouteRow,
    ScopeSignal,
};
use std::sync::Arc;

/// The cwd popup's dynamic scope (input-capturing).
#[must_use]
pub fn cwd_scope() -> SliceScopeId {
    SliceScopeId::new("cwd", "input")
}

/// The state the actions and hook touch: the popup's single cell.
type CwdCell = TypedCell<CwdInputState>;

/// Wraps a synchronous cwd action (which also touches the cell) into an
/// [`ActionFn`].
fn action<F>(cell: &CwdCell, f: F) -> ActionFn
where
    F: Fn(&mut ActionCtx<'_>, &CwdCell) -> IntentResult + Send + Sync + 'static,
{
    let cell = cell.clone();
    ActionFn::new(move |mut ctx| f(&mut ctx, &cell))
}

/// Builds one `Action` row binding `key` in the popup scope.
fn row(
    action_name: &'static str,
    key: &'static str,
    category: &'static str,
    display: &'static str,
    run: ActionFn,
) -> RouteRow {
    RouteRow {
        // Route ids are only diagnostics/composition keys here; actions
        // dispatch by (scope, action).
        route_id: RouteId::new(action_name),
        scope: cwd_scope(),
        key,
        category,
        site: BindSite::OwnScope,
        feature: "cwd",
        outcome: jinn_slices::route::RouteOutcome::Action {
            action: action_name,
            display,
            run,
        },
    }
}

/// Attaches the popup's route rows: confirm (`<enter>`), leave (`<esc>`),
/// and Ctrl-C clear/leave. The opener row (`<leader>cd` in Normal) is
/// attached here too — an `OwnScope` binding on the *base* scope is not
/// expressible, so it uses the `StaticScopes` site on `["Normal"]` and pushes
/// the popup scope itself.
pub fn attach_cwd_rows(routes: &jinn_slices::KeyRoutes, cell: &CwdCell) {
    // Opener: seed + push scope. Site: static Normal scope (the popup scope
    // does not exist yet when the key is pressed).
    routes.attach(RouteRow {
        route_id: RouteId::new("cwd:open"),
        scope: cwd_scope(),
        key: "<leader>cd",
        category: "session",
        site: BindSite::StaticScopes(&["Normal"]),
        feature: "cwd",
        outcome: jinn_slices::route::RouteOutcome::Action {
            action: "open-cwd-input",
            display: "change session cwd (type a path)",
            run: action(cell, |ctx, cell| open_cwd_input(ctx.state, cell)),
        },
    });

    routes.attach(row(
        "confirm-cwd-input",
        "<enter>",
        "session",
        "resolve the path and change the session cwd",
        action(cell, confirm_cwd_input),
    ));
    routes.attach(row(
        "leave-cwd-input",
        "<esc>",
        "session",
        "cancel without changing the cwd",
        action(cell, |_ctx, cell| {
            leave_cwd_input(cell);
            IntentResult::empty().with_scope_signal(ScopeSignal::PopIf(cwd_scope()))
        }),
    ));
    routes.attach(row(
        "clear-or-leave-cwd-input",
        "<c-c>",
        "session",
        "clear the path, or leave when already empty",
        action(cell, |_ctx, cell| clear_or_leave_cwd_input(cell)),
    ));
    routes.attach(row(
        "accept-cwd-ghost",
        "<tab>",
        "session",
        "accept the ghosted completion",
        action(cell, |_ctx, cell| accept_cwd_ghost(cell)),
    ));
}

/// Registers the popup's editing hook: every editing intent lands in the
/// cell and mints a fresh suggestion request; non-editing intents pass
/// through (`None`).
///
/// The hook is the popup's only synchronous writer besides its actions:
/// each edit clears the ghost, bumps the staleness id, and returns a
/// `SuggestCwdCompletion` for the new input. No disk I/O happens here —
/// the request names one directory and the suggest actor reads it on a
/// blocking thread.
pub fn register_cwd_input_hook(routes: &jinn_slices::KeyRoutes, cell: &CwdCell) {
    let cell = cell.clone();
    let hook: InputHook = Arc::new(move |intent: &EditIntent| {
        let cell = cell.clone();
        let result = match intent {
            EditIntent::InsertChar(ch) => {
                cell.update(|s| s.text.insert_char(*ch));
                IntentResult::empty()
            }
            EditIntent::DeleteBackward => {
                cell.update(|s| s.text.delete());
                IntentResult::empty()
            }
            EditIntent::DeleteForward => {
                cell.update(|s| s.text.delete_forward());
                IntentResult::empty()
            }
            EditIntent::CursorLeft => {
                cell.update(|s| s.text.cursor_left());
                IntentResult::empty()
            }
            EditIntent::CursorRight => {
                cell.update(|s| s.text.cursor_right());
                IntentResult::empty()
            }
            EditIntent::CursorHome => {
                cell.update(|s| s.text.cursor_home());
                IntentResult::empty()
            }
            EditIntent::CursorEnd => {
                cell.update(|s| s.text.cursor_end());
                IntentResult::empty()
            }
            EditIntent::Paste(text) => {
                cell.update(|s| s.text.paste(text));
                IntentResult::empty()
            }
        };
        Some(result.with_message(request_suggestion(&cell)))
    });
    routes.register_input_hook(&cwd_scope(), hook);
}

/// Mints the suggestion request for the cell's current input, bumping the
/// staleness id and clearing the ghost in the same update.
///
/// The request's `parent_dir` is the lexical parent of the segment before
/// the cursor, resolved against the popup's open-time base — `None` base
/// (popup never opened through the opener) resolves against the home
/// fallback `resolve_parent_dir` already agrees on with the confirm
/// resolver; the guard still advances, keeping the protocol total. The
/// home for tilde expansion comes from the `-msg` crate's `tilde_home()`
/// (an env read, the same one the confirm resolver uses on this thread).
fn request_suggestion(cell: &CwdCell) -> SuggestCwdCompletion {
    cell.update(request_suggestion_from_state)
}

/// Opens the popup: seeds the input with the active session's cwd
/// (tilde-compressed, cursor at end) and pushes the popup scope.
fn open_cwd_input(state: &mut dyn jinn_slices::SliceActionState, cell: &CwdCell) -> IntentResult {
    let seeded = shorten_path(&state.active_session_cwd());
    let base_dir = Some(state.active_session_cwd());
    cell.update(|s| {
        let mut text = jinn_slices::LineInput::new();
        text.set(seeded);
        *s = CwdInputState {
            text,
            base_dir,
            ..CwdInputState::default()
        };
    });
    IntentResult {
        messages: Vec::new(),
        message_names: Vec::new(),
        scope_signal: Some(ScopeSignal::Push(cwd_scope())),
    }
}

/// Confirms the popup: resolves the typed path against the active session
/// cwd; on success returns `SetSessionCwd` and pops
/// the scope, clearing the cell. On failure stays open (the render footer
/// shows the inline error) and consumes the key.
fn confirm_cwd_input(ctx: &mut ActionCtx<'_>, cell: &CwdCell) -> IntentResult {
    let raw = cell.read().text.input.trim().to_owned();
    let current_cwd = ctx.state.active_session_cwd();
    match resolve_cwd_input(&raw, &current_cwd) {
        CwdResolution::Ok(path) => {
            let session_id = ctx.state.active_session_id();
            let publish: PublishClosure = ctx.state.publish_session_cwd(session_id, path);
            leave_cwd_input(cell);
            IntentResult {
                messages: vec![publish],
                message_names: vec!["SetSessionCwd"],
                scope_signal: Some(ScopeSignal::PopIf(cwd_scope())),
            }
        }
        CwdResolution::Empty | CwdResolution::NotADir(_) => IntentResult::empty(),
    }
}

/// Cancels the popup: clears the cell (the scope pop rides the route
/// result's `PopIf` when confirming; leave is dispatched from the popup
/// scope, so it pops explicitly).
fn leave_cwd_input(cell: &CwdCell) {
    cell.update(|s| *s = CwdInputState::default());
}

/// Clears nonempty input while remaining open, or leaves when already empty.
fn clear_or_leave_cwd_input(cell: &CwdCell) -> IntentResult {
    let had_text = !cell.read().text.input.is_empty();
    leave_cwd_input(cell);
    if had_text {
        IntentResult::empty()
    } else {
        IntentResult::empty().with_scope_signal(ScopeSignal::PopIf(cwd_scope()))
    }
}

/// Accepts the ghost at the cursor: prefix-extension appends the remainder;
/// type-over replaces the whole segment before the cursor with the winner.
/// The separator is appended in both cases, the cursor lands after it, and
/// a fresh suggestion request is minted for the new input (so repeated Tab
/// walks down the tree). With no ghost, Tab is consumed and nothing changes.
fn accept_cwd_ghost(cell: &CwdCell) -> IntentResult {
    let winner = cell.read().ghost.clone();
    let Some(winner) = winner else {
        return IntentResult::empty();
    };
    let request = cell.update(|s| {
        let before_cursor = &s
            .text
            .input
            .get(..s.text.cursor_pos)
            .unwrap_or_default()
            .to_owned();
        let after_cursor = s
            .text
            .input
            .get(s.text.cursor_pos..)
            .unwrap_or_default()
            .to_owned();
        let (parent_text, segment) = split_at_cursor(before_cursor);
        let remainder = crate::suggest::ghost_remainder(segment, &winner);
        let filled = if remainder.len() < winner.len() {
            // Prefix extension (the remainder is shorter than the whole
            // name): keep the typed text, append the remainder + separator.
            let mut text = s.text.clone();
            let suffix = format!("{remainder}{}", std::path::MAIN_SEPARATOR);
            for ch in suffix.chars() {
                text.insert_char(ch);
            }
            text
        } else {
            // Type-over (the remainder is the whole name): replace the
            // segment before the cursor with the winner. The separator is
            // appended unless the text after the cursor already begins
            // with one (the old separator serves; appending would double it).
            let appended_sep = if after_cursor
                .chars()
                .next()
                .is_some_and(std::path::is_separator)
            {
                String::new()
            } else {
                std::path::MAIN_SEPARATOR.to_string()
            };
            let mut text = jinn_slices::LineInput::new();
            text.set(format!("{parent_text}{winner}{appended_sep}"));
            text.input.push_str(&after_cursor);
            text.cursor_pos = text.input.len() - after_cursor.len();
            text
        };
        s.text = filled;
        request_suggestion_from_state(s)
    });
    IntentResult::empty().with_message(request)
}

/// The state-taking half of [`request_suggestion`], for callers already
/// holding the cell's write lock.
fn request_suggestion_from_state(s: &mut CwdInputState) -> SuggestCwdCompletion {
    s.ghost = None;
    s.expected_request_id = s.expected_request_id.wrapping_add(1);
    let before_cursor = s.text.input.get(..s.text.cursor_pos).unwrap_or_default();
    let (parent_text, segment) = split_at_cursor(before_cursor);
    let segment = segment.to_owned();
    let base = s.base_dir.clone().unwrap_or_else(jinn_cwd_msg::tilde_home);
    let parent_dir = resolve_parent_dir(
        parent_text,
        &base.as_marked_path(),
        &jinn_cwd_msg::tilde_home(),
    );
    SuggestCwdCompletion {
        parent_dir,
        segment,
        request_id: s.expected_request_id,
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::*;
    use crate::cwds_slot;
    use jinn_slices::KeyRoutes;
    use jinn_slices::SliceActionState;
    use jinn_slices::route::ActionCtx;
    use jinn_testutil::volume_root;
    use marked_path::{Absolute, MarkedPathAccess, MarkedPathBuf};

    /// A minimal kernel-free [`SliceActionState`] double.
    #[derive(Default)]
    struct FakeState {
        cwd: Option<MarkedPathBuf<Absolute>>,
        session_id: jinn_core_types::SessionId,
    }

    impl SliceActionState for FakeState {
        fn active_session_title(&self) -> Option<String> {
            None
        }

        fn active_session_id(&self) -> jinn_core_types::SessionId {
            self.session_id.clone()
        }

        fn push_session_error(&mut self, _message: &str) {}

        fn active_session_cwd(&self) -> MarkedPathBuf<Absolute> {
            self.cwd.clone().unwrap_or_else(volume_root)
        }

        fn publish_session_cwd(
            &self,
            _session_id: jinn_core_types::SessionId,
            _cwd: MarkedPathBuf<Absolute>,
        ) -> PublishClosure {
            Box::new(|_bus| {})
        }
    }

    fn ctx<'a>(state: &'a mut FakeState, slices: &'a jinn_slices::Slices) -> ActionCtx<'a> {
        ActionCtx {
            state,
            slices,
            config: jinn_slices::empty_config_layer(),
            key_bytes: Vec::new(),
        }
    }

    /// Mints the popup cell in a fresh registry; returns both (the cell is a
    /// clone handle, so they can be used concurrently).
    fn cell() -> (jinn_slices::Slices, CwdCell) {
        let slices = jinn_slices::Slices::new();
        let cell = slices
            .register(cwds_slot(), CwdInputState::default())
            .expect("fresh test registry has the slot free");
        (slices, cell)
    }

    #[rstest::rstest]
    fn open_seeds_input_from_session_cwd_and_pushes_scope() {
        // Given a fake state whose session cwd is an absolute path.
        let dir = jinn_testutil::tempdir();
        let cwd = dir
            .path
            .join("some-project")
            .expect("temp root joins a bare name");
        let mut state = FakeState {
            cwd: Some(cwd),
            ..FakeState::default()
        };
        let (slices, cell) = cell();
        let cx = ctx(&mut state, &slices);

        // When the opener action runs.
        let result = open_cwd_input(cx.state, &cell);

        // Then the cell is seeded with the cwd and the result pushes the
        // popup scope. The seeded form is `shorten_path`'s — the same
        // function production uses — so the expectation holds whatever the
        // platform's home directory is. (A temp root is under `$HOME` on
        // Windows, so the seeded path is tilde-compressed there and not on
        // Unix.)
        let expected = shorten_path(state.cwd.as_ref().expect("cwd set"));
        assert_eq!(cell.read().text.input, expected);
        assert_eq!(cell.read().text.cursor_pos, expected.len());
        assert!(matches!(result.scope_signal, Some(ScopeSignal::Push(_))));
    }

    #[rstest::rstest]
    fn open_seeds_tilde_compressed_path_when_cwd_under_home() {
        // Given a session cwd under $HOME.
        let home = dirs::home_dir().expect("home dir exists");
        let mut state = FakeState {
            cwd: Some(
                MarkedPathBuf::<Absolute>::new(home.join("projects/my-app"))
                    .expect("home-joined path is absolute"),
            ),
            ..FakeState::default()
        };
        let (slices, cell) = cell();
        let cx = ctx(&mut state, &slices);

        // When the opener action runs.
        open_cwd_input(cx.state, &cell);

        // Then the seeded input is the tilde-compressed form, with the
        // platform's native separators (PathBuf-built expectation).
        let expected = std::path::Path::new("~")
            .join("projects")
            .join("my-app")
            .display()
            .to_string();
        assert_eq!(cell.read().text.input, expected);
    }

    #[rstest::rstest]
    fn confirm_valid_relative_dir_publishes_and_pops() {
        // Given a real tempdir next to the session cwd, with the popup seeded
        // with the tempdir's basename.
        let temp = jinn_testutil::tempdir();
        let target = &temp.path;
        let mut state = FakeState {
            cwd: Some(target.parent().expect("parent").to_owned()),
            ..FakeState::default()
        };
        let (slices, cell) = cell();
        cell.update(|s| {
            s.text.set(
                target
                    .file_name()
                    .expect("name")
                    .to_string_lossy()
                    .to_string(),
            );
        });
        let mut cx = ctx(&mut state, &slices);

        // When confirming.
        let result = confirm_cwd_input(&mut cx, &cell);

        // Then one SetSessionCwd is published and the scope pops.
        assert_eq!(result.message_names, vec!["SetSessionCwd"]);
        assert_eq!(result.messages.len(), 1);
        assert!(matches!(result.scope_signal, Some(ScopeSignal::PopIf(_))));
        // And the cell is cleared.
        assert_eq!(cell.read().text.input, "");
    }

    #[rstest::rstest]
    fn confirm_nonexistent_path_stays_open_unchanged() {
        // Given a popup seeded with a path that does not exist.
        let dir = jinn_testutil::tempdir();
        let mut state = FakeState {
            cwd: Some(dir.path.clone()),
            ..FakeState::default()
        };
        let (slices, cell) = cell();
        cell.update(|s| s.text.set("/this/does/not/exist".to_owned()));
        let mut cx = ctx(&mut state, &slices);

        // When confirming.
        let result = confirm_cwd_input(&mut cx, &cell);

        // Then nothing is published, no scope signal fires, and the input
        // stays for the footer to show the inline error.
        assert!(result.messages.is_empty());
        assert!(result.scope_signal.is_none());
        assert_eq!(cell.read().text.input, "/this/does/not/exist");
    }

    #[rstest::rstest]
    fn confirm_empty_input_is_noop() {
        // Given a popup with empty input.
        let dir = jinn_testutil::tempdir();
        let mut state = FakeState {
            cwd: Some(dir.path.clone()),
            ..FakeState::default()
        };
        let (slices, cell) = cell();
        let mut cx = ctx(&mut state, &slices);

        // When confirming.
        let result = confirm_cwd_input(&mut cx, &cell);

        // Then nothing is published and the popup stays open.
        assert!(result.messages.is_empty());
        assert!(result.scope_signal.is_none());
    }

    #[rstest::rstest]
    fn clear_or_leave_with_text_clears_and_stays_open() {
        // Given a popup with typed text.
        let (_slices, cell) = cell();
        cell.update(|s| s.text.set("/some/path".to_owned()));

        // When Ctrl-C is requested.
        let result = clear_or_leave_cwd_input(&cell);

        // Then the input clears without leaving the popup.
        assert!(result.scope_signal.is_none());
        assert_eq!(cell.read().text.input, "");
    }

    #[rstest::rstest]
    fn clear_or_leave_with_empty_text_clears_and_leaves() {
        // Given an empty popup.
        let (_slices, cell) = cell();

        // When Ctrl-C is requested.
        let result = clear_or_leave_cwd_input(&cell);

        // Then the popup requests its own conditional pop.
        assert_eq!(result.scope_signal, Some(ScopeSignal::PopIf(cwd_scope())));
        assert_eq!(cell.read().text.input, "");
    }

    #[rstest::rstest]
    fn leave_clears_the_cell() {
        // Given a popup with typed text.
        let _state = FakeState::default();
        let (_slices, cell) = cell();
        cell.update(|s| s.text.set("/some/path".to_owned()));

        // When leaving.
        leave_cwd_input(&cell);

        // Then the cell is cleared.
        assert_eq!(cell.read().text.input, "");
    }

    #[rstest::rstest]
    fn rows_bind_open_confirm_leave_and_clear() {
        // Given a route table with the cwd rows attached.
        let routes = KeyRoutes::new();
        let (_slices, cell) = cell();
        attach_cwd_rows(&routes, &cell);

        // When enumerating the rows.
        let rows = routes.rows();

        // Then the opener binds <leader>cd on the static Normal scopes and
        // confirm/leave/clear bind in the popup's own scope.
        let open = rows
            .iter()
            .find(|row| row.route_id.as_str() == "cwd:open")
            .expect("opener row");
        assert_eq!(open.key, "<leader>cd");
        assert!(matches!(open.site, BindSite::StaticScopes(_)));
        let confirm = rows
            .iter()
            .find(|row| row.route_id.as_str() == "confirm-cwd-input")
            .expect("confirm row");
        assert_eq!(confirm.key, "<enter>");
        assert!(matches!(confirm.site, BindSite::OwnScope));
        let leave = rows
            .iter()
            .find(|row| row.route_id.as_str() == "leave-cwd-input")
            .expect("leave row");
        assert_eq!(leave.key, "<esc>");
        let clear = rows
            .iter()
            .find(|row| row.route_id.as_str() == "clear-or-leave-cwd-input")
            .expect("clear row");
        assert_eq!(clear.key, "<c-c>");
    }

    #[rstest::rstest]
    fn input_hook_edits_the_cell() {
        // Given a route table with the input hook registered.
        let routes = KeyRoutes::new();
        let (_slices, cell) = cell();
        register_cwd_input_hook(&routes, &cell);
        let hook = routes
            .input_hook(&cwd_scope())
            .expect("hook registered for the cwd scope");

        // When editing intents flow through the hook.
        let _ = hook(&EditIntent::InsertChar('a'));
        let _ = hook(&EditIntent::InsertChar('b'));
        let _ = hook(&EditIntent::CursorLeft);
        let _ = hook(&EditIntent::InsertChar('x'));

        // Then the cell reflects the edits (xab with cursor before b).
        assert_eq!(cell.read().text.input, "axb");
    }

    #[rstest::rstest]
    fn input_hook_moves_cursor_home_and_consumes_intent() {
        // Given a route table with the input hook registered and text.
        let routes = KeyRoutes::new();
        let (_slices, cell) = cell();
        cell.update(|s| s.text.set("hello".to_owned()));
        register_cwd_input_hook(&routes, &cell);
        let hook = routes
            .input_hook(&cwd_scope())
            .expect("hook registered for the cwd scope");

        // When moving the cursor home.
        let result = hook(&EditIntent::CursorHome);

        // Then the intent is consumed and the cursor reaches the start.
        assert!(result.is_some());
        assert_eq!(cell.read().text.cursor_pos, 0);
    }

    #[rstest::rstest]
    fn accept_appends_the_remainder_and_requests_a_fresh_suggestion() {
        // Given a popup whose typed segment prefixes the ghost.
        let dir = jinn_testutil::tempdir();
        let (_slices, cell) = cell();
        cell.update(|s| {
            s.base_dir = Some(dir.path.clone());
            s.text.set("~/co".to_owned());
            s.ghost = Some("completed-projects".to_owned());
            s.expected_request_id = 3;
        });

        // When Tab accepts the ghost.
        let result = accept_cwd_ghost(&cell);

        // Then the input extends to winner + separator with the cursor
        // after it, and a fresh suggestion request rides the result.
        assert_eq!(
            cell.read().text.input,
            format!("~/completed-projects{}", std::path::MAIN_SEPARATOR)
        );
        assert_eq!(cell.read().text.cursor_pos, cell.read().text.input.len());
        assert_eq!(result.message_names, vec!["SuggestCwdCompletion"]);
        assert_eq!(cell.read().expected_request_id, 4);
    }

    #[rstest::rstest]
    fn accept_type_over_replaces_the_segment_before_the_cursor() {
        // Given a popup whose typed segment is a typo of the ghost, with
        // text after the cursor.
        let dir = jinn_testutil::tempdir();
        let (_slices, cell) = cell();
        cell.update(|s| {
            s.base_dir = Some(dir.path.clone());
            s.text.set("~/mkn/more".to_owned());
            s.text.cursor_pos = "~/mkn".len();
            s.ghost = Some("mkdir".to_owned());
            s.expected_request_id = 3;
        });

        // When Tab accepts the ghost.
        let _ = accept_cwd_ghost(&cell);

        // Then the segment is replaced wholesale; the old separator after
        // the cursor serves (no doubling), the rest is preserved, and the
        // cursor sits right after the winner.
        assert_eq!(cell.read().text.input, "~/mkdir/more");
        assert_eq!(cell.read().text.cursor_pos, "~/mkdir".len());
    }

    #[rstest::rstest]
    fn accept_after_a_trailing_separator_fills_the_first_child_for_the_walk() {
        // Given a popup whose input ends in a separator with a first-child
        // ghost (the walk shape).
        let dir = jinn_testutil::tempdir();
        let (_slices, cell) = cell();
        cell.update(|s| {
            s.base_dir = Some(dir.path.clone());
            s.text.set("~/".to_owned());
            s.ghost = Some("projects".to_owned());
            s.expected_request_id = 3;
        });

        // When Tab accepts the ghost.
        let _ = accept_cwd_ghost(&cell);

        // Then the child name is appended with its own separator — ready
        // for the next Tab to walk into it.
        assert_eq!(
            cell.read().text.input,
            format!("~/projects{}", std::path::MAIN_SEPARATOR)
        );
    }

    #[rstest::rstest]
    fn accept_without_a_ghost_is_a_consumed_noop() {
        // Given a popup with no ghost.
        let (_slices, cell) = cell();
        cell.update(|s| {
            s.text.set("~/co".to_owned());
            s.expected_request_id = 3;
        });

        // When Tab is pressed.
        let result = accept_cwd_ghost(&cell);

        // Then nothing changes and nothing publishes — but the key is
        // still consumed (an empty, signal-free result).
        assert_eq!(cell.read().text.input, "~/co");
        assert!(result.messages.is_empty());
        assert!(result.scope_signal.is_none());
    }

    #[rstest::rstest]
    fn input_hook_clears_the_ghost_and_bumps_the_request_id_per_edit() {
        // Given a route table with the hook registered and a cell holding
        // a ghost and a prior request id.
        let dir = jinn_testutil::tempdir();
        let routes = KeyRoutes::new();
        let (_slices, cell) = cell();
        cell.update(|s| {
            s.ghost = Some("alpha".to_owned());
            s.expected_request_id = 7;
            s.base_dir = Some(dir.path.clone());
            s.text.set("~/al".to_owned());
        });
        register_cwd_input_hook(&routes, &cell);
        let hook = routes
            .input_hook(&cwd_scope())
            .expect("hook registered for the cwd scope");

        // When an edit flows through the hook.
        let _ = hook(&EditIntent::InsertChar('p'));

        // Then the ghost is cleared and the id bumped.
        let state = cell.read();
        assert_eq!(state.ghost, None);
        assert_eq!(state.expected_request_id, 8);
    }

    #[rstest::rstest]
    fn input_hook_mints_a_suggestion_request_for_the_new_input() {
        // Given the hook registered and the popup stamped with the base.
        let dir = jinn_testutil::tempdir();
        let routes = KeyRoutes::new();
        let (_slices, cell) = cell();
        cell.update(|s| {
            s.base_dir = Some(dir.path.clone());
            s.text.set("~/al".to_owned());
            s.expected_request_id = 7;
        });
        register_cwd_input_hook(&routes, &cell);
        let hook = routes
            .input_hook(&cwd_scope())
            .expect("hook registered for the cwd scope");

        // When an edit flows through the hook.
        let result = hook(&EditIntent::InsertChar('p')).expect("editing intents are consumed");

        // Then one SuggestCwdCompletion rides the result, carrying the
        // bumped id, the resolved parent (~/ under the temp home region),
        // and the segment before the cursor.
        assert_eq!(result.message_names, vec!["SuggestCwdCompletion"]);
        assert_eq!(result.messages.len(), 1);
    }

    #[rstest::rstest]
    fn input_hook_moves_cursor_end_and_consumes_intent() {
        // Given a route table with the input hook registered and text.
        let routes = KeyRoutes::new();
        let (_slices, cell) = cell();
        cell.update(|s| s.text.set("héllo".to_owned()));
        cell.update(|s| s.text.cursor_home());
        register_cwd_input_hook(&routes, &cell);
        let hook = routes
            .input_hook(&cwd_scope())
            .expect("hook registered for the cwd scope");

        // When moving the cursor to the end.
        let result = hook(&EditIntent::CursorEnd);

        // Then the intent is consumed and the cursor reaches the end.
        assert!(result.is_some());
        assert_eq!(cell.read().text.cursor_pos, "héllo".len());
    }
}
