//! The `[project]` umbrella — the curated project list.
//!
//! A list of tables at the top level of its umbrella, so the element type
//! declares itself with `ConfigList`: `[[project.entry]]` stays a list
//! and the layer matches entries by their identity field.
//!
//! An entry used to carry a `command_policy` of its own. Those rules are
//! stream rules now, scoped by their `project` field, so the field is gone
//! and a project's rules live with every other rule in one list.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

impl jinn_config::ConfigList for ProjectConfig {
    const KEY: &'static str = "project.entry";
    const ENTRY_KEY: &'static str = "path";
    const ENTRY_FIELDS: &'static [&'static str] = &["path"];
}

/// A curated project directory shown in the project picker.
///
/// Defined in `jinn.toml` under `[[project.entry]]`. The `path` field
/// is the array key the patcher matches entries by, so add/remove
/// operations target a single table without disturbing siblings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectConfig {
    /// The absolute (or `~`-prefixed) directory path.
    pub path: PathBuf,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]

    use std::sync::Arc;

    use jinn_config::{ConfigLayer, InMemoryConfigStorage};

    use super::ProjectConfig;

    #[rstest::rstest]
    fn project_entries_read_from_the_umbrella_key() {
        // Given a document listing two projects under the umbrella.
        let doc = r#"
            [[project.entry]]
            path = "/tmp/a"

            [[project.entry]]
            path = "/tmp/b"
        "#
        .parse()
        .expect("test TOML parses");
        let layer = ConfigLayer::load(Arc::new(InMemoryConfigStorage::new(doc))).expect("load");

        // When reading the list.
        let projects = layer.get_list::<ProjectConfig>().expect("list reads");

        // Then both entries are read, in document order.
        assert_eq!(projects.len(), 2);
        assert_eq!(projects[0].path.to_string_lossy(), "/tmp/a");
        assert_eq!(projects[1].path.to_string_lossy(), "/tmp/b");
    }

    #[rstest::rstest]
    fn an_absent_project_list_reads_empty() {
        // Given a document with no project umbrella at all.
        let doc = "[tools]\ndefault_timeout_secs = 60"
            .parse()
            .expect("test TOML parses");
        let layer = ConfigLayer::load(Arc::new(InMemoryConfigStorage::new(doc))).expect("load");

        // When reading the list.
        let projects = layer.get_list::<ProjectConfig>().expect("list reads");

        // Then it is empty rather than an error.
        assert!(projects.is_empty());
    }

    #[rstest::rstest]
    fn a_project_renders_as_a_path_and_reads_back() {
        // Given a project with no rules of its own.
        let project = ProjectConfig {
            path: "/tmp/demo".parse().expect("path parses"),
        };
        let storage = Arc::new(InMemoryConfigStorage::new(
            "[tools]\nx = 1\n".parse().expect("parses"),
        ));
        let layer = ConfigLayer::load(storage.clone()).expect("load");

        // When saving it, then saving the identical value again.
        layer
            .put_list::<ProjectConfig>(std::slice::from_ref(&project))
            .expect("first save");
        let once = storage.text();
        layer
            .put_list::<ProjectConfig>(std::slice::from_ref(&project))
            .expect("second save");
        let twice = storage.text();

        // Then an identical re-save does not move the file.
        assert_eq!(
            once, twice,
            "document moved:\nonce:\n{once}\ntwice:\n{twice}"
        );

        // And it reads back on the right project.
        let read = layer.get_list::<ProjectConfig>().expect("read");
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].path, project.path);
    }
}
