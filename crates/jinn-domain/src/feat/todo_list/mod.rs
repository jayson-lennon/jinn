//! Todo list subsystem - phased task tracking for agent sessions.
//!
//! Provides a structured task list with one level of nesting: phases contain tasks.
//! The data model is stored per-session on [`SessionCore`](crate::feat::session::chat_session::SessionCore)
//! and persists across restarts via the existing session serialization pipeline.

pub mod picker_entry;
pub mod tools;

#[cfg(test)]
mod types_tests;

use std::fmt;

use rand::Rng;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// ID generation
// ---------------------------------------------------------------------------

/// Characters used for random ID generation: a-z and 0-9, excluding `p` and `t`.
///
/// `p` and `t` are excluded to avoid ambiguity with the phase/task ID prefixes.
/// 34 characters → 34³ = 39,304 possible IDs per type.
const ID_CHARSET: &[u8] = b"abcdefghijklmnqrsuvwxyz0123456789";

/// Generates 3 random characters from the ID charset.
#[expect(clippy::expect_used, reason = "infallible")]
fn generate_id_chars() -> [u8; 3] {
    let mut rng = rand::rng();
    [
        *ID_CHARSET
            .get(rng.random_range(0..ID_CHARSET.len()))
            .expect("range bounded by ID_CHARSET.len()"),
        *ID_CHARSET
            .get(rng.random_range(0..ID_CHARSET.len()))
            .expect("range bounded by ID_CHARSET.len()"),
        *ID_CHARSET
            .get(rng.random_range(0..ID_CHARSET.len()))
            .expect("range bounded by ID_CHARSET.len()"),
    ]
}

// ---------------------------------------------------------------------------
// ID types
// ---------------------------------------------------------------------------

/// Unique identifier for a phase within a task list.
///
/// Random 3-char alphanumeric (excluding `p` and `t`) prefixed with `p`.
/// Globally unique within the task list - collision-checked against existing IDs.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
pub struct PhaseId(String);

impl PhaseId {
    #[expect(clippy::expect_used, reason = "infallible")]
    fn new(existing: &[PhaseId]) -> Self {
        loop {
            let chars = generate_id_chars();
            let candidate = format!(
                "p{}",
                std::str::from_utf8(&chars).expect("charset is valid UTF-8")
            );
            if !existing.iter().any(|e| e.0 == candidate) {
                return Self(candidate);
            }
        }
    }

    /// Creates a PhaseId from a known string (for testing).
    #[cfg(test)]
    pub(crate) fn new_for_test(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl fmt::Display for PhaseId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl AsRef<str> for PhaseId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Unique identifier for a task within a task list.
///
/// Random 3-char alphanumeric (excluding `p` and `t`) prefixed with `t`.
/// Globally unique across all phases - collision-checked against existing IDs.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
pub struct TaskId(String);

impl TaskId {
    #[expect(clippy::expect_used, reason = "charset is valid UTF-8")]
    fn new(existing: &[TaskId]) -> Self {
        loop {
            let chars = generate_id_chars();
            let candidate = format!(
                "t{}",
                std::str::from_utf8(&chars).expect("charset is valid UTF-8")
            );
            if !existing.iter().any(|e| e.0 == candidate) {
                return Self(candidate);
            }
        }
    }

    /// Creates a TaskId from a known string (for testing).
    #[cfg(test)]
    pub(crate) fn new_for_test(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl fmt::Display for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl AsRef<str> for TaskId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

// ---------------------------------------------------------------------------
// Task status
// ---------------------------------------------------------------------------

/// The status of a task item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TaskStatus {
    /// Task is pending - not yet done.
    #[default]
    Pending,
    /// Task has been completed.
    Completed,
    /// Task has been postponed to a later phase.
    #[serde(rename = "Deferred")]
    Postponed,
    /// Task has been cancelled and will not be done.
    Cancelled,
}

impl TaskStatus {
    /// Returns the display indicator for this status.
    pub fn indicator(&self) -> &'static str {
        match self {
            Self::Pending => "\u{25CB}",
            Self::Completed => "\u{2713}",
            Self::Postponed => "\u{25BC}",
            Self::Cancelled => "\u{2717}",
        }
    }
}

// ---------------------------------------------------------------------------
// Declarative write inputs
// ---------------------------------------------------------------------------

/// Input for one phase in a declarative write
/// ([`TaskList::set_from_inputs`] / [`TaskList::set_phase_from_input`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhaseInput {
    /// Human-readable phase description. Doubles as the match key for
    /// description-keyed writes ([`TaskList::set_phase_from_input`]).
    pub description: String,
    /// Ordered tasks as `(description, status)` pairs; status is declared
    /// inline rather than remembered from a previous write.
    pub tasks: Vec<(String, TaskStatus)>,
}

// ---------------------------------------------------------------------------
// Task
// ---------------------------------------------------------------------------

/// A single task item within a phase.
///
/// Tasks have a stable ID, a description, and a completion status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    /// Unique identifier for this task.
    pub id: TaskId,
    /// Human-readable description of the task.
    pub description: String,
    /// Current status of the task.
    pub status: TaskStatus,
}

impl Task {
    /// Returns this task's unique identifier.
    pub fn id(&self) -> &TaskId {
        &self.id
    }

    /// Returns this task's description.
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Returns this task's current status.
    pub fn status(&self) -> TaskStatus {
        self.status
    }
}

// ---------------------------------------------------------------------------
// Phase
// ---------------------------------------------------------------------------

/// A phase - a named container of ordered tasks.
///
/// Phases represent high-level stages of work (e.g., "Research", "Build", "Test").
/// Tasks within a phase are ordered and can be repositioned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Phase {
    /// Unique identifier for this phase.
    pub id: PhaseId,
    /// Human-readable description of the phase.
    pub description: String,
    /// Ordered tasks within this phase.
    pub tasks: Vec<Task>,
}

impl Phase {
    /// Returns this phase's unique identifier.
    pub fn id(&self) -> &PhaseId {
        &self.id
    }

    /// Returns this phase's description.
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Returns the tasks in this phase.
    pub fn tasks(&self) -> &[Task] {
        &self.tasks
    }

    /// Returns true if this phase has no tasks.
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// Returns true if this phase contains any task in the [`TaskStatus::Pending`] state.
    ///
    /// Postponed, Cancelled, and Completed tasks are not "work to do".
    pub fn has_pending_work(&self) -> bool {
        self.tasks.iter().any(|t| t.status == TaskStatus::Pending)
    }
}

// ---------------------------------------------------------------------------
// TaskList
// ---------------------------------------------------------------------------

/// A phased task list - the top-level container for agent planning.
///
/// Contains ordered phases, each containing ordered tasks.
/// Stored per-session on [`SessionCore`](crate::feat::session::chat_session::SessionCore).
///
/// # Persistence
///
/// Derives `Serialize`/`Deserialize` - the existing session save/load pipeline
/// handles persistence automatically. The `#[serde(default)]` attribute on the
/// `SessionCore` field ensures backward compatibility with old sessions.
/// Old serialized data with counter fields (`next_phase_id`, `next_task_id`) will
/// deserialize cleanly - unknown fields are ignored by serde's default behavior.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskList {
    /// Ordered phases in this task list.
    #[serde(default)]
    pub phases: Vec<Phase>,
}

impl TaskList {
    /// Creates a new empty task list.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a new phase and returns its ID.
    pub fn add_phase(&mut self, description: &str) -> PhaseId {
        let existing: Vec<_> = self.phases.iter().map(|p| p.id.clone()).collect();
        let id = PhaseId::new(&existing);
        self.phases.push(Phase {
            id: id.clone(),
            description: description.to_owned(),
            tasks: Vec::new(),
        });
        id
    }

    /// Clears the entire task list — removes all phases and tasks.
    ///
    /// Used by `todo_set_list` when the model supplies an empty `phases` array
    /// (a subagent discarding an inherited list it doesn't need).
    pub fn clear(&mut self) {
        self.phases.clear();
    }

    /// Replaces the entire task list with phases built from declarative inputs.
    ///
    /// The complete replacement list (with freshly minted IDs) is constructed
    /// before `self` is touched, then swapped in atomically — input is already
    /// validated by the tool-layer parser, so the swap cannot fail partway.
    pub fn set_from_inputs(&mut self, phases: &[PhaseInput]) {
        let built = {
            // Full replacement: no legacy IDs to avoid.
            let (mut phase_ids, mut task_ids) = (Vec::new(), Vec::new());
            let mut built = Vec::with_capacity(phases.len());
            for input in phases {
                built.push(Self::build_phase(input, &mut phase_ids, &mut task_ids));
            }
            built
        };
        self.phases = built;
    }

    /// Replaces the first phase whose description matches `input.description`
    /// (exact comparison on trimmed strings), or appends a new phase when no
    /// phase matches.
    ///
    /// Returns `true` when an existing phase was replaced, `false` when a new
    /// phase was appended. The replacement phase (with freshly minted,
    /// collision-checked IDs) is built before `self` is mutated.
    pub fn set_phase_from_input(&mut self, input: &PhaseInput) -> bool {
        let built = {
            // Seed the collision sets from the retained list so new IDs never
            // collide with tasks in other phases.
            let mut phase_ids: Vec<_> = self.phases.iter().map(|p| p.id.clone()).collect();
            let mut task_ids: Vec<_> = self
                .phases
                .iter()
                .flat_map(|p| &p.tasks)
                .map(|t| t.id.clone())
                .collect();
            Self::build_phase(input, &mut phase_ids, &mut task_ids)
        };
        if let Some(phase) = self
            .phases
            .iter_mut()
            .find(|p| p.description.trim() == input.description.trim())
        {
            *phase = built;
            true
        } else {
            self.phases.push(built);
            false
        }
    }

    /// Builds a fresh `Phase` from one input, minting a phase ID and task IDs
    /// that avoid every ID in the passed collision sets (extended as IDs are
    /// minted).
    fn build_phase(
        input: &PhaseInput,
        phase_ids: &mut Vec<PhaseId>,
        task_ids: &mut Vec<TaskId>,
    ) -> Phase {
        let id = PhaseId::new(phase_ids);
        phase_ids.push(id.clone());
        let tasks = input
            .tasks
            .iter()
            .map(|(description, status)| {
                let task_id = TaskId::new(task_ids);
                task_ids.push(task_id.clone());
                Task {
                    id: task_id,
                    description: description.clone(),
                    status: *status,
                }
            })
            .collect();
        Phase {
            id,
            description: input.description.clone(),
            tasks,
        }
    }

    /// Returns the ordered phases in this task list.
    pub fn phases(&self) -> &[Phase] {
        &self.phases
    }

    /// Returns `(completed, total)` task counts across all phases.
    ///
    /// Phase boundaries are ignored. `completed` counts only [`TaskStatus::Completed`];
    /// `total` counts every task regardless of status. Returns `(0, 0)` when there are
    /// no tasks.
    ///
    /// Used by the session preview badge to render `{completed}/{total} · {pct}%`.
    #[must_use]
    pub fn completion_counts(&self) -> (usize, usize) {
        let total = self.phases.iter().map(|p| p.tasks.len()).sum();
        let completed = self
            .phases
            .iter()
            .flat_map(|p| &p.tasks)
            .filter(|t| t.status == TaskStatus::Completed)
            .count();
        (completed, total)
    }

    /// Returns the earliest phase that still has pending work.
    ///
    /// A phase has pending work if it contains at least one task in the
    /// [`TaskStatus::Pending`] state. The "active" phase is the one the agent
    /// is currently supposed to be working on.
    ///
    /// Returns `None` when:
    /// - the list is empty, or
    /// - every phase contains only Completed / Cancelled / Postponed tasks
    ///   (i.e., nothing left to do).
    #[must_use]
    pub fn active_phase(&self) -> Option<&Phase> {
        self.phases.iter().find(|p| p.has_pending_work())
    }

    /// Returns true if the task list has no phases.
    pub fn is_empty(&self) -> bool {
        self.phases.is_empty()
    }

    /// Renders the task list as formatted markdown text.
    ///
    /// Used by tools to return the current state to the LLM.
    pub fn render_text(&self) -> String {
        if self.phases.is_empty() {
            return "No phases defined.".to_owned();
        }

        let mut lines = Vec::new();
        for (i, phase) in self.phases.iter().enumerate() {
            lines.push(format!("## Phase {}: {}", i + 1, phase.description));
            Self::push_task_lines(&phase.tasks, &mut lines);
        }

        // Remove trailing newline.
        if lines.last() == Some(&String::new()) {
            lines.pop();
        }

        lines.join("\n")
    }
    /// Renders the task list as formatted markdown text, with `(Blocked by previous
    /// phase)` prefixed on every non-active phase header that still has pending work.
    ///
    /// The active phase (the earliest phase with any [`TaskStatus::Pending`] task)
    /// renders normally. Completed phases (no pending work) also render normally.
    /// This is the variant used by all `todo_*` tool returns to give the agent a
    /// salient cue about which phases it should not be jumping into.
    #[must_use]
    pub fn render_text_with_blockers(&self) -> String {
        if self.phases.is_empty() {
            return "No phases defined.".to_owned();
        }

        let active_id = self.active_phase().map(|p| &p.id);
        let mut lines = Vec::new();
        for (i, phase) in self.phases.iter().enumerate() {
            let prefix = match active_id {
                Some(active) if active == &phase.id => String::new(),
                Some(_) if phase.has_pending_work() => "(Blocked by previous phase) ".to_owned(),
                _ => String::new(),
            };
            lines.push(format!(
                "## Phase {}: {}{}",
                i + 1,
                prefix,
                phase.description
            ));
            Self::push_task_lines(&phase.tasks, &mut lines);
            lines.push(String::new());
        }

        // Remove trailing newline.
        if lines.last() == Some(&String::new()) {
            lines.pop();
        }

        lines.join("\n")
    }

    /// Appends rendered task lines (or `(no tasks)`) for a slice of tasks.
    /// Postponed tasks are filtered out before rendering.
    fn push_task_lines(tasks: &[Task], out: &mut Vec<String>) {
        if tasks.is_empty() {
            out.push("  (no tasks)".to_owned());
            return;
        }
        let visible: Vec<_> = tasks
            .iter()
            .filter(|t| t.status != TaskStatus::Postponed)
            .collect();
        if visible.is_empty() {
            out.push("  (no tasks)".to_owned());
            return;
        }
        for task in visible {
            let (check, desc) = match task.status {
                TaskStatus::Pending | TaskStatus::Postponed => (" ", task.description.clone()),
                TaskStatus::Completed => ("\u{2713}", task.description.clone()),
                TaskStatus::Cancelled => ("\u{2717}", format!("CANCELLED: {}", task.description)),
            };
            out.push(format!("- [{}] {}", check, desc));
        }
    }

    /// Produces the `→ NEXT` cue line for a tool return.
    ///
    /// Three branches:
    /// 1. Active phase has pending work →
    ///    `→ NEXT: {desc} ({n} pending in phase: {phase_description})`
    /// 2. No active phase, but at least one task exists →
    ///    `→ All phases complete — stop.`
    /// 3. No phases / no tasks ever → empty string (caller omits the line).
    ///
    /// Identifies tasks and phases by description only — the tool surface is
    /// id-free, so the model can act on the cue without looking anything up.
    ///
    /// # Panics
    ///
    /// Panics if `active_phase()` returns a phase with no `TaskStatus::Pending`
    /// task — which the constructor of `Phase` and `active_phase()` invariantly forbid.
    #[must_use]
    #[expect(clippy::expect_used, reason = "infallible")]
    pub fn render_next_block(&self) -> String {
        if self.phases.is_empty() {
            return String::new();
        }

        if let Some(active) = self.active_phase() {
            // active_phase() implies has_pending_work(), which implies
            // at least one task with TaskStatus::Pending.
            let next_task = active
                .tasks
                .iter()
                .find(|t| t.status == TaskStatus::Pending)
                .expect("active_phase must contain at least one pending task");
            let remaining = active
                .tasks
                .iter()
                .filter(|t| t.status == TaskStatus::Pending)
                .count();
            return format!(
                "→ NEXT: {} ({} pending in phase: {})",
                next_task.description, remaining, active.description
            );
        }

        // No active phase.
        let any_tasks_ever = self.phases.iter().any(|p| !p.tasks.is_empty());
        if any_tasks_ever {
            "→ All phases complete — stop.".to_owned()
        } else {
            String::new()
        }
    }

    /// Returns a NEXT block that is aware of which task was just completed.
    ///
    /// Same shape as [`render_next_block`] when there is still work in the same phase,
    /// but switches to a 'phase complete — proceed to verify' message when the completed
    /// task was the last pending one in its phase, regardless of whether later phases
    /// still have work (those are blocked until verification passes). Phases are
    /// named by description.
    ///
    /// # Arguments
    ///
    /// * `completed_phase_id` - The phase ID of the task that was just marked complete.
    #[must_use]
    pub fn render_next_block_after_completion(&self, completed_phase_id: &PhaseId) -> String {
        // Find the phase that just had a task completed.
        let completed_phase = self.phases.iter().find(|p| &p.id == completed_phase_id);

        let Some(completed_phase) = completed_phase else {
            // Phase no longer exists (e.g., list replaced); fall back to global next.
            return self.render_next_block();
        };

        if completed_phase.has_pending_work() {
            // Same phase still has work; emit the normal NEXT line for that phase.
            let pending: Vec<_> = completed_phase
                .tasks
                .iter()
                .filter(|t| t.status == TaskStatus::Pending)
                .collect();
            let Some(next_task) = pending.first() else {
                return self.render_next_block();
            };
            let remaining = pending.len();
            return format!(
                "→ NEXT: {} ({} pending in phase: {})",
                next_task.description, remaining, completed_phase.description
            );
        }

        // Phase is fully complete.
        // Are there later phases that still have work? Those are blocked until verify.
        let completed_idx = self.phases.iter().position(|p| &p.id == completed_phase_id);
        let later_blocked = match completed_idx {
            Some(idx) => self
                .phases
                .get(idx + 1..)
                .is_some_and(|tail| tail.iter().any(Phase::has_pending_work)),
            None => false,
        };

        if later_blocked {
            format!(
                "→ Phase \"{}\" complete — proceed to verify. Later phases are blocked until then.",
                completed_phase.description
            )
        } else {
            format!(
                "→ Phase \"{}\" complete — proceed to verify.",
                completed_phase.description
            )
        }
    }
}
