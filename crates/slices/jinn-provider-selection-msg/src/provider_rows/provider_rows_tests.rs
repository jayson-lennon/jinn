//! The shared provider-row builder's tests: loading (availability,
//! aliases, remote merge) and sorting (blocks, promotion, active
//! marking). Moved with the builder from the slice's `entries` module —
//! the TUI's own tests keep exercising the same behavior through the
//! slice's re-exports.

//! Test code: assertions use expect/panic to state what went wrong.
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code"
)]

use jinn_provider_config::AliasEntry;
use jinn_provider_config::ApiKeys;
use jinn_provider_config::ProviderEntry;
use jinn_provider_config::ProviderRegistry;
use jinn_provider_config::ProvidersConfig;
use std::collections::BTreeMap;

use crate::ProviderPickerEntry;
use crate::provider_rows::{
    FilteredEntry, TextSegment, filter_entries, load_provider_entries, promote_selected_to_top,
    sorted_entries,
};
use jinn_theme::default_theme;

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
    providers: BTreeMap<String, ProviderEntry>,
    aliases: Vec<AliasEntry>,
    default_provider: Option<&str>,
) -> ProvidersConfig {
    ProvidersConfig {
        providers,
        aliases,
        default_provider: default_provider.map(String::from),
        endpoint_defaults: Vec::new(),
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
    // When loading its provider entries.
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
    // When loading its provider entries.
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
        vec![AliasEntry {
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
    // When loading its provider entries.
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
    // When reading the requested field off the alias entry.
    let (_, alias) = load_entries_with_alias();
    let actual = match field {
        "name" => alias.name.as_str(),
        "provider_id" => alias.provider_id.as_str(),
        "alias_target" => alias.alias_target.as_deref().unwrap_or(""),
        "provider_name" => alias.provider_name.as_str(),
        "backend" => alias.backend.as_str(),
        "model" => alias.model.as_str(),
        _ => panic!("unknown field: {field}"),
    };

    // Then the alias entry has the expected field value.
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
            AliasEntry {
                name: "fast".to_owned(),
                target: "ollama/llama3".to_owned(),
            },
            AliasEntry {
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
    let mut cache = jinn_provider_config::ModelCache::new();
    cache.entries.insert(
        "ollama".to_owned(),
        vec![jinn_provider_config::ModelInfo {
            id: "mistral".to_owned(),
            context_length: None,
            input_modalities: jinn_provider_config::InputModalities::text(),
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
    let mut cache = jinn_provider_config::ModelCache::new();
    cache.entries.insert(
        "ollama".to_owned(),
        vec![jinn_provider_config::ModelInfo {
            id: "mistral".to_owned(),
            context_length: None,
            input_modalities: jinn_provider_config::InputModalities::text(),
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
    let mut cache = jinn_provider_config::ModelCache::new();
    cache.entries.insert(
        "ollama".to_owned(),
        vec![
            jinn_provider_config::ModelInfo {
                id: "llama3".to_owned(),
                context_length: None,
                input_modalities: jinn_provider_config::InputModalities::text(),
            },
            jinn_provider_config::ModelInfo {
                id: "mistral".to_owned(),
                context_length: None,
                input_modalities: jinn_provider_config::InputModalities::text(),
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
    let mut cache = jinn_provider_config::ModelCache::new();
    cache.entries.insert(
        "ollama".to_owned(),
        vec![
            jinn_provider_config::ModelInfo {
                id: "llama3".to_owned(),
                context_length: None,
                input_modalities: jinn_provider_config::InputModalities::text(),
            },
            jinn_provider_config::ModelInfo {
                id: "mistral".to_owned(),
                context_length: None,
                input_modalities: jinn_provider_config::InputModalities::text(),
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
    let mut cache = jinn_provider_config::ModelCache::new();
    cache.entries.insert(
        "openrouter".to_owned(),
        vec![jinn_provider_config::ModelInfo {
            id: "claude-3".to_owned(),
            context_length: None,
            input_modalities: jinn_provider_config::InputModalities::text(),
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
    let mut cache = jinn_provider_config::ModelCache::new();
    cache.entries.insert(
        "openrouter".to_owned(),
        vec![jinn_provider_config::ModelInfo {
            id: "claude-3".to_owned(),
            context_length: None,
            input_modalities: jinn_provider_config::InputModalities::text(),
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

    let mut cache = jinn_provider_config::ModelCache::new();
    cache.entries.insert(
        "ollama".to_owned(),
        vec![
            jinn_provider_config::ModelInfo {
                id: "mistral".to_owned(),
                context_length: None,
                input_modalities: jinn_provider_config::InputModalities::text(),
            },
            jinn_provider_config::ModelInfo {
                id: "codellama".to_owned(),
                context_length: None,
                input_modalities: jinn_provider_config::InputModalities::text(),
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
        make_sorted_entry("a"),
        make_sorted_entry("b"),
        make_sorted_entry("c"),
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
        make_sorted_entry("a"),
        make_sorted_entry("b"),
        make_sorted_entry("c"),
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
    let entries = vec![make_sorted_entry("a"), make_sorted_entry("b")];

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
        make_sorted_entry("z").with_availability(false),
        make_sorted_entry("a"),
        make_sorted_entry("b").with_availability(false),
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
        make_model_entry("a", "zebra"),
        make_model_entry("b", "alpha"),
    ];

    // When sorting with empty filter and no active provider.
    let result = sorted_entries(&entries, "", "__no_provider__");

    // Then entries are sorted alphabetically by model name.
    assert_eq!(result[0].provider_id, "b/alpha");
    assert_eq!(result[1].provider_id, "a/zebra");
}

/// One available `{name}/{model}` entry with an explicit model.
fn make_model_entry(name: &str, model: &str) -> ProviderPickerEntry {
    ProviderPickerEntry {
        provider_id: format!("{name}/{model}"),
        name: name.to_owned(),
        provider_name: name.to_owned(),
        backend: name.to_owned(),
        model: model.to_owned(),
        search_text: format!("{model} {name}"),
        is_alias: false,
        alias_target: None,
        is_available: true,
        is_remote: false,
        is_active: false,
        selected: false,
        theme: default_theme(),
    }
}

/// One available `{name}/model` entry for the sort tests.
fn make_sorted_entry(name: &str) -> ProviderPickerEntry {
    ProviderPickerEntry {
        provider_id: format!("{name}/model"),
        name: name.to_owned(),
        provider_name: name.to_owned(),
        backend: name.to_owned(),
        model: "model".to_owned(),
        search_text: format!("model {name}"),
        is_alias: false,
        alias_target: None,
        is_available: true,
        is_remote: false,
        is_active: false,
        selected: false,
        theme: default_theme(),
    }
}

impl ProviderPickerEntry {
    /// A chainable availability flip for test setup readability.
    fn with_availability(mut self, is_available: bool) -> Self {
        self.is_available = is_available;
        self
    }
}

#[rstest::rstest]
fn promote_selected_to_top_moves_checked_entries_first() {
    // Given alphabetically ordered entries where one in the middle is selected.
    let mut entries = vec![
        {
            let mut e = make_sorted_entry("a");
            e.model = "alpha".to_owned();
            e
        },
        {
            let mut e = make_sorted_entry("b");
            e.model = "beta".to_owned();
            e.selected = true;
            e
        },
        {
            let mut e = make_sorted_entry("c");
            e.model = "gamma".to_owned();
            e
        },
    ];

    // When promoting the selected entries to the top.
    promote_selected_to_top(&mut entries);

    // Then the selected entry is first and the others keep their order.
    assert_eq!(entries[0].provider_id, "b/model");
    assert!(entries[0].selected);
    assert_eq!(entries[1].provider_id, "a/model");
    assert_eq!(entries[2].provider_id, "c/model");
}

// ── The shared fuzzy filter (entries level) ─────────────────────────

#[rstest::rstest]
fn filter_entries_empty_filter_passes_entries_through_unchanged() {
    // Given sorted entries in a known order.
    let input_order = ["a/llama3", "b/mistral"];
    let entries = vec![
        make_model_entry("a", "llama3"),
        make_model_entry("b", "mistral"),
    ];

    // When filtering with an empty filter.
    let filtered = filter_entries(&entries, "");

    // Then every entry passes through in input order with no match indices.
    let ids: Vec<&str> = filtered
        .iter()
        .map(|f| f.entry.provider_id.as_str())
        .collect();
    assert_eq!(ids, input_order);
    assert!(filtered.iter().all(|f| f.match_indices.is_empty()));
}

#[rstest::rstest]
fn filter_entries_fuzzy_query_matches_what_no_substring_could() {
    // Given an entry whose search text is "gpt-4o openai".
    let entries = vec![
        make_model_entry("openai", "gpt-4o"),
        make_model_entry("ollama", "llama3"),
    ];

    // When filtering with the fuzzy abbreviation "gp4" (a subsequence,
    // not a substring, of the search text).
    let filtered = filter_entries(&entries, "gp4");

    // Then only the gpt entry survives, and the substring filter would
    // have dropped it — the parity-discriminating case.
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].entry.provider_id, "openai/gpt-4o");
    assert!(!filtered[0].entry.search_text.contains("gp4"));
}

#[rstest::rstest]
fn filter_entries_multi_term_requires_every_term() {
    // Given a claude entry and a gpt entry.
    let entries = vec![
        make_model_entry("anthropic", "claude-3.5-sonnet"),
        make_model_entry("openai", "gpt-4o"),
    ];

    // When filtering with terms one entry satisfies, then terms none does.
    let both = filter_entries(&entries.clone(), "claude sonnet");
    let none = filter_entries(&entries, "claude gpt");

    // Then the AND keeps only the entry with both terms and drops all
    // otherwise.
    assert_eq!(both.len(), 1);
    assert_eq!(both[0].entry.provider_id, "anthropic/claude-3.5-sonnet");
    assert!(none.is_empty());
}

#[rstest::rstest]
fn filter_entries_orders_by_the_shared_match_core() {
    // Given entries whose alphabetical (sorted) order disagrees with the
    // fuzzy score order for the term "sonnet" (the word-boundary match
    // scores higher than the interior-substring one — the pairing the
    // widget's pinned sort test uses).
    let entries = vec![
        make_model_entry("a", "jsonnet-tools"),
        make_model_entry("b", "claude-sonnet"),
    ];

    // When filtering with "sonnet".
    let filtered = filter_entries(&entries, "sonnet");

    // Then the order is the shared core's score order, not the input's.
    assert_eq!(filtered[0].entry.provider_id, "b/claude-sonnet");
}

#[rstest::rstest]
fn filter_entries_ranges_agree_with_the_shared_match_core() {
    // Given entries the core was also handed directly.
    let entries = vec![
        make_model_entry("openai", "gpt-4o"),
        make_model_entry("ollama", "llama3"),
    ];
    let labels: Vec<&str> = entries.iter().map(|e| e.search_text.as_str()).collect();

    // When filtering both ways with a fuzzy query.
    let filtered = filter_entries(&entries.clone(), "gpt");
    let core = jinn_selection_widget::fuzzy_filter(&labels, "gpt");

    // Then each kept entry carries exactly the ranges the core
    // computed for its position, in the core's order.
    assert_eq!(filtered.len(), core.len());
    for (f, c) in filtered.iter().zip(&core) {
        assert_eq!(f.entry.search_text, labels[c.index]);
        assert_eq!(f.match_indices, c.match_indices);
    }
}

// ── The highlight segments (matched/plain runs) ─────────────────────

#[rstest::rstest]
fn segments_empty_indices_yield_one_plain_run_per_field() {
    // Given a filtered entry with empty match indices (the empty-filter
    // passthrough shape).
    let filtered = FilteredEntry {
        entry: make_model_entry("openai", "gpt-4o"),
        match_indices: Vec::new(),
    };

    // When splitting into segments.
    let segments = filtered.segments();

    // Then each field is one unmarked run of its full text.
    assert_eq!(
        segments.model,
        vec![TextSegment {
            text: "gpt-4o".to_owned(),
            matched: false,
        }]
    );
    assert_eq!(
        segments.provider,
        vec![TextSegment {
            text: "openai".to_owned(),
            matched: false,
        }]
    );
}

#[rstest::rstest]
fn segments_mark_matched_runs_in_both_fields() {
    // Given a filtered entry for "gpt-4o openai" where the query
    // matches bytes in the model and the provider name.
    let entry = make_model_entry("openai", "gpt-4o");
    // search_text = "gpt-4o openai": bytes 0-2 = "gpt" (model), byte 8
    // = "p" starting "pen" inside the provider name (provider offset
    // = 6 + 1 = 7).
    let filtered = FilteredEntry {
        entry,
        match_indices: vec![0, 1, 2, 8],
    };

    // When splitting into segments.
    let segments = filtered.segments();

    // Then the model field marks "gpt" and leaves "-4o" plain, and the
    // provider field marks "pen" (the matcher matched the 'p' and the
    // highlight run extends to the grapheme where marking stops).
    assert_eq!(
        segments.model,
        vec![
            TextSegment {
                text: "gpt".to_owned(),
                matched: true,
            },
            TextSegment {
                text: "-4o".to_owned(),
                matched: false,
            },
        ]
    );
    assert_eq!(
        segments.provider,
        vec![
            TextSegment {
                text: "o".to_owned(),
                matched: false,
            },
            TextSegment {
                text: "p".to_owned(),
                matched: true,
            },
            TextSegment {
                text: "enai".to_owned(),
                matched: false,
            },
        ]
    );
}

#[rstest::rstest]
fn segments_splits_on_grapheme_boundaries_for_multibyte_text() {
    // Given a provider name with a multibyte grapheme cluster.
    let entry = make_model_entry("örüm", "llama3");
    // search_text = "llama3 örüm": the model is 6 bytes, the separator at 6,
    // the provider starts at 7; the 'ü' grapheme spans provider-relative
    // bytes 2-3, so absolute byte 9 (its second byte) remaps to 2 — the
    // start of that cluster.
    let filtered = FilteredEntry {
        entry,
        match_indices: vec![9],
    };

    // When splitting into segments.
    let segments = filtered.segments();

    // Then the runs split without panicking on a UTF-8 boundary and
    // the concatenation of every run reconstructs each field exactly.
    assert_eq!(
        segments
            .provider
            .iter()
            .map(|s| s.text.as_str())
            .collect::<String>(),
        "örüm"
    );
    let marked: Vec<&str> = segments
        .provider
        .iter()
        .filter(|s| s.matched)
        .map(|s| s.text.as_str())
        .collect();
    assert_eq!(marked.join(""), "r");
}
