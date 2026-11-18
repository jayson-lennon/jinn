//! Provider picker loader — builds provider picker entries into the
//! picker's `SelectionState`.
//!
//! The `set_endpoint_picker_items` helper lives in
//! [`crate::endpoint_loader`] as actor methods.

use jinn_core_types::ModelSelection;
use jinn_picker::PickerEntry;
use jinn_provider_config::ModelCache;
use jinn_provider_selection_msg::ProviderPickerEntry;
use jinn_provider_selection_msg::load_provider_entries;
use jinn_provider_selection_msg::promote_selected_to_top;
use jinn_provider_selection_msg::sorted_entries;
use jinn_selection_widget::SelectionState;

use jinn_kernel::Services;

/// Loads provider entries into the picker state, ready for display.
///
/// Reads from the provider registry and the model-cache snapshot through
/// the shared row builder (`provider_rows`), applies available-first
/// sorting and active-provider promotion, then wraps the entries through
/// the provider spec's hooks and stores them via `SelectionState::set_items`.
pub(crate) fn load_provider_picker_items(
    services: &Services,
    picker: &mut SelectionState<PickerEntry<ProviderPickerEntry>>,
    model_cache: Option<&ModelCache>,
    theme: &jinn_theme::Theme,
    model_selection: &ModelSelection,
    alloy_mode: bool,
) {
    let registry = services.provider_registry.read();
    let api_keys = services.api_keys.read();
    let all = load_provider_entries(&registry, &api_keys, model_cache, theme);

    let active_model = model_selection.display_str().to_owned();
    let mut entries = sorted_entries(&all, "", &active_model);

    // Pre-check entries matching the current model selection, but only when
    // the picker is in alloy mode. Single mode never builds checkmarks.
    if alloy_mode {
        jinn_provider_selection_msg::pre_check_active_models(&mut entries, model_selection);
        promote_selected_to_top(&mut entries);
    }

    // Wrap through the picker's own hooks — the same wrap point the row hook
    // and the filter text live in, so a second definition could not drift.
    let wrapped = crate::provider_picker_actions::wrap_entries(entries);
    picker.set_items(wrapped);
}
