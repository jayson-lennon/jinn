#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::uninlined_format_args,
    clippy::unreachable,
    clippy::string_slice,
    reason = "test code"
)]
//! Tests for the task list data model.

//!
//! BDD-style tests following AGENTS.md conventions.
//! Each test covers a single behavior.

use crate::todo_list::{PhaseId, PhaseInput, TaskList, TaskStatus};

// ---------------------------------------------------------------------------
// set_from_inputs
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[test]
fn set_from_inputs_creates_phase_with_id_and_description() {
    // Given an empty task list.
    let mut list = TaskList::new();

    // When setting the list from a single phase input.
    list.set_from_inputs(&[PhaseInput {
        description: "Research".to_owned(),
        tasks: vec![],
    }]);
    // Then the phase gets an ID of a 'p' prefix plus 3 random chars.
    let id = list.phases()[0].id.to_string();
    assert!(id.starts_with('p'));
    assert_eq!(id.len(), 4);
    // And the phase carries its description and no tasks.
    let phase = &list.phases()[0];
    assert_eq!(phase.description(), "Research");
    assert!(phase.is_empty());
}

#[rstest::rstest]
#[test]
fn set_from_inputs_generates_distinct_ids() {
    // Given an empty task list.
    let mut list = TaskList::new();

    // When setting the list from three phase inputs.
    list.set_from_inputs(&[
        PhaseInput {
            description: "Research".to_owned(),
            tasks: vec![],
        },
        PhaseInput {
            description: "Build".to_owned(),
            tasks: vec![],
        },
        PhaseInput {
            description: "Test".to_owned(),
            tasks: vec![],
        },
    ]);

    // Then every phase receives a distinct ID.
    let ids: Vec<_> = list.phases().iter().map(|p| p.id.clone()).collect();
    assert_ne!(ids[0], ids[1]);
    assert_ne!(ids[1], ids[2]);
    assert_ne!(ids[0], ids[2]);
}

// ---------------------------------------------------------------------------
// is_empty
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[test]
fn is_empty_true_when_no_phases() {
    // Given a newly created task list.
    let list = TaskList::new();

    // When asking whether the list is empty.
    // Then the list reports no phases.
    assert!(list.is_empty());
}

#[rstest::rstest]
#[test]
fn is_empty_false_when_has_phases() {
    // Given an empty task list.
    let mut list = TaskList::new();

    // When setting the list from one phase input.
    list.set_from_inputs(&[PhaseInput {
        description: "Build".to_owned(),
        tasks: vec![],
    }]);

    // Then the list reports a phase.
    assert!(!list.is_empty());
}

// ---------------------------------------------------------------------------
// render_text
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[test]
fn render_text_returns_empty_placeholder() {
    // Given a task list with no phases.
    let list = TaskList::new();

    // When rendering the list as text.
    // Then the empty placeholder is rendered.
    assert_eq!(list.render_text(), "No phases defined.");
}

#[rstest::rstest]
#[test]
fn render_text_shows_phases_and_tasks() {
    // Given a phase with two pending tasks.
    let mut list = TaskList::new();

    // When rendering the list as text.
    list.set_from_inputs(&[PhaseInput {
        description: "Research".to_owned(),
        tasks: vec![
            ("Read docs".to_owned(), TaskStatus::Pending),
            ("Call API".to_owned(), TaskStatus::Pending),
        ],
    }]);

    // Then the phase and both task lines are rendered.
    let rendered = list.render_text();
    assert!(rendered.contains("Phase 1: Research"));
    assert!(rendered.contains("[ ] Read docs"));
    assert!(rendered.contains("[ ] Call API"));
}

#[rstest::rstest]
#[test]
fn render_text_shows_completed_task() {
    // Given a phase with one completed task.
    let mut list = TaskList::new();

    // When rendering the list as text.
    list.set_from_inputs(&[PhaseInput {
        description: "Build".to_owned(),
        tasks: vec![("Write code".to_owned(), TaskStatus::Completed)],
    }]);

    // Then the task line carries the completed marker.
    let rendered = list.render_text();
    assert!(rendered.contains("[✓] Write code"));
}

// ---------------------------------------------------------------------------
// Serde roundtrip
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[test]
fn serde_roundtrip_preserves_state() {
    // Given a list of two phases with mixed task statuses.
    let mut list = TaskList::new();
    list.set_from_inputs(&[
        PhaseInput {
            description: "Research".to_owned(),
            tasks: vec![
                ("Read docs".to_owned(), TaskStatus::Completed),
                ("Call API".to_owned(), TaskStatus::Pending),
            ],
        },
        PhaseInput {
            description: "Build".to_owned(),
            tasks: vec![("Write code".to_owned(), TaskStatus::Pending)],
        },
    ]);

    // When serialising and deserialising the list.
    let json = serde_json::to_string(&list).unwrap();
    let restored: TaskList = serde_json::from_str(&json).unwrap();

    // Then the restored list equals the original.
    assert_eq!(list, restored);
    assert!(!restored.is_empty());
}

#[rstest::rstest]
#[test]
fn serde_default_creates_empty_list() {
    // Given JSON with no fields at all.
    let json = "{}";

    // When deserialising it as a task list.
    let list: TaskList = serde_json::from_str(json).unwrap();

    // Then the list has no phases.
    assert!(list.is_empty());
}

#[rstest::rstest]
#[test]
fn serde_deserializes_partial_json() {
    // Given old-format JSON carrying only a phases field.
    // Only phases field (no counters) - a valid old-format JSON.
    let json = r#"{"phases":[]}"#;

    // When deserialising it as a task list.
    let list: TaskList = serde_json::from_str(json).unwrap();

    // Then the list has no phases.
    assert!(list.is_empty());
}

// ---------------------------------------------------------------------------
// TaskId / PhaseId helpers (exposed for testing via pub(crate))
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[test]
fn id_display_format() {
    // Given a phase ID and a task ID built from raw values.
    let pid = PhaseId::new_for_test("p1");
    let tid = crate::todo_list::TaskId::new_for_test("t2");

    // When displaying each identifier.
    // Then each identifier displays its raw value.
    assert_eq!(format!("{pid}"), "p1");
    assert_eq!(format!("{tid}"), "t2");
}

// ---------------------------------------------------------------------------
// Random ID generation
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[test]
fn id_format_is_correct() {
    // Given a phase with one pending task.
    // Phase IDs start with 'p' and are 4 chars total.
    // Task IDs start with 't' and are 4 chars total.
    // The remaining 3 chars are from the charset [a-z0-9 minus {p, t}].
    let mut list = TaskList::new();

    // When setting the list so the IDs are minted.
    list.set_from_inputs(&[PhaseInput {
        description: "Phase".to_owned(),
        tasks: vec![("Task".to_owned(), TaskStatus::Pending)],
    }]);

    // Then both IDs match the documented format.
    let pid_str = list.phases()[0].id.to_string();
    let tid_str = list.phases()[0].tasks[0].id.to_string();

    assert_eq!(pid_str.len(), 4);
    assert!(pid_str.starts_with('p'));
    // The 3 random chars should not contain 'p' or 't'.
    let suffix = &pid_str[1..];
    for ch in suffix.chars() {
        assert!(ch.is_ascii_alphanumeric());
        assert!(
            ch != 'p' && ch != 't',
            "char '{ch}' should not be 'p' or 't'"
        );
    }

    assert_eq!(tid_str.len(), 4);
    assert!(tid_str.starts_with('t'));
    let suffix = &tid_str[1..];
    for ch in suffix.chars() {
        assert!(ch.is_ascii_alphanumeric());
        assert!(
            ch != 'p' && ch != 't',
            "char '{ch}' should not be 'p' or 't'"
        );
    }
}

#[rstest::rstest]
#[test]
fn id_generation_no_collision() {
    // Given 50 phase inputs, each with one task.
    let mut list = TaskList::new();
    let inputs: Vec<PhaseInput> = (0..50)
        .map(|i| PhaseInput {
            description: format!("Phase {i}"),
            tasks: vec![(format!("Task {i}"), TaskStatus::Pending)],
        })
        .collect();

    // When setting the list so the IDs are minted.
    list.set_from_inputs(&inputs);

    let phase_ids: Vec<_> = list.phases().iter().map(|p| p.id.clone()).collect();
    let task_ids: Vec<_> = list
        .phases()
        .iter()
        .flat_map(|p| &p.tasks)
        .map(|t| t.id.clone())
        .collect();

    // Then every phase ID is unique.
    let mut sorted_pids = phase_ids.clone();
    sorted_pids.sort();
    sorted_pids.dedup();
    assert_eq!(sorted_pids.len(), phase_ids.len());

    // All task IDs are unique.
    let mut unique_tids = task_ids.clone();
    unique_tids.sort();
    unique_tids.dedup();
    assert_eq!(unique_tids.len(), task_ids.len());
}

#[rstest::rstest]
#[test]
fn serde_backward_compat_with_counters() {
    // Given old-format JSON carrying counter fields.
    // Old-format JSON with counter fields should deserialize cleanly.
    let json = r#"{"phases":[],"next_phase_id":5,"next_task_id":10}"#;

    // When deserialising it as a task list.
    let list: TaskList = serde_json::from_str(json).unwrap();

    // Then the list deserialises to no phases.
    assert!(list.is_empty());
}

#[rstest::rstest]
#[test]
fn render_text_excludes_postponed() {
    // Given one postponed task and one pending task in later phases.
    let mut list = TaskList::new();

    // When rendering the list as text.
    list.set_from_inputs(&[
        PhaseInput {
            description: "Research".to_owned(),
            tasks: vec![("Read docs".to_owned(), TaskStatus::Postponed)],
        },
        PhaseInput {
            description: "Build".to_owned(),
            tasks: vec![("Write code".to_owned(), TaskStatus::Pending)],
        },
    ]);

    // Then the postponed task is omitted and the pending one is shown.
    let rendered = list.render_text();

    // The deferred source should NOT appear as a task line.
    assert!(
        !rendered.contains("Read docs"),
        "postponed task should not appear in render"
    );
    // The pending task in the next phase should appear.
    assert!(rendered.contains("Write code"));
}

#[rstest::rstest]
#[test]
fn render_text_shows_no_tasks_when_all_postponed() {
    // Given a phase whose tasks are all postponed.
    let mut list = TaskList::new();

    // When rendering the list as text.
    list.set_from_inputs(&[PhaseInput {
        description: "Research".to_owned(),
        tasks: vec![
            ("Read docs".to_owned(), TaskStatus::Postponed),
            ("Write notes".to_owned(), TaskStatus::Postponed),
        ],
    }]);

    // Then the phase reports having no tasks.
    let rendered = list.render_text();

    assert!(
        rendered.contains("(no tasks)"),
        "phase with all postponed tasks should show (no tasks)"
    );
    assert!(!rendered.contains("Read docs"));
    assert!(!rendered.contains("Write notes"));
}

// ---------------------------------------------------------------------------
// clear
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[test]
fn clear_empties_all_phases() {
    // Given a task list with multiple phases and tasks.
    let mut list = TaskList::new();
    list.set_from_inputs(&[
        PhaseInput {
            description: "Research".to_owned(),
            tasks: vec![("Read docs".to_owned(), TaskStatus::Pending)],
        },
        PhaseInput {
            description: "Build".to_owned(),
            tasks: vec![("Write code".to_owned(), TaskStatus::Pending)],
        },
    ]);

    // When clearing the list.
    list.clear();

    // Then no phases remain.
    assert!(list.is_empty());
    // And the phase slice is empty.
    assert_eq!(list.phases(), []);
}

#[rstest::rstest]
#[test]
fn render_text_shows_cancelled_with_prefix() {
    // Given a phase with one cancelled task.
    let mut list = TaskList::new();

    // When rendering the list as text.
    list.set_from_inputs(&[PhaseInput {
        description: "Build".to_owned(),
        tasks: vec![("Write code".to_owned(), TaskStatus::Cancelled)],
    }]);

    // Then the task line carries the cancelled prefix and marker.
    let rendered = list.render_text();
    assert!(rendered.contains("CANCELLED: Write code"));
    assert!(rendered.contains("[\u{2717}]"));
}

#[rstest::rstest]
#[test]
fn render_text_hides_postponed_shows_cancelled() {
    // Given a phase with one postponed and one cancelled task.
    let mut list = TaskList::new();

    // When rendering the list as text.
    list.set_from_inputs(&[PhaseInput {
        description: "Research".to_owned(),
        tasks: vec![
            ("Read docs".to_owned(), TaskStatus::Postponed),
            ("Extra task".to_owned(), TaskStatus::Cancelled),
        ],
    }]);

    // Then the postponed task is omitted and the cancelled one is shown.
    let rendered = list.render_text();
    // Postponed should be hidden.
    assert!(
        !rendered.contains("Read docs"),
        "postponed task should not appear in render"
    );
    // Cancelled should be visible with prefix.
    assert!(
        rendered.contains("CANCELLED: Extra task"),
        "cancelled task should appear with CANCELLED: prefix"
    );
}

#[rstest::rstest]
#[test]
fn serde_roundtrip_with_cancelled() {
    // Given a phase with one cancelled task.
    let mut list = TaskList::new();
    list.set_from_inputs(&[PhaseInput {
        description: "Build".to_owned(),
        tasks: vec![("Write code".to_owned(), TaskStatus::Cancelled)],
    }]);

    // When serialising and deserialising the list.
    let json = serde_json::to_string(&list).unwrap();
    let restored: TaskList = serde_json::from_str(&json).unwrap();

    // Then the restored list equals the original.
    assert_eq!(list, restored);
}

// ---------------------------------------------------------------------------
// has_pending_work
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[test]
fn has_pending_work_true_when_any_pending() {
    // Given a phase with one completed and one pending task.
    let mut list = TaskList::new();

    // When setting the list from that input.
    list.set_from_inputs(&[PhaseInput {
        description: "Build".to_owned(),
        tasks: vec![
            ("Write code".to_owned(), TaskStatus::Completed),
            ("Write tests".to_owned(), TaskStatus::Pending),
        ],
    }]);

    // Then the phase reports pending work.
    assert!(list.phases()[0].has_pending_work());
}

#[rstest::rstest]
#[test]
fn has_pending_work_false_when_only_completed_cancelled_postponed() {
    // Given a phase with completed, cancelled, and postponed tasks only.
    let mut list = TaskList::new();

    // When setting the list from that input.
    list.set_from_inputs(&[PhaseInput {
        description: "Build".to_owned(),
        tasks: vec![
            ("Write code".to_owned(), TaskStatus::Completed),
            ("Write tests".to_owned(), TaskStatus::Cancelled),
            ("Write docs".to_owned(), TaskStatus::Postponed),
        ],
    }]);

    // Then the phase reports no pending work.
    assert!(
        !list.phases()[0].has_pending_work(),
        "phase with only completed/cancelled/postponed tasks has no pending work"
    );
}

// ---------------------------------------------------------------------------
// active_phase
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[test]
fn active_phase_returns_none_when_empty() {
    // Given a task list with no phases.
    let list = TaskList::new();

    // When asking for the active phase.
    // Then there is no active phase.
    assert!(list.active_phase().is_none());
}

#[rstest::rstest]
#[test]
fn active_phase_returns_earliest_with_pending() {
    // Given three phases where only the last two have pending work.
    let mut list = TaskList::new();

    // When asking for the active phase.
    list.set_from_inputs(&[
        PhaseInput {
            description: "Done".to_owned(),
            tasks: vec![("Done task".to_owned(), TaskStatus::Completed)],
        },
        PhaseInput {
            description: "Active".to_owned(),
            tasks: vec![("Pending task".to_owned(), TaskStatus::Pending)],
        },
        PhaseInput {
            description: "Later".to_owned(),
            tasks: vec![("Pending task".to_owned(), TaskStatus::Pending)],
        },
    ]);

    // Then the earliest phase with pending work is returned.
    let active = list.active_phase().expect("active phase should exist");
    assert_eq!(active.description, "Active");
}

#[rstest::rstest]
#[test]
fn active_phase_returns_none_when_all_complete() {
    // Given a single phase whose only task is completed.
    let mut list = TaskList::new();

    // When asking for the active phase.
    list.set_from_inputs(&[PhaseInput {
        description: "Build".to_owned(),
        tasks: vec![("Write code".to_owned(), TaskStatus::Completed)],
    }]);

    // Then there is no active phase.
    assert!(list.active_phase().is_none());
}

#[rstest::rstest]
#[test]
fn active_phase_skips_phase_with_only_postponed_cancelled_completed() {
    // Given a stale phase followed by two phases with pending work.
    let mut list = TaskList::new();

    // When asking for the active phase.
    list.set_from_inputs(&[
        PhaseInput {
            description: "All-stale".to_owned(),
            tasks: vec![
                ("Completed".to_owned(), TaskStatus::Completed),
                ("Cancelled".to_owned(), TaskStatus::Cancelled),
                ("Postponed".to_owned(), TaskStatus::Postponed),
            ],
        },
        PhaseInput {
            description: "Has work".to_owned(),
            tasks: vec![("Anchor".to_owned(), TaskStatus::Pending)],
        },
        PhaseInput {
            description: "Has work 2".to_owned(),
            tasks: vec![("Other".to_owned(), TaskStatus::Pending)],
        },
    ]);

    // Then the stale phase is skipped in favour of the first with pending work.
    let active = list.active_phase().expect("second phase should be active");
    assert_eq!(active.description, "Has work");
}

// ---------------------------------------------------------------------------
// completion_counts
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[test]
fn completion_counts_empty_list_is_zero_zero() {
    // Given a task list with no phases.
    let list = TaskList::new();

    // When counting completion.
    // Then nothing is completed and nothing is counted.
    assert_eq!(list.completion_counts(), (0, 0));
}

#[rstest::rstest]
#[test]
fn completion_counts_all_pending() {
    // Given a phase whose three tasks are all pending.
    let mut list = TaskList::new();

    // When counting completion.
    list.set_from_inputs(&[PhaseInput {
        description: "Research".to_owned(),
        tasks: vec![
            ("Read docs".to_owned(), TaskStatus::Pending),
            ("Call API".to_owned(), TaskStatus::Pending),
            ("Write notes".to_owned(), TaskStatus::Pending),
        ],
    }]);

    // Then 0 of 3 tasks are completed.
    assert_eq!(list.completion_counts(), (0, 3));
}

#[rstest::rstest]
#[test]
fn completion_counts_all_completed() {
    // Given a phase whose three tasks are all completed.
    let mut list = TaskList::new();

    // When counting completion.
    list.set_from_inputs(&[PhaseInput {
        description: "Research".to_owned(),
        tasks: vec![
            ("Read docs".to_owned(), TaskStatus::Completed),
            ("Call API".to_owned(), TaskStatus::Completed),
            ("Write notes".to_owned(), TaskStatus::Completed),
        ],
    }]);

    // Then 3 of 3 tasks are completed.
    assert_eq!(list.completion_counts(), (3, 3));
}

#[rstest::rstest]
#[test]
fn completion_counts_counts_only_completed() {
    // Given a phase with two completed, one pending, one postponed, one cancelled.
    let mut list = TaskList::new();

    // When counting completion.
    list.set_from_inputs(&[PhaseInput {
        description: "Research".to_owned(),
        tasks: vec![
            ("Done 1".to_owned(), TaskStatus::Completed),
            ("Done 2".to_owned(), TaskStatus::Completed),
            ("Pending".to_owned(), TaskStatus::Pending),
            ("Postponed".to_owned(), TaskStatus::Postponed),
            ("Cancelled".to_owned(), TaskStatus::Cancelled),
        ],
    }]);
    // Then only the 2 completed count toward `completed`; all 5 count as total.
    assert_eq!(list.completion_counts(), (2, 5));
}

#[rstest::rstest]
#[test]
fn completion_counts_aggregates_across_phases() {
    // Given tasks split across two phases.
    let mut list = TaskList::new();
    list.set_from_inputs(&[
        PhaseInput {
            description: "Research".to_owned(),
            tasks: vec![
                ("Read docs".to_owned(), TaskStatus::Completed),
                ("Call API".to_owned(), TaskStatus::Pending),
            ],
        },
        PhaseInput {
            description: "Build".to_owned(),
            tasks: vec![("Write code".to_owned(), TaskStatus::Completed)],
        },
    ]);

    // When counting completion across all phases.
    let (completed, total) = list.completion_counts();

    // Then counts combine across phase boundaries: 2 completed of 3 total.
    assert_eq!((completed, total), (2, 3));
}

#[rstest::rstest]
#[test]
fn completion_counts_treats_empty_phases_as_zero() {
    // Given two phases, one empty and one with two completed tasks.
    let mut list = TaskList::new();
    list.set_from_inputs(&[
        PhaseInput {
            description: "Empty".to_owned(),
            tasks: vec![],
        },
        PhaseInput {
            description: "Build".to_owned(),
            tasks: vec![
                ("Write code".to_owned(), TaskStatus::Completed),
                ("Write tests".to_owned(), TaskStatus::Completed),
            ],
        },
    ]);

    // When counting completion.
    let (completed, total) = list.completion_counts();

    // Then the empty phase contributes nothing: 2 completed of 2 total.
    assert_eq!((completed, total), (2, 2));
}

// ---------------------------------------------------------------------------
// render_text_with_blockers
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[test]
fn render_text_with_blockers_returns_empty_placeholder() {
    // Given a task list with no phases.
    let list = TaskList::new();

    // When rendering the list with blocker annotations.
    // Then the empty placeholder is rendered.
    assert_eq!(list.render_text_with_blockers(), "No phases defined.");
}

#[rstest::rstest]
#[test]
fn render_text_with_blockers_no_prefix_for_single_phase() {
    // Given a single phase with a pending task.
    let mut list = TaskList::new();

    // When rendering the list with blocker annotations.
    list.set_from_inputs(&[PhaseInput {
        description: "Research".to_owned(),
        tasks: vec![("Read docs".to_owned(), TaskStatus::Pending)],
    }]);

    // Then the phase renders without a blocker prefix.
    let rendered = list.render_text_with_blockers();
    assert!(rendered.contains("Phase 1: Research"));
    assert!(
        !rendered.contains("(Blocked by previous phase)"),
        "single-phase list should not carry blocker prefix"
    );
}

#[rstest::rstest]
#[test]
fn render_text_with_blockers_prefixes_non_active_phases() {
    // Given a completed phase, an active phase, and a later pending phase.
    let mut list = TaskList::new();

    // When rendering the list with blocker annotations.
    list.set_from_inputs(&[
        PhaseInput {
            description: "Done".to_owned(),
            tasks: vec![("Done task".to_owned(), TaskStatus::Completed)],
        },
        PhaseInput {
            description: "Active".to_owned(),
            tasks: vec![("Pending".to_owned(), TaskStatus::Pending)],
        },
        PhaseInput {
            description: "Later".to_owned(),
            tasks: vec![("Pending".to_owned(), TaskStatus::Pending)],
        },
    ]);

    // Then only the later phase carries a blocker prefix.
    let rendered = list.render_text_with_blockers();

    // Phase 1: done phases render normally (no prefix needed; nothing blocked).
    assert!(rendered.contains("Phase 1: Done"));
    assert!(!rendered.contains("(Blocked by previous phase) Phase 1"));
    // Phase 2: the active phase renders normally.
    assert!(rendered.contains("Phase 2: Active"));
    assert!(!rendered.contains("(Blocked by previous phase) Phase 2"));
    // Phase 3: not the active phase, has pending work -> prefixed.
    assert!(rendered.contains("Phase 3: (Blocked by previous phase) Later"));
}

#[rstest::rstest]
#[test]
fn render_text_with_blockers_no_prefix_when_all_complete() {
    // Given a single phase whose only task is completed.
    let mut list = TaskList::new();

    // When rendering the list with blocker annotations.
    list.set_from_inputs(&[PhaseInput {
        description: "Build".to_owned(),
        tasks: vec![("Write code".to_owned(), TaskStatus::Completed)],
    }]);

    // Then no phase carries a blocker prefix.
    let rendered = list.render_text_with_blockers();
    assert!(rendered.contains("Phase 1: Build"));
    assert!(!rendered.contains("(Blocked by previous phase)"));
}

#[rstest::rstest]
#[test]
fn render_text_with_blockers_no_prefix_for_completed_phase() {
    // Given a completed phase followed by an active phase.
    let mut list = TaskList::new();

    // When rendering the list with blocker annotations.
    list.set_from_inputs(&[
        PhaseInput {
            description: "Done".to_owned(),
            tasks: vec![("Done task".to_owned(), TaskStatus::Completed)],
        },
        PhaseInput {
            description: "Active".to_owned(),
            tasks: vec![("Pending".to_owned(), TaskStatus::Pending)],
        },
    ]);

    // Then the completed phase carries no blocker prefix.
    let rendered = list.render_text_with_blockers();
    assert!(rendered.contains("Phase 1: Done"));
    assert!(!rendered.contains("(Blocked by previous phase) Phase 1"));
}

#[rstest::rstest]
#[test]
fn render_next_block_empty_when_no_phases() {
    // Given a task list with no phases.
    let list = TaskList::new();

    // When rendering the next block.
    // Then no next block is rendered.
    assert_eq!(list.render_next_block(), "");
}

#[rstest::rstest]
#[test]
fn render_next_block_empty_when_phases_have_no_tasks() {
    // Given a single phase that has no tasks.
    let mut list = TaskList::new();

    // When rendering the next block.
    list.set_from_inputs(&[PhaseInput {
        description: "Empty phase".to_owned(),
        tasks: vec![],
    }]);
    // Then no next block is rendered.
    assert_eq!(list.render_next_block(), "");
}

#[rstest::rstest]
#[test]
fn render_next_block_points_at_active_phase_next_task() {
    // Given a completed phase followed by a phase with two pending tasks.
    let mut list = TaskList::new();

    // When rendering the next block.
    list.set_from_inputs(&[
        PhaseInput {
            description: "Done".to_owned(),
            tasks: vec![("Done task".to_owned(), TaskStatus::Completed)],
        },
        PhaseInput {
            description: "Active".to_owned(),
            tasks: vec![
                ("First pending".to_owned(), TaskStatus::Pending),
                ("Second pending".to_owned(), TaskStatus::Pending),
            ],
        },
    ]);

    // Then the block points at the active phase's next task.
    let block = list.render_next_block();
    assert_eq!(block, "→ NEXT: First pending (2 pending in phase: Active)");
}

#[rstest::rstest]
#[test]
fn render_next_block_skips_cancelled_tasks() {
    // Given a phase whose first task is cancelled and second is pending.
    // Cancelled tasks are filtered out; the next Pending task is found.
    let mut list = TaskList::new();

    // When rendering the next block.
    list.set_from_inputs(&[PhaseInput {
        description: "Active".to_owned(),
        tasks: vec![
            ("Cancelled".to_owned(), TaskStatus::Cancelled),
            ("Real next".to_owned(), TaskStatus::Pending),
        ],
    }]);

    // Then the block points past the cancelled task.
    let block = list.render_next_block();
    assert!(block.starts_with("→ NEXT: Real next"), "got: {block}");
}

#[rstest::rstest]
#[test]
fn render_next_block_includes_remaining_count() {
    // Given a phase with three pending tasks.
    let mut list = TaskList::new();

    // When rendering the next block.
    list.set_from_inputs(&[PhaseInput {
        description: "Active".to_owned(),
        tasks: vec![
            ("A".to_owned(), TaskStatus::Pending),
            ("B".to_owned(), TaskStatus::Pending),
            ("C".to_owned(), TaskStatus::Pending),
        ],
    }]);

    // Then the block names the first task and the remaining count.
    let block = list.render_next_block();
    assert!(block.contains("3 pending in phase"));
    // First pending task is identified.
    assert!(block.starts_with("→ NEXT: A"));
}

#[rstest::rstest]
#[test]
fn render_next_block_all_complete_message() {
    // Given a single phase whose only task is completed.
    let mut list = TaskList::new();

    // When rendering the next block.
    list.set_from_inputs(&[PhaseInput {
        description: "One".to_owned(),
        tasks: vec![("Do".to_owned(), TaskStatus::Completed)],
    }]);

    // Then the block reports that all phases are complete.
    let block = list.render_next_block();
    assert_eq!(block, "→ All phases complete — stop.");
}

#[rstest::rstest]
#[test]
fn render_next_block_all_complete_with_multiple_phases() {
    // Given a completed phase followed by a phase with no tasks.
    let mut list = TaskList::new();

    // When rendering the next block.
    list.set_from_inputs(&[
        PhaseInput {
            description: "One".to_owned(),
            tasks: vec![("Do".to_owned(), TaskStatus::Completed)],
        },
        PhaseInput {
            description: "Two".to_owned(),
            tasks: vec![],
        },
    ]);

    // Then the block reports that all phases are complete.
    let block = list.render_next_block();
    assert_eq!(block, "→ All phases complete — stop.");
}

#[rstest::rstest]
#[test]
fn render_next_block_empty_for_phase_with_no_tasks_anywhere() {
    // Given a single phase that has no tasks.
    let mut list = TaskList::new();

    // When rendering the next block.
    list.set_from_inputs(&[PhaseInput {
        description: "Empty".to_owned(),
        tasks: vec![],
    }]);
    // Then no next block is rendered.
    assert_eq!(list.render_next_block(), "");
}

#[rstest::rstest]
#[test]
fn render_next_block_after_completion_same_phase_remaining() {
    // Given a phase with one completed and one pending task, plus a later phase.
    let mut list = TaskList::new();
    list.set_from_inputs(&[
        PhaseInput {
            description: "Build".to_owned(),
            tasks: vec![
                ("First".to_owned(), TaskStatus::Completed),
                ("Second".to_owned(), TaskStatus::Pending),
            ],
        },
        PhaseInput {
            description: "Test".to_owned(),
            tasks: vec![("Run suite".to_owned(), TaskStatus::Pending)],
        },
    ]);

    // When rendering the next block after that phase completed.
    let completed_phase = &list.phases()[0];

    // Then the block points at the same phase's remaining task.
    let block = list.render_next_block_after_completion(&completed_phase.id);
    assert!(block.starts_with("→ NEXT: Second"), "got: {block}");
    assert!(block.contains("1 pending in phase"));
}

#[rstest::rstest]
#[test]
fn render_next_block_after_completion_phase_done_no_later() {
    // Given a single phase whose only task is completed.
    let mut list = TaskList::new();
    list.set_from_inputs(&[PhaseInput {
        description: "Build".to_owned(),
        tasks: vec![("Only".to_owned(), TaskStatus::Completed)],
    }]);

    // When rendering the next block after that phase completed.
    let completed_phase = &list.phases()[0];

    // Then the block reports the phase complete with nothing later.
    let block = list.render_next_block_after_completion(&completed_phase.id);
    assert_eq!(block, "→ Phase \"Build\" complete — proceed to verify.");
}

#[rstest::rstest]
#[test]
fn render_next_block_after_completion_phase_done_with_later_blocked() {
    // Given a completed phase followed by a phase with a pending task.
    let mut list = TaskList::new();
    list.set_from_inputs(&[
        PhaseInput {
            description: "Build".to_owned(),
            tasks: vec![("Only".to_owned(), TaskStatus::Completed)],
        },
        PhaseInput {
            description: "Test".to_owned(),
            tasks: vec![("Later".to_owned(), TaskStatus::Pending)],
        },
    ]);

    // When rendering the next block after that phase completed.
    let completed_phase = &list.phases()[0];

    // Then the block reports the phase complete and the later phase blocked.
    let block = list.render_next_block_after_completion(&completed_phase.id);
    assert_eq!(
        block,
        "→ Phase \"Build\" complete — proceed to verify. Later phases are blocked until then."
    );
}

#[rstest::rstest]
#[test]
fn render_next_block_after_completion_falls_back_when_phase_missing() {
    // Given a list whose only phase holds a pending task.
    let mut list = TaskList::new();
    list.set_from_inputs(&[PhaseInput {
        description: "Build".to_owned(),
        tasks: vec![("Task".to_owned(), TaskStatus::Pending)],
    }]);

    // When rendering the next block after an unknown phase completed.
    let bogus = crate::todo_list::PhaseId::new_for_test("pzz");
    let block = list.render_next_block_after_completion(&bogus);
    // Then the block falls back to the global next task.
    assert!(block.starts_with("→ NEXT:"));
}

// ---------------------------------------------------------------------------
// set_from_inputs / set_phase_from_input (declarative writes)
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[test]
fn set_from_inputs_replaces_entire_list_with_declared_statuses() {
    // Given a list with existing content.
    let mut list = TaskList::new();
    list.set_from_inputs(&[PhaseInput {
        description: "Old".to_owned(),
        tasks: vec![("Old task".to_owned(), TaskStatus::Pending)],
    }]);

    // When replacing the whole list from declarative inputs.
    list.set_from_inputs(&[
        PhaseInput {
            description: "Research".to_owned(),
            tasks: vec![
                ("Read docs".to_owned(), TaskStatus::Completed),
                ("Call API".to_owned(), TaskStatus::Pending),
            ],
        },
        PhaseInput {
            description: "Build".to_owned(),
            tasks: vec![("Write code".to_owned(), TaskStatus::Cancelled)],
        },
    ]);

    // Then old content is gone and declared statuses stick.
    assert_eq!(list.phases().len(), 2);
    assert_eq!(list.phases()[0].description(), "Research");
    assert_eq!(list.phases()[0].tasks()[0].status(), TaskStatus::Completed);
    assert_eq!(list.phases()[0].tasks()[1].status(), TaskStatus::Pending);
    // And the second phase carries its declared status too.
    assert_eq!(list.phases()[1].tasks()[0].status(), TaskStatus::Cancelled);
}

#[rstest::rstest]
#[test]
fn set_from_inputs_mints_unique_ids_across_all_new_tasks() {
    // Given inputs with several phases and tasks.
    let inputs = [
        PhaseInput {
            description: "A".to_owned(),
            tasks: vec![
                ("t1".to_owned(), TaskStatus::Pending),
                ("t2".to_owned(), TaskStatus::Pending),
            ],
        },
        PhaseInput {
            description: "B".to_owned(),
            tasks: vec![("t3".to_owned(), TaskStatus::Pending)],
        },
    ];

    // When building the list.
    let mut list = TaskList::new();
    list.set_from_inputs(&inputs);

    // Then all phase IDs are distinct.
    let pids: Vec<_> = list.phases().iter().map(|p| p.id.clone()).collect();
    assert_eq!(pids.len(), 2);
    assert_ne!(pids[0], pids[1]);
    // And all task IDs across phases are distinct.
    let tids: Vec<_> = list
        .phases()
        .iter()
        .flat_map(|p| &p.tasks)
        .map(|t| t.id.clone())
        .collect();
    let unique: std::collections::HashSet<_> = tids.iter().collect();
    assert_eq!(unique.len(), tids.len());
}

#[rstest::rstest]
#[test]
fn set_phase_from_input_replaces_first_matching_description() {
    // Given a list with two phases sharing the same description.
    let mut list = TaskList::new();
    list.set_from_inputs(&[
        PhaseInput {
            description: "Build".to_owned(),
            tasks: vec![("Old".to_owned(), TaskStatus::Pending)],
        },
        PhaseInput {
            description: "Build".to_owned(),
            tasks: vec![],
        },
    ]);

    // When writing to the matching description.
    let replaced = list.set_phase_from_input(&PhaseInput {
        description: "Build".to_owned(),
        tasks: vec![("New".to_owned(), TaskStatus::Completed)],
    });

    // Then the first match is the one replaced.
    assert!(replaced);
    assert_eq!(list.phases().len(), 2);
    // And the first phase carries the new content.
    assert_eq!(list.phases()[0].tasks()[0].description(), "New");
    assert_eq!(list.phases()[0].tasks()[0].status(), TaskStatus::Completed);
    // And the second duplicate phase remains untouched and empty.
    assert_eq!(list.phases()[1].description(), "Build");
    assert_eq!(list.phases()[1].tasks(), []);
}

#[rstest::rstest]
#[test]
fn set_phase_from_input_appends_when_no_description_matches() {
    // Given a list with one phase.
    let mut list = TaskList::new();
    list.set_from_inputs(&[PhaseInput {
        description: "Existing".to_owned(),
        tasks: vec![],
    }]);

    // When writing a phase with an unmatched description.
    let replaced = list.set_phase_from_input(&PhaseInput {
        description: "Brand new".to_owned(),
        tasks: vec![("Task".to_owned(), TaskStatus::Pending)],
    });

    // Then it is appended, not a replacement.
    assert!(!replaced);
    assert_eq!(list.phases().len(), 2);
    assert_eq!(list.phases()[1].description(), "Brand new");
}

#[rstest::rstest]
#[test]
fn set_phase_from_input_preserves_sibling_phases() {
    // Given a list with two phases.
    let mut list = TaskList::new();
    list.set_from_inputs(&[
        PhaseInput {
            description: "Alpha".to_owned(),
            tasks: vec![("Keep me".to_owned(), TaskStatus::Pending)],
        },
        PhaseInput {
            description: "Beta".to_owned(),
            tasks: vec![("Replace target".to_owned(), TaskStatus::Pending)],
        },
    ]);

    // When rewriting only Beta.
    let replaced = list.set_phase_from_input(&PhaseInput {
        description: "Beta".to_owned(),
        tasks: vec![
            ("Fresh".to_owned(), TaskStatus::Pending),
            ("Extra".to_owned(), TaskStatus::Pending),
        ],
    });

    // Then Beta was replaced.
    assert!(replaced);
    // And Alpha is untouched.
    assert_eq!(list.phases()[0].tasks()[0].description(), "Keep me");
    // And Beta has exactly the new tasks.
    assert_eq!(list.phases()[1].tasks().len(), 2);
    assert_eq!(list.phases()[1].tasks()[0].description(), "Fresh");
}

#[rstest::rstest]
#[test]
fn set_phase_from_input_mints_fresh_ids_not_shared_with_retained_phases() {
    // Given a list with a phase holding a task.
    let mut list = TaskList::new();
    list.set_from_inputs(&[PhaseInput {
        description: "Alpha".to_owned(),
        tasks: vec![("Keep".to_owned(), TaskStatus::Pending)],
    }]);
    let old_tid = list.phases()[0].tasks()[0].id.clone();

    // When appending a new phase via set_phase_from_input.
    let _ = list.set_phase_from_input(&PhaseInput {
        description: "New phase".to_owned(),
        tasks: vec![("t".to_owned(), TaskStatus::Pending)],
    });

    // Then the new phase's task ID differs from the retained task's ID.
    let new_tid = list.phases()[1].tasks()[0].id.clone();
    assert_ne!(new_tid, old_tid);
}

#[rstest::rstest]
#[test]
fn set_phase_from_input_matches_description_after_trimming() {
    // Given a list whose phase description has surrounding whitespace.
    let mut list = TaskList::new();
    list.set_from_inputs(&[PhaseInput {
        description: "  Build  ".to_owned(),
        tasks: vec![],
    }]);

    // When writing with a differently-trimmed description.
    let replaced = list.set_phase_from_input(&PhaseInput {
        description: "Build".to_owned(),
        tasks: vec![],
    });

    // Then the existing phase is replaced (matched on trim).
    assert!(replaced);
    assert_eq!(list.phases().len(), 1);
    assert_eq!(list.phases()[0].description(), "Build");
}

#[rstest::rstest]
#[test]
fn serde_legacy_deferred_task_loads_and_renders() {
    // Given legacy persisted JSON with a task stored under the "Deferred" alias.
    let json = r#"{"phases":[{"id":"pabc","description":"Research","tasks":[
        {"id":"txyz","description":"Old work","status":"Deferred"},
        {"id":"tmmm","description":"Live work","status":"Pending"}
    ]}]}"#;

    // When deserializing.
    let list: TaskList = serde_json::from_str(json).unwrap();

    // Then the legacy task loads as Postponed without error.
    assert_eq!(list.phases().len(), 1);
    assert_eq!(list.phases()[0].tasks()[0].status(), TaskStatus::Postponed);
    // And it is filtered from tool-facing renders.
    let rendered = list.render_text_with_blockers();
    assert!(
        !rendered.contains("Old work"),
        "postponed task should not render; got: {rendered}"
    );
    // And the pending sibling still renders.
    assert!(rendered.contains("Live work"));
}
