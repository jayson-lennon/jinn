#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "test code"
)]

use crate::feat::provider_infra::{ApiKeys, ProviderEntry, ProviderRegistry, ProvidersConfig};
use std::collections::BTreeMap;

use crate::feat::theme::default_theme;
use jinn_selection_widget::PickerItem;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

use super::entries::*;
use crate::feat::provider::picker_entry::ProviderPickerEntry;

fn ollama_entry() -> ProviderEntry {
    ProviderEntry {
        model_info: Vec::new(),
        backend: "ollama".to_owned(),
        models: vec!["llama3".to_owned()],
        base_url: Some("http://localhost:11434".to_owned()),
        api_key_env: None,
        requires_key: false,
        extra_body: None,
        context_length: None,
    }
}

fn openrouter_entry() -> ProviderEntry {
    ProviderEntry {
        model_info: Vec::new(),
        backend: "openrouter".to_owned(),
        models: vec!["gpt-4".to_owned()],
        base_url: None,
        api_key_env: Some("OPENROUTER_API_KEY".to_owned()),
        requires_key: true,
        extra_body: None,
        context_length: None,
    }
}

fn make_config(
    providers: std::collections::BTreeMap<String, ProviderEntry>,
    aliases: Vec<crate::feat::provider_infra::AliasEntry>,
    default_provider: Option<&str>,
) -> ProvidersConfig {
    ProvidersConfig {
        providers,
        aliases,
        default_provider: default_provider.map(String::from),
    }
}

/// Loads entries from a registry with ollama (keyless) and openrouter (key present).
fn load_two_providers() -> Vec<ProviderPickerEntry> {
    let config = make_config(
        BTreeMap::from([
            ("ollama".to_owned(), ollama_entry()),
            ("openrouter".to_owned(), openrouter_entry()),
        ]),
        vec![],
        None,
    );
    let registry = ProviderRegistry::from_config(config).expect("registry");
    let mut api_keys = ApiKeys::new();
    api_keys.insert("OPENROUTER_API_KEY".to_owned(), "sk-test".to_owned());
    load_provider_entries(&registry, &api_keys, None, &default_theme())
}

#[rstest::rstest]
fn load_provider_entries_returns_two_providers() {
    // Given a registry with two providers.
    let entries = load_two_providers();

    // Then exactly two entries are returned.
    assert_eq!(entries.len(), 2);
}

#[rstest::rstest]
#[case::ollama(0, "ollama/llama3", "ollama", "llama3", true)]
#[case::openrouter(1, "openrouter/gpt-4", "openrouter", "gpt-4", true)]
fn load_provider_entries_returns_provider_with_correct_fields(
    #[case] index: usize,
    #[case] provider_id: &str,
    #[case] provider_name: &str,
    #[case] model: &str,
    #[case] is_available: bool,
) {
    // Given a registry with two providers.
    let entries = load_two_providers();

    // Then the entry at the given index has the expected fields.
    let entry = &entries[index];
    assert_eq!(entry.provider_id, provider_id);
    assert_eq!(entry.provider_name, provider_name);
    assert_eq!(entry.model, model);
    assert_eq!(entry.is_available, is_available);
}

#[rstest::rstest]
fn load_provider_entries_marks_key_required_unavailable_when_key_missing() {
    // Given a registry with a key-required provider and no API key.
    let config = make_config(
        BTreeMap::from([("openrouter".to_owned(), openrouter_entry())]),
        vec![],
        None,
    );
    let registry = ProviderRegistry::from_config(config).expect("registry");
    let api_keys = ApiKeys::new();

    // When loading provider entries.
    let entries = load_provider_entries(&registry, &api_keys, None, &default_theme());

    // Then the provider is marked unavailable.
    assert_eq!(entries.len(), 1);
    assert!(!entries[0].is_available);
}

#[rstest::rstest]
fn load_provider_entries_marks_key_required_available_when_key_present() {
    // Given a registry with a key-required provider and the key set.
    let config = make_config(
        BTreeMap::from([("openrouter".to_owned(), openrouter_entry())]),
        vec![],
        None,
    );
    let registry = ProviderRegistry::from_config(config).expect("registry");
    let mut api_keys = ApiKeys::new();
    api_keys.insert("OPENROUTER_API_KEY".to_owned(), "sk-test".to_owned());

    // When loading provider entries.
    let entries = load_provider_entries(&registry, &api_keys, None, &default_theme());

    // Then the provider is marked available.
    assert_eq!(entries.len(), 1);
    assert!(entries[0].is_available);
}

#[rstest::rstest]
fn load_provider_entries_marks_keyless_always_available() {
    // Given a registry with a keyless provider and no API keys.
    let config = make_config(
        BTreeMap::from([("ollama".to_owned(), ollama_entry())]),
        vec![],
        None,
    );
    let registry = ProviderRegistry::from_config(config).expect("registry");
    let api_keys = ApiKeys::new();

    // When loading provider entries.
    let entries = load_provider_entries(&registry, &api_keys, None, &default_theme());

    // Then the keyless provider is always available.
    assert_eq!(entries.len(), 1);
    assert!(entries[0].is_available);
}

/// Loads entries from a registry with ollama and a "fast" alias.
fn load_entries_with_alias() -> (Vec<ProviderPickerEntry>, ProviderPickerEntry) {
    let config = make_config(
        BTreeMap::from([("ollama".to_owned(), ollama_entry())]),
        vec![crate::feat::provider_infra::AliasEntry {
            name: "fast".to_owned(),
            target: "ollama/llama3".to_owned(),
        }],
        None,
    );
    let registry = ProviderRegistry::from_config(config).expect("registry");
    let api_keys = ApiKeys::new();
    let entries = load_provider_entries(&registry, &api_keys, None, &default_theme());
    let alias = entries
        .iter()
        .find(|e| e.is_alias)
        .expect("alias entry")
        .clone();
    (entries, alias)
}

#[rstest::rstest]
fn load_provider_entries_alias_count() {
    // Given a registry with one provider and one alias.
    let (entries, _) = load_entries_with_alias();

    // Then both entries are returned.
    assert_eq!(entries.len(), 2);
}

#[rstest::rstest]
#[case::name("name", "fast")]
#[case::provider_id("provider_id", "ollama/llama3")]
#[case::alias_target("alias_target", "ollama/llama3")]
#[case::provider_name("provider_name", "ollama")]
#[case::backend("backend", "ollama")]
#[case::model("model", "llama3")]
fn load_provider_entries_alias_field_matches(#[case] field: &str, #[case] expected: &str) {
    // Given a registry with one provider and one alias.
    let (_, alias) = load_entries_with_alias();

    // Then the alias entry has the expected field value.
    let actual = match field {
        "name" => alias.name.as_str(),
        "provider_id" => alias.provider_id.as_str(),
        "alias_target" => alias.alias_target.as_deref().unwrap_or(""),
        "provider_name" => alias.provider_name.as_str(),
        "backend" => alias.backend.as_str(),
        "model" => alias.model.as_str(),
        _ => panic!("unknown field: {field}"),
    };
    assert_eq!(actual, expected);
}

#[rstest::rstest]
fn load_provider_entries_alias_inherits_availability() {
    // Given a registry with an alias pointing to an available provider
    // and an alias pointing to an unavailable provider.
    let config = make_config(
        BTreeMap::from([
            ("ollama".to_owned(), ollama_entry()),
            ("openrouter".to_owned(), openrouter_entry()),
        ]),
        vec![
            crate::feat::provider_infra::AliasEntry {
                name: "fast".to_owned(),
                target: "ollama/llama3".to_owned(),
            },
            crate::feat::provider_infra::AliasEntry {
                name: "cloud".to_owned(),
                target: "openrouter/gpt-4".to_owned(),
            },
        ],
        None,
    );
    let registry = ProviderRegistry::from_config(config).expect("registry");
    let api_keys = ApiKeys::new(); // No keys - openrouter is unavailable.

    // When loading provider entries.
    let entries = load_provider_entries(&registry, &api_keys, None, &default_theme());

    // Then the "fast" alias (→ollama) is available and "cloud" alias (→openrouter) is not.
    let fast = entries.iter().find(|e| e.name == "fast").expect("fast");
    let cloud = entries.iter().find(|e| e.name == "cloud").expect("cloud");
    assert!(fast.is_available);
    assert!(!cloud.is_available);
}

#[rstest::rstest]
fn static_entries_present_after_cache_merge() {
    // Given a registry with one keyless provider (ollama/llama3).
    let config = make_config(
        BTreeMap::from([("ollama".to_owned(), ollama_entry())]),
        vec![],
        None,
    );
    let registry = ProviderRegistry::from_config(config).expect("registry");
    let api_keys = ApiKeys::new();

    // And a cache with an additional model for the same provider.
    let mut cache = crate::feat::provider_infra::ModelCache::new();
    cache.entries.insert(
        "ollama".to_owned(),
        vec![crate::feat::provider_infra::ModelInfo {
            id: "mistral".to_owned(),
            context_length: None,
            input_modalities: crate::feat::provider_infra::InputModalities::text(),
        }],
    );

    // When loading provider entries with the cache.
    let entries = load_provider_entries(&registry, &api_keys, Some(&cache), &default_theme());

    // Then the static entry is present.
    assert_eq!(entries.len(), 2);
    let static_entry = entries
        .iter()
        .find(|e| e.model == "llama3")
        .expect("static");
    assert!(!static_entry.is_remote);
}

#[rstest::rstest]
fn remote_entries_present_after_cache_merge() {
    // Given a registry with one keyless provider (ollama/llama3).
    let config = make_config(
        BTreeMap::from([("ollama".to_owned(), ollama_entry())]),
        vec![],
        None,
    );
    let registry = ProviderRegistry::from_config(config).expect("registry");
    let api_keys = ApiKeys::new();

    // And a cache with an additional model for the same provider.
    let mut cache = crate::feat::provider_infra::ModelCache::new();
    cache.entries.insert(
        "ollama".to_owned(),
        vec![crate::feat::provider_infra::ModelInfo {
            id: "mistral".to_owned(),
            context_length: None,
            input_modalities: crate::feat::provider_infra::InputModalities::text(),
        }],
    );

    // When loading provider entries with the cache.
    let entries = load_provider_entries(&registry, &api_keys, Some(&cache), &default_theme());

    // Then the remote entry is present with correct metadata.
    let remote_entry = entries
        .iter()
        .find(|e| e.model == "mistral")
        .expect("remote");
    assert!(remote_entry.is_remote);
    assert_eq!(remote_entry.provider_id, "ollama/mistral");
    assert!(remote_entry.is_available); // Keyless provider
}

#[rstest::rstest]
fn static_entry_not_duplicated_on_collision() {
    // Given a registry with ollama/llama3.
    let config = make_config(
        BTreeMap::from([("ollama".to_owned(), ollama_entry())]),
        vec![],
        None,
    );
    let registry = ProviderRegistry::from_config(config).expect("registry");
    let api_keys = ApiKeys::new();

    // And a cache that also contains ollama/llama3 (collision).
    let mut cache = crate::feat::provider_infra::ModelCache::new();
    cache.entries.insert(
        "ollama".to_owned(),
        vec![
            crate::feat::provider_infra::ModelInfo {
                id: "llama3".to_owned(),
                context_length: None,
                input_modalities: crate::feat::provider_infra::InputModalities::text(),
            },
            crate::feat::provider_infra::ModelInfo {
                id: "mistral".to_owned(),
                context_length: None,
                input_modalities: crate::feat::provider_infra::InputModalities::text(),
            },
        ],
    );

    // When loading provider entries.
    let entries = load_provider_entries(&registry, &api_keys, Some(&cache), &default_theme());

    // Then the static entry is kept (not duplicated).
    let llama3_entries: Vec<_> = entries.iter().filter(|e| e.model == "llama3").collect();
    assert_eq!(llama3_entries.len(), 1);
    assert!(!llama3_entries[0].is_remote);
}

#[rstest::rstest]
fn new_remote_entry_added_on_collision() {
    // Given a registry with ollama/llama3.
    let config = make_config(
        BTreeMap::from([("ollama".to_owned(), ollama_entry())]),
        vec![],
        None,
    );
    let registry = ProviderRegistry::from_config(config).expect("registry");
    let api_keys = ApiKeys::new();

    // And a cache that also contains ollama/llama3 (collision).
    let mut cache = crate::feat::provider_infra::ModelCache::new();
    cache.entries.insert(
        "ollama".to_owned(),
        vec![
            crate::feat::provider_infra::ModelInfo {
                id: "llama3".to_owned(),
                context_length: None,
                input_modalities: crate::feat::provider_infra::InputModalities::text(),
            },
            crate::feat::provider_infra::ModelInfo {
                id: "mistral".to_owned(),
                context_length: None,
                input_modalities: crate::feat::provider_infra::InputModalities::text(),
            },
        ],
    );

    // When loading provider entries.
    let entries = load_provider_entries(&registry, &api_keys, Some(&cache), &default_theme());

    // Then only the new remote model is added.
    let mistral_entries: Vec<_> = entries.iter().filter(|e| e.model == "mistral").collect();
    assert_eq!(mistral_entries.len(), 1);
    assert!(mistral_entries[0].is_remote);
}

#[rstest::rstest]
fn remote_entry_present_when_key_missing() {
    // Given a registry with a key-required provider (openrouter).
    let config = make_config(
        BTreeMap::from([("openrouter".to_owned(), openrouter_entry())]),
        vec![],
        None,
    );
    let registry = ProviderRegistry::from_config(config).expect("registry");
    let api_keys = ApiKeys::new(); // No keys set.

    // And a cache with additional models.
    let mut cache = crate::feat::provider_infra::ModelCache::new();
    cache.entries.insert(
        "openrouter".to_owned(),
        vec![crate::feat::provider_infra::ModelInfo {
            id: "claude-3".to_owned(),
            context_length: None,
            input_modalities: crate::feat::provider_infra::InputModalities::text(),
        }],
    );

    // When loading provider entries.
    let entries = load_provider_entries(&registry, &api_keys, Some(&cache), &default_theme());

    // Then the remote model is present.
    let remote = entries
        .iter()
        .find(|e| e.model == "claude-3")
        .expect("remote");
    assert!(remote.is_remote);
}

#[rstest::rstest]
fn remote_entry_marked_unavailable_when_key_missing() {
    // Given a registry with a key-required provider (openrouter).
    let config = make_config(
        BTreeMap::from([("openrouter".to_owned(), openrouter_entry())]),
        vec![],
        None,
    );
    let registry = ProviderRegistry::from_config(config).expect("registry");
    let api_keys = ApiKeys::new(); // No keys set.

    // And a cache with additional models.
    let mut cache = crate::feat::provider_infra::ModelCache::new();
    cache.entries.insert(
        "openrouter".to_owned(),
        vec![crate::feat::provider_infra::ModelInfo {
            id: "claude-3".to_owned(),
            context_length: None,
            input_modalities: crate::feat::provider_infra::InputModalities::text(),
        }],
    );

    // When loading provider entries.
    let entries = load_provider_entries(&registry, &api_keys, Some(&cache), &default_theme());

    // Then the remote model is marked unavailable (no API key).
    let remote = entries
        .iter()
        .find(|e| e.model == "claude-3")
        .expect("remote");
    assert!(!remote.is_available);
}

#[rstest::rstest]
fn load_provider_entries_includes_all_remote_models() {
    // Given a registry and cache with remote models.
    let config = make_config(
        BTreeMap::from([("ollama".to_owned(), ollama_entry())]),
        vec![],
        None,
    );
    let registry = ProviderRegistry::from_config(config).expect("registry");
    let api_keys = ApiKeys::new();

    let mut cache = crate::feat::provider_infra::ModelCache::new();
    cache.entries.insert(
        "ollama".to_owned(),
        vec![
            crate::feat::provider_infra::ModelInfo {
                id: "mistral".to_owned(),
                context_length: None,
                input_modalities: crate::feat::provider_infra::InputModalities::text(),
            },
            crate::feat::provider_infra::ModelInfo {
                id: "codellama".to_owned(),
                context_length: None,
                input_modalities: crate::feat::provider_infra::InputModalities::text(),
            },
        ],
    );

    // When loading entries (no filter - returns everything).
    let entries = load_provider_entries(&registry, &api_keys, Some(&cache), &default_theme());

    // Then all 3 entries are present (1 static + 2 remote).
    assert_eq!(entries.len(), 3);
}

#[rstest::rstest]
fn active_provider_promoted_to_first() {
    // Given entries ["a/model", "b/model", "c/model"] with active_provider "c/model" and empty filter.
    let entries = vec![
        ProviderPickerEntry {
            provider_id: "a/model".into(),
            name: "a".into(),
            provider_name: "a".into(),
            backend: "a".into(),
            model: "model".into(),
            search_text: "model a".into(),
            is_alias: false,
            alias_target: None,
            is_available: true,
            is_remote: false,
            is_active: false,
            selected: false,
            theme: default_theme(),
        },
        ProviderPickerEntry {
            provider_id: "b/model".into(),
            name: "b".into(),
            provider_name: "b".into(),
            backend: "b".into(),
            model: "model".into(),
            search_text: "model b".into(),
            is_alias: false,
            alias_target: None,
            is_available: true,
            is_remote: false,
            is_active: false,
            selected: false,
            theme: default_theme(),
        },
        ProviderPickerEntry {
            provider_id: "c/model".into(),
            name: "c".into(),
            provider_name: "c".into(),
            backend: "c".into(),
            model: "model".into(),
            search_text: "model c".into(),
            is_alias: false,
            alias_target: None,
            is_available: true,
            is_remote: false,
            is_active: false,
            selected: false,
            theme: default_theme(),
        },
    ];

    // When sorting with empty filter and active_provider "c/model".
    let result = sorted_entries(&entries, "", "c/model");

    // Then "c/model" is first (promoted).
    assert_eq!(result[0].provider_id, "c/model");
    assert_eq!(result[1].provider_id, "a/model");
    assert_eq!(result[2].provider_id, "b/model");
}

#[rstest::rstest]
fn active_entry_marked_active() {
    // Given entries ["a/model", "b/model", "c/model"] with active_provider "c/model" and empty filter.
    let entries = vec![
        ProviderPickerEntry {
            provider_id: "a/model".into(),
            name: "a".into(),
            provider_name: "a".into(),
            backend: "a".into(),
            model: "model".into(),
            search_text: "model a".into(),
            is_alias: false,
            alias_target: None,
            is_available: true,
            is_remote: false,
            is_active: false,
            selected: false,
            theme: default_theme(),
        },
        ProviderPickerEntry {
            provider_id: "b/model".into(),
            name: "b".into(),
            provider_name: "b".into(),
            backend: "b".into(),
            model: "model".into(),
            search_text: "model b".into(),
            is_alias: false,
            alias_target: None,
            is_available: true,
            is_remote: false,
            is_active: false,
            selected: false,
            theme: default_theme(),
        },
        ProviderPickerEntry {
            provider_id: "c/model".into(),
            name: "c".into(),
            provider_name: "c".into(),
            backend: "c".into(),
            model: "model".into(),
            search_text: "model c".into(),
            is_alias: false,
            alias_target: None,
            is_available: true,
            is_remote: false,
            is_active: false,
            selected: false,
            theme: default_theme(),
        },
    ];

    // When sorting with empty filter and active_provider "c/model".
    let result = sorted_entries(&entries, "", "c/model");

    // Then only the promoted entry is marked active.
    assert!(result[0].is_active);
    assert!(!result[1].is_active);
    assert!(!result[2].is_active);
}

#[rstest::rstest]
fn sorted_entries_preserves_order_when_filtering() {
    // Given entries ["a/model", "b/model"] with active_provider "b/model" and non-empty filter.
    let entries = vec![
        ProviderPickerEntry {
            provider_id: "a/model".into(),
            name: "a".into(),
            provider_name: "a".into(),
            backend: "a".into(),
            model: "model".into(),
            search_text: "model a".into(),
            is_alias: false,
            alias_target: None,
            is_available: true,
            is_remote: false,
            is_active: false,
            selected: false,
            theme: default_theme(),
        },
        ProviderPickerEntry {
            provider_id: "b/model".into(),
            name: "b".into(),
            provider_name: "b".into(),
            backend: "b".into(),
            model: "model".into(),
            search_text: "model b".into(),
            is_alias: false,
            alias_target: None,
            is_available: true,
            is_remote: false,
            is_active: false,
            selected: false,
            theme: default_theme(),
        },
    ];

    // When sorting with filter "a" and active_provider "b/model".
    let result = sorted_entries(&entries, "a", "b/model");

    // Then order is unchanged (filter is non-empty).
    assert_eq!(result[0].provider_id, "a/model");
    assert_eq!(result[1].provider_id, "b/model");
}

#[rstest::rstest]
fn available_entry_comes_first() {
    // Given entries with mixed availability.
    let entries = vec![
        ProviderPickerEntry {
            provider_id: "z/model".into(),
            name: "z".into(),
            provider_name: "z".into(),
            backend: "z".into(),
            model: "model".into(),
            search_text: "model z".into(),
            is_alias: false,
            alias_target: None,
            is_available: false,
            is_remote: false,
            is_active: false,
            selected: false,
            theme: default_theme(),
        },
        ProviderPickerEntry {
            provider_id: "a/model".into(),
            name: "a".into(),
            provider_name: "a".into(),
            backend: "a".into(),
            model: "model".into(),
            search_text: "model a".into(),
            is_alias: false,
            alias_target: None,
            is_available: true,
            is_remote: false,
            is_active: false,
            selected: false,
            theme: default_theme(),
        },
        ProviderPickerEntry {
            provider_id: "b/model".into(),
            name: "b".into(),
            provider_name: "b".into(),
            backend: "b".into(),
            model: "model".into(),
            search_text: "model b".into(),
            is_alias: false,
            alias_target: None,
            is_available: false,
            is_remote: false,
            is_active: false,
            selected: false,
            theme: default_theme(),
        },
    ];

    // When sorting with empty filter and no active provider.
    let result = sorted_entries(&entries, "", "__no_provider__");

    // Then the available entry comes first.
    assert_eq!(result.len(), 3);
    assert_eq!(result[0].provider_id, "a/model");
    assert!(result[0].is_available);
}

#[rstest::rstest]
fn sorted_entries_sorts_by_model_name_within_blocks() {
    // Given entries with different model names.
    let entries = vec![
        ProviderPickerEntry {
            provider_id: "a/zebra".into(),
            name: "a".into(),
            provider_name: "a".into(),
            backend: "a".into(),
            model: "zebra".into(),
            search_text: "zebra a".into(),
            is_alias: false,
            alias_target: None,
            is_available: true,
            is_remote: false,
            is_active: false,
            selected: false,
            theme: default_theme(),
        },
        ProviderPickerEntry {
            provider_id: "b/alpha".into(),
            name: "b".into(),
            provider_name: "b".into(),
            backend: "b".into(),
            model: "alpha".into(),
            search_text: "alpha b".into(),
            is_alias: false,
            alias_target: None,
            is_available: true,
            is_remote: false,
            is_active: false,
            selected: false,
            theme: default_theme(),
        },
    ];

    // When sorting with empty filter and no active provider.
    let result = sorted_entries(&entries, "", "__no_provider__");

    // Then entries are sorted alphabetically by model name.
    assert_eq!(result[0].provider_id, "b/alpha");
    assert_eq!(result[1].provider_id, "a/zebra");
}

/// Flattens a line's spans into a single owned string for assertion.
fn line_text(line: &ratatui::text::Line<'_>) -> String {
    line.spans.iter().map(|s| &*s.content).collect()
}

#[rstest::rstest]
fn format_footers_without_timestamp_shows_never() {
    // Given no model cache.
    // When formatting the footers.
    let lines = format_footers(None, 80, &default_theme(), 0, false);

    // Then line 1 contains the refresh info.
    assert_eq!(lines.len(), 2, "footer must have exactly two lines");
    let refresh = line_text(&lines[0]);
    assert!(refresh.contains("Updated never"));
    assert!(refresh.contains("CTRL+R to refresh"));
}

#[rstest::rstest]
fn format_footers_with_timestamp_shows_age() {
    // Given a model cache with a recent timestamp (1 second ago).
    let ts = jiff::Timestamp::now()
        .checked_sub(jiff::Span::new().try_seconds(1).unwrap())
        .unwrap();
    let mut cache = crate::feat::provider_infra::ModelCache::new();
    cache.last_updated_at = Some(ts);

    // When formatting the footers.
    let lines = format_footers(Some(&cache), 120, &default_theme(), 0, false);

    // Then line 1 contains the refresh info.
    let refresh = line_text(&lines[0]);
    assert!(refresh.contains("Updated"));
    assert!(refresh.contains("ago"));
    assert!(refresh.contains("CTRL+R to refresh"));
}

#[rstest::rstest]
fn format_footers_truncates_each_line_to_width() {
    // Given no timestamp and a very narrow width.
    // When formatting the footers with width 10.
    let lines = format_footers(None, 10, &default_theme(), 0, false);

    // Then each line fits within 10 columns.
    for (i, line) in lines.iter().enumerate() {
        let total_len: usize = line
            .spans
            .iter()
            .map(|s| s.content.graphemes(true).count())
            .sum();
        assert!(total_len <= 10, "line {i} too long: {total_len}");
    }
}

#[rstest::rstest]
fn format_footers_single_mode_line2_has_toggle_hint_and_enter() {
    // Given single mode (alloy_mode = false).
    // When formatting the footers.
    let lines = format_footers(None, 120, &default_theme(), 0, false);

    // Then line 2 always shows the toggle hint and the single-mode enter hint.
    let mode = line_text(&lines[1]);
    assert!(
        mode.contains("CTRL+A (toggle alloy)"),
        "toggle hint missing: {mode}"
    );
    assert!(mode.contains("Enter selects"), "enter hint missing: {mode}");
    assert!(
        !mode.contains("selected"),
        "count should not show in single mode: {mode}"
    );
}

#[rstest::rstest]
fn format_footers_alloy_mode_line2_has_toggle_hint_count_and_tab() {
    // Given alloy mode with 3 selected entries.
    // When formatting the footers.
    let lines = format_footers(None, 120, &default_theme(), 3, true);

    // Then line 2 shows the toggle hint, the count, and the TAB hint.
    let mode = line_text(&lines[1]);
    assert!(
        mode.contains("CTRL+A (toggle alloy)"),
        "toggle hint missing: {mode}"
    );
    assert!(mode.contains("3 selected"), "count missing: {mode}");
    assert!(mode.contains("TAB toggle"), "tab hint missing: {mode}");
    assert!(
        mode.contains("Enter adds+confirms"),
        "enter hint missing: {mode}"
    );
}

#[rstest::rstest]
fn age_color_returns_light_green_within_two_weeks() {
    // Given 1 second (well within 2 weeks).
    // When computing age color.
    let color = age_color(1, &default_theme());

    // Then the color is LightGreen.
    assert_eq!(color, ratatui::style::Color::LightGreen);
}

#[rstest::rstest]
fn age_color_returns_light_green_at_exactly_two_weeks() {
    // Given exactly 2 weeks in seconds.
    let two_weeks = 14 * 24 * 60 * 60;

    // When computing age color.
    let color = age_color(two_weeks, &default_theme());

    // Then the color is LightGreen.
    assert_eq!(color, ratatui::style::Color::LightGreen);
}

#[rstest::rstest]
fn age_color_returns_yellow_between_two_and_four_weeks() {
    // Given 3 weeks in seconds (between 2 and 4 weeks).
    let three_weeks = 21 * 24 * 60 * 60;

    // When computing age color.
    let color = age_color(three_weeks, &default_theme());

    // Then the color is Yellow.
    assert_eq!(color, ratatui::style::Color::Yellow);
}

#[rstest::rstest]
fn age_color_returns_yellow_at_exactly_four_weeks() {
    // Given exactly 4 weeks in seconds.
    let four_weeks = 28 * 24 * 60 * 60;

    // When computing age color.
    let color = age_color(four_weeks, &default_theme());

    // Then the color is Yellow.
    assert_eq!(color, ratatui::style::Color::Yellow);
}

#[rstest::rstest]
fn age_color_returns_red_beyond_four_weeks() {
    // Given 5 weeks in seconds (beyond 4 weeks).
    let five_weeks = 35 * 24 * 60 * 60;

    // When computing age color.
    let color = age_color(five_weeks, &default_theme());

    // Then the color is Red.
    assert_eq!(color, ratatui::style::Color::Red);
}

#[rstest::rstest]
fn truncate_line_noop_when_fits() {
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};

    // Given a line that is 10 characters wide.
    let line = Line::from(vec![
        Span::styled("hello ".to_owned(), Style::default()),
        Span::styled("world".to_owned(), Style::default().fg(Color::Red)),
    ]);

    // When truncating to width 20.
    let result = truncate_line(line.clone(), 20);

    // Then the line is unchanged.
    assert_eq!(result.spans.len(), 2);
    assert_eq!(result.spans[0].content, "hello ");
    assert_eq!(result.spans[1].content, "world");
}

#[rstest::rstest]
fn truncate_line_fits_within_width() {
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};

    // Given a line that is 20 characters wide.
    let line = Line::from(vec![
        Span::styled("hello world ".to_owned(), Style::default()),
        Span::styled("test12345".to_owned(), Style::default().fg(Color::Red)),
    ]);

    // When truncating to width 8.
    let result = truncate_line(line, 8);

    // Then the total character count is exactly 8.
    let total_len: usize = result
        .spans
        .iter()
        .map(|s| s.content.graphemes(true).count())
        .sum();
    assert_eq!(total_len, 8);
}

#[rstest::rstest]
fn truncate_keeps_first_span_whole() {
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};

    // Given a line where the second span will be partially truncated.
    let line = Line::from(vec![
        Span::styled("hello ".to_owned(), Style::default()),
        Span::styled("world".to_owned(), Style::default().fg(Color::Red)),
    ]);

    // When truncating to width 8.
    let result = truncate_line(line, 8);

    // Then the first span is kept whole.
    assert_eq!(result.spans.len(), 2);
    assert_eq!(result.spans[0].content, "hello ");
}

#[rstest::rstest]
fn truncate_truncates_second_span() {
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};

    // Given a line where the second span will be partially truncated.
    let line = Line::from(vec![
        Span::styled("hello ".to_owned(), Style::default()),
        Span::styled("world".to_owned(), Style::default().fg(Color::Red)),
    ]);

    // When truncating to width 8.
    let result = truncate_line(line, 8);

    // Then the second span is truncated.
    assert_eq!(result.spans[1].content, "wo");
}

#[rstest::rstest]
fn partial_span_retains_style() {
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};

    // Given a line where the second span will be partially truncated.
    let line = Line::from(vec![
        Span::styled("hello ".to_owned(), Style::default()),
        Span::styled("world".to_owned(), Style::default().fg(Color::Red)),
    ]);

    // When truncating to width 8.
    let result = truncate_line(line, 8);

    // Then the partial span retains its style.
    assert_eq!(result.spans[1].style.fg, Some(Color::Red));
}

fn make_picker_entry(
    model: &str,
    provider_name: &str,
    is_available: bool,
    is_alias: bool,
) -> ProviderPickerEntry {
    ProviderPickerEntry {
        provider_id: format!("{provider_name}/{model}"),
        name: provider_name.to_owned(),
        provider_name: provider_name.to_owned(),
        backend: "test".to_owned(),
        model: model.to_owned(),
        search_text: format!("{model} {provider_name}"),
        is_alias,
        alias_target: None,
        is_available,
        is_remote: false,
        is_active: false,
        selected: false,
        theme: default_theme(),
    }
}

#[rstest::rstest]
fn render_row_with_empty_match_indices_same_as_render_row() {
    // Given a provider entry.
    let entry = make_picker_entry("llama3", "ollama", true, false);

    // When rendering with and without match indices.
    let normal = entry.render_row(false);
    let highlighted = entry.render_row_with_highlight(false, &[]);

    // Then the output is identical.
    assert_eq!(normal.spans.len(), highlighted.spans.len());
    for (n, h) in normal.spans.iter().zip(highlighted.spans.iter()) {
        assert_eq!(n.content, h.content);
        assert_eq!(n.style, h.style);
    }
}

#[rstest::rstest]
fn provider_highlight_row_contains_matched_char() {
    // Given a provider entry with model "llama3".
    let entry = make_picker_entry("llama3", "ollama", true, false);

    // When highlighting with match at byte 0 (the "l").
    #[expect(
        clippy::single_range_in_vec_init,
        reason = "genuinely want a slice containing one Range<usize>"
    )]
    let highlights: &[Range<usize>] = &[0..1];
    let line = entry.render_row_with_highlight(false, highlights);

    // Then the row text contains the matched character.
    let text: String = line.spans.iter().map(|s| &*s.content).collect();
    assert!(text.contains('l'), "highlighted row should contain 'l'");
}

#[rstest::rstest]
fn render_row_with_highlight_preserves_provider_name_suffix() {
    // Given a provider entry with model "gpt-4" and provider "openrouter".
    let entry = make_picker_entry("gpt-4", "openrouter", true, false);

    // When highlighting with match at byte 0.
    #[expect(
        clippy::single_range_in_vec_init,
        reason = "genuinely want a slice containing one Range<usize>"
    )]
    let highlights: &[Range<usize>] = &[0..1];
    let line = entry.render_row_with_highlight(false, highlights);

    // Then the full text still contains provider name.
    let text: String = line.spans.iter().map(|s| &*s.content).collect();
    assert!(text.contains("openrouter"), "should contain provider name");
    assert!(text.contains("gpt-4"), "should contain model");
}

#[rstest::rstest]
fn display_label_includes_provider_name() {
    // Given a ProviderPickerEntry with model "glm5.1" and provider_name "zai".
    let entry = make_picker_entry("glm5.1", "zai", true, false);

    // Then display_label contains both model and provider name.
    assert!(entry.display_label().contains("glm5.1"));
    assert!(entry.display_label().contains("zai"));
}

#[rstest::rstest]
fn display_label_allows_searching_by_provider_name() {
    // Given two entries with different providers.
    let zai_entry = make_picker_entry("glm5.1", "zai", true, false);
    let openrouter_entry = make_picker_entry("gpt-4o", "openrouter", true, false);

    // Then the zai entry's display_label contains "zai".
    assert!(zai_entry.display_label().contains("zai"));
    // And the openrouter entry's display_label does NOT contain "zai".
    assert!(!openrouter_entry.display_label().contains("zai"));
}

#[rstest::rstest]
fn provider_name_match_appears_in_highlighted_row() {
    // Given a provider entry with model "llama3" and provider "ollama".
    let entry = make_picker_entry("llama3", "ollama", true, false);

    // When highlighting with match in the provider-name portion (byte 7..13 = "ollama").
    #[expect(
        clippy::single_range_in_vec_init,
        reason = "API takes a slice of ranges"
    )]
    let line = entry.render_row_with_highlight(false, &[7..13]);

    // Then the row text contains the provider name characters.
    let text: String = line.spans.iter().map(|s| &*s.content).collect();
    assert!(
        text.contains('o'),
        "highlighted row should contain 'o' from provider name"
    );
}
