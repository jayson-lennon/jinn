//! Response formatting for the edit tool.
//!
//! After a successful edit, the response includes a cat -n snippet of the
//! changed region (± a few context lines), letting the LLM chain follow-up
//! edits without re-reading the file.

use crate::visible_lines::get_visible_lines;

// ─── Constants ──────────────────────────────────────────────────────────

/// Context lines around the changed region in snippets.
const SNIPPET_CONTEXT_LINES: usize = 2;
/// Byte budget for snippets (50KB).
const SNIPPET_TEXT_BUDGET_BYTES: usize = 50 * 1024;

// ─── Types ─────────────────────────��────────────────────────────────────

/// The formatted changed-region snippet for a response.
#[derive(Debug)]
pub enum ChangedSnippet {
    /// A formatted cat -n block for the changed region.
    Block { text: String },
    /// Snippet omitted because the span was too large.
    Omitted,
    /// File is empty after edit.
    EmptyFile,
}

// ─── Snippet building ───────────────────────────────────────────────────

/// Builds the cat -n snippet for a successful edit response.
///
/// Takes the post-edit content (LF-normalized) and the changed-line range
/// from [`super::engine::compute_changed_line_range`].
#[must_use]
pub fn build_changed_snippet(
    result_content: &str,
    first_changed_line: Option<usize>,
    last_changed_line: Option<usize>,
) -> ChangedSnippet {
    let visible = get_visible_lines(result_content);

    if visible.is_empty() {
        return ChangedSnippet::EmptyFile;
    }

    let Some((start, end)) = snippet_range(first_changed_line, last_changed_line, visible.len())
    else {
        return ChangedSnippet::Omitted;
    };

    let region: Vec<&str> = visible
        .get(start.saturating_sub(1)..end)
        .unwrap_or_default()
        .to_vec();
    let formatted = format_numbered_region(&region, start);
    let block_text = format!("--- lines {start}-{end} ---\n{formatted}");

    if block_text.len() > SNIPPET_TEXT_BUDGET_BYTES {
        return ChangedSnippet::Omitted;
    }

    ChangedSnippet::Block { text: block_text }
}

/// Computes the snippet range: changed lines ± context, clamped to the file.
fn snippet_range(
    first_changed_line: Option<usize>,
    last_changed_line: Option<usize>,
    result_line_count: usize,
) -> Option<(usize, usize)> {
    let first = first_changed_line?;
    let last = last_changed_line?;

    if result_line_count == 0 {
        return None;
    }

    let start = first.saturating_sub(SNIPPET_CONTEXT_LINES).max(1);
    let end = (last + SNIPPET_CONTEXT_LINES).min(result_line_count);

    if end < start {
        return None;
    }
    Some((start, end))
}

/// Formats lines as cat -n: right-aligned number, a tab, then content.
fn format_numbered_region(region: &[&str], start_line: usize) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(region.iter().map(|l| l.len() + 8).sum());
    let max_line = start_line + region.len().saturating_sub(1);
    let width = format!("{max_line}").len();

    for (i, line) in region.iter().enumerate() {
        let line_num = start_line + i;
        let _ = writeln!(out, "{line_num:>width$}\t{line}");
    }

    out
}

// ─── Response formatting ──────────────────────────────────���─────────────

/// Formats the full response text for a successful edit.
#[must_use]
pub fn format_success_response(path: &str, snippet: &ChangedSnippet) -> String {
    let snippet_text = match snippet {
        ChangedSnippet::Block { text } => text.clone(),
        ChangedSnippet::Omitted => {
            "Snippet omitted; use read to see the current content.".to_owned()
        }
        ChangedSnippet::EmptyFile => "File is empty after the edit.".to_owned(),
    };

    format!("Edited {path}\n{snippet_text}")
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

    #[rstest::rstest]
    fn snippet_range_includes_context_lines() {
        // Given a changed region of lines 3-5 in a 10-line file.
        // When computing the snippet range.
        // Then it includes 2 context lines on each side.
        let range = snippet_range(Some(3), Some(5), 10);
        assert_eq!(range, Some((1, 7)));
    }

    #[rstest::rstest]
    fn snippet_range_clamps_to_file_bounds() {
        // Given a change at the end of a 5-line file.
        // When computing the snippet range.
        // Then end is clamped to 5.
        let range = snippet_range(Some(4), Some(5), 5);
        assert_eq!(range, Some((2, 5)));
    }

    #[rstest::rstest]
    fn numbered_region_uses_number_tab_content() {
        // Given a two-line region starting at line 1.
        // When formatting.
        // Then each line is "number + tab + content".
        let formatted = format_numbered_region(&["fn main() {", "}"], 1);
        let lines: Vec<&str> = formatted.lines().collect();
        assert_eq!(lines[0], "1\tfn main() {");
        assert_eq!(lines[1], "2\t}");
    }

    #[rstest::rstest]
    fn build_changed_snippet_formats_changed_region() {
        // Given a 3-line file whose line 2 changed.
        // When building the snippet.
        // Then the block spans lines 1-3 with cat -n formatting.
        let snippet = build_changed_snippet("alpha\nBETA\ngamma\n", Some(2), Some(2));
        let ChangedSnippet::Block { text } = snippet else {
            panic!("expected block, got {snippet:?}");
        };
        assert!(text.starts_with("--- lines 1-3 ---"));
        assert!(text.contains("\tBETA"));
    }

    #[rstest::rstest]
    fn build_changed_snippet_empty_file() {
        // Given empty post-edit content.
        // When building the snippet.
        // Then it reports the empty file.
        assert!(matches!(
            build_changed_snippet("", Some(1), Some(1)),
            ChangedSnippet::EmptyFile
        ));
    }

    #[rstest::rstest]
    fn success_response_includes_snippet() {
        // Given a snippet block.
        // When formatting the success response.
        // Then the response names the file and embeds the snippet.
        let snippet = build_changed_snippet("a\nB\nc\n", Some(2), Some(2));
        let text = format_success_response("f.rs", &snippet);
        assert!(text.starts_with("Edited f.rs"));
        assert!(text.contains("--- lines 1-3 ---"));
    }
}
