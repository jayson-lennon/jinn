//! Provider picker loader - loads provider entries into picker state.

use super::entries::{load_provider_entries, promote_selected_to_top, sorted_entries};
use crate::common::services::Services;
use crate::common::tcaps::provider::{
    FrontendProviderPickerWrite, ModelCacheWrite, ProviderPickerWrite, ProviderView,
};
use crate::feat::endpoint::picker_entry::EndpointEntry;
use crate::feat::provider_infra;
use crate::feat::session::model_selection::ModelSelection;

/// Loads provider entries into the picker state, ready for display.
///
/// Reads from the provider registry and model cache, applies available-first
/// sorting and active-provider promotion, then stores the entries via
/// `SelectionState::set_items`.
pub fn load_provider_picker_items(services: &Services, view: &mut ProviderView<'_>) {
    let registry = services.provider_registry.read();
    let api_keys = services.api_keys.read();
    let all = load_provider_entries(
        &registry,
        &api_keys,
        view.provider.model_cache(),
        view.provider_frontend.theme(),
    );

    let model_selection = view.session.active_session().profile().model.clone();
    let active_model = model_selection.display_str().to_owned();
    let mut entries = sorted_entries(&all, "", &active_model);

    // Pre-check entries matching the current model selection, but only when
    // the picker is in alloy mode. Single mode never builds checkmarks.
    if view.provider.is_alloy_mode() {
        pre_check_active_models(&mut entries, &model_selection);
        promote_selected_to_top(&mut entries);
    }

    let wrapped = crate::feat::picker::registry::build_picker_registry()
        .make_items(crate::feat::picker::registry::PROVIDER_ID, entries)
        .unwrap_or_default();
    view.provider.set_provider_picker_items(wrapped);
}

/// Sets `selected = true` on entries matching the current model selection.
///
/// For `Single`, checks the one matching entry. For `Alloy`, checks all member entries.
pub(crate) fn pre_check_active_models(
    entries: &mut [crate::protocol::ProviderPickerEntry],
    selection: &ModelSelection,
) {
    let model_ids: Vec<&str> = match selection {
        ModelSelection::Single(s) => vec![s],
        ModelSelection::Alloy { models, .. } => models.iter().map(String::as_str).collect(),
    };
    for entry in entries.iter_mut() {
        if model_ids.iter().any(|id| *id == entry.provider_id) {
            entry.selected = true;
        }
    }
}

/// The credentials + model id needed to query OpenRouter's `/endpoints`.
///
/// Returned by [`resolve_openrouter_target`] only when the active session's
/// model is `Single` and its configured backend resolves to OpenRouter.
pub(crate) struct OpenRouterTarget {
    model_id: String,
    base_url: String,
    api_key: String,
}

impl OpenRouterTarget {
    /// The resolved model id — used as the per-model endpoint cache key.
    pub(crate) fn model_id(&self) -> &str {
        &self.model_id
    }
}

/// Resolves whether the active session's model is served via OpenRouter.
///
/// Returns `Some(target)` only when the model selection is `Single` and the
/// resolved provider's backend parses to `Backend::OpenRouter`. Otherwise
/// returns `None` — the picker then renders a single "not served via
/// OpenRouter" explanatory row.
///
/// This is the backend gate: it runs in the actor (which owns `Services`)
/// rather than the validator, because the `IntentHandler` cannot reach the
/// provider registry.
pub(crate) fn resolve_openrouter_target(
    services: &Services,
    model: &ModelSelection,
) -> Option<OpenRouterTarget> {
    let ModelSelection::Single(provider_id) = model else {
        return None;
    };
    let registry = services.provider_registry.read();
    let resolved = registry.get(&provider_infra::ProviderId::new(provider_id.clone()))?;
    if resolved.backend.parse::<jinn_provider::Backend>().ok()?
        != jinn_provider::Backend::OpenRouter
    {
        return None;
    }
    let api_key = if resolved.requires_key {
        let env_var = resolved.api_key_env.as_ref()?;
        services.api_keys.get(env_var)?
    } else {
        String::new()
    };
    let base_url = resolved.base_url.clone().unwrap_or_else(|| {
        jinn_provider::ProviderConfig::openrouter()
            .default_base_url
            .to_owned()
    });
    Some(OpenRouterTarget {
        model_id: resolved.model.clone(),
        base_url,
        api_key,
    })
}

/// Fetches the OpenRouter endpoints for `target` from the network.
///
/// Returns `Ok(endpoints)` on success or `Err(())` on any failure (network,
/// HTTP, parse). The caller decides what to render from the outcome.
pub(crate) async fn fetch_endpoints(
    target: &OpenRouterTarget,
) -> Result<Vec<jinn_provider::EndpointInfo>, ()> {
    jinn_provider::list_endpoints_default_client(
        &target.base_url,
        &target.model_id,
        &target.api_key,
        &[],
    )
    .await
    .map_err(|_e| ())
}

/// Builds the picker entries from a list of upstream endpoints.
///
/// Always prepends the "Default (auto-route)" sentinel; real endpoints are
/// mapped one-to-one. The currently pinned endpoint, if any, is marked
/// `is_active`. This is pure: same inputs always produce the same entries,
/// so both the fresh-fetch path and the cache-hit path reuse it.
pub(crate) fn build_endpoint_entries(
    endpoints: &[jinn_provider::EndpointInfo],
    theme: &crate::feat::theme::Theme,
    pinned: Option<&crate::feat::endpoint::Endpoint>,
) -> Vec<EndpointEntry> {
    let mut entries = Vec::with_capacity(endpoints.len() + 1);
    entries.push(EndpointEntry::auto_route(pinned.is_none(), theme.clone()));
    for ep in endpoints {
        let is_active = pinned.is_some_and(|p| p.tag == ep.tag);
        entries.push(EndpointEntry {
            tag: ep.tag.clone(),
            provider_name: ep.provider_name.clone(),
            uptime_30m: ep.uptime_30m,
            prompt_price: ep.prompt_price.clone(),
            completion_price: ep.completion_price.clone(),
            quantization: ep.quantization.clone(),
            max_completion_tokens: ep.max_completion_tokens,
            is_active,
            theme: theme.clone(),
        });
    }
    entries
}

/// Builds the single-row list shown when the active model is not served via
/// OpenRouter (or is an alloy). The row is an inert explanatory placeholder;
/// its empty `tag` means confirming it clears any pin.
pub(crate) fn unavailable_endpoint_entries(
    theme: crate::feat::theme::Theme,
    pinned: Option<&crate::feat::endpoint::Endpoint>,
) -> Vec<EndpointEntry> {
    vec![EndpointEntry::auto_route(pinned.is_none(), theme)]
}

/// Loads OpenRouter endpoint entries into the picker from already-fetched data.
pub(crate) fn set_endpoint_picker_items(view: &mut ProviderView<'_>, entries: Vec<EndpointEntry>) {
    view.provider_frontend.set_endpoint_picker_items(entries);
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::*;
    use crate::common::app_state::AppState;
    use crate::common::services::test_services::TestServices;
    use crate::common::tcaps::provider::ProviderView;
    use crate::feat::provider_infra::{ProviderEntry, ProvidersConfig};
    use crate::feat::session::model_selection::{AlloyStrategy, ModelSelection};
    use std::collections::BTreeMap;

    #[rstest::rstest]
    fn build_endpoint_entries_prepends_sentinel_and_marks_pinned_active() {
        // Given two upstream endpoints and a pin on the second one's tag.
        let theme = crate::feat::theme::default_theme();
        let endpoints = vec![
            jinn_provider::EndpointInfo {
                tag: "azure".to_owned(),
                provider_name: "Azure".to_owned(),
                uptime_30m: Some(99.9),
                prompt_price: None,
                completion_price: None,
                quantization: None,
                max_completion_tokens: None,
            },
            jinn_provider::EndpointInfo {
                tag: "anthropic".to_owned(),
                provider_name: "Anthropic".to_owned(),
                uptime_30m: Some(99.7),
                prompt_price: None,
                completion_price: None,
                quantization: None,
                max_completion_tokens: None,
            },
        ];
        let pinned = crate::feat::endpoint::Endpoint {
            tag: "anthropic".to_owned(),
            provider_name: "Anthropic".to_owned(),
        };

        // When building entries (pure: same inputs always produce same output).
        let entries = build_endpoint_entries(&endpoints, &theme, Some(&pinned));

        // Then the auto-route sentinel is prepended and is NOT active (a pin exists).
        assert!(entries[0].tag.is_empty(), "first entry is the sentinel");
        assert!(!entries[0].is_active, "sentinel inactive when a pin exists");
        // And the pinned endpoint (anthropic) is the only active one.
        let anthropic = entries
            .iter()
            .find(|e| e.tag == "anthropic")
            .expect("anthropic entry");
        assert!(anthropic.is_active, "pinned entry must be active");
        assert_eq!(
            entries.iter().filter(|e| e.is_active).count(),
            1,
            "exactly one active entry"
        );
    }

    #[rstest::rstest]
    fn load_picker_with_single_model_checks_matching_entry() {
        // Given a state with a single model and a provider picker.
        let services = TestServices::builder()
            .with_providers(ProvidersConfig {
                providers: BTreeMap::from([(
                    "ollama".to_owned(),
                    ProviderEntry {
                        model_info: Vec::new(),
                        backend: "ollama".to_owned(),
                        models: vec!["llama3".to_owned(), "mistral".to_owned()],
                        base_url: Some("http://localhost:11434".to_owned()),
                        api_key_env: None,
                        requires_key: false,
                        extra_body: None,
                        context_length: None,
                    },
                )]),
                aliases: vec![],
                default_provider: None,
            })
            .build();

        let mut state = AppState::default();
        state
            .active_session_mut()
            .set_model(ModelSelection::Single("ollama/llama3".to_owned()));

        state.provider.set_alloy_mode(true);
        // When loading provider picker items.
        load_provider_picker_items(
            &services,
            &mut ProviderView::from_app_state_for_test(&mut state),
        );

        // Then the entry matching the session model has selected = true.
        let items = state.provider.provider_picker.items();
        let llama = items
            .iter()
            .find(|e| e.entry().provider_id == "ollama/llama3")
            .expect("llama3");
        assert!(llama.entry().selected, "llama3 should be selected");

        // And the other entry is not selected.
        let mistral = items
            .iter()
            .find(|e| e.entry().provider_id == "ollama/mistral")
            .expect("mistral");
        assert!(!mistral.entry().selected, "mistral should not be selected");
    }

    #[rstest::rstest]
    fn load_picker_with_alloy_checks_all_member_entries() {
        // Given a state with an alloy of 2 models.
        let services = TestServices::builder()
            .with_providers(ProvidersConfig {
                providers: BTreeMap::from([(
                    "ollama".to_owned(),
                    ProviderEntry {
                        model_info: Vec::new(),
                        backend: "ollama".to_owned(),
                        models: vec![
                            "llama3".to_owned(),
                            "mistral".to_owned(),
                            "gemma".to_owned(),
                        ],
                        base_url: Some("http://localhost:11434".to_owned()),
                        api_key_env: None,
                        requires_key: false,
                        extra_body: None,
                        context_length: None,
                    },
                )]),
                aliases: vec![],
                default_provider: None,
            })
            .build();

        let mut state = AppState::default();
        state.active_session_mut().set_model(ModelSelection::Alloy {
            models: vec!["ollama/llama3".to_owned(), "ollama/mistral".to_owned()],
            strategy: AlloyStrategy::RoundRobin { index: 0 },
        });

        state.provider.set_alloy_mode(true);
        // When loading provider picker items.
        load_provider_picker_items(
            &services,
            &mut ProviderView::from_app_state_for_test(&mut state),
        );

        // Then both alloy members are selected.
        let items = state.provider.provider_picker.items();
        let llama = items
            .iter()
            .find(|e| e.entry().provider_id == "ollama/llama3")
            .expect("llama3");
        assert!(llama.entry().selected, "llama3 should be selected");

        let mistral = items
            .iter()
            .find(|e| e.entry().provider_id == "ollama/mistral")
            .expect("mistral");
        assert!(mistral.entry().selected, "mistral should be selected");

        // And the non-member is not selected.
        let gemma = items
            .iter()
            .find(|e| e.entry().provider_id == "ollama/gemma")
            .expect("gemma");
        assert!(!gemma.entry().selected, "gemma should not be selected");
    }

    #[rstest::rstest]
    fn pre_checked_alloy_members_sort_to_top() {
        // Given a state with an alloy of llama3 and mistral, plus gemma as non-member.
        let services = TestServices::builder()
            .with_providers(ProvidersConfig {
                providers: BTreeMap::from([(
                    "ollama".to_owned(),
                    ProviderEntry {
                        model_info: Vec::new(),
                        backend: "ollama".to_owned(),
                        models: vec![
                            "gemma".to_owned(),
                            "llama3".to_owned(),
                            "mistral".to_owned(),
                        ],
                        base_url: Some("http://localhost:11434".to_owned()),
                        api_key_env: None,
                        requires_key: false,
                        extra_body: None,
                        context_length: None,
                    },
                )]),
                aliases: vec![],
                default_provider: None,
            })
            .build();

        let mut state = AppState::default();
        state.active_session_mut().set_model(ModelSelection::Alloy {
            models: vec!["ollama/llama3".to_owned(), "ollama/mistral".to_owned()],
            strategy: AlloyStrategy::RoundRobin { index: 0 },
        });

        state.provider.set_alloy_mode(true);
        // When loading picker items.
        load_provider_picker_items(
            &services,
            &mut ProviderView::from_app_state_for_test(&mut state),
        );

        // Then selected entries (llama3, mistral) appear before non-selected (gemma).
        let items = state.provider.provider_picker.items();
        let llama_idx = items
            .iter()
            .position(|e| e.entry().provider_id == "ollama/llama3")
            .expect("llama3");
        let mistral_idx = items
            .iter()
            .position(|e| e.entry().provider_id == "ollama/mistral")
            .expect("mistral");
        let gemma_idx = items
            .iter()
            .position(|e| e.entry().provider_id == "ollama/gemma")
            .expect("gemma");
        assert!(llama_idx < gemma_idx, "llama3 should sort above gemma");
        assert!(mistral_idx < gemma_idx, "mistral should sort above gemma");
    }
}
