//! Dynamic slice identity — scope ids and data-carried intents.
//!
//! Slices that are wired in composition (dashboard, quake bar, future
//! guest plugins) must not require central enum edits: the vocabulary
//! here carries identity as data. A slice mints a [`SliceScopeId`] for
//! its focus scope and addresses its actions through
//! [`DynamicIntent`]; the handler dispatches dynamic intents *only*
//! through the route table, so a slice that never registered rows is
//! inert by construction.

/// Identifies one slice's focus scope.
///
/// Two components: the slice (e.g. `quake-bar`) and the scope name
/// within that slice (e.g. `open`). Ordered and hashable so keymaps and
/// manifests can key on it. The canonical constructors are the slice
/// features' own consts — there is no registry of ids, and spelling a
/// new one is exactly the act of creating a slice.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SliceScopeId {
    /// The slice that owns this scope, e.g. `quake-bar`.
    slice: String,
    /// The scope's name within the slice, e.g. `open`.
    name: String,
    /// Whether the scope captures text input (lights up input-focused UI
    /// and serves the editing intents through its input hook). Navigation
    /// surfaces (browser lists, tabs) set this false.
    captures_input: bool,
}

impl SliceScopeId {
    /// Mints a slice scope id from its two components. The scope is
    /// input-capturing by default; see [`SliceScopeId::navigation`] for
    /// navigation-only surfaces.
    #[must_use]
    pub fn new(slice: &str, name: &str) -> Self {
        Self {
            slice: slice.to_owned(),
            name: name.to_owned(),
            captures_input: true,
        }
    }

    /// Mints a navigation-only scope id: keys drive a cursor/selection,
    /// no text input is captured, and the scope reports `Mode::Normal`
    /// so input-focused UI stays dark.
    #[must_use]
    pub fn navigation(slice: &str, name: &str) -> Self {
        Self {
            slice: slice.to_owned(),
            name: name.to_owned(),
            captures_input: false,
        }
    }

    /// Whether this scope captures text input.
    #[must_use]
    pub fn captures_input(&self) -> bool {
        self.captures_input
    }

    /// The owning slice's identifier.
    #[must_use]
    pub fn slice(&self) -> &str {
        &self.slice
    }

    /// The scope's name within the slice.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The `slice:name` display form, also the `FromStr` roundtrip form.
    #[must_use]
    pub fn key(&self) -> String {
        format!("{}:{}", self.slice, self.name)
    }
}

impl std::fmt::Display for SliceScopeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.key())
    }
}

impl std::str::FromStr for SliceScopeId {
    type Err = ();

    /// Parses the `slice:name` form produced by
    /// [`Display`](std::fmt::Display). Total: unparseable input is an
    /// `Err`, never a panic.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (slice, name) = s.split_once(':').ok_or(())?;
        if slice.is_empty() || name.is_empty() {
            return Err(());
        }
        Ok(Self {
            slice: slice.to_owned(),
            name: name.to_owned(),
            captures_input: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::SliceScopeId;

    #[rstest::rstest]
    #[test]
    fn display_and_from_str_roundtrip() {
        // Given a slice scope id.
        let id = SliceScopeId::new("quake-bar", "open");

        // When converting to a string and back.
        let parsed = SliceScopeId::from_str(&id.to_string());

        // Then the roundtrip preserves both components.
        assert_eq!(parsed.expect("valid form"), id);
    }

    #[rstest::rstest]
    #[test]
    fn from_str_rejects_malformed_keys() {
        // Given strings missing the separator or a component.
        // When parsing.
        // Then each fails without panicking.
        assert!(SliceScopeId::from_str("no-separator").is_err());
        assert!(SliceScopeId::from_str(":name").is_err());
        assert!(SliceScopeId::from_str("slice:").is_err());
    }

    #[rstest::rstest]
    #[test]
    fn ids_order_by_slice_then_name() {
        // Given two ids differing only in name.
        let a = SliceScopeId::new("quake-bar", "open");
        let b = SliceScopeId::new("quake-bar", "scroll");

        // Then ordering is by name within the same slice.
        assert!(a < b);
    }
}
