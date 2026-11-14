//! Environment context sections - per-section builders for the system prompt.
//!
//! Each builder renders exactly one environment section of the assembled
//! system prompt: the static `_static.md` section, the active persona,
//! project context files (AGENTS.md/CLAUDE.md), current date, and working
//! directory. Sections that have no content return an empty string so the
//! assembler can omit them.

// The context-file loaders join a fixed candidate filename onto a directory
// already typed `Absolute`; a bare-name join cannot make the result
// relative, so those joins cannot fail at runtime.
#![allow(
    clippy::expect_used,
    reason = "bare-name joins onto typed-absolute dirs are infallible by construction"
)]

use std::fmt::Write;

use jinn_slices::Persona;
use marked_path::{Absolute, MarkedPath, MarkedPathAccess, MarkedPathBuf};

/// Candidates for project context files, checked in order.
const CONTEXT_FILE_CANDIDATES: &[&str] = &["AGENTS.md", "AGENTS.MD", "CLAUDE.md", "CLAUDE.MD"];

/// Renders the static system-prompt section: the user's `_static.md`
/// content under a `# jinn` heading, verbatim.
///
/// This is where harness facts reach every prompt without a skill load.
/// Empty string when there is no content (`None`) or it is blank, so the
/// assembler omits the section entirely.
#[must_use]
pub fn static_section(content: Option<&str>) -> String {
    let Some(content) = content.map(str::trim).filter(|c| !c.is_empty()) else {
        return String::new();
    };
    format!("# jinn\n\n{content}")
}

/// Renders the persona section: the active persona body (identity + guidelines).
///
/// Empty string when no persona is active or the persona body is empty.
#[must_use]
pub fn persona_section(persona: Option<&Persona>) -> String {
    persona
        .filter(|p| !p.body.is_empty())
        .map_or(String::new(), |p| p.body.clone())
}

/// Renders the handoff-resume section — how to continue working from a
/// compacted context and what a handoff recap must contain.
///
/// Empty string when handoff compaction is disabled, so the section is
/// omitted entirely from the assembled prompt (the reference
/// implementation ships the
/// variable "empty without compaction").
#[must_use]
pub fn handoff_resume_section(enabled: bool) -> String {
    const SECTION: &str = "# Handoff Compaction\n\n\
        This session uses handoff compaction: when the working context grows\n\
        past its limit, you will be asked to stop and write a handoff recap\n\
        (objective, established facts, applied changes, verification, dead\n\
        ends, remaining plan). The conversation then folds down to the\n\
        user's messages, pinned context, and your most recent recap — older\n\
        tool output and prose is summarized into that recap, not kept.\n\n\
        When resuming after a fold, treat the most recent recap as your\n\
        working state: continue from its remaining plan, trust its\n\
        established facts, and never re-derive what it records. Re-read a\n\
        file with a tool before editing it if the recap does not state its\n\
        current contents.";

    if !enabled {
        return String::new();
    }
    SECTION.to_owned()
}

/// Renders the project context files section (AGENTS.md/CLAUDE.md contents).
///
/// Empty string when there are no context files.
#[must_use]
pub fn context_files_section(context_files: &[ContextFile]) -> String {
    if context_files.is_empty() {
        return String::new();
    }
    let mut section = String::from("# Project Context\n\n");
    section.push_str("Project-specific instructions and guidelines:\n\n");
    for file in context_files {
        let _ = write!(section, "## {}\n\n{}\n\n", file.path, file.content);
    }
    section
}

/// Renders the current date section.
#[must_use]
pub fn date_section() -> String {
    format!("Current date: {}", format_current_date())
}

/// Renders the working directory section.
#[must_use]
pub fn cwd_section(cwd: &MarkedPathBuf<Absolute>) -> String {
    format!("Current working directory: {cwd}")
}

/// A loaded project context file (e.g., AGENTS.md).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ContextFile {
    /// The file path (for display in the prompt).
    pub path: MarkedPathBuf<Absolute>,
    /// The file content.
    pub content: String,
}

/// Loads project context files from the CWD and all ancestor directories.
///
/// Searches for AGENTS.md, AGENTS.MD, CLAUDE.md, CLAUDE.MD in each directory
/// from CWD up to root. Returns files ordered from root → CWD (so closer
/// files come last, matching pi-mono's ordering).
pub async fn load_project_context_files(cwd: &MarkedPathBuf<Absolute>) -> Vec<ContextFile> {
    let mut files = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let mut current = Some(cwd.clone());
    while let Some(dir) = current {
        let view = dir.as_marked_path();
        if let Some(file) = load_context_file_from_dir(&view).await {
            let canonical = file.path.clone();
            if seen.insert(canonical) {
                files.push(file);
            }
        }

        // Stop at root.
        let parent = view.parent();
        match parent {
            Some(p) if p.as_path() != view.as_path() => {
                current = Some(p.to_owned());
            }
            _ => break,
        }
    }

    // Reverse so root files come first, CWD files come last.
    files.reverse();
    files
}

/// Loads a context file from a single directory (first match wins).
async fn load_context_file_from_dir(dir: &MarkedPath<'_, Absolute>) -> Option<ContextFile> {
    for filename in CONTEXT_FILE_CANDIDATES {
        let path = dir
            .join(filename)
            .expect("absolute dir joins a bare filename");
        if !path.as_path().exists() {
            continue;
        }
        match tokio::fs::read_to_string(&path).await {
            Ok(content) => return Some(ContextFile { path, content }),
            // Best-effort discovery, but a file that exists and cannot be
            // read is worth naming rather than silently skipping.
            Err(error) => tracing::warn!(
                path = %path,
                %error,
                "context file exists but could not be read"
            ),
        }
    }
    None
}

/// Synchronous version of [`load_project_context_files`].
///
/// Used when context rescan must happen outside an async runtime
/// (e.g., from the TUI suspend handler).
pub fn load_project_context_files_sync(cwd: &MarkedPathBuf<Absolute>) -> Vec<ContextFile> {
    let mut files = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let mut current = Some(cwd.clone());
    while let Some(dir) = current {
        let view = dir.as_marked_path();
        if let Some(file) = load_context_file_from_dir_sync(&view) {
            let canonical = file.path.clone();
            if seen.insert(canonical) {
                files.push(file);
            }
        }

        // Stop at root.
        let parent = view.parent();
        match parent {
            Some(p) if p.as_path() != view.as_path() => {
                current = Some(p.to_owned());
            }
            _ => break,
        }
    }

    // Reverse so root files come first, CWD files come last.
    files.reverse();
    files
}

/// Synchronous single-directory context file loader.
fn load_context_file_from_dir_sync(dir: &MarkedPath<'_, Absolute>) -> Option<ContextFile> {
    for filename in CONTEXT_FILE_CANDIDATES {
        let path = dir
            .join(filename)
            .expect("absolute dir joins a bare filename");
        if !path.as_path().exists() {
            continue;
        }
        match std::fs::read_to_string(&path) {
            Ok(content) => return Some(ContextFile { path, content }),
            Err(error) => tracing::warn!(
                path = %path,
                %error,
                "context file exists but could not be read"
            ),
        }
    }
    None
}

/// Returns the current date as YYYY-MM-DD.
fn format_current_date() -> String {
    let now = std::time::SystemTime::now();
    let duration = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs() as i64;
    // Simple date calculation: days since epoch.
    let days = secs / 86400;
    // Gregorian calendar date from epoch (1970-01-01).
    date_from_days(days)
}

/// Converts days since Unix epoch to YYYY-MM-DD string.
fn date_from_days(total_days: i64) -> String {
    // Algorithm from Howard Hinnant.
    let z = total_days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
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

    fn test_persona(body: &str) -> Persona {
        Persona {
            name: "test".to_owned(),
            description: "Test persona".to_owned(),
            body: body.to_owned(),
            handoff_prompt: String::new(),
        }
    }

    #[rstest::rstest]
    fn static_section_renders_content_under_jinn_heading() {
        // Given static section content from `_static.md`.
        let content = "jinn carries its own Luau runtime (`jinn luau`).";

        // When building the static section.
        let result = static_section(Some(content));

        // Then the content is rendered verbatim under the `# jinn` heading.
        assert_eq!(result, format!("# jinn\n\n{content}"));
    }

    #[rstest::rstest]
    fn static_section_is_empty_without_content() {
        // Given no static section content.
        // When building the static section.
        let result = static_section(None);

        // Then no section is rendered.
        assert_eq!(result, "");
    }

    #[rstest::rstest]
    fn static_section_is_empty_when_content_blank() {
        // Given blank static section content.
        // When building the static section.
        let result = static_section(Some("  \n\t "));

        // Then no section is rendered — blank is absent, never a bare heading.
        assert_eq!(result, "");
    }

    #[rstest::rstest]
    fn static_section_trims_surrounding_whitespace_only() {
        // Given static content with surrounding padding but real body text.
        // When building the static section.
        let result = static_section(Some("\n  body line  \n"));

        // Then only the outer padding is trimmed; the body stays verbatim.
        assert_eq!(result, "# jinn\n\nbody line");
    }

    #[rstest::rstest]
    fn persona_section_includes_persona_body() {
        // Given a persona with body text.
        let persona = test_persona("You are a helpful assistant.");

        // When building the persona section.
        let result = persona_section(Some(&persona));

        // Then the persona body is included.
        assert_eq!(result, "You are a helpful assistant.");
    }

    #[rstest::rstest]
    fn persona_section_is_empty_without_persona() {
        // Given no persona.
        // When building the persona section.
        let result = persona_section(None);

        // Then the section is empty.
        assert_eq!(result, "");
    }

    #[rstest::rstest]
    fn persona_section_is_empty_for_blank_body() {
        // Given a persona with an empty body.
        let persona = test_persona("");

        // When building the persona section.
        let result = persona_section(Some(&persona));

        // Then the section is empty.
        assert_eq!(result, "");
    }

    #[rstest::rstest]
    fn persona_section_contains_no_compaction_block() {
        // Given a parsed persona whose handoff_prompt holds block text.
        let mut persona = test_persona("You are a helpful assistant.");
        persona.handoff_prompt = "Recap the critique headings.".to_owned();

        // When building the persona section.
        let result = persona_section(Some(&persona));

        // Then the section is exactly the body — no tag, no block text.
        assert_eq!(result, "You are a helpful assistant.");
        assert!(!result.contains("<compaction>"));
        assert!(!result.contains("Recap the critique headings."));
    }

    #[rstest::rstest]
    fn handoff_resume_section_is_empty_when_disabled() {
        // Given handoff compaction disabled.
        // When building the resume section.
        let result = handoff_resume_section(false);

        // Then the section is empty.
        assert_eq!(result, "");
    }

    #[rstest::rstest]
    fn handoff_resume_section_teaches_resume_when_enabled() {
        // Given handoff compaction enabled.
        // When building the resume section.
        let result = handoff_resume_section(true);

        // Then the section names the recap structure and the resume rule.
        assert!(result.contains("# Handoff Compaction"));
        assert!(result.contains("recap"));
        assert!(result.contains("remaining plan"));
    }

    #[rstest::rstest]
    fn context_files_section_includes_project_context_files() {
        // Given a context file under a temp root.
        let dir = jinn_testutil::tempdir();
        let files = vec![ContextFile {
            path: dir
                .path
                .join("AGENTS.md")
                .expect("temp root joins a bare name"),
            content: "# Style Guide\nUse Rust.".to_owned(),
        }];

        // When building the context files section.
        let result = context_files_section(&files);

        // Then project context section is included.
        assert!(result.contains("# Project Context"));
        assert!(result.contains("AGENTS.md"));
        assert!(result.contains("Use Rust."));
    }

    #[rstest::rstest]
    fn context_files_section_is_empty_without_files() {
        // Given no context files.
        // When building the context files section.
        let result = context_files_section(&[]);

        // Then the section is empty.
        assert_eq!(result, "");
    }

    #[rstest::rstest]
    fn date_section_includes_date() {
        // Given the current clock.
        // When building the date section.
        let result = date_section();

        // Then the date is included.
        assert!(result.contains("Current date:"));
    }

    #[rstest::rstest]
    fn cwd_section_includes_working_directory() {
        // Given a working directory under a temp root.
        let dir = jinn_testutil::tempdir();
        let cwd = dir
            .path
            .join("project")
            .expect("temp root joins a bare name");

        // When building the cwd section.
        let result = cwd_section(&cwd);

        // Then the working directory is included.
        assert_eq!(
            result,
            format!("Current working directory: {}", cwd.to_string_lossy())
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn load_project_context_files_finds_agents_md() {
        // Given a temp directory with AGENTS.md.
        let dir = jinn_testutil::tempdir();
        std::fs::write(
            dir.path
                .join("AGENTS.md")
                .expect("temp root joins a bare name"),
            "# Test",
        )
        .expect("write");

        // When loading context files.
        let files = load_project_context_files(&dir.path).await;

        // Then the file is found.
        assert_eq!(files.len(), 1);
        assert!(files[0].path.ends_with("AGENTS.md"));
        assert_eq!(files[0].content, "# Test");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn load_project_context_files_returns_empty_for_missing() {
        // Given a directory with no context files.
        let dir = jinn_testutil::tempdir();

        // When loading context files.
        let files = load_project_context_files(&dir.path).await;

        // Then no files are found.
        assert!(files.is_empty());
    }

    #[rstest::rstest]
    fn date_from_days_epoch() {
        // Given 0 days since epoch.
        // When converting.
        let date = date_from_days(0);

        // Then it's 1970-01-01.
        assert_eq!(date, "1970-01-01");
    }

    #[rstest::rstest]
    fn date_from_days_known_date() {
        // Given 20000 days since epoch (2024-10-04 roughly).
        // When converting.
        let date = date_from_days(20000);

        // Then it's a valid date format.
        assert!(date.starts_with("20"));
        assert_eq!(date.len(), 10);
    }

    #[rstest::rstest]
    fn date_from_days_one_day() {
        // Given 1 day since epoch.
        // When converting.
        let date = date_from_days(1);

        // Then it's 1970-01-02.
        assert_eq!(date, "1970-01-02");
    }

    #[rstest::rstest]
    fn date_from_days_negative() {
        // Given -1 days (1969-12-31).
        // When converting.
        let date = date_from_days(-1);

        // Then it's 1969-12-31.
        assert_eq!(date, "1969-12-31");
    }

    #[rstest::rstest]
    fn date_from_days_large_known_date() {
        // Given 20023 days = 2024-10-27 (verified externally).
        // When converting.
        let date = date_from_days(20023);

        // Then it produces an exact known date.
        assert_eq!(date, "2024-10-27");
    }

    #[rstest::rstest]
    fn date_from_days_year_2000() {
        // Given 10957 days = 2000-01-01 (30 years including leap days).
        // 365 * 30 + 8 leap days (72,76,80,84,88,92,96,00 - 00 is leap in Gregorian) = 10957 + 7 = 10958?
        // Actually 10957 = 2000-01-01.
        // When converting.
        let date = date_from_days(10957);

        // Then it starts with 2000.
        assert!(date.starts_with("2000-"));
    }

    #[rstest::rstest]
    fn format_current_date_is_not_empty() {
        // Given the current clock.
        // When formatting the current date.
        let date = format_current_date();

        // Then it is non-empty, has length 10, and is YYYY-MM-DD.
        assert_eq!(date.len(), 10);
        assert_eq!(date.chars().nth(4), Some('-'));
        assert_eq!(date.chars().nth(7), Some('-'));
    }

    #[rstest::rstest]
    fn format_current_date_is_not_xyzzy() {
        // Given the current clock.
        // When formatting the current date.
        let date = format_current_date();

        // Then it is not a placeholder string.
        assert_ne!(date, "xyzzy");
        assert_ne!(date, "");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn load_project_context_files_stops_at_root() {
        // Given a deeply nested temp dir with no context files anywhere.
        let root = jinn_testutil::tempdir();
        let deep = root
            .path
            .join("a/b/c")
            .expect("temp root joins a bare name");
        std::fs::create_dir_all(&deep).expect("create");

        // When loading context files.
        let files = load_project_context_files(&deep).await;

        // Then no files are found (no AGENTS.md anywhere on path).
        assert!(files.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn load_project_context_files_finds_file_in_parent() {
        // Given parent/AGENTS.md and child/ as CWD.
        let parent = jinn_testutil::tempdir();
        std::fs::write(
            parent
                .path
                .join("AGENTS.md")
                .expect("temp root joins a bare name"),
            "parent context",
        )
        .expect("write");
        let child = parent
            .path
            .join("subdir")
            .expect("temp root joins a bare name");
        std::fs::create_dir_all(&child).expect("create");

        // When loading from child.
        let files = load_project_context_files(&child).await;

        // Then the parent's file is found.
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].content, "parent context");
    }

    #[rstest::rstest]
    fn sync_load_finds_agents_md() {
        // Given a temp directory with AGENTS.md.
        let dir = jinn_testutil::tempdir();
        std::fs::write(
            dir.path
                .join("AGENTS.md")
                .expect("temp root joins a bare name"),
            "# Sync Test",
        )
        .expect("write");

        // When loading context files synchronously.
        let files = load_project_context_files_sync(&dir.path);

        // Then the file is found.
        assert_eq!(files.len(), 1);
        assert!(files[0].path.ends_with("AGENTS.md"));
        assert_eq!(files[0].content, "# Sync Test");
    }

    #[rstest::rstest]
    fn sync_load_returns_empty_for_missing() {
        // Given a directory with no context files.
        let dir = jinn_testutil::tempdir();

        // When loading context files synchronously.
        let files = load_project_context_files_sync(&dir.path);

        // Then no files are found.
        assert!(files.is_empty());
    }

    #[rstest::rstest]
    fn sync_load_finds_file_in_parent() {
        // Given parent/AGENTS.md and child/ as CWD.
        let parent = jinn_testutil::tempdir();
        std::fs::write(
            parent
                .path
                .join("AGENTS.md")
                .expect("temp root joins a bare name"),
            "parent sync context",
        )
        .expect("write");
        let child = parent
            .path
            .join("subdir")
            .expect("temp root joins a bare name");
        std::fs::create_dir_all(&child).expect("create");

        // When loading from child synchronously.
        let files = load_project_context_files_sync(&child);

        // Then the parent's file is found.
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].content, "parent sync context");
    }
}
