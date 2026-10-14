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
//! - [`skills`] — per-dir skill scanning and the system→global→project
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
    pub prompts: Result<PromptTemplateStore, String>,
    /// The bounded-walk context files with contents read.
    pub context: Vec<ContextFile>,
}

/// Runs the context-files scan: resolve candidates in bounded-walk
/// order (least-local → cwd), then read each file. Files that vanish
/// between resolution and read are skipped silently.
#[must_use]
pub fn read_context_files(cwd: &std::path::Path, home: &std::path::Path) -> Vec<ContextFile> {
    project_context_files(cwd, home)
        .into_iter()
        .filter_map(|path| read_one_context_file(&path))
        .collect()
}

/// Reads a single context file, returning `None` if it can no longer
/// be read.
fn read_one_context_file(path: &std::path::Path) -> Option<ContextFile> {
    let content = std::fs::read_to_string(path).ok()?;
    Some(ContextFile {
        path: path.to_path_buf(),
        content,
    })
}

/// Loads the prompt-template store from the user/system/project dirs.
///
/// The store's error is returned verbatim so the worker's event
/// carries a description identical to the published summary.
pub fn load_prompts(
    user_dir: &std::path::Path,
    system_dir: &std::path::Path,
    project_dirs: &[std::path::PathBuf],
) -> Result<PromptTemplateStore, String> {
    PromptTemplateStore::load_from_dirs_ordered(user_dir, system_dir, project_dirs)
        .map_err(|e| format!("{e:?}"))
}
