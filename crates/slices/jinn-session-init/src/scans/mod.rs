//! The session-init slice's scans — the pure filesystem logic the
//! discovery worker runs on blocking threads.
//!
//! The scan kernels live in their behavior crates. Portable prompt/context
//! values come from `jinn-context`; portable skill values come from
//! `jinn-skills-msg`.
//!
//! - [`vcs`] — marker-based VCS-root detection.
//! - [`walk`] — the bounded cwd→ancestor walk (exclusive `$HOME`,
//!   inclusive VCS root) plus per-resource dir/file resolution.
//! - [`skills`] — per-dir skill scanning and the global→project
//!   merged result (project overrides, most-local wins).
//! - [`workflows`] — the launch-global workflows dir scan, delegating to
//!   the workflows slice's exported kernel.

pub mod skills;
pub mod vcs;
pub mod walk;
pub mod workflows;

pub use skills::scan_skills_merged;
pub use walk::{project_context_files, project_prompts_dirs, project_skills_dirs};
pub use workflows::scan_workflows;

use jinn_context::PromptTemplateStore;
use jinn_context::env_context::ContextFile;
use marked_path::{Absolute, MarkedPathBuf};

/// Relative location of project skills under a project root, as path
/// segments joined natively by [`walk::project_skills_dirs`].
///
/// The segments are joined one component at a time (not as a single
/// `/`-separated string) so a Windows root never acquires a
/// mixed-separator tail such as `C:\proj\.agents/skills`.
pub const SKILLS_SUBDIR: &[&str] = &[".agents", "skills"];

/// Relative location of project prompts under a project root, as path
/// segments joined natively by [`walk::project_prompts_dirs`].
pub const PROMPTS_SUBDIR: &[&str] = &[".agents", "prompts"];

/// Candidate project context filenames, checked in order.
pub const CONTEXT_FILE_CANDIDATES: &[&str] = &["AGENTS.md", "AGENTS.MD", "CLAUDE.md", "CLAUDE.MD"];

/// The three resource-scan results a discovery run collects, in settle
/// order (skills, prompts, context files).
///
/// Each resource's payload is already the event's wire data; errors are
/// carried in the resource's `Loaded` event, not here.
pub struct ScanOutputs {
    /// The merged discovered skills.
    pub skills: Vec<jinn_skills_msg::Skill>,
    /// The merged prompt-template store.
    pub prompts:
        Result<PromptTemplateStore, error_stack::Report<jinn_context::PromptTemplateStoreError>>,
    /// The bounded-walk context files with contents read.
    pub context: Vec<ContextFile>,
}

/// Runs the context-files scan: resolve candidates in bounded-walk
/// order (least-local → cwd), then read each file. Files that vanish
/// between resolution and read are skipped silently.
#[must_use]
pub fn read_context_files(
    cwd: &MarkedPathBuf<Absolute>,
    home: &MarkedPathBuf<Absolute>,
) -> Vec<ContextFile> {
    project_context_files(cwd, home)
        .into_iter()
        .filter_map(|path| read_one_context_file(&path))
        .collect()
}

/// Reads a single context file, returning `None` if it can no longer
/// be read.
fn read_one_context_file(path: &MarkedPathBuf<Absolute>) -> Option<ContextFile> {
    let content = std::fs::read_to_string(path).ok()?;
    Some(ContextFile {
        path: path.clone(),
        content,
    })
}

/// Loads the prompt-template store from the user/project dirs.
///
/// # Errors
///
/// Returns the store's own error, so the worker's event carries a
/// description identical to the published summary.
pub fn load_prompts(
    user_dir: &MarkedPathBuf<Absolute>,
    project_dirs: &[MarkedPathBuf<Absolute>],
) -> Result<PromptTemplateStore, error_stack::Report<jinn_context::PromptTemplateStoreError>> {
    PromptTemplateStore::load_from_dirs_ordered(user_dir, project_dirs)
}

/// Loads the static system-prompt section from the user prompts dir.
///
/// The user-dir system resource `_static.md`; missing or blank means no
/// section. The file is a single small read, riding the prompts scan so an
/// edit reaches the next turn through the same `ResourcesChanged` re-scan
/// that refreshes the template store.
#[must_use]
pub fn load_static_section(user_dir: &MarkedPathBuf<Absolute>) -> Option<String> {
    let content = jinn_common::system_resource::try_load_system_resource(
        "_static.md",
        &user_dir.as_marked_path(),
    )?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

#[cfg(test)]
mod static_section_tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unwrap_used,
        reason = "test code"
    )]

    use super::*;

    #[rstest::rstest]
    fn load_static_section_returns_trimmed_file_content() {
        // Given a user prompts dir with a padded `_static.md`.
        let root = jinn_testutil::tempdir();
        let prompts = root
            .path
            .join("prompts")
            .expect("temp root joins a bare name");
        std::fs::create_dir_all(&prompts).expect("create prompts dir");
        std::fs::write(
            prompts
                .join("_static.md")
                .expect("typed parent joins a bare name"),
            "\n  static body  \n",
        )
        .expect("write");

        // When loading the static section.
        let section = load_static_section(&prompts);

        // Then the content is returned trimmed.
        assert_eq!(section, Some("static body".to_owned()));
    }

    #[rstest::rstest]
    fn load_static_section_is_none_when_file_missing() {
        // Given a user prompts dir with no `_static.md`.
        let root = jinn_testutil::tempdir();
        let prompts = root
            .path
            .join("prompts")
            .expect("temp root joins a bare name");
        std::fs::create_dir_all(&prompts).expect("create prompts dir");

        // When loading the static section.
        let section = load_static_section(&prompts);

        // Then there is no section.
        assert_eq!(section, None);
    }

    #[rstest::rstest]
    fn load_static_section_is_none_when_file_blank() {
        // Given a user prompts dir whose `_static.md` holds only whitespace.
        let root = jinn_testutil::tempdir();
        let prompts = root
            .path
            .join("prompts")
            .expect("temp root joins a bare name");
        std::fs::create_dir_all(&prompts).expect("create prompts dir");
        std::fs::write(
            prompts
                .join("_static.md")
                .expect("typed parent joins a bare name"),
            "  \n\t\n",
        )
        .expect("write");

        // When loading the static section.
        let section = load_static_section(&prompts);

        // Then there is no section — blank is absent, never an empty render.
        assert_eq!(section, None);
    }
}
