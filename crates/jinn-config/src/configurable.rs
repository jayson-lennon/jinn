//! The `Configurable` trait — the one registration a `jinn.toml` section
//! needs to be readable and writable by the configuration layer.
//!
//! Every section declares its own dotted key and, when it owns a
//! list-of-tables, the field identifying an entry. The layer stores raw
//! TOML and is generic over the section type, so it never names a
//! section: adding a subsystem's config means implementing this trait in
//! that subsystem, and touching nothing else.

use serde::{Serialize, de::DeserializeOwned};

/// A section of `jinn.toml` a subsystem owns.
///
/// Implementing this is the only registration a section needs. `KEY` and
/// `ENTRY_KEY` are compile-time facts the layer reads; nothing outside
/// this impl names the section.
///
/// # At most one list-of-tables
///
/// `ENTRY_KEY` is singular because every section in the tree owns at
/// most one list-of-tables. A section that grew a second one at a
/// different depth would silently lose that list's per-entry comments —
/// the patcher would rewrite it positionally and reattach a user's
/// comment onto a different entry on the next save. Adding a
/// `Vec` field whose entries carry per-entry comments is what would
/// break this first; extend `ENTRY_KEY` to a list of (path, field)
/// pairs at that point.
pub trait Configurable:
    Serialize + DeserializeOwned + Default + Clone + Send + Sync + 'static
{
    /// The dotted `jinn.toml` path this slice owns, e.g.
    /// `"context_curation.auto_prune"`. The layer walks it opaquely.
    ///
    /// Single-key sections absorb their own name into the umbrella
    /// (`[term]`, not `[term.interactive_term]`), so the umbrella never
    /// stutters.
    const KEY: &'static str;

    /// The field identifying an entry, for this section's
    /// list-of-tables.
    ///
    /// `None` for a list of scalars (no per-entry fields to preserve), a
    /// map of tables (the map key is already the entry's identity), and
    /// a plain sub-table.
    ///
    /// Without it, the patcher rewrites such a list positionally and
    /// silently reattaches a user's per-entry comment onto a different
    /// entry on the next save.
    ///
    /// The path is relative to this section's own key, because a
    /// section's list is a field inside the section table: the list at
    /// `lifecycle` in a section keyed `session_lifecycle` is declared
    /// `EntryKey::new("lifecycle", "name")`, and a list nested under a
    /// sub-table uses the dotted path through it.
    const ENTRY_KEY: Option<EntryKey> = None;

    /// Builds the value from its table, layering the section's present
    /// keys over `Self::default()`.
    ///
    /// Present keys overwrite; absent keys keep their `Default` value.
    /// This is why a partial section behaves the way a reader expects:
    /// writing only one key of a section does not reset the others to
    /// zero-values. Without the merge, a user who wrote just
    /// `max_results = 5` would silently lose `engine`'s default.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigSectionError::Malformed`] when the merged table
    /// does not deserialize into `Self`.
    fn from_table(table: &toml::Table) -> Result<Self, ConfigSectionError> {
        let base = toml::Value::try_from(Self::default())
            .map_err(|err| ConfigSectionError::malformed(Self::KEY, err.to_string()))?;
        let merged = merge_toml(base, toml::Value::Table(table.clone()));
        let value: Self = toml::Value::try_into(merged).map_err(|err: toml::de::Error| {
            ConfigSectionError::malformed(Self::KEY, err.to_string())
        })?;
        Ok(value)
    }
}

/// The entry-identity field for a section's one list-of-tables, plus the
/// path to that list relative to the section's own key.
///
/// A section's list is often not at the section key itself:
/// `auto_prune`'s rules live at `auto_prune.regex.rules`, so a bare field
/// name could not tell the patcher where the list is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryKey {
    path: &'static str,
    field: &'static str,
}

impl EntryKey {
    /// Identifies a list at `path` below the section's own key.
    ///
    /// `path` is the list's own key within the section table — the field
    /// name for a list directly in the section (`"lifecycle"`), or a
    /// dotted path through sub-tables for a nested one
    /// (`"regex.rules"`).
    #[must_use]
    pub const fn new(path: &'static str, field: &'static str) -> Self {
        Self { path, field }
    }

    /// The entry-identity field's name.
    #[must_use]
    pub const fn field(&self) -> &'static str {
        self.field
    }

    /// The list's full document-root-relative path: the section's key
    /// followed by the relative path.
    #[must_use]
    pub fn full_path(&self, section_key: &'static str) -> Vec<&'static str> {
        let mut path: Vec<&'static str> = section_key.split('.').collect();
        path.extend(self.path.split('.').filter(|segment| !segment.is_empty()));
        path
    }
}

/// A section of `jinn.toml` that is a bare array of tables at its key,
/// e.g. `[[project.entry]]`.
///
/// This is the companion to [`Configurable`] for the one shape a plain
/// table cannot express. A wrapper struct holding `Vec<T>` would read
/// and write correctly, but it would move the list *inside* a table:
/// `[[project]]` with a nested `projects = [...]` array instead of
/// `[[project.entry]]`, which is not the shape a user expects and
/// loses the per-entry comment the entry key exists to preserve.
///
/// # At most one list-of-tables
///
/// This is again singular for the same reason [`Configurable::ENTRY_KEY`]
/// is: a section that owned two lists would have the second rewritten
/// positionally, silently reattaching a user's per-entry comment onto a
/// different entry on the next save.
pub trait ConfigList: Serialize + DeserializeOwned + Send + Sync + 'static {
    /// The dotted `jinn.toml` path this list occupies.
    const KEY: &'static str;

    /// The dotted path an older file may still carry this list at, if the
    /// list was renamed.
    ///
    /// Reads of the list also consult this key and merge what they find
    /// after the entries read from [`Self::KEY`], so a config written
    /// before the rename keeps working. `None` (the default) means the
    /// list was never renamed. Nothing writes the legacy key: a save
    /// touches only [`Self::KEY`], so the next time the user edits an
    /// entry the file converges on its own.
    const LEGACY_KEY: Option<&'static str> = None;

    /// The field identifying an entry. Always present here: a
    /// list-of-tables with no identity field is the exact case the entry
    /// key exists for.
    ///
    /// An element type need not be `Default`: a list section's absent
    /// state is the empty list, so there is no "default entry" for the
    /// bound to supply.
    const ENTRY_KEY: &'static str;

    /// Every field an entry may carry, whether or not a given save
    /// serializes it.
    ///
    /// A save *replaces* each entry it matches, so a field absent from the
    /// serialized value is one the user cleared and must disappear from the
    /// file. The field list is therefore the schema, not the serialized
    /// keys: `skip_serializing_if` hides exactly the fields this exists to
    /// catch, so deriving it from a save would make replacement a no-op.
    ///
    /// Keys outside this list are the user's, and a save leaves them alone.
    /// TOML attaches a bare trailing key to the table above it, so an entry
    /// can legitimately hold a key no field of this struct describes.
    const ENTRY_FIELDS: &'static [&'static str];
}

/// Layers `overlay` onto `base`: tables merge key-wise, every other value
/// replaces wholesale.
///
/// The asymmetry is deliberate. A table is a namespace, so a nested
/// table a user partially wrote should not clobber the defaults of the
/// keys they omitted. An array or scalar is a single value — replacing it
/// is what "the user set this" means, and merging element-wise would
/// invent entries nobody asked for.
fn merge_toml(base: toml::Value, overlay: toml::Value) -> toml::Value {
    match (base, overlay) {
        (toml::Value::Table(mut base), toml::Value::Table(overlay)) => {
            for (key, value) in overlay {
                let merged = match base.get(&key) {
                    Some(existing) => merge_toml(existing.clone(), value),
                    None => value,
                };
                base.insert(key, merged);
            }
            toml::Value::Table(base)
        }
        (_, overlay) => overlay,
    }
}

/// Why a config section could not be read.
#[derive(Debug, Clone, PartialEq, Eq, wherror::Error)]
pub enum ConfigSectionError {
    /// A segment of the section's key exists but is not a table.
    #[error("config section [{key}] has non-table segment `{segment}`")]
    NotATable {
        /// The section's full dotted key.
        key: &'static str,
        /// The segment along the path that was not a table.
        segment: String,
    },
    /// The section's table does not deserialize into the section's type.
    #[error("config section [{key}] is malformed: {detail}")]
    Malformed {
        /// The section's full dotted key.
        key: &'static str,
        /// The deserializer's complaint.
        detail: String,
    },
    /// The section's values are internally inconsistent with another
    /// section's: a registered cross-section check rejected the
    /// configuration as a whole.
    #[error("config section [{key}]: {detail}")]
    Invalid {
        /// The section whose key the complaint is filed under.
        key: &'static str,
        /// What is inconsistent, phrased for the user.
        detail: String,
    },
}

impl From<ConfigSectionError> for std::io::Error {
    fn from(error: ConfigSectionError) -> Self {
        Self::new(std::io::ErrorKind::InvalidData, error.to_string())
    }
}

impl ConfigSectionError {
    /// Builds a malformed-section error naming the section.
    #[must_use]
    pub fn malformed(key: &'static str, detail: String) -> Self {
        Self::Malformed { key, detail }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, reason = "test code")]

    use serde::Deserialize;

    use super::{ConfigSectionError, Configurable, merge_toml};

    #[derive(Debug, Default, Clone, PartialEq, Deserialize, serde::Serialize)]
    struct SectionCfg {
        #[serde(default)]
        enabled: bool,
        #[serde(default)]
        retries: u32,
    }

    impl Configurable for SectionCfg {
        const KEY: &'static str = "test.section";
    }

    fn table(body: &str) -> toml::Table {
        toml::from_str(body).expect("test TOML parses")
    }

    #[rstest::rstest]
    #[test]
    fn from_table_reads_present_keys() {
        // Given a section table carrying both of the section's keys.
        let table = table("enabled = true\nretries = 3");

        // When building the section value from it.
        let value = SectionCfg::from_table(&table).expect("section parses");

        // Then both keys come from the table.
        assert_eq!(
            value,
            SectionCfg {
                enabled: true,
                retries: 3
            }
        );
    }

    #[rstest::rstest]
    #[test]
    fn from_table_keeps_defaults_for_absent_keys() {
        // Given a section table carrying only one of the section's keys.
        let table = table("retries = 3");

        // When building the section value from it.
        let value = SectionCfg::from_table(&table).expect("section parses");

        // Then the omitted key keeps its Default value.
        assert_eq!(value.retries, 3);
        assert!(!value.enabled, "the absent key kept its default");
    }

    #[rstest::rstest]
    #[test]
    fn from_table_reports_malformed_naming_the_section() {
        // Given a section table whose key has the wrong type.
        let table = table("retries = \"many\"");

        // When building the section value from it.
        let result = SectionCfg::from_table(&table);

        // Then the failure names the section.
        assert!(matches!(
            result,
            Err(ConfigSectionError::Malformed { key, .. }) if key == "test.section"
        ));
    }

    #[rstest::rstest]
    #[test]
    fn merge_toml_merges_tables_keywise() {
        // Given a base table with a nested table the overlay also names.
        let base: toml::Value = toml::toml! {
            outer = { kept = 1, replaced = 2 }
        }
        .into();
        let overlay: toml::Value = toml::toml! {
            outer = { replaced = 99, added = 3 }
        }
        .into();

        // When merging.
        let merged = merge_toml(base, overlay);

        // Then the un-named key survives and the named ones take the overlay.
        let outer = merged.get("outer").and_then(toml::Value::as_table);
        let outer = outer.expect("outer is still a table");
        assert_eq!(outer.get("kept").and_then(toml::Value::as_integer), Some(1));
        assert_eq!(
            outer.get("replaced").and_then(toml::Value::as_integer),
            Some(99)
        );
        assert_eq!(
            outer.get("added").and_then(toml::Value::as_integer),
            Some(3)
        );
    }

    #[rstest::rstest]
    #[test]
    fn merge_toml_replaces_arrays_wholesale() {
        // Given a base array and an overlay array of different length.
        let base: toml::Value = toml::toml! { items = [1, 2, 3] }.into();
        let overlay: toml::Value = toml::toml! { items = [4] }.into();

        // When merging.
        let merged = merge_toml(base, overlay);

        // Then the overlay replaces the array rather than merging into it.
        let items = merged.get("items").and_then(toml::Value::as_array);
        let items = items.expect("items is an array");
        assert_eq!(items.len(), 1, "the array was replaced, not merged");
    }

    #[rstest::rstest]
    #[test]
    fn merge_toml_overlay_scalar_replaces_base_table() {
        // Given a base table the overlay names with a scalar.
        let base: toml::Value = toml::toml! { key = { nested = 1 } }.into();
        let overlay: toml::Value = toml::toml! { key = 7 }.into();

        // When merging.
        let merged = merge_toml(base, overlay);

        // Then the scalar wins — shapes do not get blended.
        assert_eq!(merged.get("key").and_then(toml::Value::as_integer), Some(7));
    }
}
