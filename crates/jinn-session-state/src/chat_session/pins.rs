//! Pinned tool-loop display: pin/unpin, shown-propagation, and the display-row projection.

use super::*;

impl ChatSessionState {
    /// Pin an entry by ID, setting its pin position.
    ///
    /// If no entry with the given ID exists, this is a no-op.
    /// Pin an entry by ID, setting its pin position.
    ///
    /// If no entry with the given ID exists, this is a no-op.
    ///
    /// When pinning an ignored entry inside a shown (expanded) ignored block,
    /// the pinned entry becomes a block splitter in `build_visual_items`.
    /// This propagates `shown_ignored_blocks` to any new forward sub-block
    /// created by the split, keeping all entries visible.
    pub fn pin_entry(&mut self, id: &ChatEntryId, position: PinPosition) {
        let Some(entry) = self.core.history_work.history.iter().find(|e| e.id == *id) else {
            return;
        };
        // Captured before pinning: a pin makes the entry in-context, but the
        // propagation below only applies when the entry was ignored.
        let was_ignored = !entry.is_in_context();
        let index = self
            .core
            .history_work
            .history
            .iter()
            .position(|e| e.id == *id)
            .unwrap_or_default();
        // Chunk semantics: pinning a member pins the whole tool loop.
        self.edit_history().pin(id, position);
        self.propagate_shown_after_pin(index, was_ignored);
    }

    /// Propagate `shown_ignored_blocks` when pinning an ignored entry inside a
    /// shown (expanded) block split it.
    fn propagate_shown_after_pin(&mut self, idx: usize, was_ignored: bool) {
        // Propagation: only for entries that were ignored before the pin.
        if !was_ignored {
            return;
        }

        // Scan backward to find the containing block's start.
        // Same boundary rules as `build_visual_items` and `toggle_ignored_block_visibility`.
        let mut block_start = idx;
        while block_start > 0
            && self
                .core
                .history_work
                .history
                .get(block_start - 1)
                .is_some_and(|e| !e.is_in_context())
            && self
                .core
                .history_work
                .history
                .get(block_start - 1)
                .is_some_and(|e| e.pin_position.is_none())
        {
            block_start -= 1;
        }

        let Some(block_entry) = self.core.history_work.history.get(block_start) else {
            return;
        };
        let block_representative = block_entry.id.clone();
        let was_shown = self.with_view(
            |v| v.shown_ignored_blocks.contains(&block_representative),
            || false,
        );
        if !was_shown {
            return; // Block was collapsed - nothing to propagate.
        }

        // Scan forward from the pinned entry to find the new forward sub-block.
        let forward_start = idx + 1;
        if forward_start >= self.core.history_work.history.len() {
            return; // No entries after the pin.
        }

        let Some(forward_entry) = self.core.history_work.history.get(forward_start) else {
            return;
        };
        if forward_entry.is_in_context() || forward_entry.pin_position.is_some() {
            return; // Forward entry is not part of an ignored block.
        }

        // The forward sub-block's representative is its first entry.
        let forward_representative = forward_entry.id.clone();
        self.update_view(|v| {
            v.shown_ignored_blocks.insert(forward_representative);
        });
    }

    /// Unpin an entry by ID, clearing its pin position.
    ///
    /// Chunk semantics: unpinning a member unpins the whole tool loop.
    /// If no entry with the given ID exists, this is a no-op.
    pub fn unpin_entry(&mut self, id: &ChatEntryId) {
        self.edit_history().unpin(id);
    }

    /// Returns all pinned entries in history order.
    pub fn pinned_entries(&self) -> Vec<&ChatEntry> {
        self.core
            .history_work
            .history
            .iter()
            .filter(|e| e.is_pinned())
            .collect()
    }

    /// Projects the pinned entries into pins-sidebar display rows.
    ///
    /// A pinned tool loop is pinned as a whole (provider APIs require intact
    /// call/result structure), but it displays as one row per completed
    /// `ToolResult` — plus one `⚒` row per call with no completed result in
    /// the loop — while every non-loop pinned entry keeps its own row.
    /// Rows sort TOP → RELATIVE → BOTTOM, stable by history order within
    /// each group, matching the `pin_sort_key` order of sidebar consumers.
    ///
    /// This is a display projection only: pin state itself is unchanged, so
    /// unpinning a row's entry still clears the whole loop's pins.
    #[must_use]
    pub fn pinned_display_rows(&self) -> Vec<PinDisplayRow> {
        let mut rows = display_rows(self.history());
        // Stable sort by display position: TOP, then RELATIVE/None, then
        // BOTTOM, preserving history order within each group.
        rows.sort_by_key(|row| pin_display_sort_key(row.position));
        rows
    }

    /// The number of rows the pins sidebar displays for this session.
    ///
    /// Sidebar consumers must count display rows, not pinned entries: a
    /// pinned tool loop contributes its result rows, not its member count.
    #[must_use]
    pub fn pinned_display_count(&self) -> usize {
        self.pinned_display_rows().len()
    }
}

/// One row of the pins sidebar: a representative entry plus how to label it.
///
/// The projection from pinned entries to rows lives in
/// [`ChatSessionState::pinned_display_rows`]; this type is only its output.
/// A row's `entry_id` is always a real history entry id, so selection,
/// unpin, and pin-cycle operations address it like any pinned entry, while
/// chunk pin semantics stay unchanged (unpinning still clears the loop).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinDisplayRow {
    /// The id the row's selection, unpin, and pin-cycle operations address:
    /// the id of the representative entry itself.
    pub entry_id: ChatEntryId,
    /// The pin position the row's badge shows, inherited from the entry.
    pub position: PinPosition,
    /// What the row displays.
    pub label: PinRowLabel,
}

/// What a pins-sidebar display row shows.
///
/// The sidebar turns a label into styled text; the prefix is rendered from
/// the variant (`Verbatim` keeps the entry kind's usual prefix, `Result` and
/// `SkillResult` render with no prefix, `Call` renders with the `⚒` prefix).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinRowLabel {
    /// Every non-tool-loop entry kind, displayed exactly as today under its
    /// existing per-kind prefix.
    Verbatim,
    /// A completed tool result: `✓ name: content` on success,
    /// `✗ name: content` on failure.
    Result {
        /// The tool's name.
        name: String,
        /// The result content.
        content: String,
        /// Whether the result completed successfully (`✗` otherwise).
        success: bool,
    },
    /// A loaded-skill tool result: the skill summary label, no prefix.
    SkillResult {
        /// The result content, parsed into the skill summary label.
        content: String,
    },
    /// A tool call without a completed result in its loop: `name(...)`.
    Call {
        /// The tool's name.
        name: String,
    },
}

/// The display sort weight for a pin position: TOP first, then RELATIVE
/// (or absent), then BOTTOM — the same order the sidebar consumers apply.
fn pin_display_sort_key(position: PinPosition) -> u8 {
    match position {
        PinPosition::Top => 0,
        PinPosition::Relative => 1,
        PinPosition::Bottom => 2,
    }
}

/// Projects a history into display rows in history order (unsorted).
///
/// Standalone pinned entries become one row each. A pinned tool loop
/// contributes one row per completed result, one row per call with no
/// completed result, and nothing for the opener or interstitials. Call rows
/// appear where the call sits; result rows where the result sits — history
/// order, so parallel calls/results interleave naturally.
fn display_rows(history: &[ChatEntry]) -> Vec<PinDisplayRow> {
    let chunks = chunking(history);

    let mut rows = Vec::new();
    for (index, entry) in history.iter().enumerate() {
        let Some(position) = entry.pin_position else {
            continue;
        };
        // Every index below history.len() lies in some chunk, so this can
        // only miss if `chunking` and the enumerate disagree — skip, not panic.
        let Some(row_chunk) = chunk_containing(&chunks, index) else {
            continue;
        };

        if is_standalone(&row_chunk) {
            rows.push(standalone_row(entry, position));
            continue;
        }

        let Some(row) = loop_member_row(entry, &row_chunk, history, position) else {
            continue;
        };
        rows.push(row);
    }
    rows
}

/// Whether a chunk is a single standalone entry rather than a tool loop.
fn is_standalone(chunk: &Chunk) -> bool {
    chunk.range.len() == 1
}

/// The display row for a standalone pinned entry (a single-entry chunk).
///
/// Every non-loop kind keeps its own row under its existing prefix; tool
/// kinds pinned on their own (never produced by normal flow, but legal in
/// restored history) project the same label text a loop member would.
fn standalone_row(entry: &ChatEntry, position: PinPosition) -> PinDisplayRow {
    let label = match &entry.kind {
        ChatEntryKind::ToolResult {
            name,
            content,
            status,
            ..
        } => result_label(content, name, *status),
        ChatEntryKind::ToolCall { name, .. } => call_label(name),
        _ => PinRowLabel::Verbatim,
    };
    PinDisplayRow {
        entry_id: entry.id.clone(),
        position,
        label,
    }
}

/// The display row for one member of a pinned multi-entry tool loop.
///
/// Returns `None` for members that render no row: the assistant opener,
/// interstitial entries (system, actor, thinking, transient, annotation,
/// rule-interrupt guidance), and *pending* results — a pending result is
/// represented by its call's row, so when the result completes the call's
/// ⚒ row becomes the result's ✓ row in place and the row count is stable.
/// A completed result always renders its row; a call renders only when no
/// completed result for its id exists anywhere in the same chunk.
fn loop_member_row(
    entry: &ChatEntry,
    chunk: &Chunk,
    history: &[ChatEntry],
    position: PinPosition,
) -> Option<PinDisplayRow> {
    match &entry.kind {
        ChatEntryKind::ToolResult {
            name,
            content,
            status,
            ..
        } if *status != ToolResultStatus::Pending => Some(PinDisplayRow {
            entry_id: entry.id.clone(),
            position,
            label: result_label(content, name, *status),
        }),
        ChatEntryKind::ToolCall { id, name, .. } => {
            if chunk_has_completed_result_for(history, chunk, id) {
                return None;
            }
            Some(PinDisplayRow {
                entry_id: entry.id.clone(),
                position,
                label: call_label(name),
            })
        }
        // Assistant opener and interstitials never display a row.
        _ => None,
    }
}

/// Whether any completed (non-pending) result for `call_id` sits inside
/// `chunk`.
///
/// Matching is by id within the chunk, never by adjacency, so a loop whose
/// calls and results are separated by interstitials still resolves — and a
/// result outside the chunk can never satisfy a call inside it.
fn chunk_has_completed_result_for(
    history: &[ChatEntry],
    chunk: &Chunk,
    call_id: &ToolCallId,
) -> bool {
    let members = history.get(chunk.range.start..chunk.range.end);
    members.is_some_and(|members| {
        members.iter().any(|member| {
            matches!(
                &member.kind,
                ChatEntryKind::ToolResult { id, status, .. }
                    if id == call_id && *status != ToolResultStatus::Pending
            )
        })
    })
}

/// Assembles the label for a tool result row.
///
/// Loaded skills are pinned as `<skill name="X" ...>` XML; they display the
/// parsed skill summary instead of the raw content. Every other result is
/// `icon name: content`, `✓` for success and `✗` for failure.
fn result_label(content: &str, name: &str, status: ToolResultStatus) -> PinRowLabel {
    if name == "skill" {
        return PinRowLabel::SkillResult {
            content: loaded_skill_summary_label(content),
        };
    }
    PinRowLabel::Result {
        name: name.to_owned(),
        content: content.to_owned(),
        success: status == ToolResultStatus::Success,
    }
}

/// Assembles the label for a pending-call row.
fn call_label(name: &str) -> PinRowLabel {
    PinRowLabel::Call {
        name: name.to_owned(),
    }
}

impl PinDisplayRow {
    /// The position badge every pins section shows before a row's text:
    /// `[TOP]`, `[BOT]`, or `[REL]`.
    #[must_use]
    pub fn badge_text(&self) -> &'static str {
        match self.position {
            PinPosition::Top => "[TOP]",
            PinPosition::Bottom => "[BOT]",
            PinPosition::Relative => "[REL]",
        }
    }

    /// The kind prefix and the truncated content for this row, exactly as
    /// the terminal sidebar's pins section builds them — `> ` for a user
    /// entry, `\u{2666} ` for an assistant reply, `\u{2692} ` for a tool
    /// call, and so on, each cut to the section's own width.
    ///
    /// `kind` is the entry's kind for a [`PinRowLabel::Verbatim`] row; `None`
    /// when the entry is no longer in the history, which reads as an empty
    /// preview. Tool rows (`Result`, `SkillResult`, `Call`) are labeled from
    /// the row alone and never consult `kind`.
    #[must_use]
    pub fn prefix_and_content(&self, kind: Option<&ChatEntryKind>) -> (&'static str, String) {
        entry_prefix_and_content(kind, &self.label)
    }

    /// One line of text, exactly as the terminal sidebar's pins section
    /// labels it: the position badge, a space, the kind prefix, and the
    /// truncated content. The web dash's pin rows are built from this.
    #[must_use]
    pub fn label_text(&self, kind: Option<&ChatEntryKind>) -> String {
        let (prefix, content) = self.prefix_and_content(kind);
        format!("{} {prefix}{content}", self.badge_text())
    }
}

// ---------------------------------------------------------------------------
// Label rendering — shared by every surface that shows a pins row
// ---------------------------------------------------------------------------

/// Returns the display prefix and truncated content for a display row.
///
/// `kind` is the resolved entry kind for `Verbatim` rows (or `None` when the
/// entry no longer exists in history, which renders as an empty preview).
/// Tool rows (`Result`/`SkillResult`/`Call`) assemble their text from the
/// label and carry no kind prefix (`⚒` for a call).
fn entry_prefix_and_content(
    kind: Option<&ChatEntryKind>,
    label: &PinRowLabel,
) -> (&'static str, String) {
    match label {
        PinRowLabel::Verbatim => match kind {
            Some(resolved) => verbatim_prefix_and_content(resolved),
            None => ("", String::new()),
        },
        PinRowLabel::Result {
            name,
            content,
            success,
        } => {
            let icon = if *success { "✓" } else { "✗" };
            (
                "",
                format!(
                    "{} {}: {}",
                    icon,
                    truncate_str(name, 15),
                    truncate_str(content, 20)
                ),
            )
        }
        PinRowLabel::SkillResult { content } => ("", truncate_str(content, 40)),
        PinRowLabel::Call { name } => ("\u{2692} ", format!("{}(...)", truncate_str(name, 20))),
    }
}

/// The per-kind prefix and content a `Verbatim` row shows, exactly as
/// the sidebar has always rendered non-tool entries.
fn verbatim_prefix_and_content(kind: &ChatEntryKind) -> (&'static str, String) {
    match kind {
        ChatEntryKind::User { display, .. } => ("> ", truncate_str(display, 40)),
        ChatEntryKind::Assistant(text) => ("\u{2666} ", truncate_str(text, 40)),
        ChatEntryKind::System(text) => ("\u{2699} ", truncate_str(text, 40)),
        ChatEntryKind::Error(text) => ("\u{26a0} ", truncate_str(text, 40)),
        ChatEntryKind::Actor { source, text } => {
            let content = format!("[{}] {}", source, truncate_str(text, 30));
            ("", content)
        }
        // Inter-session mail is self-describing: the text already carries the
        // sender's attribution, so the preview is the text alone.
        ChatEntryKind::Intersession { text, .. } => ("", truncate_str(text, 25)),
        // Table entries, annotations, and tool kinds are not shown in the
        // pinned panel summary. Tool kinds on a Verbatim row cannot occur:
        // the projection labels every tool result/call with a tool label
        // variant.
        ChatEntryKind::Compaction { .. }
        | ChatEntryKind::Annotation { .. }
        | ChatEntryKind::ToolCall { .. }
        | ChatEntryKind::ToolResult { .. } => ("", String::new()),
        // Rule guidance shows the rule that fired, not the guidance body.
        ChatEntryKind::RuleInterrupt { rule, .. } => ("\u{26a1} ", truncate_str(rule, 40)),
        // Thinking entries are not shown in the pinned panel summary.
        ChatEntryKind::Thinking(text) => ("", truncate_str(text, 40)),

        ChatEntryKind::Transient(s) => ("\u{2139} ", truncate_str(s, 40)),
    }
}

/// Truncates a string to the given max grapheme length, appending an ellipsis if needed.
/// Truncate a string to at most `max_width` display columns, appending \u{2026} if truncated.
///
/// Uses `unicode_width` to measure each grapheme's display width. Wide characters
/// (emoji, CJK) that would overflow the budget are skipped entirely rather than
/// rendered partially.
/// Truncates a string to the given max display width, appending an ellipsis
/// if needed. The terminal sidebar clamps an assembled label to its cell
/// budget with this; the label itself is already truncated to the section's
/// own widths.
#[must_use]
pub fn truncate_to_width(s: &str, max_width: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;

    let s_width = UnicodeWidthStr::width(s);
    if s_width <= max_width {
        return s.to_owned();
    }

    // Zero budget: no room for anything, not even an ellipsis.
    if max_width == 0 {
        return String::new();
    }

    // Walk graphemes, accumulating display width.
    // Stop before a grapheme that would overflow the budget
    // (leaving room for the ellipsis).
    let ellipsis_width = 1; // \u{2026} is 1 cell
    let target_width = max_width.saturating_sub(ellipsis_width);

    let mut accumulated = 0usize;
    let mut result = String::new();
    for g in s.graphemes(true) {
        let gw = UnicodeWidthStr::width(g);
        if accumulated + gw > target_width {
            break;
        }
        result.push_str(g);
        accumulated += gw;
    }
    result.push('\u{2026}');
    result
}

/// Strip ANSI escape sequences and truncate to at most `max_width` display columns.
fn truncate_str(s: &str, max_width: usize) -> String {
    truncate_to_width(&strip_ansi_escapes::strip_str(s), max_width)
}

#[cfg(test)]
mod label_text_tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        reason = "test assertions need expect, and a let-else on a known label is unreachable by construction"
    )]
    use super::*;

    /// A Verbatim row at the given position.
    fn verbatim(position: PinPosition) -> PinDisplayRow {
        PinDisplayRow {
            entry_id: ChatEntryId::new(),
            position,
            label: PinRowLabel::Verbatim,
        }
    }

    /// The label a row gives an entry.
    fn label(row: &PinDisplayRow, entry: &ChatEntry) -> String {
        row.label_text(Some(&entry.kind))
    }

    #[rstest::rstest]
    #[case::top(PinPosition::Top, "[TOP]")]
    #[case::bottom(PinPosition::Bottom, "[BOT]")]
    #[case::relative(PinPosition::Relative, "[REL]")]
    fn label_starts_with_the_position_badge(#[case] position: PinPosition, #[case] badge: &str) {
        // Given a pinned user entry.
        let entry = ChatEntry::user("hello");

        // When labeling it at that position.
        let text = label(&verbatim(position), &entry);

        // Then the label starts with the position's badge.
        assert!(text.starts_with(badge), "got {text:?}");
    }

    #[rstest::rstest]
    #[test]
    fn user_entry_is_labeled_with_a_caret_and_its_display_text() {
        // Given a pinned user entry.
        let entry = ChatEntry::user("show me the weather");

        // When labeling it.
        let text = label(&verbatim(PinPosition::Top), &entry);

        // Then the label is the badge, the user prefix, and the text.
        assert_eq!(text, "[TOP] > show me the weather");
    }

    #[rstest::rstest]
    #[test]
    fn assistant_entry_is_labeled_with_a_diamond_and_its_text() {
        // Given a pinned assistant entry.
        let entry = ChatEntry::assistant("the sky is clear");

        // When labeling it.
        let text = label(&verbatim(PinPosition::Top), &entry);

        // Then the label is the badge, the assistant prefix, and the text.
        assert_eq!(text, "[TOP] \u{2666} the sky is clear");
    }

    #[rstest::rstest]
    #[test]
    fn system_entry_is_labeled_with_the_gear_and_its_text() {
        // Given a pinned system entry.
        let entry = ChatEntry::system("compact mode on");

        // When labeling it.
        let text = label(&verbatim(PinPosition::Top), &entry);

        // Then the label is the badge, the system prefix, and the text.
        assert_eq!(text, "[TOP] \u{2699} compact mode on");
    }

    #[rstest::rstest]
    #[test]
    fn error_entry_is_labeled_with_the_warning_sign_and_its_text() {
        // Given a pinned error entry.
        let entry = ChatEntry::error("rate limited");

        // When labeling it.
        let text = label(&verbatim(PinPosition::Top), &entry);

        // Then the label is the badge, the error prefix, and the text.
        assert!(text.starts_with("[TOP] \u{26A0} "), "got {text:?}");
    }

    #[rstest::rstest]
    #[test]
    fn transient_entry_is_labeled_with_the_info_mark_and_its_text() {
        // Given a pinned transient entry.
        let entry = ChatEntry::transient("note to self");

        // When labeling it.
        let text = label(&verbatim(PinPosition::Top), &entry);

        // Then the label is the badge, the transient prefix, and the text.
        assert_eq!(text, "[TOP] \u{2139} note to self");
    }

    #[rstest::rstest]
    #[test]
    fn long_text_is_cut_to_forty_columns_with_an_ellipsis() {
        // Given a user entry sixty characters long.
        let entry = ChatEntry::user("x".repeat(60));

        // When labeling it.
        let text = label(&verbatim(PinPosition::Top), &entry);

        // Then the content is 40 columns wide, ending in an ellipsis.
        let content = text.strip_prefix("[TOP] > ").expect("the label's prefix");
        assert_eq!(content.chars().count(), 40);
        assert!(content.ends_with('\u{2026}'));
    }

    #[rstest::rstest]
    #[test]
    fn tool_call_row_is_labeled_with_the_hammer_and_the_truncated_name() {
        // Given a row for a tool call named "read_file".
        let row = PinDisplayRow {
            entry_id: ChatEntryId::new(),
            position: PinPosition::Relative,
            label: PinRowLabel::Call {
                name: "read_file".to_owned(),
            },
        };

        // When labeling it.
        let text = row.label_text(None);

        // Then the label is the badge, the hammer, and the name.
        assert_eq!(text, "[REL] \u{2692} read_file(...)");
    }

    #[rstest::rstest]
    #[test]
    fn successful_result_row_shows_a_check_and_the_result() {
        // Given a row for a successful tool result.
        let row = PinDisplayRow {
            entry_id: ChatEntryId::new(),
            position: PinPosition::Relative,
            label: PinRowLabel::Result {
                name: "read".to_owned(),
                content: "file body".to_owned(),
                success: true,
            },
        };

        // When labeling it.
        let text = row.label_text(None);

        // Then the label is the badge, a check, and name: content.
        assert_eq!(text, "[REL] ✓ read: file body");
    }

    #[rstest::rstest]
    #[test]
    fn failed_result_row_shows_a_cross_and_the_result() {
        // Given a row for a failed tool result.
        let row = PinDisplayRow {
            entry_id: ChatEntryId::new(),
            position: PinPosition::Relative,
            label: PinRowLabel::Result {
                name: "bash".to_owned(),
                content: "exit 1".to_owned(),
                success: false,
            },
        };

        // When labeling it.
        let text = row.label_text(None);

        // Then the label is the badge, a cross, and name: content.
        assert_eq!(text, "[REL] ✗ bash: exit 1");
    }

    #[rstest::rstest]
    #[test]
    fn skill_result_row_is_labeled_with_its_content_only() {
        // Given a row for a skill's result.
        let row = PinDisplayRow {
            entry_id: ChatEntryId::new(),
            position: PinPosition::Bottom,
            label: PinRowLabel::SkillResult {
                content: "3 matches found".to_owned(),
            },
        };

        // When labeling it.
        let text = row.label_text(None);

        // Then the label is the badge and the content, with no prefix.
        assert_eq!(text, "[BOT] 3 matches found");
    }

    #[rstest::rstest]
    #[test]
    fn missing_entry_reads_as_an_empty_preview() {
        // Given a Verbatim row whose entry is no longer in the history.
        let row = verbatim(PinPosition::Top);

        // When labeling it without a kind.
        let text = row.label_text(None);

        // Then the label is the badge alone.
        assert_eq!(text, "[TOP] ");
    }
}

#[cfg(test)]
mod truncation_tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]

    use super::truncate_str;
    use super::truncate_to_width;

    #[rstest::rstest]
    fn truncate_to_width_returns_string_unchanged_when_it_fits() {
        // Given a string that fits within the budget.

        // When truncating to the budget.
        let result = truncate_to_width("hello", 10);

        // Then it is returned unchanged.
        assert_eq!(result, "hello");
    }

    #[rstest::rstest]
    fn truncate_to_width_truncates_ascii_and_appends_ellipsis() {
        // Given a string that exceeds the budget.

        // When truncating to the budget.
        let result = truncate_to_width("hello world", 5);

        // Then it is truncated to fit with an ellipsis.
        assert_eq!(result, "hell\u{2026}");
    }

    #[rstest::rstest]
    fn truncate_to_width_fits_double_wide_char_within_budget() {
        // Given a string with a double-wide char that fits.

        // When truncating to the budget.
        let result = truncate_to_width("\u{2705} ok", 10);

        // Then it is returned unchanged (2+1+2=5 <= 10).
        assert_eq!(result, "\u{2705} ok");
    }

    #[rstest::rstest]
    fn truncate_to_width_skips_wide_char_at_boundary() {
        // Given a string where a 2-cell char would overflow the budget.

        // When truncating to the budget.
        let result = truncate_to_width("ab\u{2705}", 3);

        // Then the wide char is skipped and ellipsis is appended.
        assert_eq!(result, "ab\u{2026}");
    }

    #[rstest::rstest]
    fn truncate_to_width_fits_wide_char_exactly() {
        // Given a string where a wide char fills the budget exactly.

        // When truncating to the budget.
        let result = truncate_to_width("a\u{2705}b", 4);

        // Then it is returned unchanged (1+2+1=4).
        assert_eq!(result, "a\u{2705}b");
    }

    #[rstest::rstest]
    fn truncate_to_width_only_wide_chars_budget_one() {
        // Given only wide chars with a budget of 1.

        // When truncating to the budget.
        let result = truncate_to_width("\u{2705}\u{274c}", 1);

        // Then only the ellipsis fits.
        assert_eq!(result, "\u{2026}");
    }

    #[rstest::rstest]
    fn truncate_to_width_empty_string_returns_empty() {
        // Given an empty string.

        // When truncating to the budget.
        let result = truncate_to_width("", 5);

        // Then it is returned unchanged.
        assert_eq!(result, "");
    }

    #[rstest::rstest]
    fn truncate_to_width_zero_budget_returns_empty() {
        // Given a non-empty string with zero budget.

        // When truncating to the budget.
        let result = truncate_to_width("hello", 0);

        // Then an empty string is returned (no room for anything).
        assert_eq!(result, "");
    }

    #[rstest::rstest]
    fn truncate_str_strips_ansi_before_truncating() {
        // Given a string with ANSI escape codes.

        // When truncating to the budget.
        let result = truncate_str("\x1b[31mhello\x1b[0m", 3);

        // Then ANSI is stripped and the plain text is truncated.
        assert_eq!(result, "he\u{2026}");
    }
}
