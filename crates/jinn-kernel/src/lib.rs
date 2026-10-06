//! The kernel - shared cross-slice vocabulary, actor infrastructure, and the
//! glue that binds slices together.
//!
//! Feature-specific logic lives in the slice crates under `crates/slices/`.
//! What remains here is the vocabulary more than one slice needs, plus the
//! infrastructure every slice is wired through:
//!
//! - **Protocol types** (`protocol/`) - cross-cutting value types shared across
//!   feature boundaries: `Intent`, `Key`, `Mode`, and system events. Slice-owned
//!   commands and events live in their canonical `*-msg` crates; the actor bus
//!   routes by `TypeId` via the marker trait `BusMessage` (in `common/bus.rs`)
//!   rather than a central enum.
//! - **Residual features** (`feat/`) - the feature modules not yet absorbed into
//!   a slice family. Each colocates its actors, intents, UI elements, and state
//!   implementation; the extraction model moves them out as slices take over.
//! - **Common** (`common/`) - shared infrastructure (bus, services, app paths,
//!   TOML patching), most of which is re-exported from the `jinn-common` crate.
//!
//! The shared application state and the session-creation operations that
//! mutate it are **not** defined here: they live in `jinn-app-state`, a crate
//! beneath the kernel, so a slice can reach the state without depending on the
//! kernel. [`AppState`] and [`session_lifecycle`] below re-export those
//! definitions, which keeps every existing `jinn_kernel::…` path resolving
//! while leaving the dependency pointing one way.
//!
//! Foundational types are re-exported at the crate root for convenience.

/// Installs the process-wide rustls crypto provider (ring) in this crate's
/// test binary. reqwest is built with `rustls-no-provider` (see the workspace
/// `Cargo.toml`), so without a default provider every `reqwest::Client` panics
/// with "No provider set" at construction. Test binaries never run `main()`.
/// `install_default` errors on the second call; the result is deliberately
/// ignored.
#[cfg(test)]
#[ctor::ctor]
fn install_rustls_provider_for_tests() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

pub mod common;
pub mod feat;
pub mod session_lifecycle;
pub mod state;

// Kernel-side protocol vocabulary: intents, keys, and system events.
pub mod protocol;

// Re-export actor types that are still in use
// Re-export component types (state, UI)
pub use common::app_paths::{AppPaths, BrowserProfileMode};
// The shared state is defined in `jinn-app-state`; these keep the long-standing
// `jinn_kernel::AppState` paths resolving without duplicating the type.
pub use common::bridge::{Bridge, BridgeClosure};
pub use common::bus::BusMessage;
pub use common::render_ctx::RenderCtx;
pub use common::state::{State, StateReadGuard, StateWriteGuard};
pub use common::{AppUiRegistry, register_all_ui_elements};
pub use jinn_app_state::app_state::pin_sort_key;
pub use jinn_app_state::app_state::{AppState, SessionState};
pub use jinn_app_state::frontend_state::{FrontendState, PendingSessionCreation};
pub use jinn_context::PromptTemplateStore;
pub use jinn_core_types::NO_PROVIDER_ID;
pub use jinn_session_state::ChatSessionState;
pub use jinn_slices::{FocusScope, ScopeStack, TuiSignals};

// Re-export services types
pub use common::services::Services;
pub use common::services::bus_service::BusService;
pub use common::services::test_services::TestServices;

// Re-export core types
pub use common::core::{AppCore, SHUTDOWN_TIMEOUT, STARTUP_TIMEOUT, wait_for_system_ready};

// Re-export intent types
pub use feat::intent::IntentHandler;

// Re-export providers types
pub use jinn_provider_config::{
    ApiKeys, ApiKeysService, ConfigStorageService, EndpointDefault, FakeLlmServiceFactory,
    FilesystemConfigStorage, InMemoryConfigStorage, InitProvidersOutcome, LlmServiceFactoryService,
    ModelCache, NoProvidersAvailableFactory, ProviderEntry, ProviderId, ProviderRegistry,
    ProviderRegistryService, ProvidersConfig, ScriptedResponse, TOOL_LOOP_TRIGGER, cache_path,
    config_path, init_default_providers_to,
};
// Re-export context types

// Re-export session types
// The SQLite implementation lives in the jinn-session-store slice crate —
// import it from there (`jinn_session_store::sqlite::SqliteSessionStore`).

pub use jinn_session_msg::PhaseKind;

// Re-export reasoning types
// The reasoning-effort vocabulary is owned by the provider-selection slice's
// msg crate (kernel→msg direction); re-exported here so the long-standing
// `jinn_kernel::ReasoningEffort` paths keep resolving.
pub use jinn_provider_selection_msg::ReasoningEffort;
pub use jinn_provider_selection_msg::resolve_effort;

// Re-export services submodules

// Re-export protocol types at crate root
pub use jinn_provider_selection_msg::ProviderPickerEntry;
pub use protocol::entries_to_messages;
pub use protocol::{
    ChatEntry, ChatEntryId, ChatEntryKind, IntentResult, KernelIntent, Key, KeyEvent, Mode,
    Modifiers, PickerKind, PinPosition,
};

// Re-export domain types from their canonical locations

pub use jinn_session_history_msg::PushChatEntry;
pub use jinn_session_history_msg::{PinChatEntry, UnpinChatEntry};
// The curation contracts are owned by the context-curation slice's msg
// crate (kernel→msg direction, same as the stream contracts); re-exported
// here so the long-standing `jinn_kernel::TriggerCompaction` path keeps
// resolving.
pub use jinn_context_curation_msg::TriggerCompaction;
// Stream contracts are owned by the inference slice's msg crate (kernel→msg
// direction, jinn-session-msg precedent); re-exported here so the long-standing
// `jinn_kernel::X` paths keep resolving.
// Provider-selection contracts are owned by the provider-selection slice's
// msg crate (kernel→msg direction); re-exported here so the long-standing
// `jinn_kernel::X` paths keep resolving.
pub use jinn_provider_selection_msg::{
    LoadProviderPickerEntries, ModelCacheLoaded, ModelsRefreshed, ProviderSwitch, ProviderSwitched,
    RefreshModels,
};
// The prompt-scan contracts are owned by the session-init slice's msg crate
// (kernel→msg direction, skills precedent); re-exported here so the
// long-standing `jinn_kernel::X` paths keep resolving.
pub use jinn_inference_msg::{
    CancelTurn, SendToLlmProvider, StreamCompleted, StreamCompletedReason, StreamOrigin,
    StreamToken,
};
pub use jinn_session_init_msg::{PromptTemplate, PromptTemplatesLoaded, RescanPromptTemplates};
