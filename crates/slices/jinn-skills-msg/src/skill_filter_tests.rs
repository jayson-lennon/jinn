//! Tests for the shared skills-modal fuzzy filter.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code"
)]

use crate::skill::Skill;
use crate::skill_filter::filter_skills;

/// A minimal skill under `name`, with a `description`.
fn skill(name: &str, description: &str) -> Skill {
    let root = jinn_testutil::tempdir().path;
    Skill {
        name: name.to_owned(),
        description: description.to_owned(),
        body: String::new(),
        file_path: root
            .join_all([name, "SKILL.md"])
            .expect("typed parent joins bare names"),
        base_dir: root.join(name).expect("typed parent joins a bare name"),
        source: crate::skill::SkillSource::Global,
    }
}

#[rstest::rstest]
#[test]
fn filter_skills_with_empty_filter_passes_every_skill_through_in_order() {
    // Given skills in their caller-sorted order.
    let skills = vec![skill("alpha", "first skill"), skill("beta", "second skill")];

    // When filtering with an empty query.
    let filtered = filter_skills(&skills, "");

    // Then every skill passes through in input order with no match
    // indices — nothing is highlighted on an empty filter.
    assert_eq!(filtered.len(), 2);
    assert!(filtered.iter().all(|row| row.match_indices.is_empty()));
    assert_eq!(filtered[0].skill.name, "alpha");
    assert_eq!(filtered[1].skill.name, "beta");
}

#[rstest::rstest]
#[test]
fn filter_skills_narrows_to_matching_skills() {
    // Given skills where only one matches the query.
    let skills = vec![
        skill("alpha", "first skill"),
        skill("phased-loop", "runs phases in order"),
    ];

    // When filtering for "phase".
    let filtered = filter_skills(&skills, "phase");

    // Then exactly the matching skill remains, with the first index
    // landing inside the matched name.
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].skill.name, "phased-loop");
    assert!(
        !filtered[0].match_indices.is_empty()
            && filtered[0].match_indices[0] < filtered[0].skill.name.len()
    );
}

#[rstest::rstest]
#[test]
fn filter_skills_matches_the_description_field_too() {
    // Given a skill whose distinguishing text lives in its description.
    let skills = vec![
        skill("alpha", "first skill"),
        skill("beta", "loads the canvas toolset"),
    ];

    // When filtering for the description-only word "toolset".
    let filtered = filter_skills(&skills, "toolset");

    // Then exactly the beta skill remains — the search text spans both
    // fields, the TUI's `"{name} {description}"` closure.
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].skill.name, "beta");
}

#[rstest::rstest]
#[test]
fn segments_reconstruct_both_fields_and_mark_the_name() {
    // Given a filtered skill whose name matches.
    let skills = vec![skill("canvas", "browser tooling")];
    let filtered = filter_skills(&skills, "canvas");

    // When splitting into per-field runs.
    let segments = filtered[0].segments();

    // Then the name is one wholly matched run, and the description's
    // runs reconstruct its text exactly with no matched run (nothing
    // in the query landed there).
    assert_eq!(segments.name.len(), 1);
    assert!(segments.name[0].matched);
    let desc_rebuilt: String = segments
        .description
        .iter()
        .map(|run| run.text.as_str())
        .collect();
    assert_eq!(desc_rebuilt, "browser tooling");
    assert!(segments.description.iter().all(|run| !run.matched));
}

#[rstest::rstest]
#[test]
fn segments_keep_the_separator_unhighlighted() {
    // Given a skill and its search text — the separator offset (the
    // space at `name_len`) must belong to neither field.
    let skills = vec![skill("ab", "ab description")];
    let filtered = filter_skills(&skills, "");
    let name_len = filtered[0].skill.name.len();

    // When splitting with only the separator offset marked.
    let segments = crate::skill_filter::FilteredSkillRow {
        skill: skills[0].clone(),
        match_indices: vec![name_len],
    }
    .segments();

    // Then neither field carries a matched run — the separator's
    // offset maps into neither the name's range nor the description's.
    assert!(segments.name.iter().all(|run| !run.matched));
    assert!(segments.description.iter().all(|run| !run.matched));
}
