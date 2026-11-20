//! Persona rows for every picker surface — the TUI popup and the web
//! modal.
//!
//! This module is the shared vocabulary the provider-picker precedent
//! names: the row list, the fuzzy filter, and the field-split that cuts
//! the filter's match indices into per-field highlight runs are defined
//! once here and consumed by both surfaces, so the two lists cannot
//! drift.
//!
//! The row type is presentation-free: no theme, no ratatui. The TUI
//! adapter (in the persona slice) attaches [`jinn_theme::Theme`] and the
//! picker row hooks; the web renders the segments as `<mark>` spans.
//! Neither surface re-derives what a row is.

use jinn_slices::Persona;
use unicode_segmentation::UnicodeSegmentation as _;

/// One persona row ready for display, presentation-free.
///
/// The active mark travels on the row so the "where am I" signal is
/// computed once, at row build, for both surfaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonaRow {
    /// The persona's name — the identity a switch POST validates
    /// against the rendered rows.
    pub name: String,
    /// Short description shown after the name.
    pub description: String,
    /// Whether this row is the session's current persona.
    pub is_active: bool,
}

impl PersonaRow {
    /// Builds the rows from the scanned personas, marking the active one.
    ///
    /// Ordering is the personas cell's own order (name-sorted), so every
    /// surface's list matches every other persona list in the app.
    #[must_use]
    pub fn rows(personas: &[Persona], active: Option<&str>) -> Vec<Self> {
        personas
            .iter()
            .map(|persona| Self {
                name: persona.name.clone(),
                description: persona.description.clone(),
                is_active: active == Some(persona.name.as_str()),
            })
            .collect()
    }

    /// The row's search text: name then description — the same shape the
    /// TUI's filter has always matched against (`"{name} {description}"`),
    /// so a query filters both surfaces identically.
    #[must_use]
    pub fn search_text(&self) -> String {
        format!("{} {}", self.name, self.description)
    }

    /// Splits the row's two text fields into matched/plain runs from the
    /// filter's match indices.
    ///
    /// The indices are byte offsets into [`Self::search_text`]; the split
    /// follows the same field mapping the TUI's highlighter uses (name
    /// occupies `[0, name_len)`, the single-byte space separator sits at
    /// `name_len`, the description starts at `name_len + 1`). Offsets on
    /// the separator belong to neither field. Runs are cut on grapheme
    /// boundaries, so slicing is always a valid UTF-8 boundary.
    #[must_use]
    pub fn segments(&self, match_indices: &[usize]) -> PersonaRowSegments {
        let name_len = self.name.len();
        let desc_offset = name_len.saturating_add(1); // the space separator
        let name_hits: Vec<usize> = match_indices
            .iter()
            .copied()
            .filter(|&i| i < name_len)
            .collect();
        let desc_hits: Vec<usize> = match_indices
            .iter()
            .copied()
            .filter_map(|i| i.checked_sub(desc_offset))
            .collect();
        PersonaRowSegments {
            name: text_segments(&self.name, &name_hits),
            description: text_segments(&self.description, &desc_hits),
        }
    }
}

/// One row's two text fields, each split into matched/plain runs.
///
/// Presentation-free: the web stamps `<mark>` on matched runs, the TUI
/// stamps a highlight background. The provider crate's `TextSegment`
/// twin, defined here because the persona crate cannot depend on the
/// provider crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonaRowSegments {
    /// The name field's runs, in display order.
    pub name: Vec<TextSegment>,
    /// The description field's runs, in display order.
    pub description: Vec<TextSegment>,
}

/// One run of a row's field text: matched runs render highlighted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextSegment {
    /// The run's text (a whole number of grapheme clusters).
    pub text: String,
    /// Whether this run matched the filter (render highlighted).
    pub matched: bool,
}

/// One fuzzy-filtered row: the row plus where its search text matched.
#[derive(Debug, Clone)]
pub struct FilteredPersonaRow {
    /// The matched row, in filter display order.
    pub row: PersonaRow,
    /// Sorted, deduplicated byte offsets into the row's search text
    /// that the filter matched. Empty when the filter is empty.
    pub match_indices: Vec<usize>,
}

impl FilteredPersonaRow {
    /// Splits into per-field runs via the row's own field mapping.
    #[must_use]
    pub fn segments(&self) -> PersonaRowSegments {
        self.row.segments(&self.match_indices)
    }
}

/// The shared fuzzy filter over persona rows — the same match core the
/// TUI picker routes through.
///
/// Every whitespace-separated term of `filter` must fuzzy-match the
/// row's search text (`jinn_selection_widget::fuzzy_filter`, the
/// `SkimMatcherV2` core); matches order by cumulative score descending,
/// ties by input order. An empty filter returns the rows unchanged (the
/// caller's order — the personas cell's name sort) with no ranges.
#[must_use]
pub fn filter_rows(rows: &[PersonaRow], filter: &str) -> Vec<FilteredPersonaRow> {
    if filter.is_empty() {
        return rows
            .iter()
            .map(|row| FilteredPersonaRow {
                row: row.clone(),
                match_indices: Vec::new(),
            })
            .collect();
    }
    let labels: Vec<String> = rows.iter().map(PersonaRow::search_text).collect();
    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    jinn_selection_widget::fuzzy_filter(&label_refs, filter)
        .into_iter()
        .filter_map(|matched| {
            rows.get(matched.index).map(|row| FilteredPersonaRow {
                row: row.clone(),
                match_indices: matched.match_indices,
            })
        })
        .collect()
}

/// Splits `text` into runs of matched and plain text.
///
/// `matched` holds sorted byte offsets into `text` (the filter's match
/// positions, remapped to one field). Runs are cut on grapheme
/// boundaries; an empty `matched` (or empty text) yields one plain run.
#[must_use]
fn text_segments(text: &str, matched: &[usize]) -> Vec<TextSegment> {
    if matched.is_empty() || text.is_empty() {
        return vec![TextSegment {
            text: text.to_owned(),
            matched: false,
        }];
    }
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut in_match = false;
    for (byte_off, grapheme) in text.grapheme_indices(true) {
        let is_matched = matched.binary_search(&byte_off).is_ok();
        if is_matched != in_match {
            if !current.is_empty() {
                segments.push(TextSegment {
                    text: std::mem::take(&mut current),
                    matched: in_match,
                });
            }
            in_match = is_matched;
        }
        current.push_str(grapheme);
    }
    if !current.is_empty() {
        segments.push(TextSegment {
            text: current,
            matched: in_match,
        });
    }
    segments
}

#[cfg(test)]
mod persona_rows_tests;
