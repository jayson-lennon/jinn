//! The workflows directory scan — pure filesystem logic, run on a blocking
//! thread by the session-init discovery worker.
//!
//! Scans `*/workflow.md` in direct subdirectories of the workflows dir.
//! Admission is strict (see [`crate::frontmatter`]); a workflow admitted
//! without a frontmatter `name` takes the directory name. A nonexistent
//! workflows dir yields an empty list, never an error.

use std::path::Path;

use jinn_workflows_msg::Workflow;

use crate::frontmatter::parse_workflow_frontmatter;

/// The name of the file that defines a workflow inside its directory.
pub const WORKFLOW_FILE: &str = "workflow.md";

/// Scans a workflows directory for admitted workflows.
///
/// Looks for `*/workflow.md` in direct subdirectories of `dir`, parses each
/// with the shared `+++` TOML frontmatter parser, and admits only those
/// with valid frontmatter and a non-blank `description`. The result is
/// sorted case-insensitively by name.
#[must_use]
pub fn scan_workflows(dir: &Path) -> Vec<Workflow> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut workflows = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let workflow_md = path.join(WORKFLOW_FILE);
        let Ok(content) = std::fs::read_to_string(&workflow_md) else {
            continue;
        };
        let Ok(frontmatter) = parse_workflow_frontmatter(&content) else {
            continue;
        };
        workflows.push(Workflow {
            name: frontmatter
                .name
                .unwrap_or_else(|| entry.file_name().to_string_lossy().to_string()),
            description: frontmatter.description.unwrap_or_default(),
            dir: path,
        });
    }

    workflows.sort_by_key(|workflow| workflow.name.to_lowercase());
    workflows
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::*;
    use std::fs;

    /// Writes `<base>/<name>/workflow.md` with `content`, creating dirs.
    fn write_workflow(base: &Path, name: &str, content: &str) {
        let dir = base.join(name);
        fs::create_dir_all(&dir).expect("create workflow dir");
        fs::write(dir.join(WORKFLOW_FILE), content).expect("write workflow.md");
    }

    #[rstest::rstest]
    fn scan_finds_a_workflow_with_valid_frontmatter() {
        // Given a workflows dir with one admissible workflow.
        let dir = tempfile::tempdir().expect("temp dir");
        write_workflow(
            dir.path(),
            "research-report",
            "+++\nname = \"research-report\"\ndescription = \"Research a topic.\"\n+++\n\n## Flow\n",
        );

        // When scanning.
        let workflows = scan_workflows(dir.path());

        // Then one workflow is found with the frontmatter identity.
        assert_eq!(workflows.len(), 1);
        assert_eq!(workflows[0].name, "research-report");
        assert_eq!(workflows[0].description, "Research a topic.");
        assert_eq!(workflows[0].dir, dir.path().join("research-report"));
    }

    #[rstest::rstest]
    fn scan_skips_a_workflow_md_without_frontmatter() {
        // Given a directory whose workflow.md carries the legacy H1 header
        // and no +++ block.
        let dir = tempfile::tempdir().expect("temp dir");
        write_workflow(
            dir.path(),
            "legacy",
            "# Name: legacy\n# Description: old style\n\n## Flow\n",
        );

        // When scanning.
        let workflows = scan_workflows(dir.path());

        // Then nothing is admitted.
        assert_eq!(workflows, [] as [jinn_workflows_msg::Workflow; 0]);
    }

    #[rstest::rstest]
    fn scan_skips_malformed_frontmatter() {
        // Given a workflow.md whose +++ block is not valid TOML.
        let dir = tempfile::tempdir().expect("temp dir");
        write_workflow(dir.path(), "broken", "+++\nname = [unclosed\n+++\nbody");

        // When scanning.
        let workflows = scan_workflows(dir.path());

        // Then nothing is admitted.
        assert_eq!(workflows, [] as [jinn_workflows_msg::Workflow; 0]);
    }

    #[rstest::rstest]
    fn scan_skips_a_missing_or_blank_description() {
        // Given one workflow with no description and one with a blank one.
        let dir = tempfile::tempdir().expect("temp dir");
        write_workflow(dir.path(), "no-desc", "+++\nname = \"no-desc\"\n+++\nbody");
        write_workflow(
            dir.path(),
            "blank-desc",
            "+++\nname = \"blank\"\ndescription = \"   \"\n+++\nbody",
        );

        // When scanning.
        let workflows = scan_workflows(dir.path());

        // Then neither is admitted.
        assert_eq!(workflows, [] as [jinn_workflows_msg::Workflow; 0]);
    }

    #[rstest::rstest]
    fn scan_falls_back_to_the_directory_name() {
        // Given an admissible workflow whose frontmatter omits the name.
        let dir = tempfile::tempdir().expect("temp dir");
        write_workflow(
            dir.path(),
            "fallback-name",
            "+++\ndescription = \"No name in frontmatter\"\n+++\nbody",
        );

        // When scanning.
        let workflows = scan_workflows(dir.path());

        // Then the directory name is used.
        assert_eq!(workflows.len(), 1);
        assert_eq!(workflows[0].name, "fallback-name");
    }

    #[rstest::rstest]
    fn scan_ignores_stray_files_and_directories_without_workflow_md() {
        // Given a dir holding a bare file and a subdir without workflow.md
        // beside one admissible workflow.
        let dir = tempfile::tempdir().expect("temp dir");
        write_workflow(
            dir.path(),
            "real",
            "+++\nname = \"real\"\ndescription = \"Real\"\n+++\nbody",
        );
        fs::create_dir_all(dir.path().join("no-md")).expect("subdir");
        fs::write(dir.path().join("loose.md"), "+++\ndescription = \"x\"\n+++")
            .expect("stray file");

        // When scanning.
        let workflows = scan_workflows(dir.path());

        // Then only the admissible workflow comes back.
        assert_eq!(workflows.len(), 1);
        assert_eq!(workflows[0].name, "real");
    }

    #[rstest::rstest]
    fn scan_returns_empty_for_a_nonexistent_dir() {
        // Given a workflows dir that does not exist.
        let dir = Path::new("/nonexistent/workflows");

        // When scanning.
        let workflows = scan_workflows(dir);

        // Then the result is empty, not an error.
        assert_eq!(workflows, [] as [jinn_workflows_msg::Workflow; 0]);
    }

    #[rstest::rstest]
    fn scan_sorts_case_insensitively_by_name() {
        // Given three admissible workflows in non-alphabetical order.
        let dir = tempfile::tempdir().expect("temp dir");
        write_workflow(
            dir.path(),
            "zeta",
            "+++\nname = \"zeta\"\ndescription = \"last\"\n+++\nbody",
        );
        write_workflow(
            dir.path(),
            "alpha",
            "+++\nname = \"Alpha\"\ndescription = \"first\"\n+++\nbody",
        );
        write_workflow(
            dir.path(),
            "mid",
            "+++\nname = \"mid\"\ndescription = \"middle\"\n+++\nbody",
        );

        // When scanning.
        let workflows = scan_workflows(dir.path());

        // Then they come back case-insensitively sorted.
        let names: Vec<&str> = workflows.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, ["Alpha", "mid", "zeta"]);
    }
}
