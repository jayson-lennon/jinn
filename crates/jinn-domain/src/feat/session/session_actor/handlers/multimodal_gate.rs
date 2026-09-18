//! Vision-capability gate for multimodal sends.
//!
//! Before a user entry carrying image attachments is dispatched to the LLM,
//! the active model is checked against the overlaid model cache first (which
//! carries `providers.toml` `model_info` overrides, API discovery, and
//! models.dev enrichment), falling back to raw models.dev reference data. A
//! model that is **not confirmed** vision-capable is blocked: instead of
//! dispatching, an explanatory `ChatEntryKind::Error` entry is pushed and the
//! session returns to `Idle`.
//!
//! Only a model positively identified as image-capable is allowed through.
//! Unknown models are blocked, not allowed: the user marks a model capable via
//! `[[providers.model_info]]` `input_modalities` in `providers.toml`.

use crate::common::services::Services;
use crate::common::state::State;
use crate::feat::provider_infra::{Modality, ModelCache, ModelsDevData};
use crate::protocol::{ChatEntry, ChatEntryKind, SessionId};

/// Decides whether a user entry with attachments may be dispatched to the model.
///
/// Returns `None` only when the send is allowed — a text-only entry, or a model
/// confirmed image-capable by the overlaid model cache or models.dev. Returns
/// `Some(error_entry)` when the entry carries attachments but the model is
/// **not confirmed** image-capable.
///
/// Precedence: the overlaid model cache (which carries config, API, and
/// models.dev enrichment) is consulted first; raw models.dev is the fallback.
/// The `model_id` here is the full `provider/model` display form.
#[must_use]
pub fn attachment_gate(
    model_id: Option<&str>,
    entry: &ChatEntry,
    models_dev: &ModelsDevData,
    model_cache: Option<&ModelCache>,
) -> Option<ChatEntry> {
    let ChatEntryKind::User { attachments, .. } = &entry.kind else {
        return None;
    };
    if attachments.is_empty() {
        return None;
    }

    // No provider configured — nothing to gate (dispatch is a no-op).
    let model_id = model_id?;

    if let Some(info) = resolve_cached_model_info(model_cache, model_id) {
        // An entry in the overlaid cache decides the gate outright.
        return if info.input_modalities.contains(Modality::Image) {
            None
        } else {
            Some(blocked_error_entry(model_id))
        };
    }

    let bare_id = model_id.split_once('/').map_or(model_id, |(_, rest)| rest);
    match models_dev.supports_images(bare_id) {
        // Only a positively confirmed vision model is allowed through.
        Some(true) => None,
        // Some(false) → known text-only. None → unknown (not in reference data).
        // Both block: the user marks a model capable via providers.toml / models.dev.
        Some(false) | None => Some(blocked_error_entry(model_id)),
    }
}

/// Loads the models.dev reference data, resolves the session's active model,
/// and runs [`attachment_gate`].
///
/// Shared by both the Idle dispatch path (`SessionPersistenceActor`) and the
/// queue-drain path (the turn-dispatch slice's queue actor) so they gate
/// identically. Returns
/// `Some(error_entry)` when the entry carries attachments but the active model
/// is not confirmed image-capable; `None` when the entry is text-only or the
/// model is confirmed vision-capable (allowed through).
///
/// `model_id` is resolved from the session's profile at call time; if the model
/// changes between enqueue and drain (rare), the drain-time gate uses the
/// *current* model — the intended behavior.
#[must_use]
pub fn evaluate_attachment_gate(
    services: &Services,
    state: &State,
    session_id: &SessionId,
    entry: &ChatEntry,
) -> Option<ChatEntry> {
    let models_dev = ModelsDevData::load(
        services.paths.models_dev_user_path().as_path(),
        services.paths.models_dev_system_path().as_path(),
    );
    let (model_id, model_cache) = {
        let guard = state.read();
        let model_id = guard
            .session
            .get(session_id)
            .and_then(|s| s.profile().model.last_model())
            .map(str::to_owned);
        (model_id, guard.provider.model_cache.clone())
    };
    attachment_gate(
        model_id.as_deref(),
        entry,
        &models_dev,
        model_cache.as_ref(),
    )
}

/// Resolves a `provider/model` display form against the overlaid model cache.
///
/// Splits on the first `/` (so path-shaped model ids keep their slashes),
/// matching the status bar's lookup. Returns `None` when the cache is absent
/// or the model is not recorded in it.
fn resolve_cached_model_info<'a>(
    model_cache: Option<&'a ModelCache>,
    active_model: &str,
) -> Option<&'a crate::feat::provider_infra::ModelInfo> {
    let cache = model_cache?;
    let (provider_name, model_suffix) = active_model.split_once('/')?;
    cache
        .entries
        .get(provider_name)?
        .iter()
        .find(|m| m.id == model_suffix)
}

/// Builds the `Error` entry shown when a model is not confirmed vision-capable.
///
/// Names the model and points the user at `providers.toml` `model_info` so
/// they can mark it image-capable.
fn blocked_error_entry(model_id: &str) -> ChatEntry {
    ChatEntry::error(format!(
        "{model_id} is not confirmed to support image input, so this attachment was not sent. Switch to a vision-capable model, or mark it image-capable via [[providers.model_info]] input_modalities in providers.toml."
    ))
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
    use jinn_provider::Attachment;

    fn entry_with_image() -> ChatEntry {
        let mut entry = ChatEntry::user("describe this");
        if let ChatEntryKind::User { attachments, .. } = &mut entry.kind {
            attachments.push(Attachment::image("image/png", vec![1, 2, 3]));
        }
        entry
    }

    fn cache_with_image_modalities() -> crate::feat::provider_infra::ModelCache {
        let mut entries = std::collections::HashMap::new();
        entries.insert(
            "ollama".to_owned(),
            vec![crate::feat::provider_infra::ModelInfo {
                id: "llama3".to_owned(),
                context_length: Some(8192),
                input_modalities: {
                    let mut m = crate::feat::provider_infra::InputModalities::text();
                    m.insert(crate::feat::provider_infra::Modality::Image);
                    m
                },
            }],
        );
        crate::feat::provider_infra::ModelCache {
            entries,
            last_updated_at: None,
        }
    }

    fn cache_text_only() -> crate::feat::provider_infra::ModelCache {
        let mut entries = std::collections::HashMap::new();
        entries.insert(
            "ollama".to_owned(),
            vec![crate::feat::provider_infra::ModelInfo {
                id: "llama3".to_owned(),
                context_length: Some(8192),
                input_modalities: crate::feat::provider_infra::InputModalities::text(),
            }],
        );
        crate::feat::provider_infra::ModelCache {
            entries,
            last_updated_at: None,
        }
    }

    #[rstest::rstest]
    #[test]
    fn gate_allows_model_confirmed_image_capable_via_cache() {
        // Given an overlaid cache entry with image modalities for the active model.
        let models_dev = ModelsDevData::new();
        let cache = cache_with_image_modalities();
        let entry = entry_with_image();

        // When gating with the cache populated.
        let result = attachment_gate(Some("ollama/llama3"), &entry, &models_dev, Some(&cache));

        // Then the attachment is allowed through.
        assert!(result.is_none());
    }

    #[rstest::rstest]
    #[test]
    fn gate_blocks_model_marked_text_only_in_cache() {
        // Given an overlaid cache entry that is explicitly text-only.
        let models_dev = ModelsDevData::new();
        let cache = cache_text_only();
        let entry = entry_with_image();

        // When gating with the cache populated.
        let blocked = attachment_gate(Some("ollama/llama3"), &entry, &models_dev, Some(&cache))
            .expect("blocked");

        // Then an Error entry is produced.
        assert!(matches!(blocked.kind, ChatEntryKind::Error(_)));
    }

    #[rstest::rstest]
    #[test]
    fn gate_falls_back_to_models_dev_when_model_not_in_cache() {
        // Given an empty cache and a models.dev vision model.
        let models_dev = ModelsDevData::new();
        let cache = cache_text_only();
        let entry = entry_with_image();

        // When gating a model that is not in the cache.
        let result = attachment_gate(Some("ollama/other"), &entry, &models_dev, Some(&cache));

        // Then the models.dev fallback applies (unknown → blocked).
        assert!(result.is_some());
    }

    #[rstest::rstest]
    #[test]
    fn gate_blocks_unknown_model_with_no_cache_entry() {
        // Given no cache and no models.dev data.
        let models_dev = ModelsDevData::new();
        let entry = entry_with_image();

        // When gating an unknown model.
        let blocked = attachment_gate(Some("llama.cpp/my-model.gguf"), &entry, &models_dev, None)
            .expect("blocked");

        // Then the attachment is blocked.
        assert!(matches!(blocked.kind, ChatEntryKind::Error(_)));
    }

    #[rstest::rstest]
    #[test]
    fn gate_resolves_path_shaped_model_id_from_cache() {
        // Given a cache entry whose model id is an absolute file path.
        let models_dev = ModelsDevData::new();
        let mut entries = std::collections::HashMap::new();
        entries.insert(
            "llama.cpp".to_owned(),
            vec![crate::feat::provider_infra::ModelInfo {
                id: "/models/Qwen3-35B.gguf".to_owned(),
                context_length: Some(32768),
                input_modalities: {
                    let mut m = crate::feat::provider_infra::InputModalities::text();
                    m.insert(crate::feat::provider_infra::Modality::Image);
                    m
                },
            }],
        );
        let cache = crate::feat::provider_infra::ModelCache {
            entries,
            last_updated_at: None,
        };
        let entry = entry_with_image();

        // When gating with the full `provider/path/model.gguf` display form.
        let result = attachment_gate(
            Some("llama.cpp//models/Qwen3-35B.gguf"),
            &entry,
            &models_dev,
            Some(&cache),
        );

        // Then the path-shaped id resolves through the first-slash split.
        assert!(result.is_none());
    }

    #[rstest::rstest]
    #[test]
    fn allows_text_only_entry_regardless_of_model() {
        // Given a text-only entry and a non-vision model.
        let models_dev = ModelsDevData::new();
        let entry = ChatEntry::user("hello");

        // When gating.
        // Then None is returned (no block).
        assert!(attachment_gate(Some("gpt-3.5-turbo"), &entry, &models_dev, None).is_none());
    }

    #[rstest::rstest]
    #[test]
    fn blocks_attachment_on_known_text_only_model() {
        // Given an image entry and a model known to lack image support.
        let mut models_dev = ModelsDevData::new();
        models_dev
            .image_support
            .insert("gpt-3.5-turbo".to_owned(), false);
        let entry = entry_with_image();

        // When gating.
        let blocked = attachment_gate(Some("gpt-3.5-turbo"), &entry, &models_dev, None);

        // Then an error entry is returned.
        assert!(blocked.is_some(), "expected a block error entry");
    }

    #[rstest::rstest]
    #[test]
    fn allows_attachment_on_vision_model() {
        // Given an image entry and a vision-capable model.
        let mut models_dev = ModelsDevData::new();
        models_dev.image_support.insert("gpt-4o".to_owned(), true);
        let entry = entry_with_image();

        // When gating.
        // Then None is returned (allowed).
        assert!(attachment_gate(Some("gpt-4o"), &entry, &models_dev, None).is_none());
    }

    #[rstest::rstest]
    #[test]
    fn blocks_attachment_on_unknown_model() {
        // Given an image entry and a model not in the reference data.
        let models_dev = ModelsDevData::new();
        let entry = entry_with_image();

        // When gating.
        let blocked = attachment_gate(Some("my-custom-llama"), &entry, &models_dev, None);

        // Then an error entry is returned (unknown models block, not allowed).
        assert!(
            blocked.is_some(),
            "unknown models must be blocked unless positively confirmed vision-capable"
        );
    }

    #[rstest::rstest]
    #[test]
    fn blocked_error_entry_names_model_and_points_at_providers_toml() {
        // Given an unknown model id.
        let models_dev = ModelsDevData::new();
        let entry = entry_with_image();

        // When gating.
        let blocked =
            attachment_gate(Some("my-custom-llama"), &entry, &models_dev, None).expect("blocked");

        // Then the Error entry text names the model id AND points at providers.toml model_info.
        let text = match &blocked.kind {
            ChatEntryKind::Error(t) => t.clone(),
            _ => panic!("expected an Error entry"),
        };
        assert!(
            text.contains("my-custom-llama"),
            "must name the model id: {text}"
        );
        assert!(
            text.contains("providers.toml"),
            "must point the user at providers.toml model_info: {text}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn allows_attachment_when_no_provider_configured() {
        // Given an image entry and no configured model.
        let models_dev = ModelsDevData::new();
        let entry = entry_with_image();

        // When gating.
        // Then None is returned (no provider → no-op dispatch).
        assert!(attachment_gate(None, &entry, &models_dev, None).is_none());
    }
}
