//! Provider capsule: cap + view + extension traits, colocated.
//!
//! Write access to [`ProviderState`] is gated by an unforgeable ZST token
//! ([`ProviderCap`]). The projection method [`State::with_provider`] hands the
//! cap-holder a narrow borrowed view ([`ProviderView`]) plus read-only access to
//! the other sub-structs the provider actor legitimately reads.

use crate::ProviderPickerEntry;
use crate::SessionState;
#[cfg(test)]
use crate::common::app_state::AppState;
use crate::common::state::State;
use crate::feat::endpoint::picker_entry::EndpointEntry;
use crate::feat::provider::ProviderState;
use crate::feat::provider_infra::ModelCache;
use crate::feat::ui::frontend_state::FrontendState;
use crate::feat::ui::picker_states::PickerExt;

// ── The cap ──────────────────────────────────────────────────────────────────

/// Proof of authority to write `ProviderState` (and the provider-owned pickers
/// on `FrontendState`). Minted only via [`crate::common::tcaps::mint`].
#[derive(Clone, Copy)]
pub struct ProviderCap(());

impl ProviderCap {
    /// Private constructor scoped to the `tcaps/` subtree.
    ///
    /// MUST be `pub(in crate::common::tcaps)`, NOT `pub(crate)`. `pub(crate)`
    /// lets any module forge the cap.
    pub(in crate::common::tcaps) fn new() -> Self {
        Self(())
    }
}

// ── Per-struct narrow newtypes ───────────────────────────────────────────────

/// Narrow write-handle to `ProviderState`. The tuple field is PRIVATE — reaching
/// `.0` from an actor is a compile error. Only opted-in trait methods are
/// reachable.
pub struct ProviderOps<'a>(&'a mut ProviderState);

/// Narrow write-handle to the provider-owned pickers on `FrontendState`
/// (reasoning effort picker). The provider actor loads these, so it owns them.
pub struct FrontendProviderOps<'a>(&'a mut FrontendState);

// ── Composite facade ─────────────────────────────────────────────────────────

/// What a provider-writer sees: mutable access to `provider` plus the two
/// `frontend` pickers it owns, plus read-only access to `session`.
///
/// `frontend` is deliberately absent as a separate shared borrow: the provider
/// actor both reads `frontend.theme` and writes the provider-owned pickers on
/// the *same* `FrontendState`. Holding both `&FrontendState` and
/// `&mut FrontendState` would alias. Instead, [`FrontendProviderOps::theme`]
/// exposes the read.
pub struct ProviderView<'a> {
    /// Read-only session access (snapshot-safe): needed to resolve the active
    /// session's model selection when loading provider picker entries.
    pub session: &'a SessionState,
    /// Mutable provider-owned frontend pickers + theme read.
    pub provider_frontend: FrontendProviderOps<'a>,
    /// Mutable provider state.
    pub provider: ProviderOps<'a>,
}

impl FrontendProviderOps<'_> {
    /// Read-only access to the frontend theme (used when constructing picker
    /// entries).
    pub fn theme(&self) -> &crate::feat::theme::Theme {
        &self.0.theme
    }
}

// ── Extension traits (the opt-in method menu) ───────────────────────────────

/// Write access to the provider picker + alloy mode.
pub trait ProviderPickerWrite {
    fn set_provider_picker_items(
        &mut self,
        items: Vec<jinn_picker::PickerEntry<ProviderPickerEntry>>,
    );
    fn is_alloy_mode(&self) -> bool;
}

/// Write access to the model cache.
pub trait ModelCacheWrite {
    fn set_model_cache(&mut self, cache: Option<ModelCache>);
    fn model_cache(&self) -> Option<&ModelCache>;
}

/// Write access to the provider-owned frontend pickers.
pub trait FrontendProviderPickerWrite {
    fn set_endpoint_picker_items(&mut self, items: Vec<EndpointEntry>);
    fn set_endpoint_loading(&mut self, loading: bool);
    fn set_endpoint_fetched_at(&mut self, at: Option<jiff::Timestamp>);
}

impl ProviderPickerWrite for ProviderOps<'_> {
    fn set_provider_picker_items(
        &mut self,
        items: Vec<jinn_picker::PickerEntry<ProviderPickerEntry>>,
    ) {
        self.0.provider_picker.set_items(items);
    }
    fn is_alloy_mode(&self) -> bool {
        self.0.is_alloy_mode()
    }
}

impl ModelCacheWrite for ProviderOps<'_> {
    fn set_model_cache(&mut self, cache: Option<ModelCache>) {
        self.0.model_cache = cache;
    }
    fn model_cache(&self) -> Option<&ModelCache> {
        self.0.model_cache.as_ref()
    }
}

impl FrontendProviderPickerWrite for FrontendProviderOps<'_> {
    fn set_endpoint_picker_items(&mut self, items: Vec<EndpointEntry>) {
        let wrapped = crate::feat::picker::registry::build_picker_registry()
            .make_items(crate::feat::picker::registry::ENDPOINT_ID, items)
            .unwrap_or_default();
        self.0.endpoint_picker_mut().set_items(wrapped);
    }
    fn set_endpoint_loading(&mut self, loading: bool) {
        self.0.pickers.endpoint_loading = loading;
    }
    fn set_endpoint_fetched_at(&mut self, at: Option<jiff::Timestamp>) {
        self.0.pickers.endpoint_fetched_at = at;
    }
}

// ── Projection method ────────────────────────────────────────────────────────

impl State {
    /// Write access to the provider capsule, scoped via [`ProviderView`].
    ///
    /// The cap is taken by reference to prove authority; it is not consumed.
    /// Reads of other sub-structs are safe under the snapshot model.
    pub fn with_provider<R, F>(&self, _cap: &ProviderCap, f: F) -> R
    where
        F: FnOnce(&mut ProviderView<'_>) -> R,
    {
        let mut guard = self.write_lock();
        let app = &mut *guard;
        f(&mut ProviderView {
            session: &app.session,
            provider_frontend: FrontendProviderOps(&mut app.frontend),
            provider: ProviderOps(&mut app.provider),
        })
    }
}

// ── Test helper ─────────────────────────────────────────────────────────────

#[cfg(test)]
impl<'a> ProviderView<'a> {
    /// Open a [`ProviderView`] from raw [`AppState`] without a cap.
    ///
    /// Tests are trusted — they set up state and call the production loaders.
    /// The cap exists to enforce ownership at *actor* call sites, not test sites.
    pub fn from_app_state_for_test(app: &'a mut AppState) -> Self {
        ProviderView {
            session: &app.session,
            provider_frontend: FrontendProviderOps(&mut app.frontend),
            provider: ProviderOps(&mut app.provider),
        }
    }
}
