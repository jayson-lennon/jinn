//! Coherent projection of the session-owned fields required for prompt assembly.

use std::collections::HashSet;

use jinn_context::ContextFile;
use jinn_core_types::{ChatEntry, NameFilter, SessionId};
use jinn_skills_msg::Skill;

use crate::ChatSessionState;
use marked_path::{Absolute, MarkedPathBuf};

/// Immutable session values required by the context-assembly service.
///
/// The projection is captured under the application's read guard so every
/// session-owned assembly field comes from one authoritative state version.
/// Persona and tool definitions are not session-owned and are resolved by the
/// context-assembly slice when it builds the final crossing input.
#[derive(Debug, Clone)]
pub struct AssemblySessionProjection {
    /// Session identity.
    pub session_id: SessionId,
    /// Working directory rendered into the environment block.
    pub cwd: MarkedPathBuf<Absolute>,
    /// Selected persona name before persona-cell resolution.
    pub persona_name: String,
    /// Full conversation history, including pins.
    pub history: Vec<ChatEntry>,
    /// Which tools the session may use, by session policy. `None` means the
    /// session has no filter and inherits.
    pub tool_filter: Option<NameFilter>,
    /// Provider selected by the session profile.
    pub provider_name: String,
    /// Skills discovered for the session.
    pub skills: Vec<Skill>,
    /// Which skills the session may load, by session policy. `None` means
    /// the session has no filter and inherits.
    pub skill_filter: Option<NameFilter>,
    /// Skills whose bodies are loaded.
    pub loaded_skills: HashSet<String>,
    /// Project context files discovered for the session.
    pub context_files: Vec<ContextFile>,
    /// The static system-prompt section discovered for the session;
    /// `None` renders no section.
    pub static_section: Option<String>,

    /// Whether this session is a bot, which decides whether it is
    /// offered the bot-only tools.
    pub is_bot: bool,
    /// Whether a bot session has a parent. Only meaningful when
    /// [`Self::is_bot`] holds; decides whether `notify_parent` is
    /// offered alongside `conclude`.
    pub bot_has_parent: bool,
}

impl AssemblySessionProjection {
    /// Captures all session-owned assembly fields from the authoritative aggregate.
    #[must_use]
    pub fn capture(session_id: &SessionId, session: &ChatSessionState) -> Self {
        Self {
            session_id: session_id.clone(),
            cwd: session.cwd().clone(),
            persona_name: session.persona_name().to_owned(),
            history: session.history().to_vec(),
            tool_filter: session.tool_filter().cloned(),
            provider_name: session.model_selection().provider_name().to_owned(),
            skills: session.discovered_skills().to_vec(),
            skill_filter: session.skill_filter().cloned(),
            loaded_skills: session.loaded_skills(),
            context_files: session.discovered_context_files().to_vec(),
            static_section: session.discovered_static_section().map(str::to_owned),
            is_bot: session.is_bot(),
            bot_has_parent: session.parent_session().is_some(),
        }
    }
}
