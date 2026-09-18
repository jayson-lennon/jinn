//! Provider entries - loading, sorting, and formatting.
//!
//! Contains loader functions, sorting, and formatting utilities for the
//! provider picker overlay. The [`ProviderPickerEntry`] struct and [`PickerItem`]
//! implementation live in `crate::protocol`.

use crate::feat::picker::style::promote_active_to_top;
use crate::feat::theme::Theme;
use crate::protocol::ProviderPickerEntry;
/// Reorders entries so that available entries appear first (sorted by model name),
/// followed by unavailable entries (sorted by model name). When `filter` is empty,
/// the entry matching `active_provider` is promoted to the very top and marked active.
///
/// `active_provider` is in `{name}/{model}` format (e.g., `"ollama/llama3"`).
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
pub(crate) fn promote_selected_to_top(entries: &mut Vec<ProviderPickerEntry>) {
    // Stable partition: selected entries move to top, preserving alphabetical order within each group.
    let selected: Vec<ProviderPickerEntry> = entries.extract_if(.., |e| e.selected).collect();
    let mut result = selected;
    result.append(entries);
    *entries = result;
}

/// Formats the two footer lines for the provider picker.
///
/// - Line 1: refresh info (`CTRL+R to refresh | Updated ...`).
/// - Line 2: mode info (`CTRL+A (toggle alloy) ...`) — always starts with the
///   toggle hint, then appends mode-specific ENTER/selection semantics.
///
/// Each line is independently truncated to `width`.
pub fn format_footers(
    model_cache: Option<&crate::feat::provider_infra::ModelCache>,
    width: usize,
    theme: &Theme,
    selected_count: usize,
    alloy_mode: bool,
) -> Vec<ratatui::text::Line<'static>> {
    vec![
        truncate_line(refresh_line(model_cache, theme), width),
        truncate_line(mode_line(theme, selected_count, alloy_mode), width),
    ]
}

/// Line 1 of the provider footer: refresh keybind + last-updated time.
fn refresh_line(
    model_cache: Option<&crate::feat::provider_infra::ModelCache>,
    theme: &Theme,
) -> ratatui::text::Line<'static> {
    use ratatui::style::Style;
    use ratatui::text::{Line, Span};

    let gray = Style::default().fg(theme.muted_text);
    let orange = Style::default().fg(theme.accent_action);
    let pipe = Span::styled("|".to_owned(), gray);

    let left = Span::styled("CTRL+R to refresh ".to_owned(), orange);

    let ts = model_cache.and_then(|c| c.last_updated_at);
    let right = match ts {
        Some(ts) => {
            let elapsed = jiff::Timestamp::now() - ts;
            let secs = elapsed.total(jiff::Unit::Second).unwrap_or(0.0).round() as u64;
            let duration = std::time::Duration::from_secs(secs);
            let human = humantime::format_duration(duration);
            let age_color = age_color(secs, theme);
            let formatted_ts = format!("{ts:.0}");
            let mid = format!(" Updated {formatted_ts} (");
            let ago = format!("{human} ago)");
            vec![
                Span::styled(mid, gray),
                Span::styled(ago, Style::default().fg(age_color)),
            ]
        }
        None => {
            vec![Span::styled(" Updated never".to_owned(), gray)]
        }
    };

    let mut spans = vec![left, pipe.clone()];
    spans.extend(right);
    Line::from(spans)
}

/// Line 2 of the provider footer: mode info.
///
/// Always leads with `CTRL+A (toggle alloy)`. Single mode then shows
/// `Enter selects`; alloy mode shows `N selected · TAB toggle · Enter adds+confirms`.
fn mode_line(
    theme: &Theme,
    selected_count: usize,
    alloy_mode: bool,
) -> ratatui::text::Line<'static> {
    use ratatui::style::Style;
    use ratatui::text::{Line, Span};

    let gray = Style::default().fg(theme.muted_text);
    let orange = Style::default().fg(theme.accent_action);

    let toggle = Span::styled("CTRL+A (toggle alloy)".to_owned(), orange);

    let rest: Vec<Span<'static>> = if alloy_mode {
        vec![
            Span::styled(format!(" · {selected_count} selected"), orange),
            Span::styled(" · TAB toggle".to_owned(), gray),
            Span::styled(" · Enter adds+confirms".to_owned(), gray),
        ]
    } else {
        vec![Span::styled(" · Enter selects".to_owned(), gray)]
    };

    let mut spans = vec![toggle];
    spans.extend(rest);
    Line::from(spans)
}

/// Returns the age-based color for the "time ago" text.
///
/// - `<= 2 weeks` → age_fresh
/// - `> 2 weeks, <= 4 weeks` → warning
/// - `> 4 weeks` → age_stale
pub fn age_color(secs: u64, theme: &Theme) -> ratatui::style::Color {
    const TWO_WEEKS: u64 = 14 * 24 * 60 * 60;
    const FOUR_WEEKS: u64 = 28 * 24 * 60 * 60;
    if secs <= TWO_WEEKS {
        theme.age_fresh
    } else if secs <= FOUR_WEEKS {
        theme.warning
    } else {
        theme.age_stale
    }
}

/// Truncates a styled line to fit within `width` terminal columns.
pub fn truncate_line(
    line: ratatui::text::Line<'static>,
    width: usize,
) -> ratatui::text::Line<'static> {
    use ratatui::text::{Line, Span};
    use unicode_segmentation::UnicodeSegmentation as _;

    let total_len: usize = line
        .spans
        .iter()
        .map(|s| s.content.graphemes(true).count())
        .sum();
    if total_len <= width {
        return line;
    }

    let mut remaining = width;
    let mut spans = Vec::new();
    for span in line.spans {
        let char_count = span.content.graphemes(true).count();
        if remaining == 0 {
            break;
        }
        if char_count <= remaining {
            spans.push(span);
            remaining -= char_count;
        } else {
            let truncated: String = span.content.graphemes(true).take(remaining).collect();
            spans.push(Span::styled(truncated, span.style));
            remaining = 0;
        }
    }
    Line::from(spans)
}

/// Builds a [`ProviderPickerEntry`] from a resolved provider.
///
/// Checks availability against the registry and API keys.
/// The entry is marked `is_remote: false` and `is_alias: false`.
fn static_provider_entry(
    provider: &crate::feat::provider_infra::ResolvedProvider,
    registry: &crate::feat::provider_infra::ProviderRegistry,
    api_keys: &crate::feat::provider_infra::ApiKeys,
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
fn alias_entry(
    alias: &crate::feat::provider_infra::AliasEntry,
    registry: &crate::feat::provider_infra::ProviderRegistry,
    api_keys: &crate::feat::provider_infra::ApiKeys,
    theme: &Theme,
) -> ProviderPickerEntry {
    let resolved = registry.resolve_alias(&alias.name);
    let is_available = resolved.is_some_and(|r| registry.is_available(&r.id.clone(), api_keys));

    ProviderPickerEntry {
        provider_id: resolved.map(|r| r.id.to_string()).unwrap_or_default(),
        name: alias.name.clone(),
        provider_name: resolved.map(|r| r.name.clone()).unwrap_or_default(),
        backend: resolved.map(|r| r.backend.clone()).unwrap_or_default(),
        model: resolved.map(|r| r.model.clone()).unwrap_or_default(),
        search_text: format!(
            "{} {}",
            resolved
                .as_ref()
                .map(|r| r.model.as_str())
                .unwrap_or_default(),
            resolved
                .as_ref()
                .map(|r| r.name.as_str())
                .unwrap_or_default()
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
/// Static entries win on collision - if a static entry already claims
/// `{provider_name}/{model}`, the remote version is skipped.
fn merge_remote_entries(
    entries: &mut Vec<ProviderPickerEntry>,
    static_ids: &std::collections::HashSet<String>,
    registry: &crate::feat::provider_infra::ProviderRegistry,
    api_keys: &crate::feat::provider_infra::ApiKeys,
    cache: &crate::feat::provider_infra::ModelCache,
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

/// Loads all provider and alias entries from the registry, ready for `set_items()`.
///
/// Reads the provider registry, API keys, and optional model cache
/// to produce the full list of entries. No filtering is applied - that is
/// handled by [`SelectionState`] via fuzzy matching on [`PickerItem::display_label`].
///
/// Remote models from the cache are merged in after static entries. Static entries
/// win on collision (same `{provider_name}/{model}` key). Remote entries are marked
/// with `is_remote: true`.
///
/// [`SelectionState`]: jinn_selection_widget::SelectionState
/// [`PickerItem`]: jinn_selection_widget::PickerItem
pub fn load_provider_entries(
    registry: &crate::feat::provider_infra::ProviderRegistry,
    api_keys: &crate::feat::provider_infra::ApiKeys,
    model_cache: Option<&crate::feat::provider_infra::ModelCache>,
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
