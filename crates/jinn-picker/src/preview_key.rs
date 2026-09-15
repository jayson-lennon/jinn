//! Preview cache identity — the opt-in key for cached preview renders.

/// Identity key for a cached preview render.
///
/// Specs that opt into preview caching supply this per entry via
/// [`crate::hooks::PickerPreviewKeyFn`]; the key is combined with the pane
/// width by the cache contract (`jinn-selection-widget`'s `PreviewCache`).
/// A dedicated newtype keeps preview identity from flowing into unrelated
/// `String` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PreviewKey(pub String);

impl PreviewKey {
    /// The inner cache key string (combined with pane width by the cache).
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PreviewKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
