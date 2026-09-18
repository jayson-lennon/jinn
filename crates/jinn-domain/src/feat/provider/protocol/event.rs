//! Provider events.

use serde::{Deserialize, Serialize};

use crate::feat::context::protocol::prompt_template::PromptTemplate;
use crate::protocol::SessionId;

/// The active provider was switched.
///
/// Emitted after a successful [`ProviderSwitch`](super::ProviderSwitch) command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderSwitched {
    /// The session that switched provider.
    pub session_id: SessionId,
    /// The display name of the new provider.
    pub provider_name: String,
}

impl crate::common::bus::BusMessage for ProviderSwitched {}

/// Models refresh completed with results and errors.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelsRefreshed {
    /// The session that triggered the refresh (for routing the result back).
    pub session_id: SessionId,
    /// Provider name to list of discovered model metadata.
    pub results: std::collections::HashMap<String, Vec<jinn_provider::ModelInfo>>,
    /// Provider name to error message for providers that failed.
    pub errors: std::collections::HashMap<String, String>,
}

impl crate::common::bus::BusMessage for ModelsRefreshed {}

/// Model cache loaded from disk at startup.
///
/// Emitted by `ProviderInitActor` after loading the cache from disk.
/// `ProviderActor` handles this by writing the cache into AppState and
/// reloading picker entries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCacheLoaded {
    /// The loaded model cache.
    pub cache: crate::feat::provider_infra::ModelCache,
}

impl crate::common::bus::BusMessage for ModelCacheLoaded {}

/// Prompt templates loaded after a rescan.
///
/// Emitted by the prompt scan actor after scanning the prompts directory.
/// On success, `templates` contains the loaded templates and `error` is `None`.
/// On failure, `templates` is empty and `error` contains a description.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptTemplatesLoaded {
    /// The session whose cwd drove the scan.
    pub session_id: crate::SessionId,
    /// The loaded prompt templates.
    pub templates: Vec<PromptTemplate>,
    /// Error message if scanning failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl crate::common::bus::BusMessage for PromptTemplatesLoaded {}

jinn_slices::crossing_schema!(PromptTemplatesLoaded, "PromptTemplatesLoaded",
trouper::schema::SchemaKind::Event,
description: "Prompt templates loaded after a rescan.",
fields: [
    "session_id" => trouper::schema::FieldTy::Uuid,
    "templates" => trouper::schema::FieldTy::List(Box::new(trouper::schema::FieldTy::Json)),
]);
