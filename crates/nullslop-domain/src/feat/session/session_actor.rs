//! Session lifecycle and persistence actor - owns session state from input to streaming.
//!
//! This actor is the **sole owner** of session-related state: chat history, input
//! buffers, session phase transitions, tool call state, and streaming tokens. It
//! also handles persisting sessions to disk and restoring them on load.
//!
//! # State ownership
//!
//! This actor is the **sole writer** of the following `AppState` fields:
//! - session history (entries, tool calls, streaming state)
//! - session input buffers
//! - session phase (idle → sending → streaming → idle)
//! - `active_session`, `session_load_guard`
//!
//! # Lock discipline
//!
//! All handlers follow the same pattern: acquire state lock → mutate → release →
//! then emit. Never hold the lock during emission.

mod handlers;

use super::SessionStoreService;

use crate::SessionForkRequested;
use crate::SessionLoadRequested;
use crate::common::actor::{Actor, ActorContext, ActorEnvelope, NoDirectMsg};
use crate::common::services::Services;
use crate::common::state::State;
use crate::feat::chat_input::protocol::command::{
    EnqueueUserMessage, PushChatEntry, SetChatInputText,
};
use crate::feat::context::protocol::command::SwitchPromptStrategy;
use crate::feat::context::protocol::event::PromptAssembled;
use crate::feat::context::strategy::token_estimator::TiktokenCounter;
use crate::feat::provider::protocol::command::SendMessage;
use crate::feat::provider::protocol::event::{ModelsRefreshed, StreamCompleted, StreamToken};
use crate::feat::session::chat_session::ChatSessionState;
use crate::feat::session::protocol::archive_session::ArchiveSession;
use crate::feat::session::protocol::close_session::CloseSession;
use crate::feat::session::protocol::load_session_picker_entries::LoadSessionPickerEntries;
use crate::feat::session::protocol::session_archived::SessionArchived;
use crate::feat::session::protocol::session_closed::SessionClosed;
use crate::feat::session::protocol::session_load_completed::SessionLoadCompleted;
use crate::feat::session_lifecycle::command_runner::LifecycleCommandError;
use crate::feat::session_lifecycle::command_runner::run_setup_command;
use crate::feat::session_lifecycle::command_runner::run_teardown_command;
use crate::feat::session_lifecycle::protocol::command::{
    RunSessionSetup, RunSessionTeardown, SaveNewLifecycleSession,
};
use crate::feat::session_lifecycle::protocol::event::{
    SessionSetupCompleted, SessionTeardownFinished,
};
use crate::feat::tools_actor::protocol::event::{
    ToolBatchCompleted, ToolCallReceived, ToolCallStreaming, ToolExecutionCompleted,
    ToolExecutionOutput, ToolExecutionStarted, ToolUseStarted,
};
use crate::init::EnvironmentLoaded;
use crate::protocol::{ChatEntry, Command, Event, PromptStrategyId};

/// Session lifecycle and persistence actor.
///
/// Subscribes to session-related commands and events, mutates [`State`],
/// and emits new commands and events via the [`ActorContext`] message sink.
/// Also persists session snapshots to disk when session state changes.
pub struct SessionPersistenceActor {
    /// Shared application state.
    pub(super) state: State,
    /// Runtime services (user preferences storage for startup config loading).
    pub(super) services: Option<Services>,
    /// The session store service for writing session snapshots.
    pub(super) store: Option<SessionStoreService>,
    /// Token counter for recording token usage in the session ledger.
    pub(super) counter: TiktokenCounter,
}

/// Remove ANSI escape sequences (CSI SGR codes) from a string.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            // Consume '['.
            if chars.next() != Some('[') {
                out.push(ch);
                continue;
            }
            // Consume parameter bytes (digits, semicolons, '?').
            while let Some(&next) = chars.as_str().as_bytes().first() {
                if next.is_ascii_digit() || next == b';' || next == b'?' {
                    chars.next();
                } else {
                    break;
                }
            }
            // Consume the final byte (m, K, H, etc.).
            chars.next();
        } else {
            out.push(ch);
        }
    }
    out
}

/// Build an info chat entry for when a setup command produces no output.
///
/// Shows a yellow "No path returned" line and a white line with the fallback CWD.
fn no_output_info(default_cwd: &std::path::Path) -> ChatEntry {
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};

    ChatEntry::info(vec![
        Line::from(Span::styled(
            "No path returned by setup command.",
            Style::default().fg(Color::Yellow),
        )),
        Line::from(Span::styled(
            format!("Using {} as cwd", default_cwd.display()),
            Style::default().fg(Color::White),
        )),
    ])
}

/// INFO entry shown while a setup command is running.
fn setup_running_msg() -> ChatEntry {
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};

    ChatEntry::info(vec![Line::from(Span::styled(
        "⚙️ Running setup script...",
        Style::default().fg(Color::Yellow),
    ))])
}

/// INFO entry shown when a setup command completes successfully.
fn setup_complete_msg(cwd: &std::path::Path) -> ChatEntry {
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};

    ChatEntry::info(vec![Line::from(Span::styled(
        format!("✅ Setup complete — Using {} as cwd", cwd.display()),
        Style::default().fg(Color::Green),
    ))])
}

/// INFO entry shown while a teardown command is running.
fn teardown_running_msg() -> ChatEntry {
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};

    ChatEntry::info(vec![Line::from(Span::styled(
        "⚙️ Running teardown script...",
        Style::default().fg(Color::Yellow),
    ))])
}

/// INFO entry shown when a teardown-only command succeeds (session is kept open).
fn teardown_success_msg() -> ChatEntry {
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};

    ChatEntry::info(vec![Line::from(Span::styled(
        "✅ Teardown completed successfully.",
        Style::default().fg(Color::Green),
    ))])
}

/// Format a `LifecycleCommandError` into a clean user-facing message.
fn format_lifecycle_error(err: &LifecycleCommandError) -> String {
    match err {
        LifecycleCommandError::CommandFailed {
            exit_code,
            stdout,
            stderr,
        } => {
            let mut parts = vec![format!("Command failed (exit code: {:?})", exit_code)];
            if !stdout.is_empty() {
                parts.push(format!("stdout:\n{}", strip_ansi(stdout)));
            }
            if !stderr.is_empty() {
                parts.push(format!("stderr:\n{}", strip_ansi(stderr)));
            }
            parts.join("\n\n")
        }
        LifecycleCommandError::NoOutput => "Command produced no output".to_owned(),
        LifecycleCommandError::InvalidPath { path } => {
            format!(
                "Path does not exist or cannot be resolved: {}",
                path.display()
            )
        }
        LifecycleCommandError::NotADirectory { path } => {
            format!("Path is not a directory: {}", path.display())
        }
        LifecycleCommandError::ExecutionFailed => "Failed to execute command".to_owned(),
    }
}

/// Dependencies for [`SessionPersistenceActor`].
pub struct SessionPersistenceActorDeps {
    /// Shared application state.
    pub state: State,
    /// Runtime services.
    pub services: Option<Services>,
    /// Session persistence store.
    pub store: Option<SessionStoreService>,
    /// Token counter for usage tracking.
    pub counter: TiktokenCounter,
}

impl Actor for SessionPersistenceActor {
    type Message = NoDirectMsg;
    type Deps = SessionPersistenceActorDeps;

    fn activate(deps: Self::Deps, ctx: &mut ActorContext) -> Self {
        // Persistence subscriptions.
        ctx.subscribe_command::<SessionLoadRequested>();
        ctx.subscribe_command::<LoadSessionPickerEntries>();
        ctx.subscribe_command::<SessionForkRequested>();

        // Session lifecycle subscriptions.
        ctx.subscribe_command::<EnqueueUserMessage>();
        ctx.subscribe_command::<SetChatInputText>();
        ctx.subscribe_command::<PushChatEntry>();
        ctx.subscribe_command::<SendMessage>();
        ctx.subscribe_command::<SessionLoadCompleted>();

        // Lifecycle command subscriptions.
        ctx.subscribe_command::<RunSessionSetup>();
        ctx.subscribe_command::<RunSessionTeardown>();
        ctx.subscribe_command::<SaveNewLifecycleSession>();
        ctx.subscribe_command::<CloseSession>();
        ctx.subscribe_command::<ArchiveSession>();

        // Compaction command subscriptions.
        ctx.subscribe_command::<crate::feat::compaction_actor::protocol::command::BeginCompaction>(
        );
        ctx.subscribe_command::<crate::feat::compaction_actor::protocol::command::EndCompaction>();

        // Event subscriptions.
        ctx.subscribe_event::<PromptAssembled>();
        ctx.subscribe_event::<StreamToken>();
        ctx.subscribe_event::<StreamCompleted>();
        ctx.subscribe_event::<ToolUseStarted>();
        ctx.subscribe_event::<ToolCallReceived>();
        ctx.subscribe_event::<ToolCallStreaming>();
        ctx.subscribe_event::<ToolExecutionCompleted>();
        ctx.subscribe_event::<ToolBatchCompleted>();
        ctx.subscribe_event::<ToolExecutionStarted>();
        ctx.subscribe_event::<ToolExecutionOutput>();
        ctx.subscribe_event::<crate::feat::context::protocol::event::ChatEntryPinChanged>();
        ctx.subscribe_event::<ModelsRefreshed>();
        ctx.subscribe_event::<EnvironmentLoaded>();

        ctx.set_description("Session lifecycle and persistence");

        Self {
            state: deps.state,
            services: deps.services,
            store: deps.store,
            counter: deps.counter,
        }
    }

    async fn handle(&mut self, msg: ActorEnvelope<Self::Message>, ctx: &ActorContext) {
        match msg {
            ActorEnvelope::Event(event) => self.handle_event(&event, ctx).await,
            ActorEnvelope::Command(cmd) => self.handle_command(&cmd, ctx).await,
            _ => {}
        }
    }

    async fn on_shutdown(&mut self, _ctx: &ActorContext) {
        self.run_pending_teardowns().await;
    }
}

impl SessionPersistenceActor {
    /// Dispatches a bus event to the appropriate handler.
    async fn handle_event(&mut self, event: &Event, ctx: &ActorContext) {
        match event {
            Event::PromptAssembled(payload) => self.handle_prompt_assembled(payload, ctx).await,
            Event::StreamToken(payload) => self.on_stream_token(payload),
            Event::StreamCompleted(payload) => self.on_stream_completed(payload, ctx).await,
            Event::ToolUseStarted(payload) => self.on_tool_use_started(payload),
            Event::ToolCallReceived(payload) => self.on_tool_call_received(payload),
            Event::ToolCallStreaming(payload) => self.on_tool_call_streaming(payload),
            Event::ToolExecutionCompleted(payload) => {
                self.on_tool_execution_completed(payload).await;
            }
            Event::ToolBatchCompleted(payload) => {
                self.on_tool_batch_completed(payload, ctx);
            }
            Event::ToolExecutionStarted(payload) => {
                self.on_tool_execution_started(payload);
            }
            Event::ToolExecutionOutput(payload) => {
                self.on_tool_execution_output(payload);
            }
            Event::ModelsRefreshed(payload) => {
                self.on_models_refreshed(payload);
            }
            Event::EnvironmentLoaded(payload) => {
                self.on_environment_loaded(&payload.config, ctx).await;
            }
            Event::ChatEntryPinChanged(payload) => {
                self.save_active_session(&payload.session_id).await;
            }
            _ => {}
        }
    }

    /// Dispatches a command to the appropriate handler.
    async fn handle_command(&mut self, cmd: &Command, ctx: &ActorContext) {
        match cmd {
            Command::SessionLoadRequested(payload) => self.on_load_requested(payload, ctx).await,
            Command::SessionForkRequested(payload) => {
                self.on_session_fork_requested(payload, ctx).await;
            }
            Command::LoadSessionPickerEntries(payload) => {
                self.handle_load_session_picker_entries(payload).await;
            }
            Command::EnqueueUserMessage(payload) => {
                self.handle_enqueue_user_message(payload, ctx).await;
            }
            Command::SetChatInputText(payload) => self.handle_set_chat_input_text(payload),
            Command::PushChatEntry(payload) => self.handle_push_chat_entry(payload, ctx),
            Command::SendMessage(payload) => Self::handle_send_message(payload, ctx),
            Command::SessionLoadCompleted(payload) => {
                self.handle_session_load_completed(payload, ctx).await;
            }
            Command::RunSessionSetup(payload) => {
                self.handle_run_session_setup(payload, ctx).await;
            }
            Command::RunSessionTeardown(payload) => {
                self.handle_run_session_teardown(payload, ctx).await;
            }
            Command::CloseSession(payload) => {
                self.handle_close_session(payload, ctx).await;
            }
            Command::ArchiveSession(payload) => {
                self.handle_archive_session(payload, ctx).await;
            }
            Command::SaveNewLifecycleSession(payload) => {
                self.handle_save_new_lifecycle_session(payload).await;
            }
            Command::BeginCompaction(payload) => {
                self.handle_begin_compaction(payload);
            }
            Command::EndCompaction(payload) => {
                self.handle_end_compaction(payload, ctx).await;
            }
            // Commands NOT subscribed to - these should not arrive.
            Command::AssemblePrompt(..)
            | Command::SendToLlmProvider(..)
            | Command::ExecuteTool(..)
            | Command::ProceedWithShutdown(..)
            | Command::CancelStream(..)
            | Command::CancelCompaction(..)
            | Command::RefreshModels
            | Command::RescanPromptTemplates
            | Command::ExecuteToolBatch(..)
            | Command::RegisterTools(..)
            | Command::ProviderSwitch(..)
            | Command::LoadProviderPickerEntries(..)
            | Command::LoadContextStrategyPickerEntries(..)
            | Command::PinChatEntry(..)
            | Command::UnpinChatEntry(..)
            | Command::SwitchPromptStrategy(..)
            | Command::RestoreStrategyState(..)
            | Command::CancelToolBatch(..)
            | Command::ScanSkills
            | Command::RescanPersonas(..)
            | Command::LoadPersonaPickerEntries(..)
            | Command::UpdatePreferences(..)
            | Command::CompactContext(..) => {}
        }
    }

    /// Loads session picker entries from the session store into `AppState`.
    async fn handle_load_session_picker_entries(&self, _payload: &LoadSessionPickerEntries) {
        if let Some(ref store) = self.store {
            let theme = {
                let state = self.state.read();
                state.frontend.theme.clone()
            };
            let entries =
                crate::feat::session::entries::load_session_entries_from_store(store, &theme).await;
            let mut state = self.state.write();
            state.frontend.session_picker.set_items(entries);
        }
    }

    /// Applies config defaults to the default session profile on startup.
    ///
    /// Loads user preferences and applies `last_model` and `last_strategy`
    /// to the default session, then sends an `UpdatePreferences` command so
    /// the preferences pipeline handles persistence and state sync.
    ///
    /// NOTE: Using `active_session_mut()` is acceptable here because this runs
    /// at startup before any user interaction. There is only one session.
    async fn on_environment_loaded(
        &self,
        _config: &crate::feat::provider_infra::ProvidersConfig,
        ctx: &ActorContext,
    ) {
        let Some(ref services) = self.services else {
            return;
        };

        let prefs = match services.user_preferences_storage.load() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(err = ?e, "session-actor failed to load preferences on startup");
                return;
            }
        };

        let session_id;
        {
            let mut state = self.state.write();

            // Apply config defaults to the default session.
            let session = state.active_session_mut();
            if let Some(ref model) = prefs.last_model {
                session.set_model(model.clone());
            }
            if let Some(ref strategy_str) = prefs.last_strategy {
                let strategy_id = PromptStrategyId::new(strategy_str.clone());
                session.switch_strategy(strategy_id.clone());
            }
            session_id = state.session.active_session_id().clone();
        }

        // Load unarchived sessions from SQLite into memory.
        if let Some(ref store) = self.store {
            let summaries = match store.load_unarchived_summaries().await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(err = ?e, "session-actor failed to load unarchived summaries on startup");
                    return;
                }
            };

            if !summaries.is_empty() {
                // Sort by updated_at descending to find the most recent.
                let mut sorted = summaries;
                sorted.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

                // Load full sessions outside the lock.
                let mut loaded = Vec::new();
                for summary in &sorted {
                    if let Ok(Some(session)) = store.load_session(&summary.session_id).await {
                        loaded.push(session);
                    }
                }

                if !loaded.is_empty() {
                    let mut state = self.state.write();

                    for session in loaded {
                        state
                            .session
                            .sessions_mut()
                            .insert(session.session_id().clone(), session);
                    }

                    // Remove the empty default session if other sessions exist.
                    let default_is_empty = state
                        .session
                        .sessions()
                        .get(&session_id)
                        .is_some_and(|s| s.is_empty());
                    if default_is_empty && state.session.sessions().len() > 1 {
                        state.session.sessions_mut().remove(&session_id);
                    }

                    // Set active to the most recently updated unarchived session.
                    if let Some(most_recent) = sorted.first() {
                        state.session.set_active(most_recent.session_id.clone());
                    }
                }
            }
        }

        // Send UpdatePreferences command so the pipeline handles persistence + state sync.
        if let Err(e) = ctx.send_command(Command::UpdatePreferences(crate::feat::preferences_actor::protocol::command::UpdatePreferences {
                updates: vec![
                    crate::feat::preferences_actor::protocol::command::PreferenceUpdate::SetLastModel(prefs.last_model.clone()),
                    crate::feat::preferences_actor::protocol::command::PreferenceUpdate::SetLastStrategy(prefs.last_strategy.clone()),
                ],
            })) {
            tracing::warn!(err = ?e, "session-actor failed to send UpdatePreferences on startup");
        }

        // Emit SwitchPromptStrategy so the context actor initializes the strategy.
        if let Some(ref strategy_str) = prefs.last_strategy {
            // Get the current active session (may have changed after loading unarchived sessions).
            let active_id = self.state.read().session.active_session_id().clone();
            let strategy_id = PromptStrategyId::new(strategy_str.clone());
            if let Err(e) = ctx.send_command(Command::SwitchPromptStrategy(SwitchPromptStrategy {
                session_id: active_id,
                strategy_id,
            })) {
                tracing::error!(err = ?e, "failed to send command");
            }
        }
    }

    /// RunSessionSetup: execute the lifecycle setup command asynchronously.
    ///
    /// On success, sets the session's CWD to the command's output.
    /// On failure, sets the default CWD and pushes an error entry.
    async fn handle_run_session_setup(&self, payload: &RunSessionSetup, ctx: &ActorContext) {
        // Push "running" info entry so the user sees feedback immediately.
        {
            let mut state = self.state.write();
            if let Some(session) = state.session.sessions_mut().get_mut(&payload.session_id) {
                session.push_entry(setup_running_msg());
            }
        }

        let result = run_setup_command(&payload.command).await;

        match result {
            Ok(cwd) => {
                let mut state = self.state.write();
                if let Some(session) = state.session.sessions_mut().get_mut(&payload.session_id) {
                    session.set_cwd(cwd.clone());
                    session.push_entry(setup_complete_msg(&cwd));
                }
                drop(state);

                if let Err(e) =
                    ctx.send_event(Event::SessionSetupCompleted(SessionSetupCompleted {
                        session_id: payload.session_id.clone(),
                        cwd,
                        error: None,
                    }))
                {
                    tracing::warn!(err = ?e, "session-actor failed to emit SessionSetupCompleted");
                }
            }
            Err(report) => {
                let is_no_output = matches!(
                    report.downcast_ref::<LifecycleCommandError>(),
                    Some(LifecycleCommandError::NoOutput)
                );
                let error_msg =
                    if let Some(cmd_err) = report.downcast_ref::<LifecycleCommandError>() {
                        format_lifecycle_error(cmd_err)
                    } else {
                        strip_ansi(&format!("{report:#?}"))
                    };
                let default_cwd = {
                    let mut state = self.state.write();
                    let default = state.session.default_cwd().clone();
                    if let Some(session) = state.session.sessions_mut().get_mut(&payload.session_id)
                    {
                        session.set_cwd(default.clone());
                        let entry = if is_no_output {
                            no_output_info(&default)
                        } else {
                            ChatEntry::error(&error_msg)
                        };
                        session.push_entry(entry);
                    }
                    default
                };

                if let Err(e) =
                    ctx.send_event(Event::SessionSetupCompleted(SessionSetupCompleted {
                        session_id: payload.session_id.clone(),
                        cwd: default_cwd,
                        error: Some(error_msg),
                    }))
                {
                    tracing::warn!(err = ?e, "session-actor failed to emit SessionSetupCompleted");
                }
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    async fn handle_run_session_teardown(&self, payload: &RunSessionTeardown, ctx: &ActorContext) {
        // Push "running" info entry so the user sees feedback immediately.
        {
            let mut state = self.state.write();
            if let Some(session) = state.session.sessions_mut().get_mut(&payload.session_id) {
                session.push_entry(teardown_running_msg());
            }
        }

        let result = run_teardown_command(&payload.command).await;

        match result {
            Ok(()) => {
                if payload.close_on_success {
                    // Teardown succeeded - remove session and switch active.
                    {
                        let mut state = self.state.write();
                        state.session.sessions_mut().remove(&payload.session_id);
                        if state.session.sessions().is_empty() {
                            let model = state
                                .frontend
                                .preferences
                                .last_model
                                .clone()
                                .unwrap_or_else(|| {
                                    crate::feat::provider_infra::NO_PROVIDER_ID.to_owned()
                                });
                            let strategy = state
                                .frontend
                                .preferences
                                .last_strategy
                                .as_deref()
                                .map_or_else(
                                    crate::protocol::PromptStrategyId::passthrough,
                                    crate::protocol::PromptStrategyId::new,
                                );
                            let token_budget =
                                state.frontend.preferences.context_token_budget.budget;
                            let sliding_window_size =
                                state.frontend.preferences.context_sliding_window.size;
                            let new_session = ChatSessionState::new_with_profile(
                                crate::feat::session::profile::SessionProfile::from_config(
                                    model,
                                    strategy,
                                    token_budget,
                                    sliding_window_size,
                                ),
                            );
                            let new_id = new_session.session_id().clone();
                            state
                                .session
                                .sessions_mut()
                                .insert(new_id.clone(), new_session);
                            state.session.set_active(new_id);
                        } else if *state.session.active_session_id() == payload.session_id {
                            let next_id = state
                                .session
                                .sessions()
                                .keys()
                                .next()
                                .expect("sessions is non-empty")
                                .clone();
                            state.session.set_active(next_id);
                        }
                    }

                    if let Err(e) =
                        ctx.send_event(Event::SessionTeardownFinished(SessionTeardownFinished {
                            session_id: payload.session_id.clone(),
                            error: None,
                        }))
                    {
                        tracing::warn!(err = ?e, "session-actor failed to emit SessionTeardownFinished");
                    }

                    if let Err(e) = ctx.send_event(Event::SessionClosed(SessionClosed {
                        session_id: payload.session_id.clone(),
                    })) {
                        tracing::warn!(err = ?e, "session-actor failed to emit SessionClosed");
                    }
                } else {
                    // Teardown-only success — keep session, push info entry.
                    {
                        let mut state = self.state.write();
                        if let Some(session) =
                            state.session.sessions_mut().get_mut(&payload.session_id)
                        {
                            session.push_entry(teardown_success_msg());
                        }
                    }

                    if let Err(e) =
                        ctx.send_event(Event::SessionTeardownFinished(SessionTeardownFinished {
                            session_id: payload.session_id.clone(),
                            error: None,
                        }))
                    {
                        tracing::warn!(err = ?e, "session-actor failed to emit SessionTeardownFinished");
                    }
                }
            }
            Err(report) => {
                let error_msg =
                    if let Some(cmd_err) = report.downcast_ref::<LifecycleCommandError>() {
                        format_lifecycle_error(cmd_err)
                    } else {
                        strip_ansi(&format!("{report:#?}"))
                    };
                {
                    let mut state = self.state.write();
                    if let Some(session) = state.session.sessions_mut().get_mut(&payload.session_id)
                    {
                        session.push_entry(ChatEntry::error(&error_msg));
                    }
                }

                self.save_active_session(&payload.session_id).await;

                if let Err(e) =
                    ctx.send_event(Event::SessionTeardownFinished(SessionTeardownFinished {
                        session_id: payload.session_id.clone(),
                        error: Some(error_msg),
                    }))
                {
                    tracing::warn!(err = ?e, "session-actor failed to emit SessionTeardownFinished");
                }
            }
        }
    }

    /// CloseSession: unified close handler.
    ///
    /// - Empty session → remove from map, no archive.
    /// - Has teardown lifecycle → run teardown first, then archive on success.
    /// - No teardown → archive in SQLite, then remove from map.
    async fn handle_close_session(&self, payload: &CloseSession, ctx: &ActorContext) {
        // No-op if the session doesn't exist.
        let session_info = {
            let state = self.state.read();
            let Some(session) = state.session.sessions().get(&payload.session_id) else {
                return;
            };
            // Collect what we need before releasing the lock.
            let is_empty = session.history().is_empty();
            let lifecycle_name = session.lifecycle_name().map(str::to_owned);
            let lifecycle_args = session.lifecycle_args().to_vec();
            (is_empty, lifecycle_name, lifecycle_args)
        };

        let (is_empty, lifecycle_name, lifecycle_args) = session_info;

        if is_empty {
            // Empty session — remove without archiving.
            self.close_session_inline(&payload.session_id, ctx);
            return;
        }

        // Check for teardown lifecycle.
        let teardown_cmd = lifecycle_name.as_deref().and_then(|name| {
            let state = self.state.read();
            state
                .frontend
                .preferences
                .session_lifecycles
                .iter()
                .find(|l| l.name == name)
                .and_then(|l| l.teardown_command.clone())
        });

        if let Some(teardown_cmd) = teardown_cmd {
            // Has teardown — run it, then archive on success.
            self.close_session_with_teardown(
                &payload.session_id,
                &teardown_cmd,
                &lifecycle_args,
                ctx,
            )
            .await;
        } else {
            // No teardown — archive directly.
            if let Some(ref store) = self.store
                && let Err(e) = store.set_archived(&payload.session_id, true).await
            {
                tracing::warn!(err = ?e, "failed to archive session");
            }
            self.close_session_inline(&payload.session_id, ctx);
            if let Err(e) = ctx.send_event(Event::SessionArchived(SessionArchived {
                session_id: payload.session_id.clone(),
            })) {
                tracing::warn!(err = ?e, "session-actor failed to emit SessionArchived");
            }
        }
    }

    /// Remove session from HashMap, create new if empty, switch active,
    /// adjust sidebar cursor, and emit `SessionClosed`.
    fn close_session_inline(&self, session_id: &crate::protocol::SessionId, ctx: &ActorContext) {
        {
            let mut state = self.state.write();
            state.session.sessions_mut().remove(session_id);

            if state.session.sessions().is_empty() {
                let model = state
                    .frontend
                    .preferences
                    .last_model
                    .clone()
                    .unwrap_or_else(|| crate::feat::provider_infra::NO_PROVIDER_ID.to_owned());
                let strategy = state
                    .frontend
                    .preferences
                    .last_strategy
                    .as_deref()
                    .map_or_else(PromptStrategyId::passthrough, PromptStrategyId::new);
                let token_budget = state.frontend.preferences.context_token_budget.budget;
                let sliding_window_size = state.frontend.preferences.context_sliding_window.size;
                let new_session = ChatSessionState::new_with_profile(
                    crate::feat::session::profile::SessionProfile::from_config(
                        model,
                        strategy,
                        token_budget,
                        sliding_window_size,
                    ),
                );
                let new_id = new_session.session_id().clone();
                state
                    .session
                    .sessions_mut()
                    .insert(new_id.clone(), new_session);
                state.session.set_active(new_id);
            } else if *state.session.active_session_id() == *session_id {
                let next_id = state
                    .session
                    .sessions()
                    .keys()
                    .next()
                    .expect("sessions is non-empty")
                    .clone();
                state.session.set_active(next_id);
            }
        }

        if let Err(e) = ctx.send_event(Event::SessionClosed(SessionClosed {
            session_id: session_id.clone(),
        })) {
            tracing::warn!(err = ?e, "session-actor failed to emit SessionClosed");
        }
    }

    /// Run teardown command, then archive + close on success.
    /// On failure, push error entry and emit teardown-finished with error.
    async fn close_session_with_teardown(
        &self,
        session_id: &crate::protocol::SessionId,
        teardown_cmd: &str,
        lifecycle_args: &[String],
        ctx: &ActorContext,
    ) {
        use crate::feat::session_lifecycle::command_template::CommandTemplate;

        // Push "running" info entry.
        {
            let mut state = self.state.write();
            if let Some(session) = state.session.sessions_mut().get_mut(session_id) {
                session.push_entry(teardown_running_msg());
            }
        }

        let template = CommandTemplate::parse(teardown_cmd);
        let rendered = if lifecycle_args.is_empty() {
            teardown_cmd.to_owned()
        } else {
            template.render(lifecycle_args)
        };

        let result = run_teardown_command(&rendered).await;

        match result {
            Ok(()) => {
                // Archive + close on success.
                if let Some(ref store) = self.store
                    && let Err(e) = store.set_archived(session_id, true).await
                {
                    tracing::warn!(err = ?e, "failed to archive session after teardown");
                }
                self.close_session_inline(session_id, ctx);

                if let Err(e) = ctx.send_event(Event::SessionArchived(SessionArchived {
                    session_id: session_id.clone(),
                })) {
                    tracing::warn!(err = ?e, "session-actor failed to emit SessionArchived");
                }

                if let Err(e) =
                    ctx.send_event(Event::SessionTeardownFinished(SessionTeardownFinished {
                        session_id: session_id.clone(),
                        error: None,
                    }))
                {
                    tracing::warn!(err = ?e, "session-actor failed to emit SessionTeardownFinished");
                }
            }
            Err(report) => {
                let error_msg =
                    if let Some(cmd_err) = report.downcast_ref::<LifecycleCommandError>() {
                        format_lifecycle_error(cmd_err)
                    } else {
                        strip_ansi(&format!("{report:#?}"))
                    };
                {
                    let mut state = self.state.write();
                    if let Some(session) = state.session.sessions_mut().get_mut(session_id) {
                        session.push_entry(ChatEntry::error(&error_msg));
                    }
                }

                self.save_active_session(session_id).await;

                if let Err(e) =
                    ctx.send_event(Event::SessionTeardownFinished(SessionTeardownFinished {
                        session_id: session_id.clone(),
                        error: Some(error_msg),
                    }))
                {
                    tracing::warn!(err = ?e, "session-actor failed to emit SessionTeardownFinished");
                }
            }
        }
    }

    /// ArchiveSession: archive without running teardown.
    ///
    /// - Empty session → remove from map, no archive.
    /// - Non-empty → archive in SQLite, remove from map, emit events.
    async fn handle_archive_session(&self, payload: &ArchiveSession, ctx: &ActorContext) {
        let is_empty = {
            let state = self.state.read();
            let Some(session) = state.session.sessions().get(&payload.session_id) else {
                return;
            };
            session.history().is_empty()
        };

        if is_empty {
            self.close_session_inline(&payload.session_id, ctx);
            return;
        }

        // Archive in SQLite.
        if let Some(ref store) = self.store
            && let Err(e) = store.set_archived(&payload.session_id, true).await
        {
            tracing::warn!(err = ?e, "failed to archive session");
        }

        self.close_session_inline(&payload.session_id, ctx);

        if let Err(e) = ctx.send_event(Event::SessionArchived(SessionArchived {
            session_id: payload.session_id.clone(),
        })) {
            tracing::warn!(err = ?e, "session-actor failed to emit SessionArchived");
        }
    }

    /// SaveNewLifecycleSession: persist the session immediately.
    ///
    /// Called right after the IntentHandler creates a lifecycle session
    /// so that the session metadata survives an app crash during setup.
    async fn handle_save_new_lifecycle_session(&self, payload: &SaveNewLifecycleSession) {
        self.save_active_session(&payload.session_id).await;
    }

    /// Runs teardown commands for all open sessions that have a lifecycle with teardown.
    ///
    /// Called during coordinated shutdown. Runs commands sequentially -
    /// teardown order matters (each must complete before the next starts).
    async fn run_pending_teardowns(&self) {
        use crate::feat::session_lifecycle::command_template::CommandTemplate;

        let teardown_jobs: Vec<(crate::protocol::SessionId, String, String)> = {
            let state = self.state.read();
            let mut jobs = Vec::new();
            for (id, session) in state.session.sessions() {
                let Some(lifecycle_name) = session.lifecycle_name() else {
                    continue;
                };
                let teardown_cmd = state
                    .frontend
                    .preferences
                    .session_lifecycles
                    .iter()
                    .find(|l| l.name == lifecycle_name)
                    .and_then(|l| l.teardown_command.clone());
                let Some(teardown_cmd) = teardown_cmd else {
                    continue;
                };
                let args = session.lifecycle_args().to_vec();
                let template = CommandTemplate::parse(&teardown_cmd);
                let rendered = if args.is_empty() {
                    teardown_cmd
                } else {
                    template.render(&args)
                };
                jobs.push((id.clone(), lifecycle_name.to_owned(), rendered));
            }
            jobs
        };

        for (session_id, lifecycle_name, command) in teardown_jobs {
            tracing::info!(
                session_id = %session_id,
                lifecycle = %lifecycle_name,
                "running teardown during shutdown"
            );
            match run_teardown_command(&command).await {
                Ok(()) => {
                    tracing::info!(
                        session_id = %session_id,
                        "teardown completed during shutdown"
                    );
                }
                Err(report) => {
                    tracing::warn!(
                        session_id = %session_id,
                        err = ?report,
                        "teardown failed during shutdown"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing)]
    use super::{
        no_output_info, setup_complete_msg, setup_running_msg, strip_ansi, teardown_running_msg,
    };
    use crate::common::actor::{ActorContext, RecordingSink};
    use crate::common::app_state::AppState;
    use crate::common::state::State;
    use crate::feat::context::strategy::token_estimator::TiktokenCounter;
    use crate::feat::provider::protocol::event::{StreamCompleted, StreamCompletedReason};
    use crate::feat::session::chat_session::{ChatSessionState, SessionPhase};
    use crate::feat::tools_actor::protocol::event::ToolBatchCompleted;
    use crate::feat::tools_actor::tool_types::{ToolCall, ToolResult};
    use crate::protocol::{ChatEntry, ChatEntryKind, Command};
    use ratatui::style::Color;
    use std::path::Path;

    #[rstest::rstest]
    fn strip_ansi_removes_bold_codes() {
        // Given text with bold ANSI codes.
        let input = "\x1b[1mbold text\x1b[22m";

        // When stripping ANSI.
        let result = strip_ansi(input);

        // Then the ANSI codes are removed.
        assert_eq!(result, "bold text");
    }

    #[rstest::rstest]
    fn strip_ansi_removes_color_codes() {
        let input = "\x1b[31mred\x1b[0m";
        let result = strip_ansi(input);
        assert_eq!(result, "red");
    }

    #[rstest::rstest]
    fn strip_ansi_passes_plain_text() {
        let input = "hello world";
        let result = strip_ansi(input);
        assert_eq!(result, "hello world");
    }

    #[rstest::rstest]
    fn strip_ansi_handles_chained_codes() {
        let input = "\x1b[1m\x1b[31mbold red\x1b[0m";
        let result = strip_ansi(input);
        assert_eq!(result, "bold red");
    }

    #[rstest::rstest]
    fn strip_ansi_handles_complex_csi_sequences() {
        // 38;5;196 is foreground 256-color (bright red).
        let input = "\x1b[38;5;196mcolored\x1b[0m";
        let result = strip_ansi(input);
        assert_eq!(result, "colored");
    }

    #[rstest::rstest]
    fn strip_ansi_handles_empty_string() {
        let result = strip_ansi("");
        assert_eq!(result, "");
    }

    #[rstest::rstest]
    fn strip_ansi_handles_text_with_no_ansi() {
        let input = "normal text\nwith newlines";
        let result = strip_ansi(input);
        assert_eq!(result, "normal text\nwith newlines");
    }

    #[rstest::rstest]
    fn no_output_info_is_info_entry_with_two_lines() {
        // Given a default CWD path.
        let cwd = Path::new("/tmp/test-project");

        // When building the no-output info entry.
        let entry = no_output_info(cwd);

        // Then it is an Info entry with exactly 2 lines.
        let ChatEntryKind::Info(lines) = &entry.kind else {
            panic!("expected Info entry, got {:?}", entry.kind);
        };
        assert_eq!(lines.len(), 2);
    }

    #[rstest::rstest]
    fn no_output_info_first_line_is_yellow() {
        // Given a default CWD path.
        let cwd = Path::new("/tmp/test-project");

        // When building the no-output info entry.
        let entry = no_output_info(cwd);

        // Then the first line has yellow foreground.
        let ChatEntryKind::Info(lines) = &entry.kind else {
            panic!("expected Info entry");
        };
        let first_span = &lines[0].spans[0];
        assert_eq!(first_span.content, "No path returned by setup command.");
        assert_eq!(first_span.style.fg, Some(Color::Yellow));
    }

    #[rstest::rstest]
    fn no_output_info_second_line_is_white_with_cwd() {
        // Given an absolute default CWD path.
        let cwd = Path::new("/tmp/test-project");

        // When building the no-output info entry.
        let entry = no_output_info(cwd);

        // Then the second line is white and contains the absolute CWD.
        let ChatEntryKind::Info(lines) = &entry.kind else {
            panic!("expected Info entry");
        };
        let second_span = &lines[1].spans[0];
        assert_eq!(second_span.style.fg, Some(Color::White));
        assert!(second_span.content.contains("/tmp/test-project"));
        assert!(second_span.content.starts_with("Using "));
        assert!(second_span.content.ends_with(" as cwd"));
    }

    #[rstest::rstest]
    fn setup_running_msg_is_yellow_with_gear_emoji() {
        // When building the setup running message.
        let entry = setup_running_msg();

        // Then it is an Info entry with one line.
        let ChatEntryKind::Info(lines) = &entry.kind else {
            panic!("expected Info entry");
        };
        assert_eq!(lines.len(), 1);
        let span = &lines[0].spans[0];
        assert!(span.content.contains("⚙️"));
        assert!(span.content.contains("Running setup script"));
        assert_eq!(span.style.fg, Some(Color::Yellow));
    }

    #[rstest::rstest]
    fn setup_complete_msg_is_green_with_checkmark_and_cwd() {
        // Given a CWD path.
        let cwd = Path::new("/tmp/my-project");

        // When building the setup complete message.
        let entry = setup_complete_msg(cwd);

        // Then it is an Info entry with the checkmark and CWD.
        let ChatEntryKind::Info(lines) = &entry.kind else {
            panic!("expected Info entry");
        };
        assert_eq!(lines.len(), 1);
        let span = &lines[0].spans[0];
        assert!(span.content.contains("✅"));
        assert!(span.content.contains("Setup complete"));
        assert!(span.content.contains("Using /tmp/my-project as cwd"));
        assert_eq!(span.style.fg, Some(Color::Green));
    }

    #[rstest::rstest]
    fn teardown_running_msg_is_yellow_with_gear_emoji() {
        // When building the teardown running message.
        let entry = teardown_running_msg();

        // Then it is an Info entry with one line.
        let ChatEntryKind::Info(lines) = &entry.kind else {
            panic!("expected Info entry");
        };
        assert_eq!(lines.len(), 1);
        let span = &lines[0].spans[0];
        assert!(span.content.contains("⚙️"));
        assert!(span.content.contains("Running teardown script"));
        assert_eq!(span.style.fg, Some(Color::Yellow));
    }

    // --- StreamCompleted(Error) handler tests ---

    fn test_actor() -> super::SessionPersistenceActor {
        super::SessionPersistenceActor {
            state: State::new(AppState::default()),
            services: None,
            store: None,
            counter: TiktokenCounter::o200k_base(),
        }
    }

    fn test_context() -> (std::sync::Arc<RecordingSink>, ActorContext) {
        let sink = std::sync::Arc::new(RecordingSink::new());
        let ctx = ActorContext::new("test-session-actor", sink.clone());
        (sink, ctx)
    }

    #[tokio::test]
    async fn on_stream_completed_error_reason_finishes_streaming() {
        // Given a session actor with a session in streaming state.
        let actor = test_actor();
        let (_sink, ctx) = test_context();
        let session_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.begin_streaming();
            assert!(matches!(session.phase(), SessionPhase::Streaming));
            state.session.active_session_id().clone()
        };

        // When handling StreamCompleted with Error reason.
        let event = StreamCompleted {
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Error,
            assistant_content: None,
            tool_calls: None,
            cost: None,
        };
        actor.on_stream_completed(&event, &ctx).await;

        // Then the session is no longer streaming.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert!(!matches!(session.phase(), SessionPhase::Streaming));
    }

    #[tokio::test]
    async fn on_stream_completed_error_reason_does_not_drain_queue() {
        // Given a session actor with a session in streaming state and a queued message.
        let actor = test_actor();
        let (_sink, ctx) = test_context();
        let session_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.begin_streaming();
            session.enqueue_message(ChatEntry::user("queued message"));
            assert_eq!(session.queue_len(), 1);
            state.session.active_session_id().clone()
        };

        // When handling StreamCompleted with Error reason.
        let event = StreamCompleted {
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Error,
            assistant_content: None,
            tool_calls: None,
            cost: None,
        };
        actor.on_stream_completed(&event, &ctx).await;

        // Then the queued message is still in the queue.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert_eq!(session.queue_len(), 1);
    }

    #[tokio::test]
    async fn on_tool_batch_completed_emits_assemble_prompt() {
        // Given a session with tool call and result entries in history.
        let actor = test_actor();
        let (sink, ctx) = test_context();
        let session_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("list files"));
            session.push_entry(ChatEntry::assistant("checking"));
            session.push_entry(ChatEntry::tool_call("tc-1", "bash", r#"{"command":"ls"}"#));
            session.push_entry(ChatEntry::assistant("here are the files"));
            state.session.active_session_id().clone()
        };

        // When handling ToolBatchCompleted.
        let event = ToolBatchCompleted {
            session_id: session_id.clone(),
            results: vec![ToolResult {
                tool_call_id: "tc-1".to_owned(),
                name: "bash".to_owned(),
                content: "file1.txt".to_owned(),
                success: true,
                full_content: None,
                truncation: None,
            }],
        };
        actor.on_tool_batch_completed(&event, &ctx);

        // Then an AssemblePrompt command was emitted.
        let commands = sink.commands();
        let assemble = commands
            .iter()
            .find(|c| matches!(c, Command::AssemblePrompt(_)));
        assert!(
            assemble.is_some(),
            "expected AssemblePrompt command to be emitted"
        );
    }

    #[tokio::test]
    async fn on_tool_batch_completed_transitions_session_to_sending() {
        // Given a session in sending state (set by on_stream_completed for ToolUse).
        let actor = test_actor();
        let (_sink, ctx) = test_context();
        let session_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.begin_streaming();
            session.finish_streaming(true);
            session.begin_sending();
            state.session.active_session_id().clone()
        };

        // When handling ToolBatchCompleted.
        let event = ToolBatchCompleted {
            session_id: session_id.clone(),
            results: vec![],
        };
        actor.on_tool_batch_completed(&event, &ctx);

        // Then the session is still in sending state.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert!(matches!(session.phase(), SessionPhase::Sending));
    }

    #[tokio::test]
    async fn on_stream_completed_tool_use_counts_tool_call_arguments() {
        // Given a session with a token record (from PromptAssembled) in streaming state.
        let actor = test_actor();
        let (_sink, ctx) = test_context();
        let session_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.push_token_record(crate::feat::session::token_stats::TokenRecord {
                timestamp: jiff::Timestamp::now(),
                tokens_sent: 100,
                tokens_received: 0,
                cost: None,
            });
            session.begin_streaming();
            state.session.active_session_id().clone()
        };

        // When handling StreamCompleted(ToolUse) with tool calls.
        let event = StreamCompleted {
            session_id: session_id.clone(),
            reason: StreamCompletedReason::ToolUse,
            assistant_content: Some("checking".to_owned()),
            tool_calls: Some(vec![ToolCall {
                id: "tc-1".to_owned(),
                name: "bash".to_owned(),
                arguments: r#"{"command":"ls -la /very/long/path"}"#.to_owned(),
            }]),
            cost: None,
        };
        actor.on_stream_completed(&event, &ctx).await;

        // Then the token record includes tokens from both text and tool call arguments.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let ledger = session.token_ledger();
        assert_eq!(ledger.len(), 1);
        // tokens_received should be > just "checking" (2 tokens with tiktoken).
        // It must include the tool call arguments and name.
        assert!(
            ledger[0].tokens_received > 2,
            "expected tokens_received > 2 (text only), got {}",
            ledger[0].tokens_received
        );
    }

    // --- Teardown failure handler tests ---

    #[tokio::test]
    async fn teardown_failure_does_not_switch_active_session() {
        // Given a session actor with two sessions, second is targeted for teardown.
        let actor = test_actor();
        let (sink, ctx) = test_context();
        let (target_id, original_active) = {
            let mut state = actor.state.write();
            let original_active = state.session.active_session_id().clone();
            let second = ChatSessionState::new();
            let second_id = second.session_id().clone();
            state
                .session
                .sessions_mut()
                .insert(second_id.clone(), second);
            (second_id, original_active)
        };

        // When handling a teardown command that fails targeting the non-active session.
        actor
            .handle_run_session_teardown(
                &crate::feat::session_lifecycle::protocol::command::RunSessionTeardown {
                    session_id: target_id,
                    command: "exit 1".to_owned(),
                    args: vec![],
                    close_on_success: true,
                },
                &ctx,
            )
            .await;

        // Then the active session is unchanged.
        let state = actor.state.read();
        assert_eq!(*state.session.active_session_id(), original_active);
        drop(state);

        // And SessionTeardownFinished was emitted with error.
        let events = sink.events();
        let teardown_evt = events
            .iter()
            .find(|e| matches!(e, crate::protocol::Event::SessionTeardownFinished(..)));
        assert!(
            teardown_evt.is_some(),
            "expected SessionTeardownFinished event"
        );
    }

    #[tokio::test]
    async fn teardown_failure_does_not_push_input_scope() {
        // Given a session actor in Normal scope.
        let actor = test_actor();
        let (_sink, ctx) = test_context();
        let session_id = {
            let state = actor.state.read();
            state.session.active_session_id().clone()
        };

        // When handling a teardown command that fails.
        actor
            .handle_run_session_teardown(
                &crate::feat::session_lifecycle::protocol::command::RunSessionTeardown {
                    session_id,
                    command: "exit 1".to_owned(),
                    args: vec![],
                    close_on_success: true,
                },
                &ctx,
            )
            .await;

        // Then the scope stack does not have Input on it.
        let state = actor.state.read();
        assert!(!matches!(
            state.frontend.scope_stack.current(),
            crate::common::app_state::FocusScope::Input
        ));
    }

    #[tokio::test]
    async fn teardown_failure_pushes_error_entry_to_session() {
        // Given a session actor with a session.
        let actor = test_actor();
        let (_sink, ctx) = test_context();
        let session_id = {
            let state = actor.state.read();
            state.session.active_session_id().clone()
        };

        // When handling a teardown command that fails.
        actor
            .handle_run_session_teardown(
                &crate::feat::session_lifecycle::protocol::command::RunSessionTeardown {
                    session_id: session_id.clone(),
                    command: "exit 1".to_owned(),
                    args: vec![],
                    close_on_success: true,
                },
                &ctx,
            )
            .await;

        // Then the session has an error entry.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let last = session.history().last().expect("has an entry");
        assert!(matches!(&last.kind, ChatEntryKind::Error(msg) if msg.contains("exit code")));
    }

    #[tokio::test]
    async fn teardown_failure_emits_session_teardown_completed_with_error() {
        // Given a session actor with a session.
        let actor = test_actor();
        let (sink, ctx) = test_context();
        let session_id = {
            let state = actor.state.read();
            state.session.active_session_id().clone()
        };

        // When handling a teardown command that fails.
        actor
            .handle_run_session_teardown(
                &crate::feat::session_lifecycle::protocol::command::RunSessionTeardown {
                    session_id: session_id.clone(),
                    command: "exit 1".to_owned(),
                    args: vec![],
                    close_on_success: true,
                },
                &ctx,
            )
            .await;

        // Then SessionTeardownFinished is emitted with an error message.
        let events = sink.events();
        let found = events.iter().any(|e| {
            matches!(
                e,
                crate::protocol::Event::SessionTeardownFinished(
                    crate::feat::session_lifecycle::protocol::event::SessionTeardownFinished {
                        error: Some(..),
                        session_id: sid,
                    }
                ) if sid == &session_id
            )
        });
        assert!(found, "expected SessionTeardownFinished with error");
    }

    // --- CloseSession handler tests ---

    #[tokio::test]
    async fn remove_session_removes_session_from_hashmap() {
        // Given a session actor with two sessions.
        let actor = test_actor();
        let (sink, ctx) = test_context();
        let second = ChatSessionState::new();
        let second_id = second.session_id().clone();
        {
            let mut state = actor.state.write();
            state
                .session
                .sessions_mut()
                .insert(second_id.clone(), second);
        }

        // When handling CloseSession for the second session.
        actor
            .handle_close_session(
                &crate::feat::session::protocol::close_session::CloseSession {
                    session_id: second_id.clone(),
                },
                &ctx,
            )
            .await;

        // Then the second session is removed.
        let state = actor.state.read();
        assert!(!state.session.sessions().contains_key(&second_id));
        // And the first session still exists.
        assert_eq!(state.session.sessions().len(), 1);
        drop(state);

        // And SessionClosed is emitted.
        let events = sink.events();
        let found = events.iter().any(|e| {
            matches!(
                e,
                crate::protocol::Event::SessionClosed(
                    crate::feat::session::protocol::session_closed::SessionClosed {
                        session_id: sid,
                    }
                ) if sid == &second_id
            )
        });
        assert!(found, "expected SessionClosed event");
    }

    #[tokio::test]
    async fn remove_session_creates_new_session_when_last_removed() {
        // Given a session actor with only one session.
        let actor = test_actor();
        let (sink, ctx) = test_context();
        let only_id = {
            let state = actor.state.read();
            state.session.active_session_id().clone()
        };

        // When handling CloseSession for the only session.
        actor
            .handle_close_session(
                &crate::feat::session::protocol::close_session::CloseSession {
                    session_id: only_id.clone(),
                },
                &ctx,
            )
            .await;

        // Then the only session is removed and a new one is created.
        let state = actor.state.read();
        assert!(!state.session.sessions().contains_key(&only_id));
        assert_eq!(state.session.sessions().len(), 1);
        assert_ne!(*state.session.active_session_id(), only_id);
        drop(state);

        // And SessionClosed is emitted.
        let events = sink.events();
        let found = events.iter().any(|e| {
            matches!(
                e,
                crate::protocol::Event::SessionClosed(
                    crate::feat::session::protocol::session_closed::SessionClosed {
                        session_id: sid,
                    }
                ) if sid == &only_id
            )
        });
        assert!(found, "expected SessionClosed event");
    }

    #[tokio::test]
    async fn remove_session_switches_active_when_active_is_removed() {
        // Given a session actor with two sessions, active is the second.
        let actor = test_actor();
        let (_sink, ctx) = test_context();
        let second = ChatSessionState::new();
        let second_id = second.session_id().clone();
        {
            let mut state = actor.state.write();
            state
                .session
                .sessions_mut()
                .insert(second_id.clone(), second);
            state.session.set_active(second_id.clone());
        }

        // When handling CloseSession for the active session.
        actor
            .handle_close_session(
                &crate::feat::session::protocol::close_session::CloseSession {
                    session_id: second_id.clone(),
                },
                &ctx,
            )
            .await;

        // Then active session is switched to the remaining one.
        let state = actor.state.read();
        assert_ne!(*state.session.active_session_id(), second_id);
        assert_eq!(state.session.sessions().len(), 1);
    }

    #[tokio::test]
    async fn remove_session_emits_session_removed_event() {
        // Given a session actor with two sessions.
        let actor = test_actor();
        let (sink, ctx) = test_context();
        let second = ChatSessionState::new();
        let second_id = second.session_id().clone();
        {
            let mut state = actor.state.write();
            state
                .session
                .sessions_mut()
                .insert(second_id.clone(), second);
        }

        // When handling CloseSession.
        actor
            .handle_close_session(
                &crate::feat::session::protocol::close_session::CloseSession {
                    session_id: second_id.clone(),
                },
                &ctx,
            )
            .await;

        // Then exactly one SessionClosed event is emitted for the correct session.
        let events = sink.events();
        let count = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    crate::protocol::Event::SessionClosed(
                        crate::feat::session::protocol::session_closed::SessionClosed {
                            session_id: sid,
                        }
                    ) if sid == &second_id
                )
            })
            .count();
        assert_eq!(count, 1, "expected exactly one SessionClosed event");
    }

    #[tokio::test]
    async fn remove_session_is_noop_if_session_does_not_exist() {
        // Given a session actor with one session.
        let actor = test_actor();
        let (sink, ctx) = test_context();
        let fake_id = crate::protocol::SessionId::new();
        let original_len = {
            let state = actor.state.read();
            state.session.sessions().len()
        };

        // When handling CloseSession for a nonexistent session.
        actor
            .handle_close_session(
                &crate::feat::session::protocol::close_session::CloseSession {
                    session_id: fake_id.clone(),
                },
                &ctx,
            )
            .await;

        // Then nothing changes.
        let state = actor.state.read();
        assert_eq!(state.session.sessions().len(), original_len);
        drop(state);

        // And no SessionClosed event is emitted.
        let events = sink.events();
        let found = events.iter().any(|e| {
            matches!(
                e,
                crate::protocol::Event::SessionClosed(
                    crate::feat::session::protocol::session_closed::SessionClosed {
                        session_id: sid,
                    }
                ) if sid == &fake_id
            )
        });
        assert!(
            !found,
            "did not expect SessionClosed for nonexistent session"
        );
    }

    // --- Teardown-only (close_on_success: false) tests ---

    #[tokio::test]
    async fn teardown_only_success_does_not_remove_session() {
        // Given a session actor with a session.
        let actor = test_actor();
        let (_sink, ctx) = test_context();
        let session_id = {
            let state = actor.state.read();
            state.session.active_session_id().clone()
        };
        let original_count = {
            let state = actor.state.read();
            state.session.sessions().len()
        };

        // When teardown succeeds with close_on_success: false.
        actor
            .handle_run_session_teardown(
                &crate::feat::session_lifecycle::protocol::command::RunSessionTeardown {
                    session_id: session_id.clone(),
                    command: "echo test".to_owned(),
                    args: vec![],
                    close_on_success: false,
                },
                &ctx,
            )
            .await;

        // Then the session is NOT removed.
        let state = actor.state.read();
        assert!(state.session.sessions().contains_key(&session_id));
        assert_eq!(state.session.sessions().len(), original_count);
    }

    #[tokio::test]
    async fn teardown_only_success_does_not_emit_session_removed() {
        // Given a session actor with a session.
        let actor = test_actor();
        let (sink, ctx) = test_context();
        let session_id = {
            let state = actor.state.read();
            state.session.active_session_id().clone()
        };

        // When teardown succeeds with close_on_success: false.
        actor
            .handle_run_session_teardown(
                &crate::feat::session_lifecycle::protocol::command::RunSessionTeardown {
                    session_id: session_id.clone(),
                    command: "echo test".to_owned(),
                    args: vec![],
                    close_on_success: false,
                },
                &ctx,
            )
            .await;

        // Then no SessionClosed event is emitted.
        let events = sink.events();
        let found = events.iter().any(|e| {
            matches!(
                e,
                crate::protocol::Event::SessionClosed(
                    crate::feat::session::protocol::session_closed::SessionClosed {
                        session_id: sid,
                    }
                ) if sid == &session_id
            )
        });
        assert!(!found, "did not expect SessionClosed for teardown-only");
    }

    #[tokio::test]
    async fn teardown_only_success_emits_session_teardown_completed() {
        // Given a session actor with a session.
        let actor = test_actor();
        let (sink, ctx) = test_context();
        let session_id = {
            let state = actor.state.read();
            state.session.active_session_id().clone()
        };

        // When teardown succeeds with close_on_success: false.
        actor
            .handle_run_session_teardown(
                &crate::feat::session_lifecycle::protocol::command::RunSessionTeardown {
                    session_id: session_id.clone(),
                    command: "echo test".to_owned(),
                    args: vec![],
                    close_on_success: false,
                },
                &ctx,
            )
            .await;

        // Then SessionTeardownFinished is still emitted.
        let events = sink.events();
        let found = events.iter().any(|e| {
            matches!(
                e,
                crate::protocol::Event::SessionTeardownFinished(
                    crate::feat::session_lifecycle::protocol::event::SessionTeardownFinished {
                        session_id: sid,
                        error: None,
                    }
                ) if sid == &session_id
            )
        });
        assert!(found, "expected SessionTeardownFinished event");
    }

    #[tokio::test]
    async fn teardown_only_success_pushes_info_entry() {
        // Given a session actor with a session.
        let actor = test_actor();
        let (_sink, ctx) = test_context();
        let session_id = {
            let state = actor.state.read();
            state.session.active_session_id().clone()
        };

        // When teardown succeeds with close_on_success: false.
        actor
            .handle_run_session_teardown(
                &crate::feat::session_lifecycle::protocol::command::RunSessionTeardown {
                    session_id: session_id.clone(),
                    command: "echo test".to_owned(),
                    args: vec![],
                    close_on_success: false,
                },
                &ctx,
            )
            .await;

        // Then the session has an info entry about success.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let last = session.history().last().expect("has an entry");
        assert!(matches!(&last.kind, ChatEntryKind::Info(..)));
    }

    // --- Compaction handler tests ---

    #[tokio::test]
    async fn begin_compaction_sets_compacting_phase_and_pushes_system_entry() {
        // Given a session actor with a session.
        let actor = test_actor();
        let (_sink, _ctx) = test_context();
        let session_id = {
            let state = actor.state.read();
            state.session.active_session_id().clone()
        };

        // When handling BeginCompaction.
        actor.handle_begin_compaction(
            &crate::feat::compaction_actor::protocol::command::BeginCompaction {
                session_id: session_id.clone(),
                gathered_indices: vec![0, 1],
            },
        );

        // Then the session is in Compacting phase.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert!(matches!(session.phase(), SessionPhase::Compacting));
        assert!(!matches!(session.phase(), SessionPhase::Idle));
        drop(state);
    }

    #[tokio::test]
    async fn end_compaction_on_success_inserts_entry_and_resets_phase() {
        // Given a session actor with a session in Compacting phase.
        let actor = test_actor();
        let (_sink, ctx) = test_context();
        let session_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("old message"));
            session.begin_compacting(vec![]);
            state.session.active_session_id().clone()
        };

        // When handling EndCompaction with a successful result.
        actor
            .handle_end_compaction(
                &crate::feat::compaction_actor::protocol::command::EndCompaction {
                    session_id: session_id.clone(),
                    result: Some(
                        crate::feat::compaction_actor::protocol::command::CompactionResult {
                            summary: "summarized".to_owned(),
                            entries_compacted: 1,
                            tokens_before: 100,
                            model_used: "test/model".to_owned(),
                            boundary_index: 1,
                        },
                    ),
                    error: None,
                },
                &ctx,
            )
            .await;

        // Then the session is idle and has the compaction entry.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert!(matches!(session.phase(), SessionPhase::Idle));
        // The history should have: user entry, compaction entry, system entry.
        assert!(session.history().iter().any(ChatEntry::is_compaction));
        assert!(session.history().iter().any(
            |e| matches!(&e.kind, ChatEntryKind::System(t) if t.contains("Context was compacted"))
        ));
    }

    #[tokio::test]
    async fn end_compaction_on_failure_pushes_error_entry_and_resets_phase() {
        // Given a session actor with a session in Compacting phase.
        let actor = test_actor();
        let (_sink, ctx) = test_context();
        let session_id = {
            let mut state = actor.state.write();
            state.active_session_mut().begin_compacting(vec![]);
            state.session.active_session_id().clone()
        };

        // When handling EndCompaction with an error.
        actor
            .handle_end_compaction(
                &crate::feat::compaction_actor::protocol::command::EndCompaction {
                    session_id: session_id.clone(),
                    result: None,
                    error: Some("LLM call failed".to_owned()),
                },
                &ctx,
            )
            .await;

        // Then the session is idle and has an error entry.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert!(matches!(session.phase(), SessionPhase::Idle));
        let last = session.history().last().expect("has an entry");
        assert!(
            matches!(&last.kind, ChatEntryKind::Error(msg) if msg.contains("Compaction failed"))
        );
    }

    #[tokio::test]
    async fn end_compaction_drains_queued_messages_on_success() {
        // Given a session in Compacting phase with a queued user message.
        let actor = test_actor();
        let (sink, ctx) = test_context();
        let session_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.enqueue_message(ChatEntry::user("queued during compaction"));
            session.begin_compacting(vec![]);
            state.session.active_session_id().clone()
        };

        // When handling EndCompaction with a successful result.
        actor
            .handle_end_compaction(
                &crate::feat::compaction_actor::protocol::command::EndCompaction {
                    session_id: session_id.clone(),
                    result: Some(
                        crate::feat::compaction_actor::protocol::command::CompactionResult {
                            summary: "summarized".to_owned(),
                            entries_compacted: 1,
                            tokens_before: 100,
                            model_used: "test/model".to_owned(),
                            boundary_index: 0,
                        },
                    ),
                    error: None,
                },
                &ctx,
            )
            .await;

        // Then AssemblePrompt was emitted for the queued message.
        let commands = sink.commands();
        let has_assemble = commands
            .iter()
            .any(|c| matches!(c, Command::AssemblePrompt(_)));
        assert!(
            has_assemble,
            "expected AssemblePrompt command for queued message"
        );
    }

    #[tokio::test]
    async fn end_compaction_drains_queued_messages_on_failure() {
        // Given a session in Compacting phase with a queued user message.
        let actor = test_actor();
        let (sink, ctx) = test_context();
        let session_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.enqueue_message(ChatEntry::user("queued during compaction"));
            session.begin_compacting(vec![]);
            state.session.active_session_id().clone()
        };

        // When handling EndCompaction with a failure result.
        actor
            .handle_end_compaction(
                &crate::feat::compaction_actor::protocol::command::EndCompaction {
                    session_id: session_id.clone(),
                    result: None,
                    error: Some("LLM call failed".to_owned()),
                },
                &ctx,
            )
            .await;

        // Then AssemblePrompt was emitted for the queued message.
        let commands = sink.commands();
        let has_assemble = commands
            .iter()
            .any(|c| matches!(c, Command::AssemblePrompt(_)));
        assert!(
            has_assemble,
            "expected AssemblePrompt command for queued message after failure"
        );
    }

    #[tokio::test]
    async fn end_compaction_with_empty_queue_does_not_emit_assemble_prompt() {
        // Given a session in Compacting phase with no queued messages.
        let actor = test_actor();
        let (sink, ctx) = test_context();
        let session_id = {
            let mut state = actor.state.write();
            state.active_session_mut().begin_compacting(vec![]);
            state.session.active_session_id().clone()
        };

        // When handling EndCompaction with a successful result.
        actor
            .handle_end_compaction(
                &crate::feat::compaction_actor::protocol::command::EndCompaction {
                    session_id: session_id.clone(),
                    result: Some(
                        crate::feat::compaction_actor::protocol::command::CompactionResult {
                            summary: "summarized".to_owned(),
                            entries_compacted: 1,
                            tokens_before: 100,
                            model_used: "test/model".to_owned(),
                            boundary_index: 0,
                        },
                    ),
                    error: None,
                },
                &ctx,
            )
            .await;

        // Then no AssemblePrompt command was emitted.
        let commands = sink.commands();
        let has_assemble = commands
            .iter()
            .any(|c| matches!(c, Command::AssemblePrompt(_)));
        assert!(
            !has_assemble,
            "expected no AssemblePrompt when queue is empty"
        );
    }

    #[tokio::test]
    async fn end_compaction_drain_leaves_session_in_sending_state() {
        // Given a session in Compacting phase with a queued user message.
        let actor = test_actor();
        let (_sink, ctx) = test_context();
        let session_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.enqueue_message(ChatEntry::user("queued during compaction"));
            session.begin_compacting(vec![]);
            state.session.active_session_id().clone()
        };

        // When handling EndCompaction with a successful result.
        actor
            .handle_end_compaction(
                &crate::feat::compaction_actor::protocol::command::EndCompaction {
                    session_id: session_id.clone(),
                    result: Some(
                        crate::feat::compaction_actor::protocol::command::CompactionResult {
                            summary: "summarized".to_owned(),
                            entries_compacted: 1,
                            tokens_before: 100,
                            model_used: "test/model".to_owned(),
                            boundary_index: 0,
                        },
                    ),
                    error: None,
                },
                &ctx,
            )
            .await;

        // Then the session is in Sending state (start_turn_from_queued called begin_sending).
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert!(matches!(session.phase(), SessionPhase::Sending));
    }

    // --- EndCompaction guard tests ---

    #[tokio::test]
    async fn end_compaction_ignored_when_not_compacting() {
        // Given a session in Idle phase (e.g. compaction was cancelled).
        let actor = test_actor();
        let (_sink, ctx) = test_context();
        let session_id = {
            let state = actor.state.read();
            state.session.active_session_id().clone()
        };
        // Session is Idle by default.

        // When handling EndCompaction with a successful result.
        actor
            .handle_end_compaction(
                &crate::feat::compaction_actor::protocol::command::EndCompaction {
                    session_id: session_id.clone(),
                    result: Some(
                        crate::feat::compaction_actor::protocol::command::CompactionResult {
                            summary: "summarized".to_owned(),
                            entries_compacted: 1,
                            tokens_before: 100,
                            model_used: "test/model".to_owned(),
                            boundary_index: 0,
                        },
                    ),
                    error: None,
                },
                &ctx,
            )
            .await;

        // Then no compaction entry was inserted and the session is still Idle.
        let state = actor.state.read();
        let session = state
            .session
            .sessions()
            .get(&session_id)
            .expect("session exists");
        assert_eq!(session.phase(), SessionPhase::Idle);
        assert!(!session.history().iter().any(ChatEntry::is_compaction));
    }

    // --- Regression tests for session-crash: handle_prompt_assembled crash ---

    #[tokio::test]
    async fn handle_prompt_assembled_with_session_in_sending_transitions_to_streaming() {
        // Given a session actor with a session in Sending phase (e.g., after tool use).
        let actor = test_actor();
        let (_sink, ctx) = test_context();
        let session_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.begin_streaming();
            session.finish_streaming(true);
            session.begin_sending();
            state.session.active_session_id().clone()
        };

        // When handling PromptAssembled.
        let payload = crate::feat::context::protocol::event::PromptAssembled {
            session_id: session_id.clone(),
            system_prompt: None,
            messages: vec![],
        };
        actor.handle_prompt_assembled(&payload, &ctx).await;

        // Then the session is in Streaming phase without panicking.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert_eq!(session.phase(), SessionPhase::Streaming);
    }

    #[tokio::test]
    async fn handle_prompt_assembled_with_session_in_assembling_transitions_to_streaming() {
        // Given a session actor with a session in Assembling phase.
        let actor = test_actor();
        let (_sink, ctx) = test_context();
        let session_id = {
            let mut state = actor.state.write();
            state.active_session_mut().begin_assembling();
            state.session.active_session_id().clone()
        };

        // When handling PromptAssembled.
        let payload = crate::feat::context::protocol::event::PromptAssembled {
            session_id: session_id.clone(),
            system_prompt: None,
            messages: vec![],
        };
        actor.handle_prompt_assembled(&payload, &ctx).await;

        // Then the session is in Streaming phase.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert_eq!(session.phase(), SessionPhase::Streaming);
    }
}
