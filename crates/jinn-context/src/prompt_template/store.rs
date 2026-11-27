//! Prompt template store - holds all loaded templates and provides lookup.
//!
//! The store is populated at startup by scanning `~/.config/jinn/prompts/`
//! recursively. It supports exact name lookup and fuzzy search for the
//! autocomplete popup.

use std::sync::Arc;

use crate::PromptTemplate;
use error_stack::{Report, ResultExt as _};
use fuzzy_matcher::FuzzyMatcher as _;
use fuzzy_matcher::skim::SkimMatcherV2;
use tracing::warn;

use super::loader::parse_template_file;
use marked_path::{Absolute, MarkedPathAccess, MarkedPathBuf};

/// Errors that can occur during prompt template loading.
#[derive(Debug, wherror::Error)]
#[error(debug)]
pub enum PromptTemplateStoreError {
    /// Filesystem I/O failure.
    Io,
}

/// Maximum number of results returned by fuzzy search.
pub const MAX_FUZZY_RESULTS: usize = 20;

/// Holds all loaded prompt templates and provides lookup methods.
///
/// Uses `Arc<[PromptTemplate]>` internally so cloning is cheap - the store
/// lives in `AppState` behind an `RwLock`.
#[derive(Debug, Clone, Default)]
pub struct PromptTemplateStore {
    /// All loaded templates.
    templates: Arc<[PromptTemplate]>,
}

impl PromptTemplateStore {
    /// Creates an empty store with no templates.
    #[must_use]
    pub fn new() -> Self {
        Self {
            templates: Arc::new([]),
        }
    }

    /// Loads all `*.md` files from the given directory (recursively) and
    /// returns a populated store.
    ///
    /// If the directory does not exist, returns an empty store (no error).
    /// Files that fail to parse are logged as warnings and skipped.
    /// Duplicate names are logged as warnings; the first occurrence wins.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be read due to I/O failure.
    pub fn load_from_dir(
        path: &MarkedPathBuf<Absolute>,
    ) -> Result<Self, Report<PromptTemplateStoreError>> {
        if !path.exists() {
            return Ok(Self::new());
        }

        let mut templates = Vec::new();
        let mut seen_names = std::collections::HashSet::new();

        Self::scan_dir(path, &mut templates, &mut seen_names)?;

        Ok(Self {
            templates: templates.into(),
        })
    }

    /// Loads templates from user, and ordered project directories, merging
    /// results with most-local-wins precedence.
    ///
    /// Layering (lowest to highest priority):
    /// 1. `user_dir`
    /// 2. `project_dirs` — least-local ancestor first, cwd last. Each later
    ///    project dir overrides a template of the same name from any earlier layer.
    ///
    /// # Errors
    ///
    /// Returns an error if any directory cannot be read due to I/O failure.
    pub fn load_from_dirs_ordered(
        user_dir: &MarkedPathBuf<Absolute>,
        project_dirs: &[MarkedPathBuf<Absolute>],
    ) -> Result<Self, Report<PromptTemplateStoreError>> {
        let mut templates = Vec::new();
        let mut seen_names = std::collections::HashSet::new();

        // User templates are the base layer.
        if user_dir.exists() {
            Self::scan_dir_override(user_dir, &mut templates, &mut seen_names)?;
        }

        // Project templates (least-local first, cwd last = highest priority).
        // Each project dir is scanned with override semantics so a closer
        // ancestor replaces a template of the same name from a further layer.
        for dir in project_dirs {
            if dir.exists() {
                Self::scan_dir_override(dir, &mut templates, &mut seen_names)?;
            }
        }

        Ok(Self {
            templates: templates.into(),
        })
    }

    /// Creates a store from a pre-built list of templates (for testing).
    #[must_use]
    pub fn from_vec(templates: Vec<PromptTemplate>) -> Self {
        Self {
            templates: templates.into(),
        }
    }

    /// Returns the template with the given name, if it exists.
    #[must_use]
    pub fn find_by_name(&self, name: &str) -> Option<&PromptTemplate> {
        self.templates.iter().find(|t| t.name == name)
    }

    /// Returns fuzzy-matched templates ordered by relevance score.
    ///
    /// Results are ordered least-relevant (top) to most-relevant (bottom),
    /// capped at \[`MAX_FUZZY_RESULTS`].
    #[must_use]
    pub fn fuzzy_search(&self, query: &str) -> Vec<&PromptTemplate> {
        let matcher = SkimMatcherV2::default();

        let mut scored: Vec<(i64, &PromptTemplate)> = self
            .templates
            .iter()
            .filter_map(|t| matcher.fuzzy_match(&t.name, query).map(|score| (score, t)))
            .collect();

        // Sort by score ascending (least relevant first, most relevant last).
        scored.sort_by_key(|(score, _)| *score);
        scored.truncate(MAX_FUZZY_RESULTS);

        scored.into_iter().map(|(_, t)| t).collect()
    }

    /// Returns the number of loaded templates.
    #[must_use]
    pub fn len(&self) -> usize {
        self.templates.len()
    }

    /// Returns `true` if there are no loaded templates.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.templates.is_empty()
    }

    /// Returns a reference to the full template list.
    #[must_use]
    pub fn templates(&self) -> &[PromptTemplate] {
        &self.templates
    }

    /// Recursively scans a directory for `*.md` files and parses them.
    fn scan_dir(
        dir: &MarkedPathBuf<Absolute>,
        templates: &mut Vec<PromptTemplate>,
        seen_names: &mut std::collections::HashSet<String>,
    ) -> Result<(), Report<PromptTemplateStoreError>> {
        let entries = std::fs::read_dir(dir)
            .change_context(PromptTemplateStoreError::Io)
            .attach(format!("failed to read directory {dir}"))?;

        for entry in entries {
            let entry: std::fs::DirEntry = entry
                .change_context(PromptTemplateStoreError::Io)
                .attach(format!("failed to read dir entry in {dir}"))?;

            let path = MarkedPathBuf::<Absolute>::new(entry.path())
                .change_context(PromptTemplateStoreError::Io)
                .attach(format!("dir entry in {dir} is not a valid absolute path"))?;

            if path.is_dir() {
                Self::scan_dir(&path, templates, seen_names)?;
                continue;
            }

            if path.extension().is_none_or(|ext| ext != "md") {
                continue;
            }

            // Skip system/internal files (underscore-prefixed).
            if path
                .file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(|stem| stem.starts_with('_'))
            {
                continue;
            }

            match parse_template_file(&path) {
                Ok(template) => {
                    if seen_names.contains(&template.name) {
                        warn!(
                            name = %template.name,
                            path = %path,
                            "duplicate prompt template name, skipping"
                        );
                    } else {
                        seen_names.insert(template.name.clone());
                        templates.push(template);
                    }
                }
                Err(e) => {
                    warn!(
                        path = %path,
                        error = %e,
                        "failed to parse prompt template, skipping"
                    );
                }
            }
        }

        Ok(())
    }

    /// Recursively scans a directory, overriding existing templates with the same name.
    ///
    /// Used for layered loading - a later layer's templates replace earlier
    /// layers' templates of the same name.
    fn scan_dir_override(
        dir: &MarkedPathBuf<Absolute>,
        templates: &mut Vec<PromptTemplate>,
        seen_names: &mut std::collections::HashSet<String>,
    ) -> Result<(), Report<PromptTemplateStoreError>> {
        let entries = std::fs::read_dir(dir)
            .change_context(PromptTemplateStoreError::Io)
            .attach(format!("failed to read directory {dir}"))?;

        for entry in entries {
            let entry: std::fs::DirEntry = entry
                .change_context(PromptTemplateStoreError::Io)
                .attach(format!("failed to read dir entry in {dir}"))?;

            let path = MarkedPathBuf::<Absolute>::new(entry.path())
                .change_context(PromptTemplateStoreError::Io)
                .attach(format!("dir entry in {dir} is not a valid absolute path"))?;

            if path.is_dir() {
                Self::scan_dir_override(&path, templates, seen_names)?;
                continue;
            }

            if path.extension().is_none_or(|ext| ext != "md") {
                tracing::debug!(path = %path, "prompt template skipped: not .md");
                continue;
            }

            // Skip system/internal files (underscore-prefixed).
            if path
                .file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(|stem| stem.starts_with('_'))
            {
                tracing::debug!(path = %path, "prompt template skipped: underscore-prefixed");
                continue;
            }

            match parse_template_file(&path) {
                Ok(template) => {
                    if seen_names.contains(&template.name) {
                        // Replace the earlier layer's template with this one.
                        if let Some(pos) = templates.iter().position(|t| t.name == template.name)
                            && let Some(slot) = templates.get_mut(pos)
                        {
                            *slot = template;
                        }
                        continue;
                    }
                    seen_names.insert(template.name.clone());
                    templates.push(template);
                }
                Err(e) => {
                    warn!(
                        path = %path,
                        error = %e,
                        "failed to parse prompt template, skipping"
                    );
                }
            }
        }

        Ok(())
    }
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

    /// Writes a minimal .md template file to `dir`.
    fn write_template(
        dir: &MarkedPathBuf<Absolute>,
        filename: &str,
        name: &str,
        description: &str,
    ) {
        let content = format!(
            "+++\nname = \"{name}\"\ndescription = \"{description}\"\n+++\nBody for {name}"
        );
        std::fs::write(
            dir.join(filename).expect("typed dir joins a bare name"),
            content,
        )
        .expect("write template");
    }

    #[rstest::rstest]
    #[test]
    fn load_finds_every_parseable_template_in_the_user_dir() {
        // Given a user dir holding two well-formed templates — one with a
        // hyphenated name and a `#` inside its quoted description, the
        // shape the `/web2` e2e harness stages.
        let user_dir = jinn_testutil::tempdir();
        write_template(&user_dir.path, "plan.md", "plan", "Discuss a plan.");
        write_template(
            &user_dir.path,
            "e2e-scratch-prompt.md",
            "e2e-scratch-prompt",
            "scratch prompt for the webui2 # popup",
        );

        // When loading with no project dirs.
        let store = PromptTemplateStore::load_from_dirs_ordered(&user_dir.path, &[])
            .expect("load user dir");

        // Then both templates are in the store.
        assert_eq!(store.len(), 2);
        let names: Vec<&str> = store.templates().iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"e2e-scratch-prompt"), "names: {names:?}");
    }

    #[rstest::rstest]
    #[test]
    fn scan_dir_override_replaces_existing_template() {
        // Given a system directory with a template "greeting".
        let sys_dir = jinn_testutil::tempdir();
        write_template(&sys_dir.path, "greeting.md", "greeting", "System version");

        let mut templates = Vec::new();
        let mut seen_names = std::collections::HashSet::new();
        PromptTemplateStore::scan_dir(&sys_dir.path, &mut templates, &mut seen_names)
            .expect("scan system");
        assert_eq!(templates.len(), 1);
        assert_eq!(templates[0].description, "System version");

        // When the user directory has an override for "greeting".
        let user_dir = jinn_testutil::tempdir();
        write_template(&user_dir.path, "greeting.md", "greeting", "User override");
        PromptTemplateStore::scan_dir_override(&user_dir.path, &mut templates, &mut seen_names)
            .expect("scan override");

        // Then the user version replaces the system version.
        assert_eq!(templates.len(), 1, "should still have 1 template");
        assert_eq!(
            templates[0].description, "User override",
            "user template should override system template"
        );
    }

    #[rstest::rstest]
    #[test]
    fn scan_dir_only_loads_md_files() {
        // Given a directory with .md and .txt files.
        let dir = jinn_testutil::tempdir();
        write_template(&dir.path, "valid.md", "valid", "Valid template");
        std::fs::write(
            dir.path
                .join("notes.txt")
                .expect("temp root joins a bare name"),
            "not a template",
        )
        .expect("write txt");
        std::fs::write(
            dir.path
                .join("data.json")
                .expect("temp root joins a bare name"),
            "{}",
        )
        .expect("write json");

        // When scanning the directory.
        let mut templates = Vec::new();
        let mut seen_names = std::collections::HashSet::new();
        PromptTemplateStore::scan_dir(&dir.path, &mut templates, &mut seen_names).expect("scan");

        // Then only the .md file is loaded.
        assert_eq!(templates.len(), 1);
        assert_eq!(templates[0].name, "valid");
    }

    #[rstest::rstest]
    #[test]
    fn scan_dir_skips_underscore_prefixed_files() {
        // Given a directory with a normal and an underscore-prefixed template.
        let dir = jinn_testutil::tempdir();
        write_template(&dir.path, "visible.md", "visible", "Should load");
        write_template(&dir.path, "_hidden.md", "_hidden", "Should skip");

        // When scanning the directory.
        let mut templates = Vec::new();
        let mut seen_names = std::collections::HashSet::new();
        PromptTemplateStore::scan_dir(&dir.path, &mut templates, &mut seen_names).expect("scan");

        // Then only the non-underscore file is loaded.
        assert_eq!(templates.len(), 1);
        assert_eq!(templates[0].name, "visible");
    }

    #[rstest::rstest]
    #[test]
    fn scan_dir_skips_the_static_section_file() {
        // Given a scanned prompts dir that also carries the system resource
        // `_static.md` — markdown, underscore-prefixed, never a template.
        let dir = jinn_testutil::tempdir();
        write_template(&dir.path, "visible.md", "visible", "Should load");
        std::fs::write(
            dir.path
                .join("_static.md")
                .expect("temp root joins a bare name"),
            "jinn carries its own Luau runtime.",
        )
        .expect("write static resource");

        // When scanning the directory.
        let mut templates = Vec::new();
        let mut seen_names = std::collections::HashSet::new();
        PromptTemplateStore::scan_dir(&dir.path, &mut templates, &mut seen_names).expect("scan");

        // Then the static resource never appears as a template — it must
        // not show up in slash-command autocomplete.
        assert_eq!(templates.len(), 1);
        assert_eq!(templates[0].name, "visible");
    }
}
