//! The bounded window of transcript entries a frame carries.
//!
//! A snapshot's tail and a page of older entries are the same rule applied
//! at a different end of the history: walk the entries newest-first and
//! keep them until one of two limits is hit — [`TAIL_MAX_ENTRIES`] entries,
//! or [`TAIL_MAX_BYTES`] of wire bytes, whichever comes first. Always at
//! least one entry is carried, whatever its size, so a page is never empty
//! while history remains.
//!
//! The rule lives here, once, because three frames are bounded by it: the
//! snapshot a fresh page receives, the gap a reconnecting page resumes
//! with, and the page a scroll to the top asks for. The functions work on
//! a history slice, so a caller holds one read of state for the whole
//! answer.

use jinn_core_types::{ChatEntry, ChatEntryId};
use jinn_webui2_msg::{EntryEnvelope, TAIL_MAX_BYTES, TAIL_MAX_ENTRIES};

/// A window of entries in history order, with how much history precedes it.
#[derive(Debug, Clone, PartialEq)]
pub struct EntryWindow {
    /// The entries, oldest first.
    pub entries: Vec<EntryEnvelope>,
    /// How many entries of the session precede the oldest one carried.
    pub entries_before: usize,
}

/// The newest entries of a history, bounded by the window rule.
#[must_use]
pub fn tail(history: &[ChatEntry]) -> EntryWindow {
    window_ending_at(history, history.len())
}

/// The entries that come strictly after the entry at `cursor_index` — the
/// gap a reconnecting page missed — when that gap fits one window.
///
/// `None` when the gap is larger than one window: the caller then answers
/// with the tail, so a reconnect can never replay an unbounded stretch of
/// history.
#[must_use]
pub fn gap_after(history: &[ChatEntry], cursor_index: usize) -> Option<EntryWindow> {
    let mut newest = window_ending_at(history, history.len());
    let gap_len = history.len().saturating_sub(cursor_index + 1);
    // The gap fits exactly when the newest window reaches back past it.
    if newest.entries.len() < gap_len {
        return None;
    }
    newest.entries.drain(..newest.entries.len() - gap_len);
    Some(EntryWindow {
        entries: newest.entries,
        entries_before: 0,
    })
}

/// One page of entries ending just before the entry at `before_index` —
/// the oldest entry the page holds.
///
/// `None` when it is the first entry: there is nothing older to load.
#[must_use]
pub fn page_before(history: &[ChatEntry], before_index: usize) -> Option<EntryWindow> {
    (before_index > 0).then(|| window_ending_at(history, before_index))
}

impl EntryWindow {
    /// A window carrying nothing.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            entries: Vec::new(),
            entries_before: 0,
        }
    }

    /// The ids of every entry carried, oldest first.
    pub fn ids(&self) -> impl Iterator<Item = ChatEntryId> + '_ {
        self.entries.iter().map(EntryEnvelope::id)
    }
}

/// The window of entries ending at `end` (exclusive), walking newest-first
/// until a limit is hit, returned oldest-first.
fn window_ending_at(history: &[ChatEntry], end: usize) -> EntryWindow {
    // One envelope per entry, newest first: the same envelope both measures
    // the size and is carried, so nothing is serialized twice.
    let Some(newest_first) = history.get(..end) else {
        return EntryWindow::empty();
    };
    let projected = newest_first
        .iter()
        .rev()
        .map(EntryEnvelope::new)
        .collect::<Vec<_>>();
    let take = take_count(&projected);
    let mut entries = projected
        .get(..take)
        .map_or_else(Vec::new, <[EntryEnvelope]>::to_vec);
    entries.reverse();
    EntryWindow {
        entries,
        entries_before: end - take,
    }
}

/// How many of the newest envelopes one window carries.
///
/// The first is always carried; each next one is carried only while
/// neither limit has been hit.
fn take_count(projected: &[EntryEnvelope]) -> usize {
    let mut bytes = 0usize;
    let mut take = 0usize;
    for envelope in projected {
        if take >= TAIL_MAX_ENTRIES || (take > 0 && bytes + wire_size(envelope) > TAIL_MAX_BYTES) {
            break;
        }
        bytes += wire_size(envelope);
        take += 1;
    }
    take
}

/// An envelope's size on the wire, as the frame will carry it.
fn wire_size(envelope: &EntryEnvelope) -> usize {
    serde_json::to_string(envelope).map_or(usize::MAX, |json| json.len())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        reason = "test assertions need expect to state what went wrong"
    )]

    use super::{EntryWindow, gap_after, page_before, tail};
    use jinn_core_types::ChatEntry;
    use jinn_webui2_msg::{EntryEnvelope, TAIL_MAX_BYTES};

    /// `count` user entries, each carrying `text`.
    fn history_of(count: usize, text: &str) -> Vec<ChatEntry> {
        std::iter::repeat_n(ChatEntry::user(text), count).collect()
    }

    /// A window's bytes on the wire.
    fn wire_bytes(window: &EntryWindow) -> usize {
        window
            .entries
            .iter()
            .map(|envelope| serde_json::to_string(envelope).expect("serializes").len())
            .sum()
    }

    #[rstest::rstest]
    #[test]
    fn a_history_longer_than_the_limit_yields_the_newest_limit_entries() {
        // Given more entries than one window carries.
        let history = history_of(1_000, "x");

        // When taking the tail.
        let window = tail(&history);

        // Then it carries exactly the limit, and the rest are counted as before.
        assert_eq!(window.entries.len(), jinn_webui2_msg::TAIL_MAX_ENTRIES);
        assert_eq!(
            window.entries_before,
            1_000 - jinn_webui2_msg::TAIL_MAX_ENTRIES
        );
    }

    /// The `display` text of each carried entry, oldest first.
    fn carried_texts(window: &super::EntryWindow) -> Vec<String> {
        window
            .entries
            .iter()
            .map(EntryEnvelope::entry_text)
            .map(ToOwned::to_owned)
            .collect()
    }

    #[rstest::rstest]
    #[test]
    fn a_history_within_the_limit_is_carried_whole() {
        // Given a history of four entries, well under both limits.
        let history = ["one", "two", "three", "four"]
            .map(ChatEntry::user)
            .to_vec();

        // When taking the tail.
        let window = super::tail(&history);

        // Then it carries all four, oldest first, with nothing before them.
        assert_eq!(carried_texts(&window), ["one", "two", "three", "four"]);
        assert_eq!(window.entries_before, 0);
    }

    #[rstest::rstest]
    #[test]
    fn the_byte_limit_cuts_the_window_short_of_the_entry_limit() {
        // Given entries large enough that the byte limit is hit first.
        let history = history_of(1_000, &"y".repeat(10_000));

        // When taking the tail.
        let window = tail(&history);

        // Then fewer than the entry limit is carried, within the byte limit.
        assert!(window.entries.len() < jinn_webui2_msg::TAIL_MAX_ENTRIES);
        assert!(wire_bytes(&window) <= TAIL_MAX_BYTES);
    }

    #[rstest::rstest]
    #[test]
    fn a_single_entry_larger_than_the_byte_limit_is_still_carried() {
        // Given one entry bigger than the whole byte limit.
        let history = history_of(1, &"z".repeat(2_000_000));

        // When taking the tail.
        let window = tail(&history);

        // Then it is carried: a page is never empty while history remains.
        assert_eq!(window.entries.len(), 1);
        assert_eq!(window.entries_before, 0);
    }

    #[rstest::rstest]
    #[test]
    fn an_empty_history_yields_an_empty_window() {
        // Given no entries.
        let history: Vec<ChatEntry> = Vec::new();

        // When taking the tail.
        let window = tail(&history);

        // Then it carries nothing.
        assert_eq!(window.entries, Vec::<EntryEnvelope>::new());
        assert_eq!(window.entries_before, 0);
    }

    #[rstest::rstest]
    #[test]
    fn a_gap_within_one_window_is_the_entries_after_the_cursor() {
        // Given a page whose cursor is the third of five entries.
        let history = history_of(5, "x");

        // When asking for the gap.
        let gap = gap_after(&history, 2).expect("the gap fits");

        // Then it carries the two entries after the cursor, and nothing before.
        assert_eq!(gap.entries.len(), 2);
        assert_eq!(gap.entries_before, 0);
    }

    #[rstest::rstest]
    #[test]
    fn a_gap_larger_than_one_window_is_none() {
        // Given a page whose cursor is the first of more entries than a window holds.
        let history = history_of(1_000, "x");

        // When asking for the gap.
        let gap = gap_after(&history, 0);

        // Then there is none: the caller answers with the tail instead.
        assert!(gap.is_none());
    }

    #[rstest::rstest]
    #[test]
    fn a_page_ends_just_before_the_oldest_held_entry() {
        // Given a history of five, with the third the oldest the page holds.
        let history = history_of(5, "x");

        // When asking for the page before it.
        let page = page_before(&history, 2).expect("there are older entries");

        // Then it carries the two entries before it, oldest first.
        assert_eq!(page.entries.len(), 2);
        assert_eq!(page.entries_before, 0);
    }

    #[rstest::rstest]
    #[test]
    fn there_is_no_page_before_the_first_entry() {
        // Given a history, and a page holding its very first entry.
        let history = history_of(5, "x");

        // When asking for the page before the first entry.
        let page = page_before(&history, 0);

        // Then there is none.
        assert!(page.is_none());
    }
}
