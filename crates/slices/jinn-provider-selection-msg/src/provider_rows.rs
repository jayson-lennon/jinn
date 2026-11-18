//! The provider-row data builder — one builder for every surface that
//! lists models.
//!
//! The TUI's provider picker and the web model modal both need the
//! same rows: availability resolved against the registry and API keys,
//! aliases resolved to their targets, remote (cache-discovered) models
//! merged after static config, then the sort — available entries
//! first, alphabetical by model, the active selection promoted to the
//! top of an unfiltered list. This module is that builder. It produces
//! [`ProviderPickerEntry`] data: no ratatui lines, no HTML, no
//! theme-conditional presentation — the caller stamps its own
//! presentation onto the fields.
//!
//! Why the msg crate: both consumers already depend on it (the TUI
//! slice for the entry type, the webui for the provider cell), and a
//! `-msg` crate never depends on an implementation crate — so placing
//! the builder here is the only arrangement where the two surfaces
//! cannot drift. The TUI-side ratatui row rendering that was already
//! part of this crate (`entries::render_row`) is unaffected.

use crate::entries::ProviderPickerEntry;
use jinn_picker::picker_style::promote_active_to_top;
use jinn_provider_config::ApiKeys;
use jinn_provider_config::ModelCache;
use jinn_provider_config::ProviderRegistry;
use jinn_theme::Theme;

/// Builds a [`ProviderPickerEntry`] from a resolved provider.
///
/// Checks availability against the registry and API keys.
/// The entry is marked `is_remote: false` and `is_alias: false`.
#[must_use]
fn static_provider_entry(
    provider: &jinn_provider_config::ResolvedProvider,
    registry: &ProviderRegistry,
    api_keys: &ApiKeys,
    theme: &Theme,
) -> ProviderPickerEntry {
    ProviderPickerEntry {
        provider_id: provider.id.to_string(),
        name: provider.name.clone(),
        provider_name: provider.name.clone(),
        backend: provider.backend.clone(),
        model: provider.model.clone(),
        search_text: format!("{} {}", provider.model, provider.name),
        is_alias: false,
        alias_target: None,
        is_available: registry.is_available(&provider.id, api_keys),
        is_remote: false,
        is_active: false,
        selected: false,
        theme: theme.clone(),
    }
}

/// Builds a [`ProviderPickerEntry`] from an alias definition.
///
/// Resolves the alias through the registry. If the alias resolves, the entry
/// inherits the target provider's metadata. Availability depends on whether
/// the resolved target is available. Unresolvable aliases get empty defaults
/// and are marked unavailable.
#[must_use]
fn alias_entry(
    alias: &jinn_provider_config::AliasEntry,
    registry: &ProviderRegistry,
    api_keys: &ApiKeys,
    theme: &Theme,
) -> ProviderPickerEntry {
    let resolved = registry.resolve_alias(&alias.name);
    let is_available = resolved.is_some_and(|r| registry.is_available(&r.id.clone(), api_keys));

    ProviderPickerEntry {
        provider_id: resolved.map_or_default(|r| r.id.to_string()),
        name: alias.name.clone(),
        provider_name: resolved.map_or_default(|r| r.name.clone()),
        backend: resolved.map_or_default(|r| r.backend.clone()),
        model: resolved.map_or_default(|r| r.model.clone()),
        search_text: format!(
            "{} {}",
            resolved.as_ref().map_or_default(|r| r.model.as_str()),
            resolved.as_ref().map_or_default(|r| r.name.as_str())
        ),
        is_alias: true,
        alias_target: resolved.map(|r| r.id.to_string()),
        is_available,
        is_remote: false,
        is_active: false,
        selected: false,
        theme: theme.clone(),
    }
}

/// Builds a [`ProviderPickerEntry`] from a remote (cache-discovered) model.
///
/// Remote entries are discovered at runtime (e.g., from Ollama's `/api/tags`).
/// They are marked `is_remote: true` and are unavailable if the provider
/// requires a key that isn't set.
#[must_use]
fn remote_entry(
    provider_name: &str,
    model: &str,
    backend: &str,
    is_available: bool,
    theme: &Theme,
) -> ProviderPickerEntry {
    let provider_id = format!("{provider_name}/{model}");
    ProviderPickerEntry {
        provider_id,
        name: provider_name.to_owned(),
        provider_name: provider_name.to_owned(),
        backend: backend.to_owned(),
        model: model.to_owned(),
        search_text: format!("{model} {provider_name}"),
        is_alias: false,
        alias_target: None,
        is_available,
        is_remote: true,
        is_active: false,
        selected: false,
        theme: theme.clone(),
    }
}

/// Merges remote (cache-discovered) models into the entries list.
///
/// Static entries win on collision — if a static entry already claims
/// `{provider_name}/{model}`, the remote version is skipped.
fn merge_remote_entries(
    entries: &mut Vec<ProviderPickerEntry>,
    static_ids: &std::collections::HashSet<String>,
    registry: &ProviderRegistry,
    api_keys: &ApiKeys,
    cache: &ModelCache,
    theme: &Theme,
) {
    let config = registry.config();
    for (provider_name, models) in &cache.entries {
        let provider_entry = config.providers.get(provider_name);

        let (backend, is_available) = match provider_entry {
            Some(pe) => {
                let avail = if pe.requires_key {
                    pe.api_key_env
                        .as_ref()
                        .is_some_and(|env| api_keys.is_set(env))
                } else {
                    true
                };
                (pe.backend.as_str(), avail)
            }
            None => ("unknown", false),
        };

        for model_info in models {
            let provider_id = format!("{provider_name}/{}", model_info.id);
            if static_ids.contains(&provider_id) {
                continue;
            }
            entries.push(remote_entry(
                provider_name,
                &model_info.id,
                backend,
                is_available,
                theme,
            ));
        }
    }
}

/// Loads all provider and alias entries from the registry, ready for a
/// picker to display.
///
/// Reads the provider registry, API keys, and optional model cache
/// to produce the full list of entries. No filtering is applied — the
/// TUI filters client-side via fuzzy matching on the entry's search
/// text; the web filters server-side with the same text. Unavailable
/// providers (missing api key) contribute rows from the registry, not
/// the cache, which is why all three inputs are read together.
///
/// Remote models from the cache are merged in after static entries.
/// Static entries win on collision (same `{provider_name}/{model}` key).
/// Remote entries are marked with `is_remote: true`.
#[must_use]
pub fn load_provider_entries(
    registry: &ProviderRegistry,
    api_keys: &ApiKeys,
    model_cache: Option<&ModelCache>,
    theme: &Theme,
) -> Vec<ProviderPickerEntry> {
    let mut entries = Vec::new();
    let mut static_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Phase 1: Load static providers from config.
    // These are the providers defined in the user's config file.
    // We track their IDs to prevent duplicates when merging remote models later.
    for provider in registry.providers() {
        let entry = static_provider_entry(provider, registry, api_keys, theme);
        static_ids.insert(entry.provider_id.clone());
        entries.push(entry);
    }

    // Phase 2: Load aliases.
    // Each alias resolves to a target provider and inherits its metadata.
    // Unresolvable aliases still appear as entries (unavailable, empty defaults).
    for alias in registry.aliases() {
        let entry = alias_entry(alias, registry, api_keys, theme);
        entries.push(entry);
    }

    // Phase 3: Merge remote models from cache.
    // Remote models are discovered at runtime (e.g., from Ollama's /api/tags).
    // Static entries win on collision - if a static entry already claims
    // `{provider_name}/{model}`, the remote version is skipped.
    if let Some(cache) = model_cache {
        merge_remote_entries(&mut entries, &static_ids, registry, api_keys, cache, theme);
    }

    entries
}

/// Reorders entries so that available entries appear first (sorted by model name),
/// followed by unavailable entries (sorted by model name). When `filter` is empty,
/// the entry matching `active_provider` is promoted to the very top and marked active.
///
/// `active_provider` is in `{name}/{model}` format (e.g., `"ollama/llama3"`).
#[must_use]
pub fn sorted_entries(
    entries: &[ProviderPickerEntry],
    filter: &str,
    active_provider: &str,
) -> Vec<ProviderPickerEntry> {
    // Split into available and unavailable blocks.
    let mut available: Vec<ProviderPickerEntry> =
        entries.iter().filter(|e| e.is_available).cloned().collect();
    let mut unavailable: Vec<ProviderPickerEntry> = entries
        .iter()
        .filter(|e| !e.is_available)
        .cloned()
        .collect();

    // Sort each block alphabetically by model name (case-insensitive).
    available.sort_by_key(|e| e.model.to_lowercase());
    unavailable.sort_by_key(|e| e.model.to_lowercase());

    // Promote selected entries to top (for multi-select alloy building).
    promote_selected_to_top(&mut available);

    // Promote active provider to top when filter is empty.
    if filter.is_empty() && active_provider != jinn_core_types::NO_PROVIDER_ID {
        promote_active_to_top(&mut available, |e| e.provider_id == active_provider, filter);
    }

    // Mark active entries.
    for entry in &mut available {
        entry.is_active = entry.provider_id == active_provider;
    }

    // Merge: available first, then unavailable.
    available.extend(unavailable);
    available
}

/// Promotes entries with `selected == true` to the top of the list.
pub fn promote_selected_to_top(entries: &mut Vec<ProviderPickerEntry>) {
    // Stable partition: selected entries move to top, preserving alphabetical order within each group.
    let selected: Vec<ProviderPickerEntry> = entries.extract_if(.., |e| e.selected).collect();
    let mut result = selected;
    result.append(entries);
    *entries = result;
}

#[cfg(test)]
mod provider_rows_tests;
