//! Provider-selection crossing contracts — the commands and events other
//! actors and slices consume, plus the provider cell vocabulary.
//!
//! The crossing-schema ids ("ProviderSwitch", "ModelsRefreshed", …) derive
//! from the type names, so they are part of the wire contract and must not be
//! renamed. The prompt-scan pair
//! (`RescanPromptTemplates`/`PromptTemplatesLoaded`) belongs to
//! `jinn-session-init-msg`, its producer's crate.
//!
//! The cell vocabulary ([`ProviderCell`], [`provider_state_slot`]) lives
//! here, not in the slice crate, because the *readers* include
//! kernel-resident code (the provider/endpoint picker specs, the TUI
//! renderer) and sibling slices (status-bar, context-curation, boot);
//! the *writer* is the slice's `ProviderActor`. Neither depends on the
//! other.

pub mod cell;
pub mod endpoint;
pub mod entries;
#[cfg(test)]
mod entries_tests;
pub mod provider_picker_state;
pub mod provider_rows;
pub mod reasoning;

pub use cell::ProviderCell;
pub use cell::provider_state_slot;
pub use endpoint::EndpointEntry;
pub use endpoint::{EndpointPickerState, endpoint_picker_scope, endpoint_picker_slot};
pub use entries::ProviderPickerEntry;
pub use entries::pre_check_active_models;
pub use jinn_core_types::{Endpoint, ReasoningEffort};
pub use provider_picker_state::ProviderPickerState;
pub use provider_picker_state::provider_picker_scope;
pub use provider_picker_state::provider_picker_slot;
pub use provider_rows::load_provider_entries;
pub use provider_rows::promote_selected_to_top;
pub use provider_rows::sorted_entries;
pub use reasoning::ReasoningEffortEntry;
pub use reasoning::reasoning_row;
pub use reasoning::{
    ReasoningPickerState, reasoning_picker_scope, reasoning_picker_slot, resolve_effort,
};

mod command;
mod event;

pub use command::*;
pub use event::*;
