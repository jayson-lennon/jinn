//! The skills modal's shared fuzzy filter — the persona/provider/tools
//! precedent applied to skills.
//!
//! The row vocabulary is the shared [`Skill`] value: the TUI picker's
//! entries carry the same `name` and `description` fields and route
//! through the same match core, so a query filters both surfaces
//! identically. Web rows pair each filtered skill with its enablement
//! mark at render — enablement is session state, not vocabulary, so it
//! stays outside the filter.

use crate::skill::Skill;
use jinn_selection_widget::{TextRun, split_matched_runs};

/// One fuzzy-filtered skill row: the skill plus where its search text
/// matched.
#[derive(Debug, Clone)]
pub struct FilteredSkillRow {
    /// The matched skill, in filter display order.
    pub skill: Skill,
    /// Sorted, deduplicated byte offsets into the skill's search text
    /// that the filter matched. Empty when the filter is empty.
    pub match_indices: Vec<usize>,
}

/// One skill's two text fields, each split into matched/plain runs.
///
/// Presentation-free: the web stamps `<mark>` on matched runs, the TUI
/// stamps a highlight background.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillRowSegments {
    /// The name field's runs, in display order.
    pub name: Vec<TextRun>,
    /// The description field's runs, in display order.
    pub description: Vec<TextRun>,
}

impl FilteredSkillRow {
    /// The row's search text: name then description — the TUI skill
    /// picker's own search closure (`"{name} {description}"`), so a
    /// query filters the web modal identically.
    #[must_use]
    pub fn search_text(&self) -> String {
        format!("{} {}", self.skill.name, self.skill.description)
    }

    /// Splits the row's two text fields into matched/plain runs from the
    /// filter's match indices.
    ///
    /// The indices are byte offsets into [`Self::search_text`] (name
    /// occupies `[0, name_len)`, the single-byte space separator sits at
    /// `name_len`, the description starts at `name_len + 1`); offsets on
    /// the separator belong to neither field. Runs cut on grapheme
    /// boundaries via the shared splitter.
    #[must_use]
    pub fn segments(&self) -> SkillRowSegments {
        let name_len = self.skill.name.len();
        let desc_offset = name_len.saturating_add(1); // the space separator
        let name_hits: Vec<usize> = self
            .match_indices
            .iter()
            .copied()
            .filter(|&i| i < name_len)
            .collect();
        let desc_hits: Vec<usize> = self
            .match_indices
            .iter()
            .copied()
            .filter_map(|i| i.checked_sub(desc_offset))
            .collect();
        SkillRowSegments {
            name: split_matched_runs(&self.skill.name, &name_hits),
            description: split_matched_runs(&self.skill.description, &desc_hits),
        }
    }
}

/// The shared fuzzy filter over skills — the same match core the TUI
/// picker routes through.
///
/// Every whitespace-separated term of `filter` must fuzzy-match the
/// skill's search text (`jinn_selection_widget::fuzzy_filter`,
/// [`SkimMatcherV2`] with per-term AND logic); matches order by
/// cumulative score descending, ties by input order. An empty filter
/// returns the rows unchanged (the caller's sort order) with no ranges.
#[must_use]
pub fn filter_skills(skills: &[Skill], filter: &str) -> Vec<FilteredSkillRow> {
    if filter.is_empty() {
        return skills
            .iter()
            .map(|skill| FilteredSkillRow {
                skill: skill.clone(),
                match_indices: Vec::new(),
            })
            .collect();
    }
    let labels: Vec<String> = skills
        .iter()
        .map(|skill| format!("{} {}", skill.name, skill.description))
        .collect();
    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    jinn_selection_widget::fuzzy_filter(&label_refs, filter)
        .into_iter()
        .filter_map(|matched| {
            skills.get(matched.index).map(|skill| FilteredSkillRow {
                skill: skill.clone(),
                match_indices: matched.match_indices,
            })
        })
        .collect()
}
