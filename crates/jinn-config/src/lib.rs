//! The configuration layer — `jinn.toml` as one in-memory, reloadable,
//! read+write document.
//!
//! Every `jinn.toml` value in the program is read through this crate and
//! written through it, so a subsystem's configuration is owned by that
//! subsystem instead of by a kernel-side aggregate struct. A section
//! declares itself by implementing [`Configurable`]; the layer stays
//! generic over the type and names no section.

pub mod config_layer;
pub mod configurable;
pub mod testutil;

pub use config_layer::{
    ConfigDocumentStorage, ConfigError, ConfigLayer, FilesystemConfigStorage,
    InMemoryConfigStorage, SectionCheck,
};
pub use configurable::{ConfigList, ConfigSectionError, Configurable, EntryKey};

/// A process-lifetime configuration layer with nothing in it.
///
/// Route actions reach config through [`ActionCtx::config`], which the
/// intent handler fills. A caller assembling an `ActionCtx` by hand — a
/// test, or a slice's own unit test — has no handler to borrow from, and
/// a spec that only reads config does not need a real document. Every
/// section reads as its default through this.
///
/// # Panics
///
/// Panics if the shared empty layer cannot be constructed. That can only
/// fail if an empty document stops parsing, which is a build-time
/// invariant of the layer rather than anything a caller can cause.
#[must_use]
#[expect(
    clippy::expect_used,
    reason = "an empty document always parses; failure is a broken invariant, not a caller error"
)]
pub fn empty_config_layer() -> &'static ConfigLayer {
    static EMPTY: std::sync::OnceLock<ConfigLayer> = std::sync::OnceLock::new();
    EMPTY.get_or_init(|| {
        ConfigLayer::load(std::sync::Arc::new(InMemoryConfigStorage::default()))
            .expect("an empty document always loads")
    })
}
