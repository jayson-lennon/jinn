//! Startup session hydration and persisted-default seeding.

use jinn_core_types::SessionId;
use jinn_kernel::common::actor_deps::BusPublish;
use jinn_preferences_config::protocol::app_state_command::{AppStateUpdate, UpdateAppState};
use jinn_session_msg::SessionSeed;
use jinn_session_store_msg::SessionSummary;
use trouper::context::MsgCtx;

use crate::hydrate::HydrateSession;
use crate::session_store_actor::SessionStoreActor;

impl SessionStoreActor {
    /// Applies persisted defaults and hydrates unarchived sessions.
    pub(crate) async fn on_environment_loaded(
        &mut self,
        _config: &jinn_provider_config::ProvidersConfig,
        ctx: &mut MsgCtx<'_>,
    ) {
        let app_state = self.services.app_state_storage.read();
        let welcome_mcp_enablement = self.seed_welcome_session(&app_state);
        self.state
            .with_frontend_app_state(|ops| ops.set(app_state.clone()));

        if let Some(enablement) = welcome_mcp_enablement {
            self.publish(enablement).await;
        }

        if !self.load_unarchived_sessions(ctx).await {
            return;
        }
        // The Library cell fill sits behind the hydration dispatch rather
        // than in front of it — startup I/O keeps its declared order — and
        // the members-only read is cheap enough that ordering it after costs
        // nothing. A failed hydration dispatch skips it too; in that state
        // the store itself is failing, and the section's entry-time refresh
        // covers recovery.
        self.refresh_library_cell().await;

        self.publish(UpdateAppState {
            updates: vec![AppStateUpdate::SetLastModel(app_state.last_model.clone())],
        })
        .await;
    }

    /// Applies persisted preferences to the welcome session only.
    ///
    /// The persisted persona seeds the startup session the same way
    /// `last_model` does, normalized against the persona catalog so the
    /// welcome session can never carry a name the scan does not have.
    fn seed_welcome_session(
        &self,
        app_state: &jinn_preferences_config::app_state_file::AppStateFile,
    ) -> Option<jinn_mcp_msg::McpEnablementChanged> {
        let mut enablement = None;
        self.state.with_session(|view| {
            let session = view.session.map().active_session_mut();
            if !session.profile().model.is_no_provider() {
                return;
            }
            if let Some(model) = &app_state.last_model {
                session.set_model(model.clone());
            }
            session.profile_mut().reasoning_effort = app_state.reasoning_effort;

            // The persisted persona is the startup-session seed. An unset
            // value leaves the profile's own default; a set one is
            // normalized through the catalog (absent names fall to the
            // default persona, first-available backstop).
            if let Some(persisted) = app_state.persona_name.as_deref() {
                let catalog = self
                    .services
                    .slices
                    .reader::<jinn_persona_msg::Personas>(&jinn_persona_msg::personas_slot());
                let normalized = catalog.map_or_else(
                    || persisted.to_owned(),
                    |cell| {
                        cell.read()
                            .normalize_name(persisted, jinn_core_types::DEFAULT_PERSONA_NAME)
                    },
                );
                session.set_persona_name(normalized);
            }

            let seed = SessionSeed::from_config(&self.services.config);
            {
                let profile = session.profile_mut();
                profile.tool_filter.clone_from(&seed.tool_filter);
                profile.skill_filter.clone_from(&seed.skill_filter);
            }
            for server in &seed.enabled_mcp {
                session.enable_mcp_server(server);
            }
            if seed.has_auto_enabled_mcp() {
                enablement = Some(jinn_mcp_msg::McpEnablementChanged {
                    session_id: session.session_id().clone(),
                    enabled: seed.enabled_mcp,
                });
            }
        });
        enablement
    }

    /// Loads all unarchived sessions without switching the welcome session.
    ///
    /// Returns `false` when the store's summary read fails, preserving the
    /// existing startup path's early exit before `UpdateAppState`.
    ///
    /// The reads themselves are dispatched, not awaited: this handler reads the
    /// summaries to learn *what* to load, then hands each session to a worker
    /// and returns, so the store actor's mailbox is free while the history is
    /// still being pulled off disk. The sidebar fills in as each completion
    /// lands, under the existing hydration indicator.
    async fn load_unarchived_sessions(&mut self, ctx: &mut MsgCtx<'_>) -> bool {
        self.begin_startup_hydration();
        let summaries = match self
            .services
            .session_store
            .load_unarchived_summaries()
            .await
        {
            Ok(summaries) => summaries,
            Err(error) => {
                tracing::warn!(
                    ?error,
                    "session-actor failed to load unarchived summaries on startup"
                );
                self.finish_startup_hydration();
                return false;
            }
        };
        if summaries.is_empty() {
            self.finish_startup_hydration();
            return true;
        }

        self.dispatch_unarchived_hydration(ctx, summaries);
        true
    }

    /// Marks startup hydration as begun.
    ///
    /// Set before any job is dispatched, so a completion cannot arrive before
    /// hydration is marked started and clear the flag immediately.
    fn begin_startup_hydration(&mut self) {
        self.state
            .with_session(|view| view.session.map().begin_startup_hydration());
    }

    fn finish_startup_hydration(&self) {
        self.state
            .with_session(|view| view.session.map().finish_startup_hydration());
    }

    /// Sends one load job per unarchived session, newest first.
    ///
    /// The counter is incremented by the full batch *before* the first send, so
    /// a completion that arrives while the loop is still running cannot drive it
    /// to zero early and clear the hydration flag while jobs are still out.
    fn dispatch_unarchived_hydration(
        &mut self,
        ctx: &mut MsgCtx<'_>,
        mut summaries: Vec<SessionSummary>,
    ) {
        summaries.sort_by_key(|summary| std::cmp::Reverse(summary.updated_at));
        let pending = summaries.len();
        self.pending_hydrations = pending;
        tracing::info!(pending, "dispatching startup hydration to the worker pool");
        Self::send_hydration_jobs(ctx, summaries.into_iter().map(|summary| summary.session_id));
    }

    /// Sends one `HydrateSession` per session id to whichever worker is free.
    ///
    /// The loop is the dispatch: it sends and returns without awaiting any of
    /// the reads, which is the whole point — the actor's task is never held
    /// across a database read.
    fn send_hydration_jobs(ctx: &mut MsgCtx<'_>, session_ids: impl Iterator<Item = SessionId>) {
        for session_id in session_ids {
            ctx.send_to_any(HydrateSession {
                session_id,
                frozen: false,
            });
        }
    }

    /// Records one finished load, clearing the hydration flag on the last one.
    ///
    /// Returns whether this completion was the last one, so the caller can
    /// start work that depends on the session map being fully populated.
    ///
    /// Saturating and zero-guarded on purpose. The summary-read failure and
    /// empty-summaries paths clear the flag without ever incrementing, and a
    /// completion belonging to some other dispatch must not clear a flag that
    /// belongs to a hydration still in flight.
    pub(crate) fn note_hydration_completion(&mut self) -> bool {
        if self.pending_hydrations == 0 {
            return false;
        }
        self.pending_hydrations = self.pending_hydrations.saturating_sub(1);
        if self.pending_hydrations > 0 {
            return false;
        }
        self.finish_startup_hydration();
        true
    }
}
