//! Dashboard state — the tab's cell payload and entry model.
//!
//! The cell is a projection of the runtime's actor census, ordered by how
//! broken each row is rather than by name or by when it appeared. A reader
//! opening the tab should see the actors that need attention at the top and
//! the steady-state ones below, without filtering anything.

use std::collections::HashMap;

use jinn_core_types::ActorLifecycle;
use jinn_slices::NoteTone;
use jinn_slices::SlotKey;

/// A single actor's display data in the dashboard.
#[derive(Debug, Clone)]
pub struct DashboardEntry {
    /// The actor's display name (also its unique key).
    pub name: String,
    /// A short description of what the actor does.
    pub description: Option<String>,
    /// The runtime's verdict on this actor, as last announced.
    ///
    /// Written only by the census fold in the dashboard's canvas actor; no
    /// other code path assigns it, and a feature publishing a
    /// `ServiceStatusUpdate` cannot reach it.
    pub lifecycle: ActorLifecycle,
    /// Free-form third column; the owning feature writes its connection or
    /// resolution status here via `ServiceStatusUpdate`.
    pub status_message: Option<String>,
    /// How loudly the owning feature's note should read.
    ///
    /// A feature's opinion about its own service, distinct from the
    /// runtime's verdict on the actor. It reorders rows within a lifecycle
    /// band but never across one.
    pub note_tone: NoteTone,
}

/// Owned by the dashboard slice's canvas actor, which is its sole writer;
/// the renderer resolves a read handle.
///
/// Holds no scroll position: the offset is a pure function of the cursor
/// and the viewport, so there is nothing to remember and nothing for a
/// renderer to write back.
#[derive(Debug, Clone, Default)]
pub struct DashboardState {
    /// Actor name → entry data.
    actors: HashMap<String, DashboardEntry>,
    /// Insertion-order keys — the final tiebreak of the display order, and
    /// the only thing that keeps never-changing rows from shuffling.
    order: Vec<String>,
    /// Cursor position: an index into [`Self::actors`], not a key. A
    /// re-sorted row slides away from under the cursor rather than dragging
    /// it along, which is the less confusing of the two behaviours.
    selected_index: usize,
}

/// How badly broken an actor is, worst first. Ordering the display by this
/// is the whole point: an escalated actor must not sit below forty healthy
/// ones.
fn severity(lifecycle: ActorLifecycle) -> u8 {
    match lifecycle {
        ActorLifecycle::Escalated => 0,
        ActorLifecycle::Crashed => 1,
        ActorLifecycle::Idle => 2,
        ActorLifecycle::Running => 3,
    }
}

/// How loudly a feature's note reads, quietest first.
fn tone_rank(tone: NoteTone) -> u8 {
    match tone {
        NoteTone::Error => 0,
        NoteTone::Warning => 1,
        NoteTone::Muted => 2,
    }
}

impl DashboardState {
    /// Create an empty dashboard state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the cursor's position: an index into [`Self::actors`].
    #[must_use]
    pub fn selected_index(&self) -> usize {
        self.selected_index
    }

    /// Every tracked actor, worst first.
    ///
    /// Sorted by lifecycle severity, then note tone, then insertion order —
    /// so a crashed actor outranks an idle one regardless of when each
    /// appeared, and two rows that never change keep their original order.
    #[must_use]
    pub fn actors(&self) -> Vec<&DashboardEntry> {
        let mut entries: Vec<&DashboardEntry> = self
            .order
            .iter()
            .filter_map(|name| self.actors.get(name))
            .collect();
        // A stable sort over insertion order: the sequence number is
        // implicit in the collector above, so ties resolve to whoever was
        // announced first without a second field to carry it.
        entries.sort_by_key(|entry| (severity(entry.lifecycle), tone_rank(entry.note_tone)));
        entries
    }

    /// The first row index the viewport should show, given its height.
    ///
    /// Cursor-pivoted: the cursor sits at the vertical centre whenever the
    /// list is long enough to scroll, and the window pins to whichever end
    /// runs out first. A list shorter than the viewport pins to the top,
    /// because there is nothing below the last row to scroll to.
    #[must_use]
    pub fn offset_for_viewport(&self, viewport_rows: usize) -> usize {
        let total = self.order.len();
        let max_offset = total.saturating_sub(viewport_rows);
        self.selected_index
            .saturating_sub(viewport_rows / 2)
            .min(max_offset)
    }

    /// Moves the selection to the next actor entry.
    ///
    /// Clamps at the last entry - does nothing if already at the end.
    pub fn select_next(&mut self) {
        if self.selected_index + 1 < self.order.len() {
            self.selected_index += 1;
        }
    }

    /// Moves the selection to the previous actor entry.
    ///
    /// Clamps at the first entry - does nothing if already at the beginning.
    pub fn select_prev(&mut self) {
        self.selected_index = self.selected_index.saturating_sub(1);
    }

    /// Moves the selection to the first actor entry.
    pub fn select_first(&mut self) {
        self.selected_index = 0;
    }

    /// Moves the selection to the last actor entry.
    pub fn select_last(&mut self) {
        self.selected_index = self.order.len().saturating_sub(1);
    }

    /// Resets the grid to empty — no actors, cursor at the top.
    pub fn clear(&mut self) {
        self.actors.clear();
        self.order.clear();
        self.selected_index = 0;
    }

    /// Record that the runtime announced this actor as live.
    ///
    /// If the actor is new it is appended to the display order. Existing
    /// entries keep their description unless a new one is supplied.
    pub fn mark_running<S>(&mut self, name: S, description: Option<String>)
    where
        S: AsRef<str>,
    {
        self.upsert(name, description, ActorLifecycle::Running);
    }

    /// Record that the runtime evicted an actor for idleness.
    ///
    /// The row reads [`ActorLifecycle::Idle`]: the actor is dormant, not
    /// gone, and a partition-set entity passivated on an idle window
    /// returns on the next send to its path. Reporting it as a failure
    /// would tell a reader the actor died when it did exactly what it was
    /// configured to do.
    pub fn mark_idle<S>(&mut self, name: S)
    where
        S: AsRef<str>,
    {
        self.upsert(name, None, ActorLifecycle::Idle);
    }

    /// Record that the runtime reported a terminal failure for this actor.
    ///
    /// The row survives with the failure in its State cell — this is the
    /// one case where keeping a stopped actor on screen is the point,
    /// because there is nobody left to re-announce it and the failure
    /// would otherwise vanish without a trace.
    pub fn mark_failed<S>(&mut self, name: S, lifecycle: ActorLifecycle)
    where
        S: AsRef<str>,
    {
        debug_assert!(
            matches!(
                lifecycle,
                ActorLifecycle::Crashed | ActorLifecycle::Escalated
            ),
            "a sanitized stop must remove its row, not fail it"
        );
        self.upsert(name, None, lifecycle);
    }

    /// Drop an actor's row entirely.
    ///
    /// The sanctioned stops land here. An actor that finished on its own or
    /// was torn down by the shutdown sweep is not a failure, and leaving
    /// its row behind turns every session archive and every subagent
    /// teardown into a permanent red entry — the list grows without bound
    /// and reads as a wall of incidents.
    pub fn remove<S>(&mut self, name: S)
    where
        S: AsRef<str>,
    {
        let name = name.as_ref();
        if self.actors.remove(name).is_some() {
            self.order.retain(|key| key != name);
        }
        self.clamp_cursor();
    }

    /// Update only the free-form status message for an actor, leaving its
    /// lifecycle untouched.
    ///
    /// Creates the entry (as `Running`) if it does not already exist, so a
    /// feature can report a connection status before the runtime's spawn
    /// announcement for its actor arrives.
    pub fn set_status_message<S>(&mut self, name: S, message: Option<String>)
    where
        S: AsRef<str>,
    {
        let name = name.as_ref();
        if !self.actors.contains_key(name) {
            let mut entry = Self::new_entry(name, None, ActorLifecycle::Running);
            entry.status_message = message;
            self.insert_entry(entry);
            return;
        }
        if let Some(entry) = self.actors.get_mut(name) {
            entry.status_message = message;
        }
    }

    /// Update only the row's description, leaving lifecycle and status
    /// message untouched.
    ///
    /// A feature describes its own row independently of whether it also
    /// has a status message this time round: the two are separate optional
    /// fields of the same update and either may arrive alone. Creates the
    /// entry (as `Running`) if the row does not exist yet.
    pub fn set_description<S>(&mut self, name: S, description: Option<String>)
    where
        S: AsRef<str>,
    {
        let name = name.as_ref();
        if !self.actors.contains_key(name) {
            self.insert_entry(Self::new_entry(name, description, ActorLifecycle::Running));
            return;
        }
        if let Some(entry) = self.actors.get_mut(name)
            && description.is_some()
        {
            entry.description = description;
        }
    }

    /// Record how loudly the owning feature's note should read.
    ///
    /// Creates the entry (as `Running`) if the row does not exist yet.
    pub fn set_note_tone<S>(&mut self, name: S, tone: NoteTone)
    where
        S: AsRef<str>,
    {
        let name = name.as_ref();
        if !self.actors.contains_key(name) {
            let mut entry = Self::new_entry(name, None, ActorLifecycle::Running);
            entry.note_tone = tone;
            self.insert_entry(entry);
            return;
        }
        if let Some(entry) = self.actors.get_mut(name) {
            entry.note_tone = tone;
        }
    }

    /// A blank entry for an actor the dashboard has not seen announced.
    fn new_entry(
        name: &str,
        description: Option<String>,
        lifecycle: ActorLifecycle,
    ) -> DashboardEntry {
        DashboardEntry {
            name: name.to_owned(),
            description,
            lifecycle,
            status_message: None,
            note_tone: NoteTone::Muted,
        }
    }

    /// Appends a new entry to the display order, keeping the cursor in
    /// range.
    fn insert_entry(&mut self, entry: DashboardEntry) {
        let name = entry.name.clone();
        self.order.push(name.clone());
        self.actors.insert(name, entry);
        self.clamp_cursor();
    }

    /// Insert-or-update helper applying a new lifecycle and optional
    /// description. Does not touch `status_message` or `note_tone` on
    /// existing entries — those belong to the owning feature, and the
    /// runtime has no opinion about either.
    fn upsert<S>(&mut self, name: S, description: Option<String>, lifecycle: ActorLifecycle)
    where
        S: AsRef<str>,
    {
        let name = name.as_ref();
        if !self.actors.contains_key(name) {
            self.insert_entry(Self::new_entry(name, description, lifecycle));
            return;
        }
        if let Some(entry) = self.actors.get_mut(name) {
            entry.lifecycle = lifecycle;
            if description.is_some() {
                entry.description = description;
            }
        }
        self.clamp_cursor();
    }

    /// Keeps the cursor inside the list after rows are added or removed.
    ///
    /// The cursor is a position, not a key, so a shrinking list leaves it
    /// pointing past the end until this pulls it back onto the last row.
    fn clamp_cursor(&mut self) {
        let last = self.order.len().saturating_sub(1);
        self.selected_index = self.selected_index.min(last);
    }
}

/// The dashboard slice's slot.
///
/// Canonical key shared by the cell catalog (which mints the cell),
/// composition (which activates), the renderer (which resolves a read
/// handle), and tests.
#[must_use]
pub fn dashboard_slot() -> SlotKey {
    SlotKey::builtin("dashboard", "status")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, reason = "test code")]
    use super::*;

    /// The display order as bare names, for order assertions.
    fn names(state: &DashboardState) -> Vec<&str> {
        state
            .actors()
            .into_iter()
            .map(|entry| entry.name.as_str())
            .collect()
    }

    #[rstest::rstest]
    fn escalated_rows_sort_above_crashed_ones() {
        // Given two failed actors, the escalated one announced first.
        let mut state = DashboardState::new();
        state.mark_failed("escalated", ActorLifecycle::Escalated);
        state.mark_failed("crashed", ActorLifecycle::Crashed);

        // When reading the display order.
        let order = names(&state);

        // Then the worse failure comes first, not the earlier one.
        assert_eq!(order, vec!["escalated", "crashed"]);
    }

    #[rstest::rstest]
    fn a_failed_row_sorts_above_an_idle_and_a_running_one() {
        // Given a healthy, a dormant, and a failed actor.
        let mut state = DashboardState::new();
        state.mark_running("healthy", None);
        state.mark_idle("dormant");
        state.mark_failed("broken", ActorLifecycle::Crashed);

        // When reading the display order.
        let order = names(&state);

        // Then severity dominates, whatever the announcement order was.
        assert_eq!(order, vec!["broken", "dormant", "healthy"]);
    }

    /// A feature's note tone reorders rows WITHIN a lifecycle band, never
    /// across it: a cosmetic choice must not outrank a runtime verdict.
    #[rstest::rstest]
    fn note_tone_orders_within_a_lifecycle_band_only() {
        // Given two running actors, one reporting a failure, plus a
        // crashed actor with an ordinary note.
        let mut state = DashboardState::new();
        state.mark_running("plain", None);
        state.mark_running("loud", None);
        state.set_note_tone("loud", NoteTone::Error);
        state.mark_failed("quiet-failure", ActorLifecycle::Crashed);

        // When reading the display order.
        let order = names(&state);

        // Then the crashed row still leads, and the loud note only beats
        // the plain one among the running pair.
        assert_eq!(order, vec!["quiet-failure", "loud", "plain"]);
    }

    #[rstest::rstest]
    fn rows_of_equal_severity_and_tone_keep_announcement_order() {
        // Given four identical running actors announced in a known order.
        let mut state = DashboardState::new();
        for name in ["first", "second", "third", "fourth"] {
            state.mark_running(name, None);
        }

        // When reading the display order.
        let order = names(&state);

        // Then nothing re-sorted: the tiebreak is insertion order.
        assert_eq!(order, vec!["first", "second", "third", "fourth"]);
    }

    #[rstest::rstest]
    fn a_sanitized_stop_removes_the_row() {
        // Given a running actor with a feature note.
        let mut state = DashboardState::new();
        state.mark_running("doomed", None);
        state.set_status_message("doomed", Some("connected".to_owned()));

        // When the row is removed.
        state.remove("doomed");

        // Then it is gone from the list, not merely unlisted.
        assert_eq!(names(&state), [] as [&str; 0]);
    }

    #[rstest::rstest]
    fn removing_a_row_drops_it_from_the_display_order() {
        // Given two actors and the cursor on the second.
        let mut state = DashboardState::new();
        state.mark_running("a", None);
        state.mark_running("b", None);
        state.select_last();
        assert_eq!(state.selected_index(), 1);

        // When the first is removed.
        state.remove("a");

        // Then the survivor takes the cursor's position rather than
        // leaving it pointing past the end.
        assert_eq!(names(&state), vec!["b"]);
        assert_eq!(state.selected_index(), 0);
    }

    #[rstest::rstest]
    fn removing_a_row_the_cursor_is_not_on_keeps_the_cursor_in_range() {
        // Given three actors with the cursor on the last.
        let mut state = DashboardState::new();
        for name in ["a", "b", "c"] {
            state.mark_running(name, None);
        }
        state.select_last();
        assert_eq!(state.selected_index(), 2);

        // When an earlier row is removed.
        state.remove("a");

        // Then the cursor still addresses a real row.
        assert_eq!(state.selected_index(), 1);
        assert_eq!(names(&state), vec!["b", "c"]);
    }

    /// The cursor sits at the viewport's vertical centre, so a reader
    /// always sees what is above and below the row they are on.
    #[rstest::rstest]
    fn the_cursor_lands_at_the_viewport_centre() {
        // Given 40 actors with the cursor in the middle of them.
        let mut state = DashboardState::new();
        for i in 0..40 {
            state.mark_running(format!("actor-{i}"), None);
        }
        state.select_next();
        for _ in 0..19 {
            state.select_next();
        }
        assert_eq!(state.selected_index(), 20);

        // When asking for the offset a 10-row viewport should use.
        let offset = state.offset_for_viewport(10);

        // Then the cursor is drawn five rows down, the middle.
        assert_eq!(offset, 15);
        assert_eq!(20 - offset, 5);
    }

    /// A list shorter than the viewport has nothing to scroll, so the
    /// window pins to the top rather than to a negative offset.
    #[rstest::rstest]
    fn a_list_shorter_than_the_viewport_pins_to_the_top() {
        // Given four actors in a ten-row viewport.
        let mut state = DashboardState::new();
        for i in 0..4 {
            state.mark_running(format!("actor-{i}"), None);
        }
        state.select_last();
        assert_eq!(state.selected_index(), 3);

        // When asking for the offset.
        let offset = state.offset_for_viewport(10);

        // Then it is zero — the whole list fits.
        assert_eq!(offset, 0);
    }

    /// The end of a long list pins to the last window, so the cursor never
    /// floats in the middle with three empty rows beneath it.
    #[rstest::rstest]
    fn the_end_of_a_long_list_pins_to_the_bottom() {
        // Given 40 actors with the cursor on the last.
        let mut state = DashboardState::new();
        for i in 0..40 {
            state.mark_running(format!("actor-{i}"), None);
        }
        state.select_last();

        // When asking for the offset a 10-row viewport should use.
        let offset = state.offset_for_viewport(10);

        // Then the window ends on the last row.
        assert_eq!(offset, 30);
    }

    /// An empty dashboard has no offset to derive, and must not underflow
    /// into a huge number.
    #[rstest::rstest]
    fn an_empty_dashboard_offsets_to_zero() {
        // Given an empty dashboard.
        let state = DashboardState::new();

        // When asking for the offset in any viewport.
        let offset = state.offset_for_viewport(10);

        // Then it is zero.
        assert_eq!(offset, 0);
    }

    /// A note-first publish creates a row, and the runtime's own
    /// announcement is what gives it a lifecycle — the seed must render as
    /// a real state word rather than nothing.
    #[rstest::rstest]
    fn a_note_published_before_the_announcement_seeds_a_running_row() {
        // Given an empty dashboard.
        let mut state = DashboardState::new();

        // When a feature publishes only a note.
        state.set_status_message("early", Some("connecting".to_owned()));

        // Then the row exists and reads Running.
        let entry = state.actors().first().copied().expect("a row");
        assert_eq!(entry.lifecycle, ActorLifecycle::Running);
        assert_eq!(entry.status_message.as_deref(), Some("connecting"));
    }

    /// A failed row keeps whatever the owning feature last said, because
    /// the feature's note is the only column that carries detail.
    #[rstest::rstest]
    fn a_failure_preserves_the_features_note() {
        // Given a running actor carrying a feature note.
        let mut state = DashboardState::new();
        state.mark_running("worker", None);
        state.set_status_message("worker", Some("3 urls verified".to_owned()));

        // When the runtime reports a crash.
        state.mark_failed("worker", ActorLifecycle::Crashed);

        // Then the note survives the failure.
        let entry = state.actors().first().copied().expect("a row");
        assert_eq!(entry.lifecycle, ActorLifecycle::Crashed);
        assert_eq!(entry.status_message.as_deref(), Some("3 urls verified"));
    }

    /// A recovery clears the failure: the row is live again, and a stale
    /// verdict would misreport the present.
    #[rstest::rstest]
    fn a_respawn_clears_a_prior_failure() {
        // Given a crashed actor.
        let mut state = DashboardState::new();
        state.mark_failed("flaky", ActorLifecycle::Crashed);

        // When the runtime re-announces it live.
        state.mark_running("flaky", None);

        // Then the row reads Running again.
        let entry = state.actors().first().copied().expect("a row");
        assert_eq!(entry.lifecycle, ActorLifecycle::Running);
    }
}
