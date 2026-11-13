//! End-to-end crossing tests for the provider-selection slice.
//!
//! These drive the composed system through the provider family's real
//! crossing: the slice's `ProviderActor` is spawned by the shared
//! `launch_for_test` harness (the production activation path), and each
//! test publishes the same command/event an intent or the discover actor
//! would, then asserts on the *observable* result — the session's model,
//! the provider cell's cache, the picker items.
//!
//! Boot's provider-init actor gets its own test in `boot.rs` (it needs
//! the boot trio's `EnvironmentLoaded` trigger).
#![allow(
    clippy::indexing_slicing,
    reason = "integration test: fixture vectors with known lengths"
)]
#![allow(clippy::expect_used, clippy::panic, reason = "test code")]

use std::time::Duration;

use jinn_core_types::model_selection::ModelSelection;
use jinn_kernel::AppCore;
use jinn_provider_config::ModelInfo;
use jinn_provider_selection_msg::{
    LoadProviderPickerEntries, ModelCacheLoaded, ModelsRefreshed, ProviderCell, ProviderSwitch,
    provider_state_slot,
};
use jinn_tui::TuiApp;
use trouper::actor::ActorPath;

use crate::common::launch_for_test;

/// The provider actor's static path (the slice's `PROVIDER_ACTOR_PATH`).
const PROVIDER_PATH: &str = "jinn.provider.actor";

/// A composed app over the shared slice harness. The harness activates
/// provider-selection, so the provider + discover actors are already
/// subscribed when this returns.
async fn composed_app() -> TuiApp {
    let services = jinn_kernel::Services::new_fake().await;
    let state = jinn_kernel::State::new(jinn_kernel::AppState::default());
    let core = AppCore {
        state: state.clone(),
        bridge: services.bridge.clone(),
    };
    launch_for_test(core, services).await
}

/// The endpoint picker's cell handle from the composed app's services.
fn endpoint_picker_of(
    app: &TuiApp,
) -> jinn_slices::cell::TypedCell<jinn_provider_selection_msg::endpoint::EndpointPickerState> {
    app.services
        .slices
        .reader(&jinn_provider_selection_msg::endpoint::endpoint_picker_slot())
        .expect("provider-selection activation mints the endpoint picker cell")
}

/// The provider cell handle from the composed app's services.
fn cell_of(app: &TuiApp) -> jinn_slices::TypedCell<ProviderCell> {
    app.services
        .slices
        .reader(&provider_state_slot())
        .expect("provider-selection activation mints the provider cell")
}

#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(20))]
async fn provider_switch_sets_the_session_model() {
    // Given a composed app with the provider actor live.
    let app = composed_app().await;
    let session_id = app.core.state.read().session.active_session_id().clone();

    // When the provider switch command is delivered to the actor.
    app.services
        .trouper_system
        .tell(
            ActorPath::new(PROVIDER_PATH),
            ProviderSwitch {
                session_id,
                provider_id: ModelSelection::Single("ollama/llama3".to_owned()),
            },
        )
        .await
        .expect("provider switch command delivers");

    // The session's model is the switched selection, so wait for the state
    // to carry it. Reading it after a fixed sleep could happen before the
    // actor wrote it, and the assertion would prove nothing.
    jinn_testutil::wait::poll_until_async(Duration::from_secs(10), || {
        app.core.state.read().active_session().profile().model
            == ModelSelection::Single("ollama/llama3".to_owned())
    })
    .await;

    // Then the session's model is the switched selection.
    let model = app
        .core
        .state
        .read()
        .active_session()
        .profile()
        .model
        .clone();
    assert_eq!(model, ModelSelection::Single("ollama/llama3".to_owned()));
}

#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(20))]
async fn models_refreshed_merges_results_into_the_cell_cache() {
    // Given a composed app with the provider actor live.
    let app = composed_app().await;
    let session_id = app.core.state.read().session.active_session_id().clone();

    // When the discover actor's event arrives with one discovered model.
    let mut results = std::collections::HashMap::new();
    results.insert(
        "ollama".to_owned(),
        vec![ModelInfo {
            id: "llama3".to_owned(),
            context_length: Some(8192),
            input_modalities: jinn_provider_config::InputModalities::text(),
        }],
    );
    app.services
        .bus
        .publish(ModelsRefreshed {
            session_id,
            results,
            errors: std::collections::HashMap::new(),
        })
        .await;
    // The cell's cache carries the discovered model, so wait for the
    // cell to hold it. Reading it after a fixed sleep could happen before
    // the actor wrote it, and the assertion would prove nothing.
    jinn_testutil::wait::await_cell(&cell_of(&app), Duration::from_secs(10), |state| {
        state.model_cache.is_some()
    })
    .await;

    // Then the cell's cache carries the discovered model.
    let cache = cell_of(&app)
        .read()
        .model_cache
        .clone()
        .expect("actor should have merged the discovery results");
    assert_eq!(cache.entries["ollama"][0].id, "llama3");
    assert_eq!(cache.entries["ollama"][0].context_length, Some(8192));
}

#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(20))]
async fn models_refreshed_keeps_partial_results_when_one_provider_fails() {
    // Given a composed app with the provider actor live.
    let app = composed_app().await;
    let session_id = app.core.state.read().session.active_session_id().clone();

    // When a two-provider discovery returns one result and one error.
    let mut results = std::collections::HashMap::new();
    results.insert(
        "ollama".to_owned(),
        vec![ModelInfo {
            id: "llama3".to_owned(),
            context_length: None,
            input_modalities: jinn_provider_config::InputModalities::text(),
        }],
    );
    let mut errors = std::collections::HashMap::new();
    errors.insert("openai".to_owned(), "no api key".to_owned());
    app.services
        .bus
        .publish(ModelsRefreshed {
            session_id,
            results,
            errors,
        })
        .await;
    // The partial result is cached, so wait for the cell to hold it.
    // Reading it after a fixed sleep could happen before the actor wrote
    // it, and the assertion would prove nothing.
    jinn_testutil::wait::await_cell(&cell_of(&app), Duration::from_secs(10), |state| {
        state
            .model_cache
            .as_ref()
            .is_some_and(|cache| cache.entries.contains_key("ollama"))
    })
    .await;

    // Then the successful provider's models are cached (the failure does
    // not discard the partial result).
    let cache = cell_of(&app)
        .read()
        .model_cache
        .clone()
        .expect("partial results are still cached");
    assert!(
        cache.entries.contains_key("ollama"),
        "the successful provider's results survive a sibling failure"
    );
    assert!(
        !cache.entries.contains_key("openai"),
        "the failed provider contributes no entries"
    );
}

#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(20))]
async fn model_cache_loaded_restores_the_disk_cache_into_the_cell() {
    // Given a composed app with the provider actor live.
    let app = composed_app().await;

    // When a disk-loaded cache arrives (boot's init actor publishes it).
    let mut cache = jinn_provider_config::ModelCache::new();
    cache.entries.insert(
        "ollama".to_owned(),
        vec![ModelInfo {
            id: "llama3".to_owned(),
            context_length: Some(16384),
            input_modalities: jinn_provider_config::InputModalities::text(),
        }],
    );
    app.services.bus.publish(ModelCacheLoaded { cache }).await;
    // The cell holds the restored cache, so wait for it. Reading it
    // after a fixed sleep could happen before the actor wrote it.
    jinn_testutil::wait::await_cell(&cell_of(&app), Duration::from_secs(10), |state| {
        state.model_cache.is_some()
    })
    .await;

    // Then the cell holds the restored cache.
    let loaded = cell_of(&app)
        .read()
        .model_cache
        .clone()
        .expect("disk cache restored into the cell");
    assert_eq!(loaded.entries["ollama"][0].context_length, Some(16384));
}

#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(20))]
async fn load_provider_picker_entries_fills_the_picker_from_the_registry() {
    // Given a composed app with a provider registry seeded with one model.
    let app = {
        let app = composed_app().await;
        let services = app.services.clone();
        let registry = jinn_provider_config::ProviderRegistry::from_config(
            jinn_provider_config::ProvidersConfig {
                providers: std::collections::BTreeMap::from([(
                    "ollama".to_owned(),
                    jinn_provider_config::ProviderEntry {
                        model_info: Vec::new(),
                        backend: "ollama".to_owned(),
                        models: vec!["llama3".to_owned()],
                        base_url: None,
                        api_key_env: None,
                        requires_key: false,
                        extra_body: None,
                        context_length: None,
                    },
                )]),
                aliases: vec![],
                default_provider: None,
                endpoint_defaults: vec![],
            },
        )
        .expect("registry builds");
        services.provider_registry.replace(registry);
        app
    };

    // When the picker-open command is delivered to the actor.
    app.services
        .trouper_system
        .tell(ActorPath::new(PROVIDER_PATH), LoadProviderPickerEntries)
        .await
        .expect("load command delivers");
    // The picker cell carries the configured model, so wait for it.
    // Reading it after a fixed sleep could happen before the actor wrote
    // it, and the assertion would prove nothing.
    {
        let cell: jinn_slices::cell::TypedCell<jinn_provider_selection_msg::ProviderPickerState> =
            app.services
                .slices
                .reader(&jinn_provider_selection_msg::provider_picker_slot())
                .expect("the provider slice registers the model picker cell at activation");
        jinn_testutil::wait::await_cell(&cell, Duration::from_secs(10), |state| {
            state
                .selection
                .items()
                .iter()
                .any(|item| item.entry().model == "llama3")
        })
        .await;
    }

    // Then the model picker is populated with the configured model — in its
    // own cell, since the menu is slice-owned and has no kernel-side mirror.
    let models: Vec<String> = {
        let cell: jinn_slices::cell::TypedCell<jinn_provider_selection_msg::ProviderPickerState> =
            app.services
                .slices
                .reader(&jinn_provider_selection_msg::provider_picker_slot())
                .expect("the provider slice registers the model picker cell at activation");
        cell.read()
            .selection
            .items()
            .iter()
            .map(|i| i.entry().model.clone())
            .collect()
    };
    assert!(
        models.iter().any(|m| m == "llama3"),
        "picker should carry the configured model, got: {models:?}"
    );
}

/// A registry offering exactly one non-OpenRouter model, `ollama/llama3`.
fn ollama_only_registry() -> jinn_provider_config::ProviderRegistry {
    jinn_provider_config::ProviderRegistry::from_config(jinn_provider_config::ProvidersConfig {
        providers: std::collections::BTreeMap::from([(
            "ollama".to_owned(),
            jinn_provider_config::ProviderEntry {
                model_info: Vec::new(),
                backend: "ollama".to_owned(),
                models: vec!["llama3".to_owned()],
                base_url: None,
                api_key_env: None,
                requires_key: false,
                extra_body: None,
                context_length: None,
            },
        )]),
        aliases: vec![],
        default_provider: None,
        endpoint_defaults: vec![],
    })
    .expect("registry builds")
}

#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(20))]
async fn endpoint_load_for_non_openrouter_model_clears_loading_and_shows_one_row() {
    // Given a composed app with a non-OpenRouter model selected and the
    // loading flag pre-set (what the endpoint-picker open intent does).
    let app = composed_app().await;
    app.services
        .provider_registry
        .replace(ollama_only_registry());
    app.core
        .state
        .write()
        .active_session_mut()
        .set_model(ModelSelection::Single("ollama/llama3".to_owned()));
    let cell = cell_of(&app);
    cell.update(|c| c.endpoint_loading = true);

    // When the endpoint-picker open command is delivered.
    app.services
        .trouper_system
        .tell(
            ActorPath::new(PROVIDER_PATH),
            jinn_provider_selection_msg::LoadEndpointPickerEntries,
        )
        .await
        .expect("endpoint load delivers");
    // The endpoint picker clears the loading flag and lands its row, so
    // wait for both. Reading the cell after a fixed sleep could happen
    // before the actor wrote it, and the assertion would prove nothing.
    jinn_testutil::wait::await_cell(&cell, Duration::from_secs(10), |state| {
        !state.endpoint_loading && state.endpoint_fetched_at.is_none()
    })
    .await;
    {
        let picker = endpoint_picker_of(&app);
        jinn_testutil::wait::await_cell(&picker, Duration::from_secs(10), |state| {
            state.selection.items().len() == 1
        })
        .await;
    }

    // Then the loading flag is cleared (no stuck spinner).
    assert!(
        !cell.read().endpoint_loading,
        "the non-OpenRouter branch must clear the loading flag"
    );
    // And the fetch timestamp is untouched (this path never fetched).
    assert!(
        cell.read().endpoint_fetched_at.is_none(),
        "the non-OpenRouter branch never stamps a fetch time"
    );
    // And the picker shows exactly the auto-route sentinel row.
    let rows = endpoint_picker_of(&app).read().selection.items().len();
    assert_eq!(
        rows, 1,
        "a non-OpenRouter model shows the single explanatory row"
    );
}
