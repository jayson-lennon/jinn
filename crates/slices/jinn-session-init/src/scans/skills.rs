//! Skill scanning - discovers SKILL.md files in a directory tree.

use std::path::Path;

use jinn_skills::frontmatter::{parse_frontmatter, strip_frontmatter};
use jinn_skills::{Skill, SkillSource};

/// Scans a directory for agent skills.
///
/// Looks for `*/SKILL.md` files in direct subdirectories of `dir`.
/// Parses YAML frontmatter for name and description.
/// Skips entries without valid frontmatter or description.
/// Returns an empty vector if the directory does not exist.
pub fn scan_skills(dir: &Path) -> Vec<Skill> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut skills = Vec::new();

    for entry in entries.flatten() {
        let skill_md = entry.path().join("SKILL.md");
        if !skill_md.is_file() {
            continue;
        }

        let Ok(content) = std::fs::read_to_string(&skill_md) else {
            continue;
        };

        let Some(frontmatter) = parse_frontmatter(&content) else {
            continue;
        };

        let Some(description) = frontmatter.description else {
            continue;
        };

        if description.trim().is_empty() {
            continue;
        }

        let name = frontmatter
            .name
            .unwrap_or_else(|| entry.file_name().to_string_lossy().to_string());

        let body = strip_frontmatter(&content);

        skills.push(Skill {
            name,
            description,
            body,
            file_path: skill_md,
            base_dir: entry.path(),
            source: SkillSource::default(),
        });
    }

    skills
}

/// Scans system, global, and project skill dirs, merging by name
/// with most-local-wins precedence.
///
/// Precedence (lowest to highest):
///
/// 1. `system` — system-installed skills (e.g. `/usr/share/jinn/skills`).
/// 2. `global` — user-global skills (`~/.agents/skills`).
/// 3. `project_dirs` — project-local `.agents/skills` directories.
///
/// `project_dirs` are ordered least-local → most-local (i.e. from the root
/// of the bounded walk down to the cwd). Each entry is an already-suffixed
/// `<root>/.agents/skills` directory (as returned by
/// [`project_skills_dirs`](super::walk::project_skills_dirs));
/// it is scanned directly. Later entries in `project_dirs` override earlier
/// ones, and all project skills override the global and system ones.
///
/// Each project skill is tagged with [`SkillSource::Project { dir }`] where
/// `dir` is the walked ancestor (the project root, not the
/// `.agents/skills` subdirectory).
pub fn scan_skills_merged(
    system: &Path,
    global: &Path,
    project_dirs: &[std::path::PathBuf],
) -> Vec<Skill> {
    let mut by_name: std::collections::HashMap<String, Skill> = std::collections::HashMap::new();

    // System skills first (lowest priority).
    for skill in scan_skills(system) {
        by_name.insert(skill.name.clone(), skill);
    }

    // Global skills override system.
    for skill in scan_skills(global) {
        by_name.insert(skill.name.clone(), skill);
    }

    for dir in project_dirs {
        // Each `dir` is already the `<root>/.agents/skills` directory (as
        // returned by [`project_skills_dirs`]); scan it directly rather than
        // re-appending the suffix.
        let project_root = dir
            .ancestors()
            .nth(2)
            .map_or_else(|| dir.clone(), std::path::Path::to_path_buf);
        for skill in scan_skills(dir) {
            let tagged = Skill {
                source: SkillSource::Project {
                    dir: project_root.clone(),
                },
                ..skill
            };
            by_name.insert(tagged.name.clone(), tagged);
        }
    }

    let mut merged: Vec<_> = by_name.into_values().collect();
    merged.sort_by(|a, b| a.name.cmp(&b.name));
    merged
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
    use std::fs;

    #[rstest::rstest]
    fn scan_skills_finds_valid_skill() {
        // Given a temp directory with a valid skill.
        let dir = tempfile::tempdir().expect("create temp dir");
        let skill_dir = dir.path().join("my-skill");
        fs::create_dir_all(&skill_dir).expect("create skill dir");
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: my-skill\ndescription: A test skill\n---\n\n# Content",
        )
        .expect("write SKILL.md");

        // When scanning for skills.
        let skills = scan_skills(dir.path());

        // Then one skill is found.
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "my-skill");
        assert_eq!(skills[0].description, "A test skill");
        assert_eq!(skills[0].body, "# Content");
        assert_eq!(skills[0].file_path, skill_dir.join("SKILL.md"));
        assert_eq!(skills[0].base_dir, skill_dir);
    }

    #[rstest::rstest]
    fn scan_skills_uses_dir_name_when_no_frontmatter_name() {
        // Given a skill without name in frontmatter.
        let dir = tempfile::tempdir().expect("create temp dir");
        let skill_dir = dir.path().join("fallback-name");
        fs::create_dir_all(&skill_dir).expect("create skill dir");
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: Has desc but no name\n---\n\n# Content",
        )
        .expect("write SKILL.md");

        // When scanning for skills.
        let skills = scan_skills(dir.path());

        // Then the directory name is used.
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "fallback-name");
    }

    #[rstest::rstest]
    fn scan_skills_skips_empty_description() {
        // Given a skill with an empty description.
        let dir = tempfile::tempdir().expect("create temp dir");
        let skill_dir = dir.path().join("empty-desc");
        fs::create_dir_all(&skill_dir).expect("create skill dir");
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: empty-desc\ndescription: \n---\n\n# Content",
        )
        .expect("write SKILL.md");

        // When scanning for skills.
        let skills = scan_skills(dir.path());

        // Then no skills are found.
        assert!(skills.is_empty());
    }

    #[rstest::rstest]
    fn scan_skills_skips_missing_description() {
        // Given a skill without description.
        let dir = tempfile::tempdir().expect("create temp dir");
        let skill_dir = dir.path().join("no-desc");
        fs::create_dir_all(&skill_dir).expect("create skill dir");
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: no-desc\n---\n\n# Content",
        )
        .expect("write SKILL.md");

        // When scanning for skills.
        let skills = scan_skills(dir.path());

        // Then no skills are found.
        assert!(skills.is_empty());
    }

    #[rstest::rstest]
    fn scan_skills_skips_dirs_without_skill_md() {
        // Given a directory with a subdirectory but no SKILL.md.
        let dir = tempfile::tempdir().expect("create temp dir");
        let sub = dir.path().join("no-skill-here");
        fs::create_dir_all(&sub).expect("create sub dir");

        // When scanning for skills.
        let skills = scan_skills(dir.path());

        // Then no skills are found.
        assert!(skills.is_empty());
    }

    #[rstest::rstest]
    fn scan_skills_returns_empty_for_nonexistent_dir() {
        // Given a nonexistent directory.
        let dir = Path::new("/nonexistent/path");

        // When scanning for skills.
        let skills = scan_skills(dir);

        // Then no skills are found.
        assert!(skills.is_empty());
    }

    #[rstest::rstest]
    fn scan_skills_skips_invalid_frontmatter() {
        // Given a skill dir with SKILL.md but no frontmatter.
        let dir = tempfile::tempdir().expect("create temp dir");
        let skill_dir = dir.path().join("no-frontmatter");
        fs::create_dir_all(&skill_dir).expect("create skill dir");
        fs::write(
            skill_dir.join("SKILL.md"),
            "# Just markdown\n\nNo frontmatter.",
        )
        .expect("write SKILL.md");

        // When scanning for skills.
        let skills = scan_skills(dir.path());

        // Then no skills are found.
        assert!(skills.is_empty());
    }

    #[rstest::rstest]
    fn scan_skills_finds_multiple_skills() {
        // Given a directory with multiple skills.
        let dir = tempfile::tempdir().expect("create temp dir");

        for name in ["skill-a", "skill-b", "skill-c"] {
            let skill_dir = dir.path().join(name);
            fs::create_dir_all(&skill_dir).expect("create skill dir");
            fs::write(
                skill_dir.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: Skill {name}\n---\n\n# {name}"),
            )
            .expect("write SKILL.md");
        }

        // When scanning for skills.
        let skills = scan_skills(dir.path());

        // Then three skills are found.
        assert_eq!(skills.len(), 3);
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"skill-a"));
        assert!(names.contains(&"skill-b"));
        assert!(names.contains(&"skill-c"));
    }

    #[rstest::rstest]
    fn scan_skills_body_is_empty_for_frontmatter_only_file() {
        // Given a skill with frontmatter but no body content.
        let dir = tempfile::tempdir().expect("create temp dir");
        let skill_dir = dir.path().join("no-body");
        fs::create_dir_all(&skill_dir).expect("create skill dir");
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: no-body\ndescription: Has desc\n---",
        )
        .expect("write SKILL.md");

        // When scanning for skills.
        let skills = scan_skills(dir.path());

        // Then one skill is found with an empty body.
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].body, "");
    }

    #[rstest::rstest]
    fn scan_skills_merged_global_only() {
        // Given a global dir with one skill and no project dirs.
        let global = tempfile::tempdir().expect("create global dir");
        let skill_dir = global.path().join("g-skill");
        fs::create_dir_all(&skill_dir).expect("create skill dir");
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: g-skill\ndescription: Global\n---\n\n# G",
        )
        .expect("write SKILL.md");

        // When merging with no project dirs.
        let skills = scan_skills_merged(Path::new("/nonexistent/system"), global.path(), &[]);

        // Then the global skill is present with Global source.
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "g-skill");
        assert_eq!(skills[0].source, SkillSource::Global);
        assert_eq!(skills[0].body, "# G");
    }

    #[rstest::rstest]
    fn scan_skills_merged_project_overrides_global() {
        // Given a global skill and a project skill with the same name.
        let global = tempfile::tempdir().expect("create global dir");
        let g_skill = global.path().join("shared");
        fs::create_dir_all(&g_skill).expect("create g skill");
        fs::write(
            g_skill.join("SKILL.md"),
            "---\nname: shared\ndescription: Global version\n---\n\n# Global",
        )
        .expect("write SKILL.md");

        let project = tempfile::tempdir().expect("create project dir");
        let p_skill = project.path().join(".agents").join("skills").join("shared");
        fs::create_dir_all(&p_skill).expect("create p skill");
        fs::write(
            p_skill.join("SKILL.md"),
            "---\nname: shared\ndescription: Project version\n---\n\n# Project",
        )
        .expect("write SKILL.md");

        // When merging with the project dir.
        let skills = scan_skills_merged(
            Path::new("/nonexistent/system"),
            global.path(),
            &[project.path().join(".agents").join("skills")],
        );

        // Then the project skill overrides the global one.
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].body, "# Project");
        assert_eq!(
            skills[0].source,
            SkillSource::Project {
                dir: project.path().to_path_buf()
            }
        );
    }

    #[rstest::rstest]
    fn scan_skills_merged_most_local_ancestor_wins() {
        // Given two project ancestors each with a skill of the same name.
        let ancestor = tempfile::tempdir().expect("create ancestor dir");
        let a_skill = ancestor.path().join(".agents").join("skills").join("dup");
        fs::create_dir_all(&a_skill).expect("create a skill");
        fs::write(
            a_skill.join("SKILL.md"),
            "---\nname: dup\ndescription: Ancestor version\n---\n\n# Ancestor",
        )
        .expect("write SKILL.md");

        let local = tempfile::tempdir().expect("create local dir");
        let l_skill = local.path().join(".agents").join("skills").join("dup");
        fs::create_dir_all(&l_skill).expect("create l skill");
        fs::write(
            l_skill.join("SKILL.md"),
            "---\nname: dup\ndescription: Local version\n---\n\n# Local",
        )
        .expect("write SKILL.md");

        // When merging with ancestor first (least-local), local last (most-local).
        let skills = scan_skills_merged(
            Path::new("/nonexistent/system"),
            Path::new("/nonexistent/global"),
            &[
                ancestor.path().join(".agents").join("skills"),
                local.path().join(".agents").join("skills"),
            ],
        );

        // Then the most-local (last) entry wins and is tagged with its dir.
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].body, "# Local");
        assert_eq!(
            skills[0].source,
            SkillSource::Project {
                dir: local.path().to_path_buf()
            }
        );
    }

    #[rstest::rstest]
    #[test]
    fn scan_skills_merged_system_only() {
        // Given a system dir with one skill, no global, no project dirs.
        let system = tempfile::TempDir::new().expect("temp dir");
        let skill_dir = system.path().join("sys-skill");
        std::fs::create_dir_all(&skill_dir).expect("create skill dir");
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: sys-skill\ndescription: A system skill\n---\n\n# System",
        )
        .expect("write SKILL.md");

        // When merging with nonexistent global and no project dirs.
        let skills = scan_skills_merged(system.path(), Path::new("/nonexistent/global"), &[]);

        // Then the system skill is found with Global source.
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "sys-skill");
        assert_eq!(skills[0].body, "# System");
        assert_eq!(skills[0].source, SkillSource::Global);
    }

    #[rstest::rstest]
    #[test]
    fn scan_skills_merged_global_overrides_system() {
        // Given a system dir and a global dir with same-named skill.
        let system = tempfile::TempDir::new().expect("temp dir");
        let sys_skill = system.path().join("shared");
        std::fs::create_dir_all(&sys_skill).expect("create sys skill dir");
        std::fs::write(
            sys_skill.join("SKILL.md"),
            "---\nname: shared\ndescription: System version\n---\n\n# System body",
        )
        .expect("write system SKILL.md");

        let global = tempfile::TempDir::new().expect("temp dir");
        let g_skill = global.path().join("shared");
        std::fs::create_dir_all(&g_skill).expect("create global skill dir");
        std::fs::write(
            g_skill.join("SKILL.md"),
            "---\nname: shared\ndescription: Global version\n---\n\n# Global body",
        )
        .expect("write global SKILL.md");

        // When merging with no project dirs.
        let skills = scan_skills_merged(system.path(), global.path(), &[]);

        // Then the global version wins (overrides system).
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "shared");
        assert_eq!(skills[0].body, "# Global body");
        assert_eq!(skills[0].source, SkillSource::Global);
    }
}
