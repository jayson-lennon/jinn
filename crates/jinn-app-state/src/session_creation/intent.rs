//! Session lifecycle setup and teardown operations.
//!
//! These bridge the Intent-driven architecture with the session lifecycle
//! system. An intent handler calls these directly; they mutate
//! [`AppState`](crate::app_state::AppState) and return the messages for the
//! actor system.
//!
//! They live here, beside the state they mutate, because that is what they
//! are: synchronous state mutation that ends in a published message, the same
//! shape the intent handler has everywhere else. The lifecycle slice owns the
//! actors that act on those messages; nothing here depends on that slice or
//! on the kernel, so the eleven call sites scattered across picker routes and
//! the discord backend reach these without either one being inverted.

use crate::app_state::AppState;
use jinn_config::ConfigLayer;
use jinn_core_types::{SessionId, SessionProfile};
use jinn_preferences_config::schemas::SessionLifecycle;
use jinn_session_lifecycle_msg::CommandTemplate;
use jinn_session_lifecycle_msg::command::{RunSessionSetup, RunSessionTeardown};
use jinn_session_lifecycle_msg::event::SessionCreated;

use jinn_session_msg::SessionSeed;
use jinn_session_state::ChatSessionState;
use jinn_session_state::fields::LifecycleArgs;
use jinn_session_store_msg::PersistSession;
use jinn_slices::RouteResult as IntentResult;
use marked_path::{Absolute, MarkedPathBuf};

/// Handle `Intent::SessionLifecycleSetup`.
///
/// Creates a new session from the named lifecycle. If the lifecycle has a
/// `setup_command`, emits `Command::RunSessionSetup` for async execution.
/// If no setup command (blank or blank-like lifecycle), creates the session
/// with the default CWD immediately.
pub fn handle_session_lifecycle_setup(
    state: &mut AppState,
    lifecycle_name: &str,
    args: &[String],
    cwd: Option<&MarkedPathBuf<Absolute>>,
    config: &jinn_config::ConfigLayer,
) -> IntentResult {
    // Extract setup command before mutating state (borrow checker).
    let setup_command = find_lifecycle(config, lifecycle_name).and_then(|l| l.setup.clone());

    let model = state
        .frontend
        .app_state
        .last_model
        .clone()
        .unwrap_or_default();

    // Inherit the active session's persona — the same inheritance rule the
    // session's CWD follows below. The read happens BEFORE insert/set_active
    // so "the active session" here is the session being cloned from.
    let persona_name = state.active_session().persona_name().to_owned();

    let reasoning_effort = state.frontend.app_state.reasoning_effort;

    // Seed per-session defaults from jinn.toml (tool/skill filters +
    // auto-enabled MCP servers), matching every other session-creation path.
    let seed = SessionSeed::from_config(config);

    let mut new_session = ChatSessionState::new_with_profile(SessionProfile::new(
        model,
        persona_name,
        seed.tool_filter.clone(),
        seed.skill_filter.clone(),
        reasoning_effort,
    ));
    new_session.set_enabled_mcp_servers(seed.enabled_mcp.clone());
    let new_id = new_session.session_id().clone();

    // Set lifecycle metadata on the session core.
    new_session.set_lifecycle_name(if lifecycle_name.is_empty() {
        None
    } else {
        Some(lifecycle_name.to_owned())
    });
    let stored_args = stored_setup_args(setup_command.as_ref(), args);
    new_session.set_lifecycle_args(stored_args.clone());

    // Resolve the new session's starting CWD and project stamp. The CWD
    // precedence is:
    //   1. explicit `cwd` override (e.g. scripted callers),
    //   2. the stashed creation's starting CWD
    //      (set by the project picker then consumed here),
    //   3. inherit the active session's CWD.
    //
    // The project stamp comes only from the stashed creation - the projects UI
    // is the sole source of a project association. The two are independent on
    // purpose: setup scripts may re-cwd the session later, the project never
    // moves.
    //
    // CWD is resolved BEFORE insert/set_active — once set_active(new_id)
    // runs below, active_session() points at this new session. The stash is
    // always cleared here so it never leaks into the next creation, even when
    // an explicit `cwd` was supplied.
    //
    // A scripted lifecycle's stdout output still wins as the final CWD via the
    // session actor; this only sets the starting value.
    let (stamped_project, starting_cwd) = resolve_creation_stash(state, cwd);
    new_session.set_cwd(starting_cwd.clone());
    new_session.set_project(stamped_project);

    state.session.insert(new_session);
    state.session.set_active(new_id.clone());
    state.frontend.scope_clear_overlays();
    state.frontend.scope_push(jinn_slices::FocusScope::Input);

    // Build the session-created event.
    let created_event = SessionCreated {
        session_id: new_id.clone(),
        cwd: starting_cwd,
    };

    // If the lifecycle has a setup command, emit it for async execution.
    if let Some(ref setup_cmd) = setup_command {
        let rendered = match setup_cmd {
            jinn_preferences_config::schemas::LifecycleCommand::Shell(raw) => {
                if stored_args.is_empty() {
                    raw.clone()
                } else {
                    CommandTemplate::parse(raw)
                        .render_stored(stored_args.as_pairs())
                        .unwrap_or_default()
                }
            }
            jinn_preferences_config::schemas::LifecycleCommand::Builtin(id) => id.to_string(),
        };

        let mut result = IntentResult::empty()
            .with_message(PersistSession {
                session_id: new_id.clone(),
            })
            .with_message(RunSessionSetup {
                session_id: new_id.clone(),
                command: rendered,
                args: stored_args.values(),
                lifecycle_command: Some(setup_cmd.clone()),
            })
            .with_message(created_event);

        // Notify the MCP coordinator of any config-seeded enablement so the
        // new session's servers spawn without a picker visit. When nothing is
        // auto-enabled, the message is skipped (nothing to reconcile).
        if seed.has_auto_enabled_mcp() {
            result = result.with_message(jinn_mcp_msg::McpEnablementChanged {
                session_id: new_id,
                enabled: seed.enabled_mcp,
            });
        }

        return result;
    }

    // No setup command — the starting CWD was already set on the new session
    // before insert (above), so there's nothing more to do here.
    if !seed.has_auto_enabled_mcp() {
        return IntentResult::new_message(created_event);
    }
    IntentResult::new_message(created_event).with_message(jinn_mcp_msg::McpEnablementChanged {
        session_id: new_id,
        enabled: seed.enabled_mcp,
    })
}

/// Handle `Intent::SessionClose`.
///
/// Emits a `CloseSession` command for the active session. The session actor
/// handles teardown, archival, removal, and emits `SessionClosed`.
pub fn handle_session_close(state: &mut AppState) -> IntentResult {
    let closing_id = state.session.active_session_id().clone();
    close_session_and_switch(&closing_id)
}

/// Resolve and render the teardown command for a session by ID.
///
/// Reads the session's `lifecycle_name` + `lifecycle_args`, looks up the named
/// lifecycle in the configuration layer, takes its `teardown` command, and renders
/// it (replaying the stored args). Returns `None` when the session doesn't exist,
/// has no lifecycle name, or the named lifecycle has no teardown command.
///
/// This is the session-ID-keyed counterpart of the sidebar teardown handler —
/// UI-coupled callers resolve the selected session's ID, then delegate here.
///
/// A session whose lifecycle was resolved from a workflow-local file (it
/// carries a `lifecycle_source`) re-reads that file instead of the config —
/// the workflow's script is the one this session's lifecycle came from, and
/// jinn.toml never held it. A source file that cannot be read or parsed is a
/// visible [`TeardownFailure`], not a silent skip: worktrees leak when a
/// teardown quietly doesn't run.
pub fn build_run_session_teardown(
    state: &AppState,
    session_id: &SessionId,
    config: &jinn_config::ConfigLayer,
) -> Option<Result<RunSessionTeardown, TeardownFailure>> {
    let (name, source, args) = {
        let session = state.session.get(session_id)?;
        (
            session.lifecycle_name().map(String::from),
            session.lifecycle_source().map(String::from),
            session.lifecycle_args().clone(),
        )
    };
    let name = name?;
    lifecycle_teardown_visible(config, &name, source.as_deref()).map(|outcome| {
        outcome.map(|command| RunSessionTeardown {
            session_id: session_id.clone(),
            command: render(&command, &args),
            args: args.values(),
        })
    })
}

/// Why a recorded teardown could not be resolved, naming the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TeardownFailure {
    /// The session recorded a workflow-local lifecycle file that no longer
    /// reads.
    Unreadable {
        /// The recorded source file.
        source: String,
        /// The read or parse reason.
        reason: String,
    },
    /// The lifecycle source no longer defines the recorded entry name.
    UnknownEntry {
        /// The recorded source (a file path, or the config's name).
        source: String,
        /// The entry name the session recorded.
        name: String,
    },
    /// The resolved entry has no teardown command.
    NoTeardown {
        /// Where the entry was resolved from.
        source: String,
        /// The entry name.
        name: String,
    },
}

impl std::fmt::Display for TeardownFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TeardownFailure::Unreadable { source, reason } => {
                write!(f, "{source}: {reason}")
            }
            TeardownFailure::UnknownEntry { source, name } => {
                write!(f, "{source}: no lifecycle script named \"{name}\"")
            }
            TeardownFailure::NoTeardown { source, name } => {
                write!(
                    f,
                    "{source}: lifecycle script \"{name}\" defines no teardown command"
                )
            }
        }
    }
}

/// The placeholder name of the template's `idx`-th named parameter in
/// first-appearance order, or `None` when `idx` is past the named params.
fn named_param_at(template: &CommandTemplate, idx: usize) -> Option<String> {
    template
        .params()
        .iter()
        .filter_map(|param| match param {
            jinn_session_lifecycle_msg::command_template::Param::Named(name) => Some(name.clone()),
            _ => None,
        })
        .nth(idx)
}

/// Resolves the new session's starting CWD and project stamp from the
/// stashed creation.
///
/// The CWD precedence is:
///   1. explicit `cwd` override (e.g. scripted callers),
///   2. the stashed creation's starting CWD
///      (set by the project picker then consumed here),
///   3. inherit the active session's CWD.
///
/// The project stamp comes only from the stashed creation - the projects UI
/// is the sole source of a project association. The two are independent on
/// purpose: setup scripts may re-cwd the session later, the project never
/// moves.
///
/// The stash is always cleared here so it never leaks into the next
/// creation, even when an explicit `cwd` was supplied. A scripted
/// lifecycle's stdout output still wins as the final CWD via the session
/// actor; this only sets the starting value.
fn resolve_creation_stash(
    state: &mut AppState,
    cwd: Option<&MarkedPathBuf<Absolute>>,
) -> (Option<MarkedPathBuf<Absolute>>, MarkedPathBuf<Absolute>) {
    let pending = state.frontend.pending_creation.take();
    // Defensively clear any residual stash so it can never leak into the next
    // creation (the `.take()` above is skipped only when an explicit cwd
    // overrides, so clear unconditionally here).
    state.frontend.pending_creation = None;
    let cwd_override = cwd
        .cloned()
        .or_else(|| pending.as_ref().map(|p| p.starting_cwd.clone()));
    (
        pending.map(|p| p.project_dir),
        cwd_override.unwrap_or_else(|| state.active_session().cwd().clone()),
    )
}

/// Pairs a creation call's positional answers against the setup template's
/// placeholder names, for stamping on the session.
///
/// The template is the lifecycle's shell setup command; a caller's `idx`-th
/// answer belongs to its `idx`-th named placeholder. Past the named params
/// (or with no shell setup at all) the value keys positionally as `$n`.
fn stored_setup_args(
    setup_command: Option<&jinn_preferences_config::schemas::LifecycleCommand>,
    args: &[String],
) -> LifecycleArgs {
    let template = setup_command.as_ref().and_then(|command| match command {
        jinn_preferences_config::schemas::LifecycleCommand::Shell(cmd) => {
            Some(CommandTemplate::parse(cmd))
        }
        jinn_preferences_config::schemas::LifecycleCommand::Builtin(_) => None,
    });
    LifecycleArgs::from_pairs(args.iter().enumerate().map(|(idx, value)| {
        let key = template
            .as_ref()
            .and_then(|t| named_param_at(t, idx))
            .unwrap_or_else(|| format!("${}", idx + 1));
        (key, value.clone())
    }))
}

/// Renders a teardown command, replaying the stored args.
fn render(
    command: &jinn_preferences_config::schemas::LifecycleCommand,
    args: &LifecycleArgs,
) -> String {
    match command {
        jinn_preferences_config::schemas::LifecycleCommand::Shell(cmd) => {
            CommandTemplate::parse(cmd)
                .render_stored(args.as_pairs())
                .unwrap_or_else(|_| cmd.clone())
        }
        jinn_preferences_config::schemas::LifecycleCommand::Builtin(id) => id.to_string(),
    }
}

/// The teardown command of the named lifecycle, read from the recorded
/// source: a workflow-local file when the session carries one, the
/// `[[session_lifecycle.script]]` section otherwise.
///
/// The resolution-only view the lifecycle actor uses to *decide* whether a
/// teardown exists; failures surface through [`lifecycle_teardown_visible`]
/// on the close path, and stay silent skips here exactly where they did
/// before (no recorded lifecycle). A source file that stopped reading
/// yields `None`, matching the pre-source behavior of a lifecycle whose
/// name no longer resolves.
#[must_use]
pub fn lifecycle_command_for(
    config: &ConfigLayer,
    name: &str,
    source: Option<&str>,
) -> Option<jinn_preferences_config::schemas::LifecycleCommand> {
    match source {
        Some(path) => {
            let text = std::fs::read_to_string(path).ok()?;
            let scripts =
                jinn_preferences_config::schemas::session_lifecycle::parse_lifecycle_document(
                    &text,
                )
                .ok()?;
            scripts
                .iter()
                .find(|lifecycle| lifecycle.name == name)?
                .teardown
                .clone()
        }
        None => find_lifecycle(config, name)?.teardown,
    }
}

/// The teardown command of the named lifecycle, read from the recorded
/// source — with the failure that makes it visible.
///
/// The close-path resolver: identical source rules to
/// [`lifecycle_command_for`] (a recorded file re-read; no source reads the
/// config), but a *recorded* lifecycle that cannot be resolved comes back
/// as a [`TeardownFailure`] naming the source instead of a `None`. `None`
/// means only "nothing recorded" — no lifecycle, or a lifecycle with no
/// teardown half — which is the silent skip every caller has always had.
///
/// # Errors
///
/// The `Err` case names the file (and entry) that stopped resolving.
#[must_use]
pub fn lifecycle_teardown_visible(
    config: &ConfigLayer,
    name: &str,
    source: Option<&str>,
) -> Option<Result<jinn_preferences_config::schemas::LifecycleCommand, TeardownFailure>> {
    match source {
        Some(path) => {
            let text = match std::fs::read_to_string(path) {
                Ok(text) => text,
                Err(error) => {
                    return Some(Err(TeardownFailure::Unreadable {
                        source: path.to_owned(),
                        reason: format!("cannot read: {error}"),
                    }));
                }
            };
            let scripts =
                match jinn_preferences_config::schemas::session_lifecycle::parse_lifecycle_document(
                    &text,
                ) {
                    Ok(scripts) => scripts,
                    Err(reason) => {
                        return Some(Err(TeardownFailure::Unreadable {
                            source: path.to_owned(),
                            reason,
                        }));
                    }
                };
            let Some(script) = scripts.iter().find(|lifecycle| lifecycle.name == name) else {
                return Some(Err(TeardownFailure::UnknownEntry {
                    source: path.to_owned(),
                    name: name.to_owned(),
                }));
            };
            script.teardown.clone().map_or_else(
                || {
                    Some(Err(TeardownFailure::NoTeardown {
                        source: path.to_owned(),
                        name: name.to_owned(),
                    }))
                },
                |command| Some(Ok(command)),
            )
        }
        None => lifecycle_teardown(config, name).map(Ok),
    }
}

/// The teardown command of the named lifecycle, read from the
/// `[[session_lifecycle.script]]` section.
#[must_use]
pub fn lifecycle_teardown(
    config: &ConfigLayer,
    name: &str,
) -> Option<jinn_preferences_config::schemas::LifecycleCommand> {
    find_lifecycle(config, name)?.teardown
}

/// Looks up a lifecycle by name in the `[[session_lifecycle.script]]`
/// section.
fn find_lifecycle(config: &ConfigLayer, name: &str) -> Option<SessionLifecycle> {
    config
        .get_list::<SessionLifecycle>()
        .unwrap_or_default()
        .into_iter()
        .find(|lifecycle| lifecycle.name == name)
}

/// Emit a `CloseSession` command to the actor system.
/// The session actor handles actual removal, active session switching, and emits
/// `SessionClosed` for the sidebar actor to clamp the cursor.
fn close_session_and_switch(closing_id: &SessionId) -> IntentResult {
    use jinn_session_lifecycle_msg::CloseSession;
    IntentResult::new_message(CloseSession {
        session_id: closing_id.clone(),
    })
}

/// Handle `Intent::SessionNew`: a blank lifecycle setup.
///
/// The new-session intent is a lifecycle setup with no name, no args, and
/// no explicit cwd, so it delegates rather than duplicating the setup path.
pub fn handle_session_new(state: &mut AppState, config: &ConfigLayer) -> IntentResult {
    handle_session_lifecycle_setup(state, "", &[], None, config)
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
    use super::*;
    use crate::app_state::AppState;
    use jinn_core_types::ChatEntry;

    /// The workflow-local lifecycle document shape: the bare
    /// `[[session_lifecycle.script]]` section a workflow file ships.
    const WORKFLOW_DOC: &str = "[[session_lifecycle.script]]\nname = \"stage\"\n\
         setup_command = \"mkdir <dir>\"\nteardown_command = \"cleanup.sh $1\"\n";

    /// A layer carrying a single lifecycle, as a user's `jinn.toml` holds
    /// one under `[[session_lifecycle.script]]`.
    ///
    /// The document is built through `toml::Value` and serialized, so a
    /// command carrying a path is escaped by the TOML writer rather than
    /// pasted into a literal: a Windows path carries backslashes, and `\U`
    /// is an invalid TOML unicode escape — a raw format! produced a document
    /// that failed to parse.
    fn lifecycle_config(name: &str, setup: Option<&str>, teardown: Option<&str>) -> ConfigLayer {
        let mut script = toml::Table::new();
        script.insert("name".to_owned(), toml::Value::String(name.to_owned()));
        if let Some(setup) = setup {
            script.insert(
                "setup_command".to_owned(),
                toml::Value::String(setup.to_owned()),
            );
        }
        if let Some(teardown) = teardown {
            script.insert(
                "teardown_command".to_owned(),
                toml::Value::String(teardown.to_owned()),
            );
        }
        let mut lifecycle = toml::Table::new();
        lifecycle.insert(
            "script".to_owned(),
            toml::Value::Array(vec![toml::Value::Table(script)]),
        );
        let mut document = toml::Table::new();
        document.insert(
            "session_lifecycle".to_owned(),
            toml::Value::Table(lifecycle),
        );

        let text =
            toml::to_string(&toml::Value::Table(document)).expect("fixture document serializes");
        jinn_config::testutil::config_layer(&text)
    }

    #[rstest::rstest]
    fn new_session_inherits_active_sessions_persona() {
        // Given a state whose active session carries a persona.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .set_persona_name("learning-tutor".to_owned());

        // When creating a new session.
        let _result = handle_session_lifecycle_setup(
            &mut state,
            "",
            &[],
            None,
            jinn_config::empty_config_layer(),
        );

        // Then the new active session carries the old session's persona —
        // the same inheritance rule its CWD follows.
        assert_eq!(state.active_session().persona_name(), "learning-tutor");
    }

    #[rstest::rstest]
    fn session_lifecycle_setup_with_blank_creates_session() {
        // Given default state (no lifecycles configured).
        let mut state = AppState::default_with_scope_focus();
        // Set the active session's cwd to a distinct temp-rooted value so
        // inheritance is distinguishable from default_cwd() (the app launch
        // dir).
        let dir = jinn_testutil::tempdir();
        let inherited_cwd = dir
            .path
            .join("inherited-project")
            .expect("temp root joins a bare name");
        state.active_session_mut().set_cwd(inherited_cwd.clone());
        let old_id = state.session.active_session_id().clone();

        // When handling SessionLifecycleSetup with blank lifecycle.
        let result = handle_session_lifecycle_setup(
            &mut state,
            "",
            &[],
            None,
            jinn_config::empty_config_layer(),
        );

        // Then a new session is created.
        assert_ne!(*state.session.active_session_id(), old_id);
        // And the old empty session is preserved (no auto-close).
        assert!(state.session.contains(&old_id));
        // And two sessions exist.
        assert_eq!(state.session.session_count(), 2);
        // And one message is emitted: SessionCreated.
        assert_eq!(result.message_names.len(), 1);
        assert!(result.message_names[0].contains("SessionCreated"));
        // And the session has no lifecycle name.
        assert!(state.active_session().lifecycle_name().is_none());
        // And the new session inherited the active session's CWD, not the app
        // launch dir.
        assert_eq!(state.active_session().cwd(), &inherited_cwd);
    }

    #[rstest::rstest]
    fn explicit_cwd_override_overrides_inherited_cwd() {
        // Given a state whose active session has a distinct CWD and a
        // pending creation stashed on the frontend (as the project picker does).
        let dir = jinn_testutil::tempdir();
        let mut state = AppState::default_with_scope_focus();
        state.active_session_mut().set_cwd(
            dir.path
                .join("active-project")
                .expect("temp root joins a bare name"),
        );
        state.frontend.pending_creation = Some(crate::frontend_state::PendingSessionCreation {
            project_dir: dir
                .path
                .join("override-project")
                .expect("temp root joins a bare name"),
            starting_cwd: dir
                .path
                .join("override-project")
                .expect("temp root joins a bare name"),
        });
        let explicit_dir = dir
            .path
            .join("explicit-dir")
            .expect("temp root joins a bare name");

        // When handling SessionLifecycleSetup with an explicit cwd override.
        let _result = handle_session_lifecycle_setup(
            &mut state,
            "",
            &[],
            Some(&explicit_dir),
            jinn_config::empty_config_layer(),
        );

        // Then the new session's CWD is the explicit override, not the active
        // session's CWD and not the stashed starting CWD.
        assert_eq!(state.active_session().cwd(), &explicit_dir);
        // And the stash is cleared (never leaks to the next creation).
        assert!(state.frontend.pending_creation.is_none());
    }

    #[rstest::rstest]
    fn pending_creation_cwd_is_used_when_no_explicit_cwd_given() {
        // Given a state whose active session has a distinct CWD and a
        // pending creation stashed on the frontend.
        let dir = jinn_testutil::tempdir();
        let mut state = AppState::default_with_scope_focus();
        state.active_session_mut().set_cwd(
            dir.path
                .join("active-project")
                .expect("temp root joins a bare name"),
        );
        let override_project = dir
            .path
            .join("override-project")
            .expect("temp root joins a bare name");
        state.frontend.pending_creation = Some(crate::frontend_state::PendingSessionCreation {
            project_dir: override_project.clone(),
            starting_cwd: override_project.clone(),
        });

        // When handling SessionLifecycleSetup with no explicit cwd.
        let _result = handle_session_lifecycle_setup(
            &mut state,
            "",
            &[],
            None,
            jinn_config::empty_config_layer(),
        );

        // Then the new session's CWD is the stashed starting CWD, not the
        // active session's CWD.
        assert_eq!(state.active_session().cwd(), &override_project);
        // And the stash is cleared after consumption.
        assert!(state.frontend.pending_creation.is_none());
    }

    #[rstest::rstest]
    fn setup_stamps_project_from_pending_creation() {
        // Given a state with a pending creation stashed from the projects UI.
        let dir = jinn_testutil::tempdir();
        let mut state = AppState::default_with_scope_focus();
        let project = dir
            .path
            .join("projects/jinn")
            .expect("temp root joins a bare name");
        state.frontend.pending_creation = Some(crate::frontend_state::PendingSessionCreation {
            project_dir: project.clone(),
            starting_cwd: project.clone(),
        });

        // When handling SessionLifecycleSetup.
        let _result = handle_session_lifecycle_setup(
            &mut state,
            "",
            &[],
            None,
            jinn_config::empty_config_layer(),
        );

        // Then the new active session is stamped with the stashed project.
        assert_eq!(state.active_session().project(), Some(&project));
    }

    #[rstest::rstest]
    fn setup_without_pending_creation_leaves_project_none() {
        // Given a default state with no pending creation stash.
        let mut state = AppState::default_with_scope_focus();

        // When handling SessionLifecycleSetup.
        let _result = handle_session_lifecycle_setup(
            &mut state,
            "",
            &[],
            None,
            jinn_config::empty_config_layer(),
        );

        // Then the new active session has no project association.
        assert_eq!(state.active_session().project(), None);
    }

    #[rstest::rstest]
    fn setup_consumes_stash_exactly_once() {
        // Given a state that already consumed a pending creation.
        let dir = jinn_testutil::tempdir();
        let mut state = AppState::default_with_scope_focus();
        let first_project = dir
            .path
            .join("first-project")
            .expect("temp root joins a bare name");
        state.frontend.pending_creation = Some(crate::frontend_state::PendingSessionCreation {
            project_dir: first_project.clone(),
            starting_cwd: first_project,
        });
        let _result = handle_session_lifecycle_setup(
            &mut state,
            "",
            &[],
            None,
            jinn_config::empty_config_layer(),
        );

        // When creating a second session (the stash is now None).
        let _result = handle_session_lifecycle_setup(
            &mut state,
            "",
            &[],
            None,
            jinn_config::empty_config_layer(),
        );

        // Then the second session has no project association (no leak from
        // the first creation).
        assert_eq!(state.active_session().project(), None);
        // And the stash is still clear.
        assert!(state.frontend.pending_creation.is_none());
    }

    #[rstest::rstest]
    fn scripted_lifecycle_setup_pre_seeds_inherited_cwd_in_memory() {
        // Given a state whose active session has a distinct CWD, and a
        // lifecycle with a setup_command (so the script path is taken).
        let dir = jinn_testutil::tempdir();
        let mut state = AppState::default_with_scope_focus();
        let inherited_cwd = dir
            .path
            .join("inherited-project")
            .expect("temp root joins a bare name");
        state.active_session_mut().set_cwd(inherited_cwd.clone());
        let config = lifecycle_config(
            "fossil branch",
            Some(&format!(
                "echo {}",
                dir.path
                    .join("workdir")
                    .expect("temp root joins a bare name")
            )),
            None,
        );

        // When handling SessionLifecycleSetup with the scripted lifecycle.
        let _result =
            handle_session_lifecycle_setup(&mut state, "fossil branch", &[], None, &config);

        // Then the new session's in-memory CWD is the inherited value
        // (pre-seeded before the actor runs the script). The actor may
        // overwrite it with the script's stdout later, but at creation
        // time the inherited CWD is present.
        assert_eq!(state.active_session().cwd(), &inherited_cwd);
    }

    #[rstest::rstest]
    fn session_lifecycle_setup_with_lifecycle_emits_command() {
        // Given a state with a lifecycle that has a setup_command.
        let dir = jinn_testutil::tempdir();
        let mut state = AppState::default_with_scope_focus();
        let old_id = state.session.active_session_id().clone();
        let config = lifecycle_config(
            "fossil branch",
            Some(&format!(
                "echo {}",
                dir.path
                    .join("workdir")
                    .expect("temp root joins a bare name")
            )),
            None,
        );

        // When handling SessionLifecycleSetup.
        let result =
            handle_session_lifecycle_setup(&mut state, "fossil branch", &[], None, &config);

        // Then a new session is created.
        assert_ne!(*state.session.active_session_id(), old_id);
        // And the session has the lifecycle name.
        assert_eq!(
            state.active_session().lifecycle_name(),
            Some("fossil branch")
        );
        // And PersistSession, RunSessionSetup, SessionCreated are emitted.
        assert_eq!(result.message_names.len(), 3);
        assert!(result.message_names[0].contains("PersistSession"));
        assert!(result.message_names[1].contains("RunSessionSetup"));
        assert!(result.message_names[2].contains("SessionCreated"));
    }

    #[rstest::rstest]
    fn session_lifecycle_setup_with_args_renders_command() {
        // Given a lifecycle with $1 in the setup_command.
        let mut state = AppState::default_with_scope_focus();
        let config = lifecycle_config("fossil branch", Some("script.sh $1"), None);

        // When handling SessionLifecycleSetup with args.
        let result = handle_session_lifecycle_setup(
            &mut state,
            "fossil branch",
            &["my-branch".to_owned()],
            None,
            &config,
        );

        // Then PersistSession is emitted first.
        assert!(result.message_names[0].contains("PersistSession"));
        // And RunSessionSetup is emitted second with rendered args.
        assert!(result.message_names[1].contains("RunSessionSetup"));
        // And the session has the args stored under the template's names.
        assert_eq!(
            state.active_session().lifecycle_args(),
            &LifecycleArgs::from_pairs([("$1", "my-branch")])
        );
    }

    #[rstest::rstest]
    fn session_lifecycle_setup_clears_overlays_and_pushes_input() {
        // Given a state with a picker overlay.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_push(jinn_slices::FocusScope::Dynamic(
            jinn_project_msg::project_picker_scope(),
        ));

        // When handling SessionLifecycleSetup.
        let _result = handle_session_lifecycle_setup(
            &mut state,
            "",
            &[],
            None,
            jinn_config::empty_config_layer(),
        );

        // Then overlays are cleared and Input scope is pushed.
        assert!(matches!(
            state.frontend.scope(),
            jinn_slices::FocusScope::Input
        ));
    }

    #[rstest::rstest]
    fn session_close_without_lifecycle_emits_close_session() {
        // Given a state with two sessions.
        let mut state = AppState::default_with_scope_focus();
        let second_session = ChatSessionState::new();
        let second_id = second_session.session_id().clone();
        state.session.insert(second_session);
        state.session.set_active(second_id);

        // When handling SessionClose.
        let result = handle_session_close(&mut state);

        // Then a CloseSession command is emitted for the closed session.
        assert_eq!(result.message_names.len(), 1);
        assert!(result.message_names[0].contains("CloseSession"));
    }

    #[rstest::rstest]
    fn session_close_with_teardown_emits_close_session() {
        // Given a session with a lifecycle that has a teardown_command.
        let dir = jinn_testutil::tempdir();
        let mut state = AppState::default_with_scope_focus();
        let _config = lifecycle_config(
            "fossil branch",
            Some(&format!(
                "echo {}",
                dir.path
                    .join("workdir")
                    .expect("temp root joins a bare name")
            )),
            Some("cleanup.sh $1"),
        );
        let session_id = state.session.active_session_id().clone();
        state
            .active_session_mut()
            .set_lifecycle_name(Some("fossil branch".to_owned()));
        state
            .active_session_mut()
            .set_lifecycle_args(LifecycleArgs::from_pairs([(
                "$1".to_owned(),
                "my-branch".to_owned(),
            )]));

        // When handling SessionClose.
        let result = handle_session_close(&mut state);

        // Then a CloseSession command is emitted (actor handles teardown).
        assert!(state.session.contains(&session_id));
        assert_eq!(result.message_names.len(), 1);
        assert!(result.message_names[0].contains("CloseSession"));
    }

    #[rstest::rstest]
    fn session_close_last_session_emits_close_session() {
        // Given a state with only one session.
        let mut state = AppState::default_with_scope_focus();
        let _session_id = state.session.active_session_id().clone();
        assert_eq!(state.session.session_count(), 1);

        // When handling SessionClose.
        let result = handle_session_close(&mut state);

        // Then a CloseSession command is emitted.
        assert_eq!(result.message_names.len(), 1);
        assert!(result.message_names[0].contains("CloseSession"));
    }

    #[rstest::rstest]
    fn session_new_delegates_to_blank_lifecycle() {
        // Given default state.
        let mut state = AppState::default_with_scope_focus();
        let old_id = state.session.active_session_id().clone();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("old"));

        // When handling SessionNew (delegates to blank lifecycle setup).
        let result = crate::session_creation::intent::handle_session_new(
            &mut state,
            jinn_config::empty_config_layer(),
        );

        // Then a new session is created (same behavior as before).
        assert_ne!(*state.session.active_session_id(), old_id);
        // And one message emitted: SessionCreated.
        assert_eq!(result.message_names.len(), 1);
        assert!(result.message_names[0].contains("SessionCreated"));
    }

    #[rstest::rstest]
    fn empty_session_is_preserved_on_new_session() {
        // Given default state with a single empty session.
        let mut state = AppState::default_with_scope_focus();
        let old_id = state.session.active_session_id().clone();
        assert!(state.active_session().is_empty());

        // When creating a new session via lifecycle setup.
        let _result = handle_session_lifecycle_setup(
            &mut state,
            "",
            &[],
            None,
            jinn_config::empty_config_layer(),
        );

        // Then the old empty session is preserved (no auto-close).
        assert!(state.session.contains(&old_id));
        // And two sessions exist.
        assert_eq!(state.session.session_count(), 2);
    }

    #[rstest::rstest]
    fn session_with_history_is_preserved_on_new_session() {
        // Given an active session with history.
        let mut state = AppState::default_with_scope_focus();
        let old_id = state.session.active_session_id().clone();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("hello"));

        // When creating a new session.
        let _result = handle_session_lifecycle_setup(
            &mut state,
            "",
            &[],
            None,
            jinn_config::empty_config_layer(),
        );

        // Then the old session is preserved.
        assert!(state.session.contains(&old_id));
        // And two sessions exist.
        assert_eq!(state.session.session_count(), 2);
        // And the new session is active.
        assert_ne!(*state.session.active_session_id(), old_id);
    }

    #[rstest::rstest]
    fn lifecycle_setup_seeds_reasoning_effort_from_global_default() {
        // Given a global default effort of High.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.app_state.reasoning_effort =
            Some(jinn_provider_selection_msg::ReasoningEffort::High);

        // When creating a new session via lifecycle setup.
        let _result = handle_session_lifecycle_setup(
            &mut state,
            "",
            &[],
            None,
            jinn_config::empty_config_layer(),
        );

        // Then the new session owns the seeded effort (a copy, not a live reference).
        assert_eq!(
            state.active_session().profile().reasoning_effort,
            Some(jinn_provider_selection_msg::ReasoningEffort::High),
            "new session should be seeded from the global default"
        );
    }

    #[rstest::rstest]
    fn lifecycle_setup_seeds_none_reasoning_effort_when_global_unset() {
        // Given no global default effort.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.app_state.reasoning_effort = None;

        // When creating a new session via lifecycle setup.
        let _result = handle_session_lifecycle_setup(
            &mut state,
            "",
            &[],
            None,
            jinn_config::empty_config_layer(),
        );

        // Then the new session's effort is None (provider decides).
        assert_eq!(
            state.active_session().profile().reasoning_effort,
            None,
            "new session should be seeded as None when global is unset"
        );
    }

    #[rstest::rstest]
    fn lifecycle_setup_seeds_the_tool_filter_from_config() {
        // Given configuration withholding a tool.
        let mut state = AppState::default_with_scope_focus();
        let config = jinn_config::testutil::config_layer(
            "[tools.tool_filter]\nmode = \"deny\"\nnames = [\"bash\"]\n",
        );

        // When creating a new session via lifecycle setup.
        let _result = handle_session_lifecycle_setup(&mut state, "", &[], None, &config);

        // Then the new session withholds it.
        assert!(!state.active_session().is_tool_enabled("bash"));
    }

    #[rstest::rstest]
    fn lifecycle_setup_seeds_the_skill_filter_from_config() {
        // Given configuration withholding a skill.
        let mut state = AppState::default_with_scope_focus();
        let config = jinn_config::testutil::config_layer(
            "[skills.skill_filter]\nmode = \"deny\"\nnames = [\"phased-task-loop\"]\n",
        );

        // When creating a new session via lifecycle setup.
        let _result = handle_session_lifecycle_setup(&mut state, "", &[], None, &config);

        // Then the new session withholds it.
        assert!(!state.active_session().is_skill_enabled("phased-task-loop"));
    }

    #[rstest::rstest]
    fn lifecycle_setup_with_auto_enabled_mcp_returns_enablement_message() {
        // Given configuration with one auto-enabled MCP server.
        let mut state = AppState::default_with_scope_focus();
        let config = jinn_config::testutil::config_layer(
            "[mcp.excalimate]\nauto_enable = true\ncommand = \"npx\"\n",
        );

        // When creating a new session (blank lifecycle).
        let result = handle_session_lifecycle_setup(&mut state, "", &[], None, &config);

        // Then the new session has the server enabled.
        assert!(state.active_session().is_mcp_server_enabled("excalimate"));
        // And an McpEnablementChanged message is emitted so the coordinator
        // spawns the connection.
        assert!(
            result
                .message_names
                .iter()
                .any(|n| n.contains("McpEnablementChanged"))
        );
    }

    #[rstest::rstest]
    fn lifecycle_setup_without_auto_enable_emits_no_enablement_message() {
        // Given configuration with a server that is NOT auto-enabled.
        let mut state = AppState::default_with_scope_focus();
        let config = jinn_config::testutil::config_layer(
            "[mcp.manual]\nauto_enable = false\ncommand = \"npx\"\n",
        );

        // When creating a new session.
        let result = handle_session_lifecycle_setup(&mut state, "", &[], None, &config);

        // Then the server is not enabled on the new session.
        assert!(!state.active_session().is_mcp_server_enabled("manual"));
        // And no enablement message is emitted.
        assert!(
            !result
                .message_names
                .iter()
                .any(|n| n.contains("McpEnablementChanged"))
        );
    }

    #[rstest::rstest]
    fn scripted_lifecycle_setup_with_auto_enable_attaches_enablement_message() {
        // Given a scripted lifecycle and one auto-enabled server.
        let dir = jinn_testutil::tempdir();
        let mut state = AppState::default_with_scope_focus();
        // The lifecycle command carries the temp path, and the MCP block sits
        // beside it — the document is built through `toml::Value` so the path
        // is escaped by the TOML writer (a Windows path's backslashes are an
        // invalid TOML escape when pasted into a literal).
        let setup_command = format!(
            "echo {}",
            dir.path
                .join("workdir")
                .expect("temp root joins a bare name")
        );
        let mut script = toml::Table::new();
        script.insert(
            "name".to_owned(),
            toml::Value::String("fossil branch".to_owned()),
        );
        script.insert(
            "setup_command".to_owned(),
            toml::Value::String(setup_command),
        );
        let mut lifecycle = toml::Table::new();
        lifecycle.insert(
            "script".to_owned(),
            toml::Value::Array(vec![toml::Value::Table(script)]),
        );
        let mut mcp_server = toml::Table::new();
        mcp_server.insert("auto_enable".to_owned(), toml::Value::Boolean(true));
        mcp_server.insert("command".to_owned(), toml::Value::String("npx".to_owned()));
        let mut mcp = toml::Table::new();
        mcp.insert("excalimate".to_owned(), toml::Value::Table(mcp_server));
        let mut document = toml::Table::new();
        document.insert(
            "session_lifecycle".to_owned(),
            toml::Value::Table(lifecycle),
        );
        document.insert("mcp".to_owned(), toml::Value::Table(mcp));

        let config = jinn_config::testutil::config_layer(
            &toml::to_string(&toml::Value::Table(document)).expect("fixture document serializes"),
        );

        // When creating a session with the scripted lifecycle.
        let result =
            handle_session_lifecycle_setup(&mut state, "fossil branch", &[], None, &config);

        // Then the enablement message follows SessionCreated in the chain.
        let created_idx = result
            .message_names
            .iter()
            .position(|n| n.contains("SessionCreated"))
            .expect("SessionCreated emitted");
        let enablement_idx = result
            .message_names
            .iter()
            .position(|n| n.contains("McpEnablementChanged"))
            .expect("enablement emitted for scripted path");
        assert!(
            enablement_idx > created_idx,
            "enablement must trail SessionCreated"
        );
    }

    #[rstest::rstest]
    fn lifecycle_setup_preserves_empty_session_when_creating_lifecycle_session() {
        // Given a single empty session (app just started).
        let mut state = AppState::default_with_scope_focus();
        assert_eq!(state.session.session_count(), 1);

        // When creating a new session with a lifecycle.
        let dir = jinn_testutil::tempdir();
        let config = lifecycle_config(
            "fossil branch",
            Some(&format!(
                "echo {}",
                dir.path
                    .join("workdir")
                    .expect("temp root joins a bare name")
            )),
            None,
        );
        let result =
            handle_session_lifecycle_setup(&mut state, "fossil branch", &[], None, &config);

        // Then both sessions exist (old empty one is preserved).
        assert_eq!(state.session.session_count(), 2);
        // And the new session has the lifecycle name.
        assert_eq!(
            state.active_session().lifecycle_name(),
            Some("fossil branch")
        );
        // And PersistSession, then RunSessionSetup are emitted.
        assert!(result.message_names[0].contains("PersistSession"));
        assert!(result.message_names[1].contains("RunSessionSetup"));
    }

    #[rstest::rstest]
    fn build_run_session_teardown_renders_command_with_args() {
        // Given a session with a lifecycle that has a teardown command.
        let mut state = AppState::default_with_scope_focus();
        let config = lifecycle_config("fossil branch", None, Some("cleanup.sh $1"));
        let session_id = state.session.active_session_id().clone();
        state
            .active_session_mut()
            .set_lifecycle_name(Some("fossil branch".to_owned()));
        state
            .active_session_mut()
            .set_lifecycle_args(LifecycleArgs::from_pairs([(
                "$1".to_owned(),
                "my-branch".to_owned(),
            )]));

        // When building the teardown command.
        let msg = build_run_session_teardown(&state, &session_id, &config);

        // Then a rendered RunSessionTeardown is returned.
        let msg = msg
            .expect("teardown command should be built")
            .expect("resolves");
        assert_eq!(msg.session_id, session_id);
        assert_eq!(msg.command, "cleanup.sh my-branch");
        assert_eq!(msg.args, vec!["my-branch".to_owned()]);
    }

    #[rstest::rstest]
    fn build_run_session_teardown_returns_none_without_teardown_command() {
        // Given a session whose lifecycle has no teardown command.
        let mut state = AppState::default_with_scope_focus();
        let config = lifecycle_config("blank", None, None);
        let session_id = state.session.active_session_id().clone();
        state
            .active_session_mut()
            .set_lifecycle_name(Some("blank".to_owned()));

        // When building the teardown command.
        let msg = build_run_session_teardown(&state, &session_id, &config);

        // Then None is returned.
        assert!(msg.is_none());
    }

    #[rstest::rstest]
    fn build_run_session_teardown_returns_none_without_lifecycle_name() {
        // Given a session with no lifecycle name.
        let state = AppState::default_with_scope_focus();
        let session_id = state.session.active_session_id().clone();

        // When building the teardown command.
        let msg =
            build_run_session_teardown(&state, &session_id, jinn_config::empty_config_layer());

        // Then None is returned.
        assert!(msg.is_none());
    }

    #[rstest::rstest]
    fn build_run_session_teardown_renders_positional_arg_from_stored_args() {
        // Given a lifecycle teardown with $1 and a session storing the arg.
        let mut state = AppState::default_with_scope_focus();
        let config = lifecycle_config("fossil branch", None, Some("cleanup.sh $1"));
        let session_id = state.session.active_session_id().clone();
        state
            .active_session_mut()
            .set_lifecycle_name(Some("fossil branch".to_owned()));
        state
            .active_session_mut()
            .set_lifecycle_args(LifecycleArgs::from_pairs([(
                "$1".to_owned(),
                "feature-x".to_owned(),
            )]));

        // When building the teardown command.
        let msg = build_run_session_teardown(&state, &session_id, &config);

        // Then the $1 positional is rendered with the stored arg.
        let msg = msg
            .expect("teardown command should be built")
            .expect("resolves");
        assert_eq!(msg.command, "cleanup.sh feature-x");
    }

    #[rstest::rstest]
    fn build_run_session_teardown_re_reads_a_recorded_source_file() {
        // Given a session stamped with a workflow-local lifecycle file whose
        // entry has a teardown — and a config layer that does NOT know the
        // name (the source must be the only thing consulted).
        let file = jinn_testutil::tempdir();
        let path = file
            .path
            .join("lifecycle.toml")
            .expect("temp root joins a bare name");
        std::fs::write(&path, WORKFLOW_DOC).expect("write");
        let mut state = AppState::default_with_scope_focus();
        let session_id = state.session.active_session_id().clone();
        state
            .active_session_mut()
            .set_lifecycle_name(Some("stage".to_owned()));
        state
            .active_session_mut()
            .set_lifecycle_args(LifecycleArgs::from_pairs([(
                "$1".to_owned(),
                "wt-a".to_owned(),
            )]));
        state
            .active_session_mut()
            .set_lifecycle_source(Some(path.to_string()));

        // When building the teardown command against an empty config.
        let msg =
            build_run_session_teardown(&state, &session_id, jinn_config::empty_config_layer());

        // Then the command re-renders from the file's entry, replaying the
        // stored args.
        let msg = msg.expect("recorded lifecycle").expect("resolves");
        assert_eq!(msg.command, "cleanup.sh wt-a");
        assert_eq!(msg.args, vec!["wt-a".to_owned()]);
    }

    #[rstest::rstest]
    fn build_run_session_teardown_names_the_file_when_it_vanished() {
        // Given a session stamped with a source file that no longer exists
        // (the joined subdirectories are never created).
        let dir = jinn_testutil::tempdir();
        let gone_path = dir
            .path
            .join("gone/workflow/lifecycle.toml")
            .expect("temp root joins a bare name");
        let mut state = AppState::default_with_scope_focus();
        let session_id = state.session.active_session_id().clone();
        state
            .active_session_mut()
            .set_lifecycle_name(Some("stage".to_owned()));
        state
            .active_session_mut()
            .set_lifecycle_source(Some(gone_path.to_string()));

        // When building the teardown command.
        let outcome =
            build_run_session_teardown(&state, &session_id, jinn_config::empty_config_layer());

        // Then the failure is visible and names the file — not the silent
        // skip `None` produces, which would leak the session's worktree.
        let outcome = outcome.expect("a recorded lifecycle is not a skip");
        let failure = outcome.expect_err("the file is gone");
        assert!(
            failure.to_string().contains(&gone_path.to_string()),
            "{failure}"
        );
    }

    #[rstest::rstest]
    fn build_run_session_teardown_names_the_file_and_entry_when_the_entry_vanished() {
        // Given a stamped source whose file exists but no longer defines
        // the recorded entry.
        let file = jinn_testutil::tempdir();
        let path = file
            .path
            .join("lifecycle.toml")
            .expect("temp root joins a bare name");
        std::fs::write(&path, "[[session_lifecycle.script]]\nname = \"other\"\n").expect("write");
        let mut state = AppState::default_with_scope_focus();
        let session_id = state.session.active_session_id().clone();
        state
            .active_session_mut()
            .set_lifecycle_name(Some("stage".to_owned()));
        state
            .active_session_mut()
            .set_lifecycle_source(Some(path.to_string()));

        // When building the teardown command.
        let outcome =
            build_run_session_teardown(&state, &session_id, jinn_config::empty_config_layer());

        // Then the failure names both the file and the entry.
        let failure = outcome
            .expect("a recorded lifecycle is not a skip")
            .expect_err("the entry is gone");
        let text = failure.to_string();
        assert!(text.contains(path.to_string().as_str()), "{text}");
        assert!(text.contains("stage"), "{text}");
    }

    #[rstest::rstest]
    fn a_source_carrying_session_bypasses_the_config_entirely() {
        // Given a stamped source whose file defines the entry with a
        // DIFFERENT teardown than a config entry of the same name.
        let file = jinn_testutil::tempdir();
        let path = file
            .path
            .join("lifecycle.toml")
            .expect("temp root joins a bare name");
        std::fs::write(&path, WORKFLOW_DOC).expect("write");
        let mut state = AppState::default_with_scope_focus();
        let session_id = state.session.active_session_id().clone();
        state
            .active_session_mut()
            .set_lifecycle_name(Some("stage".to_owned()));
        state
            .active_session_mut()
            .set_lifecycle_source(Some(path.to_string()));
        let config = lifecycle_config("stage", None, Some("echo from-config"));

        // When building the teardown command.
        let msg = build_run_session_teardown(&state, &session_id, &config);

        // Then the file's teardown rendered — the config was never read.
        let msg = msg.expect("recorded lifecycle").expect("resolves");
        assert_eq!(msg.command, "cleanup.sh $1");
    }
}
