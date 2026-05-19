//! Provider actor — manages active provider, LLM factory, model cache, and picker entries.
//!
//! Subscribes to provider-related commands and events, mutates the corresponding
//! [`AppState`](crate::common::app_state::AppState) fields, and emits events for
//! other actors to react to.
//!
//! # State ownership
//!
//! This actor is the **sole writer** of the following `AppState` fields:
//! - `active_provider`
//! - `model_cache`
//! - `last_refreshed_at`
//! - `provider_picker` entries (via the loader)
//!
//! # Lock discipline
//!
//! All handlers follow the same pattern: acquire state lock → mutate → release →
//! then emit. Never hold the lock during emission.

use crate::common::actor::{Actor, ActorContext, ActorEnvelope, NoDirectMsg};
use crate::common::services::Services;
use crate::common::state::State;
use crate::feat::provider::protocol::command::ProviderSwitch;
use crate::feat::provider::protocol::event::{ModelsRefreshed, ProviderSwitched};
use crate::protocol::{Command, Event};

use super::loader::load_provider_picker_items;
use crate::feat::provider::protocol::command::LoadProviderPickerEntries;

/// The provider actor.
///
/// Subscribes to provider-related commands, mutates [`State`], and emits events
/// via the [`ActorContext`] message sink.
pub struct ProviderActor {
    /// Shared application state.
    state: State,
    /// Runtime services (provider registry, API keys, LLM service factory).
    services: Services,
}

/// Dependencies for [`ProviderActor`].
pub struct ProviderActorDeps {
    /// Shared application state.
    pub state: State,
    /// Runtime services.
    pub services: Services,
}

impl Actor for ProviderActor {
    type Message = NoDirectMsg;
    type Deps = ProviderActorDeps;

    fn activate(deps: Self::Deps, ctx: &mut ActorContext) -> Self {
        ctx.subscribe_command::<ProviderSwitch>();
        ctx.subscribe_command::<LoadProviderPickerEntries>();
        ctx.subscribe_event::<ModelsRefreshed>();

        ctx.set_description("Manages provider selection, LLM factory, and model cache");

        Self {
            state: deps.state,
            services: deps.services,
        }
    }

    async fn handle(&mut self, msg: ActorEnvelope<Self::Message>, ctx: &ActorContext) {
        match msg {
            ActorEnvelope::Command(cmd) => self.handle_command(&cmd, ctx),
            ActorEnvelope::Event(Event::ModelsRefreshed(ref payload)) => {
                self.handle_models_refreshed(payload);
            }
            _ => {}
        }
    }
}

impl ProviderActor {
    /// Dispatches a command to the appropriate handler.
    fn handle_command(&mut self, cmd: &Command, ctx: &ActorContext) {
        match cmd {
            Command::ProviderSwitch(payload) => {
                self.handle_provider_switch(payload, ctx);
            }
            Command::LoadProviderPickerEntries(payload) => {
                self.handle_load_provider_picker_entries(payload);
            }
            // Commands NOT subscribed to — these should not arrive.
            Command::SendMessage(..)
            | Command::SwitchPromptStrategy(..)
            | Command::RestoreStrategyState(..)
            | Command::PinChatEntry(..)
            | Command::UnpinChatEntry(..)
            | Command::EnqueueUserMessage(..)
            | Command::SetChatInputText(..)
            | Command::PushChatEntry(..)
            | Command::CancelStream(..)
            | Command::AssemblePrompt(..)
            | Command::SendToLlmProvider(..)
            | Command::RefreshModels
            | Command::RescanPromptTemplates
            | Command::RegisterTools(..)
            | Command::ExecuteToolBatch(..)
            | Command::ExecuteTool(..)
            | Command::CancelToolBatch(..)
            | Command::ProceedWithShutdown(..)
            | Command::SessionLoadCompleted(..)
            | Command::SessionLoadRequested(..)
            | Command::LoadSessionPickerEntries(..)
            | Command::LoadContextStrategyPickerEntries(..)
            | Command::ScanSkills
            | Command::RescanPersonas(..)
            | Command::LoadPersonaPickerEntries(..)
            | Command::UpdatePreferences(..)
            | Command::SessionForkRequested(..)
            | Command::RunSessionSetup(..)
            | Command::RunSessionTeardown(..)
            | Command::CompactContext(..)
            | Command::BeginCompaction(..)
            | Command::CancelCompaction(..)
            | Command::EndCompaction(..)
            | Command::CloseSession(..)
            | Command::ArchiveSession(..)
            | Command::SaveNewLifecycleSession(..) => {}
        }
    }

    // --- Command handlers ---

    /// ProviderSwitch: update session profile and emit ProviderSwitched event.
    fn handle_provider_switch(&self, payload: &ProviderSwitch, ctx: &ActorContext) {
        {
            let mut state = self.state.write();
            state
                .session_mut_or_create(&payload.session_id)
                .set_model(payload.provider_id.clone());
        }

        if let Err(e) = ctx.send_event(Event::ProviderSwitched(ProviderSwitched {
            session_id: payload.session_id.clone(),
            provider_name: payload.provider_id.clone(),
        })) {
            tracing::warn!(err = ?e, "provider-actor failed to emit ProviderSwitched");
        }
    }

    /// LoadProviderPickerEntries: load provider picker entries.
    fn handle_load_provider_picker_entries(&self, _payload: &LoadProviderPickerEntries) {
        let mut state = self.state.write();
        load_provider_picker_items(&self.services, &mut state);
    }

    // --- Event handlers ---

    /// ModelsRefreshed: update model cache and reload provider picker entries.
    fn handle_models_refreshed(&self, event: &ModelsRefreshed) {
        let now = jiff::Timestamp::now();
        let mut state = self.state.write();
        state.provider.model_cache = Some(crate::feat::provider_infra::ModelCache {
            entries: event.results.clone(),
            last_updated_at: Some(now),
        });
        state.provider.last_refreshed_at = Some(now);
        // Also reload provider picker entries from updated model cache.
        load_provider_picker_items(&self.services, &mut state);
    }
}
