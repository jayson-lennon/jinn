//! Provider slice - LLM provider selection, model discovery, and streaming UI.
//!
//! Provides [`ProviderState`] for tracking the active provider, model cache,
//! and provider picker state. Also contains the provider actor, discover actor,
//! and UI elements (streaming indicator).

pub mod discover_actor;
pub mod entries;
pub mod entries_to_messages;
pub mod indicator;
pub mod llm_message;
pub mod loader;
pub mod picker_entry;
pub mod protocol;
pub mod provider_actor;

#[cfg(test)]
#[cfg(test)]
mod entries_tests;

#[cfg(test)]
mod entries_to_messages_tests;

pub use indicator::StreamingIndicatorElement;

use crate::common::AppUiRegistry;

use crate::ProviderPickerEntry;

/// Provider selection state - owned by the provider-actor.
///
/// Written to exclusively by `ProviderActor` and `IntentHandler`.
/// No other actor should mutate these fields.
#[derive(Debug)]
// Some fields are private (capsule-sealed); the rest remain `pub` until
// Phase 2 caps their accessors too. Tracked in .plans/seal-capsule-walls/plan.md.
#[expect(
    clippy::partial_pub_fields,
    reason = "remaining pub fields pending Phase 2 capsule sealing"
)]
pub struct ProviderState {
    /// Last known model cache from discovery.
    /// OWNER: provider-actor (updates from ModelsRefreshed / ModelCacheLoaded events).
    pub model_cache: Option<crate::feat::provider_infra::ModelCache>,

    /// Provider picker state (items, filter text, selection index).
    /// OWNER: provider-actor (loads entries via LoadProviderPickerEntries),
    ///        IntentHandler (navigates picker, reads selected item).
    pub provider_picker:
        jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<ProviderPickerEntry>>,

    /// Whether the provider picker is in alloy-selection mode.
    /// OWNER: provider spec (set on open, flipped by the alloy bind, read on
    /// confirm).
    ///
    /// When `false`, ENTER selects the highlighted model as a single model. When
    /// `true`, TAB toggles models into the alloy set and ENTER force-includes the
    /// highlight then commits (1 model -> Single, 2+ -> Alloy).
    alloy_mode: bool,
}

impl Default for ProviderState {
    fn default() -> Self {
        Self {
            model_cache: None,
            provider_picker: jinn_selection_widget::SelectionState::new(),
            alloy_mode: false,
        }
    }
}

impl ProviderState {
    /// Whether the provider picker is in alloy-selection mode.
    pub fn is_alloy_mode(&self) -> bool {
        self.alloy_mode
    }

    /// Set whether the provider picker is in alloy-selection mode.
    pub fn set_alloy_mode(&mut self, on: bool) {
        self.alloy_mode = on;
    }

    /// Flip alloy-selection mode and return the new state.
    pub fn toggle_alloy_mode(&mut self) -> bool {
        self.alloy_mode = !self.alloy_mode;
        self.alloy_mode
    }
}

/// Register provider UI elements.
pub fn register(registry: &mut AppUiRegistry) {
    registry.register(Box::new(StreamingIndicatorElement::new()));
}
