//! Bot key actions — `b` (new bot of a session), `B` (standalone bot),
//! `G` (re-run), and `R` (reset) — and `L` (library membership).
//!
//! `G` calls into the bot slice's rerun, which owns the shared
//! fire sequence; the sidebar only resolves the highlighted session and
//! publishes what the rerun produces. `R` resets a bot's context in place:
//! every non-pinned entry is force-excluded and nothing is dispatched.
//! `b` creates a bot of the highlighted session and makes it active, landing
//! in seed mode so the user composes its instructions before anything can
//! fire. `B` creates a standalone bot — no parent — from the highlighted
//! session's environment, landing in the same seed mode. `L` moves the
//! highlighted session into the library, refusing only a bot with a parent.

use jinn_kernel::common::app_state::AppState;
use jinn_kernel::protocol::{ChatEntry, IntentResult};
use jinn_session_lifecycle_msg::event::SessionCreated;
use jinn_session_store_msg::PersistSession;
use jinn_slices::ConfigLayer;

use super::close::validate_session_close;
use crate::sections::section_trait::SidebarSectionId;
use jinn_slices::FocusScope;

/// The session the focused sidebar section's cursor names, if that section
/// lists one.
///
/// The three sections that name a session in their cursor keep separate
/// cursors over separate lists; reading whichever one belongs to the focused
/// section is a choice between identities. Resolution walks the scope
/// *stack*, not just its top: the sidebar's own popups (rename, properties)
/// push dynamic scopes above the section scope, and an action inside such a
/// popup must still resolve to the section the popup was opened from — the
/// focused section is the nearest sidebar scope on the stack, the top if it
/// is one, else the parent.
///
/// Deliberately un-gated: the rerun supersedes a busy bot, and reset refuses
/// busy on its own terms, so neither wants the close action's idle
/// requirement. `c` (continue) shares it for the same reason — resume's
/// idle check lives downstream in the session actor.
#[must_use]
pub(crate) fn selected_section_session(state: &AppState) -> Option<jinn_core_types::SessionId> {
    let focused = focused_sidebar_section(state)?;
    state.frontend.with_sections(
        |sections| match focused {
            SidebarSectionId::Sessions => sections.sessions.selected_id.clone(),
            SidebarSectionId::Bot => sections.bot.selected_id.clone(),
            SidebarSectionId::Library => sections.library.selected_id.clone(),
            _ => None,
        },
        || None,
    )
}

/// The nearest sidebar-section scope on the focus stack: the current scope
/// if it names a section, else the scope below it (the popup case).
fn focused_sidebar_section(state: &AppState) -> Option<SidebarSectionId> {
    if let Some(section) = state.frontend.sidebar_section() {
        return Some(section);
    }
    let scope = state.frontend.scope_parent()?;
    let FocusScope::Dynamic(id) = scope else {
        return None;
    };
    (id.slice() == "sidebar")
        .then(|| SidebarSectionId::from_scope_name(id.name()))
        .flatten()
}

/// The idle session under the sessions-section cursor, if any.
///
/// Same guard the other session actions use: the action only means
/// something with a selected, loaded, idle session row.
fn selected_idle_session(state: &AppState) -> Option<jinn_core_types::SessionId> {
    validate_session_close(state).ok()?;
    state
        .frontend
        .with_sections(|sections| sections.sessions.selected_id.clone(), || None)
}

/// `b` — creates a bot of the highlighted session and activates it.
///
/// The bot starts in
/// [`jinn_bot_msg::BotBehavior::Reset`]: submissions pin into
/// its context without dispatching, and its trigger is inert until the user
/// flips it out of seed. Nothing is sent to any provider from this path.
///
/// The bot inherits the parent's environment (profile, cwd, project,
/// home, enabled MCP servers) via `new_bot`, plus the config-seeded
/// tool/skill disablements every other creation path applies.
pub fn handle_new_bot_of_session(state: &mut AppState, config: &ConfigLayer) -> IntentResult {
    use jinn_session_msg::SessionSeed;

    let Some(parent_id) = selected_idle_session(state) else {
        return IntentResult::empty();
    };

    // Seed per-session defaults from jinn.toml (disablement sets +
    // auto-enabled MCP servers), matching every other session-creation path.
    let seed = SessionSeed::from_config(config);

    let bot = {
        // `new_bot` takes the parent by reference and copies its
        // environment; clone the parent out of the map so the borrow ends
        // before the insert below.
        let Some(parent) = state.session.get(&parent_id).cloned() else {
            return IntentResult::empty();
        };
        // A bot is an ordinary session: it is saved on creation and
        // again once a turn lands, exactly like a subagent's child.
        let mut bot = jinn_session_state::ChatSessionState::new_bot(&parent, true);
        {
            let p = bot.profile_mut();
            p.tool_filter.clone_from(&seed.tool_filter);
            p.skill_filter.clone_from(&seed.skill_filter);
        }
        bot
    };

    let bot_id = bot.session_id().clone();
    let bot_cwd = bot.cwd().clone();
    state.session.insert(bot);
    state.session.set_active(bot_id.clone());
    state.frontend.scope_clear_overlays();
    state.frontend.scope_push(jinn_slices::FocusScope::Input);

    // The bot is meaningfully created even though no keystroke has
    // landed in it, so it survives a save that runs before its first turn
    // (the `spawn_subagent` tool's `build_child` marks its child for the same reason).
    if let Some(a) = state.session.get_mut(&bot_id) {
        a.mark_interacted();
    }

    // The parent earns the same mark. A brand new session is not written
    // when it is created — it stays in memory until something makes it
    // worth keeping — and the store discards any snapshot for a session it
    // does not consider persistable, so asking to save an unwritten parent
    // achieves nothing on its own. Attaching a bot is that
    // something: the child names the parent in its own row, so a parent
    // that is never saved is a bot pointing at a session the store
    // has never heard of.
    if let Some(p) = state.session.get_mut(&parent_id) {
        p.mark_interacted();
    }

    let mut result = IntentResult::empty()
        // The parent first. A brand new session is not written when it is
        // created, so it exists only in memory until something forces it
        // out. The bot's own row names the parent, so saving the
        // child while the parent is unwritten is a bot pointing at
        // a session the store has never heard of — a tree with a hole
        // where the trunk should be. Saving the parent is idempotent, and
        // a no-op for the ordinary case where it was already written.
        .with_message(PersistSession {
            session_id: parent_id.clone(),
        })
        .with_message(PersistSession {
            session_id: bot_id.clone(),
        })
        .with_message(SessionCreated {
            session_id: bot_id.clone(),
            cwd: bot_cwd,
        })
        .with_message(jinn_session_history_msg::PushChatEntry {
            session_id: bot_id.clone(),
            entry: ChatEntry::system(
                "Bot created in prep mode for providing instructions. When you are done, select the bot in the sidebar and press `P` to turn prep mode off. The bot will not run until you do."
            ),
            pin: None,
        });

    if seed.has_auto_enabled_mcp() {
        result = result.with_message(jinn_mcp_msg::McpEnablementChanged {
            session_id: bot_id,
            enabled: seed.enabled_mcp,
        });
    }
    result
}

/// `B` — creates a standalone bot from the highlighted session's
/// environment and activates it.
///
/// The same sequence [`handle_new_bot_of_session`] runs, minus everything
/// the parent link causes: no parent mark, no parent persist, and a
/// `new_standalone_bot` base whose seed template is empty and whose
/// trigger can never fire. The highlighted session is the environment
/// source only.
pub fn handle_new_standalone_bot(state: &mut AppState, config: &ConfigLayer) -> IntentResult {
    use jinn_session_msg::SessionSeed;

    let Some(source_id) = selected_idle_session(state) else {
        return IntentResult::empty();
    };
    let seed = SessionSeed::from_config(config);

    let bot = {
        let Some(source) = state.session.get(&source_id).cloned() else {
            return IntentResult::empty();
        };
        let mut bot = jinn_session_state::ChatSessionState::new_standalone_bot(&source, true);
        {
            let p = bot.profile_mut();
            p.tool_filter.clone_from(&seed.tool_filter);
            p.skill_filter.clone_from(&seed.skill_filter);
        }
        bot
    };

    let bot_id = bot.session_id().clone();
    let bot_cwd = bot.cwd().clone();
    state.session.insert(bot);
    state.session.set_active(bot_id.clone());
    state.frontend.scope_clear_overlays();
    state.frontend.scope_push(jinn_slices::FocusScope::Input);

    if let Some(a) = state.session.get_mut(&bot_id) {
        a.mark_interacted();
    }

    let mut result = IntentResult::empty()
        .with_message(PersistSession {
            session_id: bot_id.clone(),
        })
        .with_message(SessionCreated {
            session_id: bot_id.clone(),
            cwd: bot_cwd,
        })
        .with_message(jinn_session_history_msg::PushChatEntry {
            session_id: bot_id.clone(),
            entry: ChatEntry::system(
                "Standalone bot created in prep mode. Type its standing instructions, then press `P` to take it out of prep mode. It will not run until you do."
            ),
            pin: None,
        });

    if seed.has_auto_enabled_mcp() {
        result = result.with_message(jinn_mcp_msg::McpEnablementChanged {
            session_id: bot_id,
            enabled: seed.enabled_mcp,
        });
    }
    result
}

/// `G` — re-runs the highlighted bot (the sessions-scope key the bot scheme
/// uses everywhere).
///
/// Delegates to the bot slice's rerun (the same sequence a trigger
/// fire uses), then publishes the cancel and dispatch it produces. A
/// no-op on a non-bot row, or on a bot still in seed mode;
/// a blocked rerun surfaces its reason as a transient system line.
pub fn handle_rerun_bot(state: &mut AppState) -> IntentResult {
    let Some(bot_id) = selected_section_session(state) else {
        return IntentResult::empty();
    };
    let Some((cancel, dispatch, reset)) = jinn_bot::rerun::rerun_in_state(state, &bot_id) else {
        // Surface the block reason when it is one the user can act on.
        if let Some(reason) = jinn_bot::rerun::rerun_blocked_reason_in(state, &bot_id) {
            let session_id = state.session.active_session_id().clone();
            return IntentResult::empty().with_message(jinn_session_history_msg::PushChatEntry {
                session_id,
                entry: ChatEntry::system(format!("⚠️ Cannot re-run: {reason}")),
                pin: None,
            });
        }
        return IntentResult::empty();
    };
    let mut result = IntentResult::empty();
    if let Some(cancel) = cancel {
        result = result.with_message(cancel);
    }
    // The confirmed-cancel cascade: every bot or subagent beneath this
    // one stops with it, recursively. A trigger deliberately does not do
    // this — it cannot know which descendant the user would want stopped —
    // but `R` is the user saying "start over", and the descendants only exist
    // to answer the question this bot is re-asking. Forks are
    // boundaries; the walk stops there.
    let mut visited = std::collections::HashSet::new();
    visited.insert(bot_id.clone());
    result = result.merge(jinn_kernel::feat::intent::cancel::cascade_descendants(
        state,
        &bot_id,
        &mut visited,
    ));
    if let Some(dispatch) = dispatch {
        result = result.with_message(dispatch);
    }
    // A `Reset` that excluded something changed what the model will see, and
    // that only survives a restart if it is written now — a bot
    // reloaded from disk would otherwise come back with its full history.
    if !reset.is_empty() {
        result = result.with_message(PersistSession { session_id: bot_id });
    }
    result
}

/// `R` — resets the highlighted bot's context without dispatching.
///
/// Force-excludes every non-pinned entry, so the model sees only the pins;
/// the transcript itself is untouched, and nothing is sent anywhere. This is
/// the reset half of the rerun sequence, run on its own: unlike the rerun it
/// refuses a busy bot rather than superseding it — starting over behind a
/// running turn would race the turn's own writes.
pub fn handle_reset_bot(state: &mut AppState) -> IntentResult {
    let Some(bot_id) = selected_section_session(state) else {
        return IntentResult::empty();
    };
    // The end of the borrow: both guard reads need the session, and the reset
    // needs it mutably, so the guards run first and hand back only the verdict.
    let blocked_reason: Option<&'static str> = {
        let Some(session) = state.session.get(&bot_id) else {
            return IntentResult::empty();
        };
        if !session.is_bot() {
            Some("not a bot")
        } else if !matches!(session.phase(), jinn_slices::PhaseView::Idle) {
            Some("the bot is busy")
        } else {
            None
        }
    };
    let Some(reason) = blocked_reason else {
        // Re-borrow mutably now that the guard borrow has ended.
        let Some(session) = state.session.get_mut(&bot_id) else {
            return IntentResult::empty();
        };
        let changed = jinn_bot::activation::reset_context(session);
        if changed.is_empty() {
            return IntentResult::empty();
        }
        // The exclusions only survive a restart if they are written now — a
        // bot reloaded from disk would otherwise come back with its full
        // history.
        return IntentResult::empty().with_message(PersistSession { session_id: bot_id });
    };
    let session_id = state.session.active_session_id().clone();
    IntentResult::empty().with_message(jinn_session_history_msg::PushChatEntry {
        session_id,
        entry: ChatEntry::system(format!("⚠️ Cannot reset: {reason}")),
        pin: None,
    })
}

/// Why a session cannot be moved into the library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LibraryError {
    /// The focused section is not the sessions list — that is the only
    /// list the move works from.
    WrongSection,
    /// No session is selected, or the selected one is not in memory.
    NoSelection,
    /// The selected session is a bot with a parent association. Shelved
    /// in the library while archived, a `ParentCompleted` bot would
    /// silently never fire; parent links are immutable, so nothing that
    /// entered could later become blocked.
    BlockedParentedBot,
}

/// Validates that the highlighted session can move into the library.
///
/// Only a *bot with a parent* is refused. Every other session — plain
/// sessions, forks, subagents, standalone bots — may enter.
///
/// # Errors
///
/// Returns [`LibraryError`] naming why the move cannot proceed.
pub fn validate_library_add(state: &AppState) -> Result<jinn_core_types::SessionId, LibraryError> {
    if state.frontend.sidebar_section() != Some(SidebarSectionId::Sessions) {
        return Err(LibraryError::WrongSection);
    }
    let session_id = state
        .frontend
        .with_sections(|sections| sections.sessions.selected_id.clone(), || None)
        .ok_or(LibraryError::NoSelection)?;
    let session = state
        .session
        .get(&session_id)
        .ok_or(LibraryError::NoSelection)?;
    if session.is_bot() && session.parent_session().is_some() {
        return Err(LibraryError::BlockedParentedBot);
    }
    Ok(session_id)
}

/// `L` — moves the highlighted session into the library.
///
/// Membership is a durable flag; the session disappears from the Sessions
/// list and appears in the Library section, whether loaded or archived.
/// Validated first, so a refusal leaves no partial mutation and publishes
/// nothing.
pub fn handle_toggle_library(state: &mut AppState) -> IntentResult {
    use jinn_session_store_msg::SetSessionLibrary;

    let Ok(session_id) = validate_library_add(state) else {
        return IntentResult::empty();
    };
    IntentResult::new_message(SetSessionLibrary {
        session_id,
        in_library: true,
    })
}
