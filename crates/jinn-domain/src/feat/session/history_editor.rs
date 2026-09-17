//! History editor - the sole writer of chat history.
//!
//! All mutations of a session's history vector route through this module. The
//! editor treats each assistant tool-call/result loop as an atomic
//! [chunk](Chunk): entry-id-keyed operations expand to the whole chunk, so the
//! provider-level invariant (an assistant that declares tool calls is followed
//! by exactly those results; a tool result always resolves a preceding call)
//! can never be broken by a mutation.
//!
//! Precedence for exclusion is **pin > user > worker**: a pinned or
//! user-force-included chunk member blocks worker exclusion, and a pinned
//! member blocks user exclusion. A [`ChangeSource::Internal`] sweep (the
//! dangling-call sweep of last resort) bypasses the pin guard.
//!
//! The trailing loop may legally be incomplete while streaming (calls without
//! results, pending results). The editor never validates the tail; assembly
//! only fires at turn boundaries where the loop has resolved.

use std::ops::Range;

use crate::feat::session::chat_entry::{
    ChangeSource, ChatEntry, ChatEntryId, ChatEntryKind, ContextOverride, PinPosition,
};
use crate::feat::session::chat_session::ChatSessionState;
use crate::feat::session::history_mutation::HistoryMutation;

/// Write access to a session's history.
///
/// Obtained via [`ChatSessionState::edit_history`]. Every method that changes
/// history lives here; reads stay on the session itself.
pub struct HistoryEditor<'a> {
    session: &'a mut ChatSessionState,
}

/// A contiguous span of history entries mutated as one unit.
///
/// A span longer than one entry is an assistant tool loop; a single-entry
/// span is a standalone entry.
#[derive(Debug, Clone)]
pub(crate) struct Chunk {
    /// Index range into the history vector.
    pub range: Range<usize>,
}

/// The end of a mutation attempt: what changed, or why nothing did.
#[derive(Debug)]
enum MutationOutcome {
    /// The chunk members whose override/pin state changed.
    Changed(Vec<ChatEntryId>),
    /// Nothing changed; a precedence guard refused the operation.
    Refused(&'static str),
    /// Nothing changed; the value already matched every member.
    Noop,
}

impl<'a> HistoryEditor<'a> {
    /// Creates an editor over the session. Call `ChatSessionState::edit_history` instead.
    pub(crate) fn new(session: &'a mut ChatSessionState) -> Self {
        Self { session }
    }

    /// Appends an entry at the tail. The trailing loop may be incomplete.
    ///
    /// Applies user-entry token expansion and the cursor/scroll bookkeeping of
    /// the old `push_entry`. Returns the new entry's history index.
    pub fn append(&mut self, mut entry: ChatEntry) -> usize {
        self.session.push_entry_raw(&mut entry)
    }

    /// Runs `f` on the entry at `index` in place. Returns `None` when out of
    /// bounds.
    ///
    /// For streaming lifecycle writes (token appends, timing finalizers,
    /// result finalization) that mutate entries in place. In-place writes can
    /// never reorder entries or split a tool loop, so no chunk logic applies.
    pub fn with_entry_at_mut<R, F>(&mut self, index: usize, f: F) -> Option<R>
    where
        F: FnOnce(&mut ChatEntry) -> R,
    {
        self.session.history_get_mut(index).map(f)
    }

    /// Runs `f` on the last entry matching `predicate`, in place. Returns
    /// `f`'s output when one matched, `None` otherwise.
    ///
    /// For streaming-lifecycle finalizers that resolve an entry by scanning
    /// recent history (e.g. finalize a tool call by id).
    pub fn with_last_matching_mut<P, R, F>(&mut self, predicate: P, f: F) -> Option<R>
    where
        P: Fn(&ChatEntry) -> bool,
        F: FnOnce(&mut ChatEntry) -> R,
    {
        let history = self.session.history();
        let index = history.iter().rposition(predicate)?;
        self.with_entry_at_mut(index, f)
    }

    /// Inserts a standalone entry after `after` (or at the head when `None`).
    ///
    /// The insertion point must be a chunk boundary: never strictly inside a
    /// loop, between its assistant and last result. A mid-loop request is
    /// warned about and advanced past the loop's last result. An unknown
    /// `after` id skips the insert. Returns the inserted index, or `None`
    /// when skipped.
    pub fn insert_standalone_after(
        &mut self,
        after: Option<&ChatEntryId>,
        entry: ChatEntry,
    ) -> Option<usize> {
        let boundary = self.resolve_boundary(after)?;
        Some(self.session.insert_entry_at(boundary, entry))
    }

    /// Sets the context override for the chunk containing `id`, expanding to
    /// all members with precedence guards.
    ///
    /// Returns the ids whose override actually changed (empty when refused).
    pub fn set_context(
        &mut self,
        id: &ChatEntryId,
        value: ContextOverride,
        source: &ChangeSource,
    ) -> Vec<ChatEntryId> {
        match self.mutate_chunk(id, value, source) {
            MutationOutcome::Changed(ids) => ids,
            MutationOutcome::Refused(reason) => {
                tracing::debug!(entry_id = %id, reason, "context override refused");
                Vec::new()
            }
            MutationOutcome::Noop => Vec::new(),
        }
    }

    /// Pins the chunk containing `id` at `position`.
    ///
    /// Every member receives the pin, mirroring the ToolResult kind-level pin.
    /// Returns the ids whose pin actually changed.
    pub fn pin(&mut self, id: &ChatEntryId, position: PinPosition) -> Vec<ChatEntryId> {
        self.apply_chunk_pins(id, Some(position))
    }

    /// Removes the pin from the chunk containing `id`.
    ///
    /// Returns the ids whose pin actually changed.
    pub fn unpin(&mut self, id: &ChatEntryId) -> Vec<ChatEntryId> {
        self.apply_chunk_pins(id, None)
    }

    /// Applies a batch of [`HistoryMutation`]s in order.
    ///
    /// The executor for worker/UI intent: `SetContextOverride`, `PinEntry`,
    /// and `UnpinEntry` go through the chunk operations above; `InsertEntry`
    /// goes through [`Self::insert_standalone_after`]. Returns the ids whose
    /// override or pin state actually changed (the existing `apply_mutations`
    /// contract, driving `ContextOverrideChanged` events).
    pub fn apply(&mut self, mutations: Vec<HistoryMutation>) -> Vec<ChatEntryId> {
        let mut changed = Vec::new();
        for mutation in mutations {
            match mutation {
                HistoryMutation::SetContextOverride {
                    entry_id,
                    value,
                    source,
                } => changed.extend(self.set_context(&entry_id, value, &source)),
                HistoryMutation::InsertEntry {
                    after_entry_id,
                    entry,
                } => {
                    self.insert_standalone_after(after_entry_id.as_ref(), entry);
                }
                HistoryMutation::PinEntry { entry_id, position } => {
                    changed.extend(self.pin(&entry_id, position));
                }
                HistoryMutation::UnpinEntry { entry_id } => {
                    changed.extend(self.unpin(&entry_id));
                }
            }
        }
        changed
    }

    /// Removes entries at `indices`, descending so earlier indices stay valid.
    ///
    /// Only valid for trailing streaming entries (stall-retry cleanup). Returns
    /// the number of entries removed.
    pub fn remove_trailing(&mut self, indices: &[usize]) -> usize {
        let mut sorted = indices.to_vec();
        sorted.sort_unstable_by(|a, b| b.cmp(a));
        sorted.dedup();
        let mut removed = 0;
        for index in sorted {
            if self.session.remove_history_entry_at(index) {
                removed += 1;
            }
        }
        removed
    }

    /// Relocates interstitial entries (System/Actor/Thinking/Transient/
    /// Annotation) that sit strictly inside a loop to immediately after the
    /// loop's last result, preserving their relative order.
    ///
    /// Idempotent. Called at every assembly entry point so committed loops
    /// never contain interstitials and the read-side converter can stay
    /// simple.
    pub fn normalize_loop_layout(&mut self) {
        let mut index = 0;
        while index < self.session.history().len() {
            match self.normalize_step(index) {
                Some(next) => index = next,
                None => index += 1,
            }
        }
    }

    /// Relocates interstitials inside the loop opening at `index`; returns
    /// the loop's end, or `None` when `index` does not open a loop.
    fn normalize_step(&mut self, index: usize) -> Option<usize> {
        let (group_end, interstitials) = self.interior_interstitials(index)?;
        if interstitials.is_empty() {
            return Some(group_end);
        }
        // Collect in ascending source order so re-insertion preserves
        // relative order; removal runs descending so indices stay valid.
        // Indices are valid by construction (same history as the scan).
        let moved: Vec<ChatEntry> = {
            let history = self.session.history();
            interstitials
                .iter()
                .rev()
                .filter_map(|&i| history.get(i).cloned())
                .collect()
        };
        self.remove_entries_at(&interstitials);
        // Removing `count` interior entries pulls the loop's end down to
        // `group_end - count`; the interstitials re-insert there, directly
        // after the last result, in their original relative order.
        let insert_at = group_end - interstitials.len();
        self.insert_entries_at(insert_at, moved);
        Some(insert_at + interstitials.len())
    }

    /// The loop end and interior interstitial indices (descending) of the
    /// loop at `index`.
    ///
    /// `None` when the entry at `index` does not open a loop. Interior means
    /// strictly between the loop's last tool call and the loop end.
    #[expect(
        clippy::indexing_slicing,
        reason = "group bounds come from tool_group_end over the same history"
    )]
    fn interior_interstitials(&self, index: usize) -> Option<(usize, Vec<usize>)> {
        let history = self.session.history();
        let group_end = tool_group_end(history, index)?;
        let group = &history[index..group_end];
        let last_call = group
            .iter()
            .rposition(|entry| matches!(entry.kind, ChatEntryKind::ToolCall { .. }))?;
        let mut interstitials = group[last_call + 1..]
            .iter()
            .enumerate()
            .filter(|(_, entry)| is_tool_loop_interstitial(entry))
            .map(|(offset, _)| index + last_call + 1 + offset)
            .collect::<Vec<_>>();
        // Descending so removals never invalidate later indices.
        interstitials.sort_unstable_by(|a, b| b.cmp(a));
        Some((group_end, interstitials))
    }

    /// Removes the entries at `indices` (descending). Returns the count.
    fn remove_entries_at(&mut self, indices: &[usize]) -> usize {
        indices
            .iter()
            .filter(|&&i| self.session.remove_history_entry_at(i))
            .count()
    }

    /// Inserts `entries` starting at `at`, preserving their order.
    fn insert_entries_at(&mut self, at: usize, entries: Vec<ChatEntry>) {
        for (offset, entry) in entries.into_iter().enumerate() {
            self.session.insert_entry_at(at + offset, entry);
        }
    }

    /// Excludes every incomplete loop (calls without completed results) as a
    /// chunk, bypassing the pin guard.
    ///
    /// The dangling sweep of last resort (hard cancel). Preserves entries for
    /// display. Returns the ids whose override changed.
    pub fn exclude_incomplete_trailing_loops(&mut self) -> Vec<ChatEntryId> {
        let incomplete: Vec<ChatEntryId> = {
            let history = self.session.history();
            let mut ids = Vec::new();
            let mut index = 0;
            while index < history.len() {
                match tool_group_end(history, index) {
                    Some(end) => {
                        // Loop bounds come from tool_group_end over this same
                        // history, so the slice is in-bounds by construction.
                        if let Some(group) = history.get(index..end)
                            && !loop_is_complete(group)
                        {
                            ids.extend(group.iter().map(|entry| entry.id.clone()));
                        }
                        index = end;
                    }
                    None => index += 1,
                }
            }
            ids
        };
        let mut changed = Vec::new();
        for id in incomplete {
            if let Some(id) = self.force_exclude(&id) {
                changed.push(id);
            }
        }
        changed
    }

    /// Applies ForcedExclude to a single member, bypassing precedence guards.
    fn force_exclude(&mut self, id: &ChatEntryId) -> Option<ChatEntryId> {
        self.session
            .with_history_entry_mut(id, |entry| {
                let changed = entry.context_override() != ContextOverride::ForcedExclude;
                if changed {
                    entry.apply_context_override(
                        ContextOverride::ForcedExclude,
                        ChangeSource::Internal {
                            label: "dangling_tool_call_sweep".into(),
                        },
                    );
                }
                changed.then(|| id.clone())
            })
            .flatten()
    }

    /// Sets the override on every member of the chunk containing `id`.
    fn mutate_chunk(
        &mut self,
        id: &ChatEntryId,
        value: ContextOverride,
        source: &ChangeSource,
    ) -> MutationOutcome {
        let chunk = self.chunk_for(id);
        let guard = evaluate_exclusion_guard(self.session.history(), &chunk, value, source);
        if let Err(reason) = guard {
            return MutationOutcome::Refused(reason);
        }
        let members: Vec<ChatEntryId> = {
            // The chunk range comes from chunking this same history.
            self.session
                .history()
                .get(chunk.range.clone())
                .into_iter()
                .flatten()
                .map(|entry| entry.id.clone())
                .collect()
        };
        self.apply_override_members(&members, value, source)
    }

    /// Applies one override value to a fixed list of member ids.
    ///
    /// A `ForcedInclude` member is never overwritten by `ForcedExclude` — the
    /// include sticks (the legacy executor's guard, preserved chunk-wide).
    fn apply_override_members(
        &mut self,
        members: &[ChatEntryId],
        value: ContextOverride,
        source: &ChangeSource,
    ) -> MutationOutcome {
        let mut changed = Vec::new();
        for member in members {
            let applied = self
                .session
                .with_history_entry_mut(member, |entry| {
                    // A worker/internal ForcedInclude sticks against a later
                    // ForcedExclude; the user's `x` key may always flip it.
                    let protected_include = entry.context_override()
                        == ContextOverride::ForcedInclude
                        && value == ContextOverride::ForcedExclude
                        && !matches!(source, ChangeSource::User);
                    if protected_include {
                        return false;
                    }
                    let was = entry.context_override() != value;
                    if was {
                        entry.apply_context_override(value, source.clone());
                    }
                    was
                })
                .unwrap_or(false);
            if applied {
                changed.push(member.clone());
            }
        }
        if changed.is_empty() {
            MutationOutcome::Noop
        } else {
            MutationOutcome::Changed(changed)
        }
    }

    /// Applies `position` (or clears pins) on every member of the chunk.
    fn apply_chunk_pins(
        &mut self,
        id: &ChatEntryId,
        position: Option<PinPosition>,
    ) -> Vec<ChatEntryId> {
        let chunk = self.chunk_for(id);
        // The chunk range comes from chunking this same history.
        let members: Vec<ChatEntryId> = self
            .session
            .history()
            .get(chunk.range.clone())
            .into_iter()
            .flatten()
            .map(|entry| entry.id.clone())
            .collect();
        let mut changed = Vec::new();
        for member in members {
            if self
                .session
                .with_history_entry_mut(&member, |entry| {
                    let was = entry.pin_position != position;
                    if was {
                        entry.pin_position = position;
                        if let ChatEntryKind::ToolResult {
                            pin_position: kind_pin,
                            ..
                        } = &mut entry.kind
                        {
                            *kind_pin = position;
                        }
                    }
                    was
                })
                .unwrap_or(false)
            {
                changed.push(member);
            }
        }
        changed
    }

    /// Resolves the insertion index for `after` at a chunk boundary.
    ///
    /// `None` when `after` names an entry that does not exist (the insert is
    /// skipped, matching the legacy executor's behavior).
    fn resolve_boundary(&self, after: Option<&ChatEntryId>) -> Option<usize> {
        let Some(id) = after else {
            return Some(0);
        };
        let history = self.session.history();
        let index = history.iter().position(|entry| &entry.id == id)?;
        match tool_group_end(history, index) {
            // `id` opened a loop that continues past itself: the boundary is
            // the loop's end, not right after `id`.
            Some(end) if end > index + 1 => Some(end),
            // `id` is standalone, a plain assistant, or a loop member whose
            // loop ended at `id` itself: insert directly after it.
            _ => Some(index + 1),
        }
    }

    /// Locates the chunk containing `id`. Unknown ids resolve to a standalone
    /// chunk that matches nothing (a no-op mutation).
    fn chunk_for(&self, id: &ChatEntryId) -> Chunk {
        let history = self.session.history();
        let Some(index) = history.iter().position(|entry| &entry.id == id) else {
            tracing::warn!(entry_id = %id, "history editor: unknown entry id");
            return Chunk { range: 0..0 };
        };
        chunk_containing(&chunking(history), index).unwrap_or(Chunk {
            range: index..index + 1,
        })
    }
}

/// Precedence evaluation for a chunk-wide override application.
///
/// `Ok(())` when the operation may proceed; `Err(reason)` when a guard
/// refuses it.
type ExclusionGuard = Result<(), &'static str>;

/// Evaluates chunk precedence for setting `value` with `source`.
fn evaluate_exclusion_guard(
    history: &[ChatEntry],
    chunk: &Chunk,
    value: ContextOverride,
    source: &ChangeSource,
) -> ExclusionGuard {
    let Some(members) = history.get(chunk.range.clone()) else {
        return Ok(());
    };
    let pinned = members.iter().any(ChatEntry::is_pinned);
    let user_included = members.iter().any(is_user_forced_include);
    match (&value, source) {
        // Worker exclusion cannot remove a pinned or user-included chunk.
        (ContextOverride::ForcedExclude, ChangeSource::Worker { .. })
            if pinned || user_included =>
        {
            Err("worker exclude refused: pin or user include wins")
        }
        // Worker inclusion cannot re-include a user-excluded chunk.
        (ContextOverride::ForcedInclude, ChangeSource::Worker { .. })
            if members.iter().any(ChatEntry::is_user_force_excluded) =>
        {
            Err("worker include refused: user exclude wins")
        }
        // User exclusion cannot remove a pinned chunk.
        (ContextOverride::ForcedExclude, ChangeSource::User)
            if pinned && !members.iter().any(ChatEntry::is_user_force_excluded) =>
        {
            Err("user exclude refused: pin wins")
        }
        // Everything else — including internal sweeps, which bypass guards
        // as the last resort before the tripwire.
        _ => Ok(()),
    }
}

/// Whether the most recent user-initiated override event forced inclusion.
fn is_user_forced_include(entry: &ChatEntry) -> bool {
    matches!(entry.context_history.last(), Some(event) if event.to == ContextOverride::ForcedInclude
        && matches!(event.source, ChangeSource::User))
}

/// Returns the end of a contiguous tool loop beginning at `index`.
///
/// A loop is an assistant entry, one or more tool calls, optional interior
/// interstitials, and their results. Returns `None` when `index` does not open
/// a loop (anything that is not an assistant followed by tool calls).
pub(crate) fn tool_group_end(history: &[ChatEntry], index: usize) -> Option<usize> {
    if !matches!(history.get(index)?.kind, ChatEntryKind::Assistant(_)) {
        return None;
    }
    let mut end = index + 1;
    let call_start = end;
    while matches!(
        history.get(end).map(|entry| &entry.kind),
        Some(ChatEntryKind::ToolCall { .. })
    ) {
        end += 1;
    }
    if end == call_start {
        return None;
    }

    // Display-only/interstitial entries can occur while a tool batch is being
    // persisted. They do not break the provider-level tool relationship.
    while history.get(end).is_some_and(is_tool_loop_interstitial) {
        end += 1;
    }
    while matches!(
        history.get(end).map(|entry| &entry.kind),
        Some(ChatEntryKind::ToolResult { .. })
    ) {
        end += 1;
    }
    Some(end)
}

/// Whether an entry may sit between a loop's calls and results without
/// breaking the provider-level relationship.
pub(crate) fn is_tool_loop_interstitial(entry: &ChatEntry) -> bool {
    matches!(
        entry.kind,
        ChatEntryKind::System(_)
            | ChatEntryKind::Actor { .. }
            | ChatEntryKind::Thinking(_)
            | ChatEntryKind::Transient(_)
            | ChatEntryKind::Annotation { .. }
    )
}

/// Whether every tool call in a loop group has a completed matching result.
fn loop_is_complete(group: &[ChatEntry]) -> bool {
    let mut pending: Vec<&str> = group
        .iter()
        .filter_map(|entry| match &entry.kind {
            ChatEntryKind::ToolCall { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    for entry in group {
        if let ChatEntryKind::ToolResult { id, status, .. } = &entry.kind
            && *status != crate::feat::session::tool_result_status::ToolResultStatus::Pending
        {
            pending.retain(|call_id| *call_id != id.as_str());
        }
    }
    pending.is_empty()
}

/// Splits a history into chunks: tool loops (assistant + calls + results)
/// and single standalone entries, in order.
pub(crate) fn chunking(history: &[ChatEntry]) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    let mut index = 0;
    while index < history.len() {
        let end = tool_group_end(history, index).unwrap_or(index + 1);
        chunks.push(Chunk { range: index..end });
        index = end;
    }
    chunks
}

/// The chunk containing history index `index`, if any.
pub(crate) fn chunk_containing(chunks: &[Chunk], index: usize) -> Option<Chunk> {
    chunks
        .iter()
        .find(|chunk| chunk.range.contains(&index))
        .cloned()
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
    use crate::feat::provider::llm_message::LlmMessage;
    use crate::feat::session::chat_entry::PinPosition;
    use crate::feat::session::tool_result_status::ToolResultStatus;

    /// A complete loop: empty assistant, one call, one result.
    fn simple_loop() -> Vec<ChatEntry> {
        vec![
            ChatEntry::user("run it"),
            ChatEntry::assistant(""),
            ChatEntry::tool_call("call-1", "bash", "{}"),
            ChatEntry::tool_result("call-1", "bash", "ok", ToolResultStatus::Success),
        ]
    }

    fn worker_source() -> ChangeSource {
        ChangeSource::Worker {
            name: "test-worker".to_owned(),
        }
    }

    fn session_with(entries: Vec<ChatEntry>) -> ChatSessionState {
        let mut session = ChatSessionState::new();
        for entry in entries {
            session.edit_history().append(entry);
        }
        session
    }

    fn entry_ids(session: &ChatSessionState) -> Vec<ChatEntryId> {
        session.history().iter().map(|e| e.id.clone()).collect()
    }

    #[rstest::rstest]
    #[test]
    fn set_context_on_tool_call_excludes_whole_loop() {
        // Given a complete loop in history.
        let mut session = session_with(simple_loop());
        let ids = entry_ids(&session);
        let call_id = ids[2].clone();

        // When excluding the call as a worker.
        let changed = session.edit_history().set_context(
            &call_id,
            ContextOverride::ForcedExclude,
            &ChangeSource::Worker {
                name: "test".into(),
            },
        );

        // Then every loop member (assistant, call, result) changed; the user
        // entry did not.
        assert_eq!(
            changed.len(),
            3,
            "assistant+call+result excluded: {changed:?}"
        );
        assert!(!changed.contains(&ids[0]));
        assert!(
            session.history()[1..4]
                .iter()
                .all(|e| e.context_override() == ContextOverride::ForcedExclude)
        );
    }

    #[rstest::rstest]
    #[test]
    fn worker_exclude_refused_for_pinned_member() {
        // Given a loop whose result is pinned (skill-load shape).
        let mut session = session_with(simple_loop());
        let ids = entry_ids(&session);
        session.core.history[3].pin_position = Some(PinPosition::Relative);

        // When a worker excludes the call.
        let changed = session.edit_history().set_context(
            &ids[2],
            ContextOverride::ForcedExclude,
            &ChangeSource::Worker {
                name: "test".into(),
            },
        );

        // Then nothing changed (pin wins).
        assert!(changed.is_empty());
        assert!(
            session.history()[1..4]
                .iter()
                .all(|e| e.context_override() == ContextOverride::Default)
        );
    }

    #[rstest::rstest]
    #[test]
    fn worker_include_refused_for_user_excluded_chunk() {
        // Given a loop the user excluded.
        let mut session = session_with(simple_loop());
        let ids = entry_ids(&session);
        session.edit_history().set_context(
            &ids[2],
            ContextOverride::ForcedExclude,
            &ChangeSource::User,
        );

        // When a worker tries to re-include it.
        let changed = session.edit_history().set_context(
            &ids[2],
            ContextOverride::ForcedInclude,
            &ChangeSource::Worker {
                name: "test".into(),
            },
        );

        // Then nothing changed (user exclude wins).
        assert!(changed.is_empty());
        assert_eq!(
            session.history()[1].context_override(),
            ContextOverride::ForcedExclude
        );
    }

    #[rstest::rstest]
    #[test]
    fn user_exclude_refused_for_pinned_chunk() {
        // Given a pinned loop.
        let mut session = session_with(simple_loop());
        let ids = entry_ids(&session);
        session.core.history[1].pin_position = Some(PinPosition::Relative);

        // When the user excludes a member.
        let changed = session.edit_history().set_context(
            &ids[2],
            ContextOverride::ForcedExclude,
            &ChangeSource::User,
        );

        // Then nothing changed (pin beats user exclude).
        assert!(changed.is_empty());
        assert!(
            session.history()[1..4]
                .iter()
                .all(|e| e.context_override() == ContextOverride::Default)
        );
    }

    #[rstest::rstest]
    #[test]
    fn internal_exclude_bypasses_pin_guard() {
        // Given a pinned but incomplete loop (hard-cancel shape).
        let mut session = session_with(vec![
            ChatEntry::user("go"),
            ChatEntry::assistant(""),
            ChatEntry::tool_call("dangling", "bash", "{}"),
        ]);
        session.core.history[1].pin_position = Some(PinPosition::Relative);

        // When the internal dangling sweep runs.
        let changed = session.edit_history().exclude_incomplete_trailing_loops();

        // Then the incomplete loop is excluded despite the pin.
        assert_eq!(changed.len(), 2, "assistant+call excluded: {changed:?}");
        assert_eq!(
            session.history()[1].context_override(),
            ContextOverride::ForcedExclude
        );
        assert_eq!(
            session.history()[2].context_override(),
            ContextOverride::ForcedExclude
        );
    }

    #[rstest::rstest]
    #[test]
    fn pin_on_result_pins_whole_loop() {
        // Given a complete loop.
        let mut session = session_with(simple_loop());
        let ids = entry_ids(&session);

        // When pinning the result.
        let changed = session.edit_history().pin(&ids[3], PinPosition::Relative);

        // Then all three loop members are pinned.
        assert_eq!(changed.len(), 3);
        assert!(
            session.history()[1..4]
                .iter()
                .all(|e| e.pin_position == Some(PinPosition::Relative))
        );
    }

    #[rstest::rstest]
    #[test]
    fn unpin_clears_kind_level_pin_mirror() {
        // Given a loop pinned via the result.
        let mut session = session_with(simple_loop());
        let ids = entry_ids(&session);
        session.edit_history().pin(&ids[3], PinPosition::Relative);

        // When unpinning.
        session.edit_history().unpin(&ids[3]);

        // Then both the entry-level and kind-level pins are cleared.
        assert!(
            session.history()[1..4]
                .iter()
                .all(|e| e.pin_position.is_none())
        );
        assert!(matches!(
            &session.history()[3].kind,
            ChatEntryKind::ToolResult { pin_position, .. } if pin_position.is_none()
        ));
    }

    #[rstest::rstest]
    #[test]
    fn insert_standalone_after_advances_past_loop() {
        // Given a complete loop followed by a user entry.
        let mut session = session_with(vec![
            ChatEntry::user("go"),
            ChatEntry::assistant(""),
            ChatEntry::tool_call("call-1", "bash", "{}"),
            ChatEntry::tool_result("call-1", "bash", "ok", ToolResultStatus::Success),
            ChatEntry::user("done"),
        ]);
        let ids = entry_ids(&session);

        // When inserting after the assistant that opened the loop.
        let steer = ChatEntry::user("steer");
        session
            .edit_history()
            .insert_standalone_after(Some(&ids[1]), steer);

        // Then the insertion landed after the loop's last result, not
        // between the call and its result.
        let kinds: Vec<&str> = session
            .history()
            .iter()
            .map(|e| match &e.kind {
                ChatEntryKind::User { .. } => "user",
                ChatEntryKind::Assistant(_) => "assistant",
                ChatEntryKind::ToolCall { .. } => "call",
                ChatEntryKind::ToolResult { .. } => "result",
                _ => "other",
            })
            .collect();
        assert_eq!(
            kinds,
            vec!["user", "assistant", "call", "result", "user", "user"]
        );
    }

    #[rstest::rstest]
    #[test]
    fn insert_standalone_after_plain_entry_inserts_directly() {
        // Given two user entries.
        let mut session = session_with(vec![ChatEntry::user("a"), ChatEntry::user("b")]);
        let ids = entry_ids(&session);

        // When inserting after the first.
        session
            .edit_history()
            .insert_standalone_after(Some(&ids[0]), ChatEntry::user("between"));

        // Then the order is a, between, b.
        let texts: Vec<&str> = session
            .history()
            .iter()
            .filter_map(|e| match &e.kind {
                ChatEntryKind::User { expanded, .. } => Some(expanded.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["a", "between", "b"]);
    }

    #[rstest::rstest]
    #[test]
    fn normalize_relocates_interior_interstitials_after_loop() {
        // Given a loop with a system entry between call and result.
        let mut session = session_with(vec![
            ChatEntry::user("go"),
            ChatEntry::assistant(""),
            ChatEntry::tool_call("call-1", "bash", "{}"),
            ChatEntry::system("status mid-loop"),
            ChatEntry::tool_result("call-1", "bash", "ok", ToolResultStatus::Success),
            ChatEntry::user("done"),
        ]);

        // When normalizing.
        session.edit_history().normalize_loop_layout();

        // Then the system entry moved after the result.
        let kinds: Vec<&str> = session
            .history()
            .iter()
            .map(|e| match &e.kind {
                ChatEntryKind::User { .. } => "user",
                ChatEntryKind::Assistant(_) => "assistant",
                ChatEntryKind::ToolCall { .. } => "call",
                ChatEntryKind::ToolResult { .. } => "result",
                ChatEntryKind::System(_) => "system",
                _ => "other",
            })
            .collect();
        assert_eq!(
            kinds,
            vec!["user", "assistant", "call", "result", "system", "user"]
        );
    }

    #[rstest::rstest]
    #[test]
    fn normalize_is_idempotent() {
        // Given an already-normalized history with an interstitial after a loop.
        let mut session = session_with(vec![
            ChatEntry::user("go"),
            ChatEntry::assistant(""),
            ChatEntry::tool_call("call-1", "bash", "{}"),
            ChatEntry::tool_result("call-1", "bash", "ok", ToolResultStatus::Success),
            ChatEntry::system("already outside"),
            ChatEntry::user("done"),
        ]);
        session.edit_history().normalize_loop_layout();
        let before: Vec<ChatEntryId> = entry_ids(&session);

        // When normalizing again.
        session.edit_history().normalize_loop_layout();

        // Then the order is unchanged.
        assert_eq!(before, entry_ids(&session));
    }

    #[rstest::rstest]
    #[test]
    fn remove_trailing_removes_descending() {
        // Given a session with five entries.
        let mut session = session_with(vec![
            ChatEntry::user("a"),
            ChatEntry::user("b"),
            ChatEntry::user("c"),
            ChatEntry::user("d"),
            ChatEntry::user("e"),
        ]);

        // When removing indices 2 and 4.
        let removed = session.edit_history().remove_trailing(&[2, 4]);

        // Then both are gone, order otherwise preserved.
        assert_eq!(removed, 2);
        let texts: Vec<&str> = session
            .history()
            .iter()
            .filter_map(|e| match &e.kind {
                ChatEntryKind::User { expanded, .. } => Some(expanded.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["a", "b", "d"]);
    }

    #[rstest::rstest]
    #[test]
    fn exclude_incomplete_loops_leaves_complete_loops() {
        // Given one complete loop and one incomplete loop.
        let mut session = session_with(vec![
            ChatEntry::user("go"),
            ChatEntry::assistant(""),
            ChatEntry::tool_call("call-1", "bash", "{}"),
            ChatEntry::tool_result("call-1", "bash", "ok", ToolResultStatus::Success),
            ChatEntry::assistant(""),
            ChatEntry::tool_call("call-2", "bash", "{}"),
        ]);

        // When the dangling sweep runs.
        let changed = session.edit_history().exclude_incomplete_trailing_loops();

        // Then only the incomplete loop's members changed.
        assert_eq!(changed.len(), 2);
        assert_eq!(
            session.history()[2].context_override(),
            ContextOverride::Default
        );
        assert_eq!(
            session.history()[4].context_override(),
            ContextOverride::ForcedExclude
        );
        assert_eq!(
            session.history()[5].context_override(),
            ContextOverride::ForcedExclude
        );
    }

    #[rstest::rstest]
    #[test]
    fn apply_executes_mutation_batch() {
        // Given a complete loop.
        let mut session = session_with(simple_loop());
        let ids = entry_ids(&session);

        // When applying a batch: worker exclude on the call.
        let changed = session
            .edit_history()
            .apply(vec![HistoryMutation::SetContextOverride {
                entry_id: ids[2].clone(),
                value: ContextOverride::ForcedExclude,
                source: ChangeSource::Worker { name: "w".into() },
            }]);

        // Then the whole loop changed.
        assert_eq!(changed.len(), 3);
    }

    #[rstest::rstest]
    #[test]
    fn normalize_preserves_relative_order_of_multiple_interstitials() {
        // Given a loop with two interior interstitials.
        let mut session = session_with(vec![
            ChatEntry::assistant(""),
            ChatEntry::tool_call("call-1", "bash", "{}"),
            ChatEntry::system("first status"),
            ChatEntry::thinking("second thought"),
            ChatEntry::tool_result("call-1", "bash", "ok", ToolResultStatus::Success),
            ChatEntry::user("done"),
        ]);

        // When normalizing.
        session.edit_history().normalize_loop_layout();

        // Then both interstitials follow the result, in original order.
        let kinds: Vec<&str> = session
            .history()
            .iter()
            .map(|e| match &e.kind {
                ChatEntryKind::User { .. } => "user",
                ChatEntryKind::Assistant(_) => "assistant",
                ChatEntryKind::ToolCall { .. } => "call",
                ChatEntryKind::ToolResult { .. } => "result",
                ChatEntryKind::System(_) => "system",
                ChatEntryKind::Thinking(_) => "thinking",
                _ => "other",
            })
            .collect();
        assert_eq!(
            kinds,
            vec!["assistant", "call", "result", "system", "thinking", "user"],
            "both interstitials must move after the loop, order preserved"
        );
    }

    #[rstest::rstest]
    #[test]
    fn normalize_noop_when_no_loops() {
        // Given a history with no tool loops at all.
        let mut session = session_with(vec![
            ChatEntry::user("a"),
            ChatEntry::assistant("b"),
            ChatEntry::system("c"),
            ChatEntry::user("d"),
        ]);
        let before = entry_ids(&session);

        // When normalizing.
        session.edit_history().normalize_loop_layout();

        // Then nothing moved.
        assert_eq!(before, entry_ids(&session));
    }

    #[rstest::rstest]
    #[test]
    fn normalize_continues_past_leading_non_loop_entries() {
        // Given a user entry before the loop and an interstitial inside it.
        let mut session = session_with(vec![
            ChatEntry::user("go"),
            ChatEntry::assistant(""),
            ChatEntry::tool_call("call-1", "bash", "{}"),
            ChatEntry::system("status mid-loop"),
            ChatEntry::tool_result("call-1", "bash", "ok", ToolResultStatus::Success),
        ]);

        // When normalizing.
        session.edit_history().normalize_loop_layout();

        // Then the interstitial moved despite the leading user entry.
        let kinds: Vec<&str> = session
            .history()
            .iter()
            .map(|e| match &e.kind {
                ChatEntryKind::User { .. } => "user",
                ChatEntryKind::Assistant(_) => "assistant",
                ChatEntryKind::ToolCall { .. } => "call",
                ChatEntryKind::ToolResult { .. } => "result",
                ChatEntryKind::System(_) => "system",
                _ => "other",
            })
            .collect();
        assert_eq!(
            kinds,
            vec!["user", "assistant", "call", "result", "system"],
            "scan must not stop at the leading user entry"
        );
    }

    #[rstest::rstest]
    #[test]
    fn pinned_skill_result_keeps_whole_loop_through_worker_exclude() {
        // Given a complete loop whose result carries a tool-requested pin
        // (the skill/save_plan shape, as finalize_tool_result now pins it).
        let mut session = session_with(simple_loop());
        let result_id = session.history()[3].id.clone();
        session
            .edit_history()
            .pin(&result_id, PinPosition::Relative);

        // When a prune worker tries to exclude the call half.
        let call_id = session.history()[2].id.clone();
        let changed = session.edit_history().set_context(
            &call_id,
            ContextOverride::ForcedExclude,
            &worker_source(),
        );

        // Then the exclusion is refused and every loop member stays in context.
        assert!(changed.is_empty(), "pin must win: {changed:?}");
        assert!(
            session.history()[1..=3]
                .iter()
                .all(ChatEntry::is_in_context),
            "whole loop remains in context"
        );
    }

    /// Whether a converted message list satisfies the provider-neutral tool
    /// sequence contract (mirrors the tripwire's invariant).
    fn sequence_is_valid(messages: &[LlmMessage]) -> bool {
        use std::collections::HashSet;
        let mut open: Option<HashSet<String>> = None;
        for message in messages {
            match message {
                LlmMessage::Assistant {
                    tool_calls: Some(calls),
                    ..
                } => {
                    if open.is_some() {
                        return false;
                    }
                    let ids: HashSet<String> = calls.iter().map(|c| c.id.clone()).collect();
                    if ids.len() != calls.len() {
                        return false;
                    }
                    open = Some(ids);
                }
                LlmMessage::Tool { tool_call_id, .. } => match open.as_mut() {
                    Some(remaining) => {
                        if !remaining.remove(tool_call_id) {
                            return false;
                        }
                        if remaining.is_empty() {
                            open = None;
                        }
                    }
                    None => return false,
                },
                LlmMessage::Assistant {
                    tool_calls: None, ..
                }
                | LlmMessage::User { .. } => {
                    if open.is_some_and(|remaining| !remaining.is_empty()) {
                        return false;
                    }
                    open = None;
                }
            }
        }
        open.is_none_or(|remaining| remaining.is_empty())
    }

    #[rstest::rstest]
    #[test]
    fn randomized_editor_ops_always_assemble_valid_sequences() {
        use rand::Rng;
        use std::collections::HashSet;

        // Given a seeded generator and a session built from editor ops only.
        let mut rng = <rand::rngs::StdRng as rand::SeedableRng>::seed_from_u64(0x5EED);
        let mut session = ChatSessionState::new();
        let mut call_counter = 0usize;
        let mut pinned: HashSet<ChatEntryId> = HashSet::new();

        for step in 0..2000 {
            let history_len = session.history().len();
            if history_len == 0 {
                session.edit_history().append(ChatEntry::user("start"));
                continue;
            }
            let pick = rng.random_range(0..8);
            let random_index = rng.random_range(0..history_len);
            let id = session.history()[random_index].id.clone();
            match pick {
                0 | 1 => {
                    session
                        .edit_history()
                        .append(ChatEntry::user(format!("u{step}")));
                }
                2 => {
                    session
                        .edit_history()
                        .append(ChatEntry::assistant(format!("a{step}")));
                }
                3 => {
                    call_counter += 1;
                    session.edit_history().append(ChatEntry::tool_call(
                        format!("c{call_counter}"),
                        "bash",
                        "{}",
                    ));
                }
                4 => {
                    let call_id = format!("c{}", rng.random_range(1..=(call_counter.max(1))));
                    session.edit_history().append(ChatEntry::tool_result(
                        call_id,
                        "bash",
                        "ok",
                        ToolResultStatus::Success,
                    ));
                }
                5 => {
                    let changed = session.edit_history().set_context(
                        &id,
                        ContextOverride::ForcedExclude,
                        &ChangeSource::Worker {
                            name: "rand".to_owned(),
                        },
                    );
                    let _ = changed;
                }
                6 => {
                    if !pinned.contains(&id) {
                        session.edit_history().pin(&id, PinPosition::Relative);
                        pinned.clear();
                        // After chunk pinning, re-derive which ids are pinned.
                        pinned.extend(
                            session
                                .history()
                                .iter()
                                .filter(|e| e.is_pinned())
                                .map(|e| e.id.clone()),
                        );
                    }
                }
                _ => {
                    session.edit_history().normalize_loop_layout();
                }
            }

            // Then the assembled message list is always sequence-valid.
            let messages =
                crate::feat::provider::entries_to_messages::entries_to_messages(session.history());
            assert!(
                sequence_is_valid(&messages),
                "step {step} produced an invalid sequence: {messages:?}"
            );
        }
    }
}
