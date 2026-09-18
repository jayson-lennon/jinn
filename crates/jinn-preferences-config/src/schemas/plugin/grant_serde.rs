//! Serde adapter for `Vec<PluginPathGrant>` as plain path strings.
//!
//! TOML shape: `grants = ["<config_dir>/themes", "<data_dir>/notes:w"]` —
//! the same syntax as the `--grant` CLI flag. The `:w` suffix marks a
//! grant writable; its absence means read-only.

use super::PluginPathGrant;
use serde::Deserialize;

/// Serializes grants back to path strings, re-adding the `:w` suffix for
/// writable grants.
///
/// # Errors
///
/// Propagates any serializer error (grant strings are always serializable).
pub fn serialize<S>(grants: &[PluginPathGrant], serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.collect_seq(grants.iter().map(|g| {
        if g.writable {
            format!("{}:w", g.path)
        } else {
            g.path.clone()
        }
    }))
}

/// Parses each grant string, splitting the trailing `:w` if present.
///
/// # Errors
///
/// Returns the deserializer's error if the value is not a sequence of strings.
pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<PluginPathGrant>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Vec::<String>::deserialize(deserializer)?;
    raw.into_iter()
        .map(|s| match s.strip_suffix(":w") {
            Some(path) => Ok(PluginPathGrant {
                path: path.to_owned(),
                writable: true,
            }),
            None => Ok(PluginPathGrant {
                path: s,
                writable: false,
            }),
        })
        .collect()
}
