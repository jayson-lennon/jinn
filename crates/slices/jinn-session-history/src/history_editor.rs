//! History editor - the sole writer of chat history.
//!
//! This is the session-history slice's lib-only payload: the editor owns
//! every sanctioned mutation of a session's history vector. There is no
//! actor here — the kernel session actor's fold handlers (streaming,
//! tool calls, stall retry) are this slice's sanctioned callers, writing
//! history alongside phase-machine, in-flight-guard, and input-drain
//! state in one lock scope.
//!
//! The editor treats each assistant tool-call/result loop as an atomic
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

use jinn_core_types::{
    ChangeSource, ChatEntry, ChatEntryId, ChatEntryKind, ContextOverride, HistoryMutation,
    PinPosition,
};

/// The write-primitive surface the kernel session state exposes to the
/// editor.
///
/// Sealed: only the kernel's `ChatSessionState` implements it (the private
/// constructor of the [`SessionHistoryAccessPriv`] supertrait keeps outside
/// crates from adding implementations). The three low-level operations here
/// are the same primitives the kernel's feature-restricted methods expose —
/// the editor's public API is and remains the sole sanctioned path over
/// them.
pub trait SessionHistoryAccess: SessionHistoryAccessPriv {
    /// Read access to the history vector.
    fn history(&self) -> &[ChatEntry];

    /// Tail push with template expansion + cursor bookkeeping.
    fn push_entry_raw(&mut self, entry: &mut ChatEntry) -> usize;

    /// Mutable entry access by index.
    fn history_get_mut(&mut self, index: usize) -> Option<&mut ChatEntry>;

    /// Insert an entry at a (pre-clamped) index; shifts ephemeral
    /// streaming indices.
    fn insert_entry_at(&mut self, index: usize, entry: ChatEntry) -> usize;

    /// Remove the entry at `index`; returns whether it existed.
    fn remove_history_entry_at(&mut self, index: usize) -> bool;

    /// Runs `f` on the entry with `id`, if it exists. Returns `f`'s output.
    ///
    /// Editor-only in-primitive for id-keyed in-place mutation.
    fn with_history_entry_mut<R>(
        &mut self,
        id: &ChatEntryId,
        f: impl FnOnce(&mut ChatEntry) -> R,
    ) -> Option<R> {
        let index = self.history().iter().position(|entry| &entry.id == id)?;
        self.history_get_mut(index).map(f)
    }
}

/// Sealing supertrait — deliberately unnameable outside this crate.
pub trait SessionHistoryAccessPriv {
    /// Private: prevents implementations outside the kernel.
    #[doc(hidden)]
    fn seal(&self) -> Priv;
}

/// The seal value. Constructible only through [`Priv::construct`], which is
/// `#[doc(hidden)]` — implementing the sealed trait requires naming it, and
/// it is not part of the public API surface.
#[derive(Debug, Clone, Copy)]
pub struct Priv(());

impl Priv {
    /// Constructs the seal token. `#[doc(hidden)]`: public only so the
    /// kernel crate can implement the sealed trait.
    #[doc(hidden)]
    #[must_use]
    pub fn construct() -> Self {
        Self(())
    }
}

/// Write access to a session's history.
///
/// Obtained via `ChatSessionState::edit_history`. Every method that changes
/// history lives here; reads stay on the session itself.
pub struct HistoryEditor<'a, S: SessionHistoryAccess + ?Sized> {
    session: &'a mut S,
}

/// A contiguous span of history entries mutated as one unit.
///
/// A span longer than one entry is an assistant tool loop; a single-entry
/// span is a standalone entry.
#[derive(Debug, Clone)]
pub struct Chunk {
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

impl<'a, S> HistoryEditor<'a, S>
where
    S: SessionHistoryAccess,
{
    /// Creates an editor over the session. Call `ChatSessionState::edit_history` instead.
    pub fn new(session: &'a mut S) -> Self {
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
pub fn tool_group_end(history: &[ChatEntry], index: usize) -> Option<usize> {
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
pub fn is_tool_loop_interstitial(entry: &ChatEntry) -> bool {
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
            && *status != jinn_core_types::ToolResultStatus::Pending
        {
            pending.retain(|call_id| *call_id != id.as_str());
        }
    }
    pending.is_empty()
}

/// Splits a history into chunks: tool loops (assistant + calls + results)
/// and single standalone entries, in order.
pub fn chunking(history: &[ChatEntry]) -> Vec<Chunk> {
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
pub fn chunk_containing(chunks: &[Chunk], index: usize) -> Option<Chunk> {
    chunks
        .iter()
        .find(|chunk| chunk.range.contains(&index))
        .cloned()
}
