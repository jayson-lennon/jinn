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

use crate::feat::todo_list::{PhaseId, PhaseInput, TaskId, TaskList, TaskListError, TaskPosition, TaskStatus};

// ---------------------------------------------------------------------------
// add_phase
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[test]
fn add_phase_creates_phase_with_id_and_description() {
    let mut list = TaskList::new();
    let id = list.add_phase("Research");
    // ID should start with 'p' and be 4 chars total (prefix + 3 random chars).
    let id_str = id.to_string();
    assert!(id_str.starts_with('p'));
    assert_eq!(id_str.len(), 4);
    let phase = list.get_phase(&id).unwrap();
    assert_eq!(phase.description(), "Research");
    assert!(phase.is_empty());
}

#[rstest::rstest]
#[test]
fn add_phase_generates_distinct_ids() {
    let mut list = TaskList::new();
    let id1 = list.add_phase("Research");
    let id2 = list.add_phase("Build");
    let id3 = list.add_phase("Test");
    assert_ne!(id1, id2);
    assert_ne!(id2, id3);
    assert_ne!(id1, id3);
}

#[rstest::rstest]
#[test]
fn add_phase_returns_distinct_ids() {
    let mut list = TaskList::new();
    let id1 = list.add_phase("A");
    let id2 = list.add_phase("B");
    assert_ne!(id1, id2);
}

// ---------------------------------------------------------------------------
// add_task - append (no position)
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[test]
fn add_task_appends_to_phase_end_when_no_position() {
    let mut list = TaskList::new();
    let pid = list.add_phase("Research");
    let t1 = list.add_task(&pid, "Read docs", TaskPosition::End).unwrap();
    let t2 = list.add_task(&pid, "Call API", TaskPosition::End).unwrap();
    let phase = list.get_phase(&pid).unwrap();
    assert_eq!(phase.tasks().len(), 2);
    assert_eq!(&phase.tasks()[0].id, &t1);
    assert_eq!(&phase.tasks()[1].id, &t2);
}

// ---------------------------------------------------------------------------
// add_task - insert_after
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[test]
fn add_task_inserts_after_specified_task() {
    let mut list = TaskList::new();
    let pid = list.add_phase("Build");
    let t1 = list
        .add_task(&pid, "Write code", TaskPosition::End)
        .unwrap();
    let t2 = list.add_task(&pid, "Test code", TaskPosition::End).unwrap();
    // Insert t3 after t1: expected order [t1, t3, t2]
    let t3 = list
        .add_task(&pid, "Write docs", TaskPosition::After(t1.clone()))
        .unwrap();
    let phase = list.get_phase(&pid).unwrap();
    assert_eq!(phase.tasks().len(), 3);
    assert_eq!(&phase.tasks()[0].id, &t1);
    assert_eq!(&phase.tasks()[1].id, &t3);
    assert_eq!(&phase.tasks()[2].id, &t2);
}

// ---------------------------------------------------------------------------
// add_task - insert_before
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[test]
fn add_task_inserts_before_specified_task() {
    let mut list = TaskList::new();
    let pid = list.add_phase("Build");
    let t1 = list
        .add_task(&pid, "Write code", TaskPosition::End)
        .unwrap();
    let t2 = list.add_task(&pid, "Test code", TaskPosition::End).unwrap();
    // Insert t3 before t2: expected order [t1, t3, t2]
    let t3 = list
        .add_task(&pid, "Write docs", TaskPosition::Before(t2.clone()))
        .unwrap();
    let phase = list.get_phase(&pid).unwrap();
    assert_eq!(phase.tasks().len(), 3);
    assert_eq!(&phase.tasks()[0].id, &t1);
    assert_eq!(&phase.tasks()[1].id, &t3);
    assert_eq!(&phase.tasks()[2].id, &t2);
}

// ---------------------------------------------------------------------------
// add_task - error cases
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[test]
fn add_task_rejects_invalid_phase_id() {
    let mut list = TaskList::new();
    let bad_id = PhaseId::new_for_test("p99");
    let result = list.add_task(&bad_id, "Task", TaskPosition::End);
    assert_eq!(result, Err(TaskListError::PhaseNotFound(bad_id)));
}

#[rstest::rstest]
#[test]
fn add_task_with_after_rejects_task_not_in_phase() {
    let mut list = TaskList::new();
    let pid = list.add_phase("Phase A");
    let _t1 = list.add_task(&pid, "Task A1", TaskPosition::End).unwrap();
    let other_id = list.add_phase("Phase B");
    let other_t = list
        .add_task(&other_id, "Task B1", TaskPosition::End)
        .unwrap();

    // Try to insert after a task from a different phase.
    let result = list.add_task(&pid, "New", TaskPosition::After(other_t.clone()));
    assert_eq!(
        result,
        Err(TaskListError::TaskNotInPhase {
            task_id: other_t,
            phase_id: pid,
        })
    );
}

// ---------------------------------------------------------------------------
// complete_task
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[test]
fn complete_task_marks_as_completed() {
    let mut list = TaskList::new();
    let pid = list.add_phase("Build");
    let t1 = list
        .add_task(&pid, "Write code", TaskPosition::End)
        .unwrap();
    let _t2 = list
        .add_task(&pid, "Write tests", TaskPosition::End)
        .unwrap();

    // Before: both pending.
    {
        let p = list.get_phase(&pid).unwrap();
        assert_eq!(p.tasks()[0].status(), TaskStatus::Pending);
        assert_eq!(p.tasks()[1].status(), TaskStatus::Pending);
    }

    list.complete_task(&t1).unwrap();

    // After: t1 completed, t2 still pending.
    {
        let p = list.get_phase(&pid).unwrap();
        assert_eq!(p.tasks()[0].status(), TaskStatus::Completed);
        assert_eq!(p.tasks()[1].status(), TaskStatus::Pending);
    }
}

#[rstest::rstest]
#[test]
fn complete_task_rejects_unknown_id() {
    let mut list = TaskList::new();
    list.add_phase("Build");
    let bad_id = crate::feat::todo_list::TaskId::new_for_test("t99");
    let result = list.complete_task(&bad_id);
    assert_eq!(result, Err(TaskListError::TaskNotFound(bad_id)));
}

// ---------------------------------------------------------------------------
// is_empty
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[test]
fn is_empty_true_when_no_phases() {
    let list = TaskList::new();
    assert!(list.is_empty());
}

#[rstest::rstest]
#[test]
fn is_empty_false_when_has_phases() {
    let mut list = TaskList::new();
    list.add_phase("Build");
    assert!(!list.is_empty());
}

// ---------------------------------------------------------------------------
// get_phase / get_task
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[test]
fn get_phase_returns_none_for_missing_id() {
    let list = TaskList::new();
    let missing = PhaseId::new_for_test("p99");
    assert!(list.get_phase(&missing).is_none());
}

#[rstest::rstest]
#[test]
fn get_task_finds_task_from_any_phase() {
    let mut list = TaskList::new();
    let p1 = list.add_phase("Research");
    let p2 = list.add_phase("Build");
    let t1 = list.add_task(&p1, "Read", TaskPosition::End).unwrap();
    let t2 = list.add_task(&p2, "Code", TaskPosition::End).unwrap();
    assert!(list.get_task(&t1).is_some());
    assert!(list.get_task(&t2).is_some());
}

#[rstest::rstest]
#[test]
fn get_task_returns_none_for_missing_id() {
    let list = TaskList::new();
    let missing = crate::feat::todo_list::TaskId::new_for_test("t99");
    assert!(list.get_task(&missing).is_none());
}

// ---------------------------------------------------------------------------
// render_text
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[test]
fn render_text_returns_empty_placeholder() {
    let list = TaskList::new();
    assert_eq!(list.render_text(), "No phases defined.");
}

#[rstest::rstest]
#[test]
fn render_text_shows_phases_and_tasks() {
    let mut list = TaskList::new();
    let pid = list.add_phase("Research");
    let _t1 = list.add_task(&pid, "Read docs", TaskPosition::End).unwrap();
    let _t2 = list.add_task(&pid, "Call API", TaskPosition::End).unwrap();

    let rendered = list.render_text();
    assert!(rendered.contains("Phase 1: Research"));
    assert!(rendered.contains("[ ] Read docs"));
    assert!(rendered.contains("[ ] Call API"));
    // Phase ID should appear in the output.
    assert!(rendered.contains(&format!("[{pid}]")));
}

#[rstest::rstest]
#[test]
fn render_text_shows_completed_task() {
    let mut list = TaskList::new();
    let pid = list.add_phase("Build");
    let t1 = list
        .add_task(&pid, "Write code", TaskPosition::End)
        .unwrap();
    list.complete_task(&t1).unwrap();

    let rendered = list.render_text();
    assert!(rendered.contains("[✓] Write code"));
}

#[rstest::rstest]
#[test]
fn render_phase_text_for_single_phase() {
    let mut list = TaskList::new();
    let pid = list.add_phase("Research");
    list.add_task(&pid, "Read docs", TaskPosition::End).unwrap();

    let rendered = list.render_phase_text(&pid).unwrap();
    assert!(rendered.contains("Phase 1: Research"));
    assert!(rendered.contains("Read docs"));
}

#[rstest::rstest]
#[test]
fn render_phase_text_returns_none_for_missing_phase() {
    let list = TaskList::new();
    let missing = PhaseId::new_for_test("p99");
    assert!(list.render_phase_text(&missing).is_none());
}

// ---------------------------------------------------------------------------
// Serde roundtrip
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[test]
fn serde_roundtrip_preserves_state() {
    let mut list = TaskList::new();
    let pid = list.add_phase("Research");
    let t1 = list.add_task(&pid, "Read docs", TaskPosition::End).unwrap();
    list.complete_task(&t1).unwrap();
    list.add_task(&pid, "Call API", TaskPosition::End).unwrap();

    let json = serde_json::to_string(&list).unwrap();
    let restored: TaskList = serde_json::from_str(&json).unwrap();

    assert_eq!(list, restored);
    assert!(!restored.is_empty());

    let pid2 = list.add_phase("Research 2");
    let _t3 = list.add_task(&pid2, "More", TaskPosition::End).unwrap();
    let json2 = serde_json::to_string(&list).unwrap();
    let restored2: TaskList = serde_json::from_str(&json2).unwrap();
    assert_eq!(list, restored2);
}

#[rstest::rstest]
#[test]
fn serde_default_creates_empty_list() {
    let json = "{}";
    let list: TaskList = serde_json::from_str(json).unwrap();
    assert!(list.is_empty());
}

#[rstest::rstest]
#[test]
fn serde_deserializes_partial_json() {
    // Only phases field (no counters) - a valid old-format JSON.
    let json = r#"{"phases":[]}"#;
    let list: TaskList = serde_json::from_str(json).unwrap();
    assert!(list.is_empty());
}

// ---------------------------------------------------------------------------
// TaskId / PhaseId helpers (exposed for testing via pub(crate))
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[test]
fn id_display_format() {
    let pid = PhaseId::new_for_test("p1");
    let tid = crate::feat::todo_list::TaskId::new_for_test("t2");
    assert_eq!(format!("{pid}"), "p1");
    assert_eq!(format!("{tid}"), "t2");
}

// ---------------------------------------------------------------------------
// Random ID generation
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[test]
fn id_format_is_correct() {
    // Phase IDs start with 'p' and are 4 chars total.
    // Task IDs start with 't' and are 4 chars total.
    // The remaining 3 chars are from the charset [a-z0-9 minus {p, t}].
    let mut list = TaskList::new();
    let pid = list.add_phase("Phase");
    let tid = list.add_task(&pid, "Task", TaskPosition::End).unwrap();

    let pid_str = pid.to_string();
    let tid_str = tid.to_string();

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
    let mut list = TaskList::new();
    let mut phase_ids = Vec::new();
    let mut task_ids = Vec::new();

    for i in 0..50 {
        let pid = list.add_phase(&format!("Phase {i}"));
        phase_ids.push(pid.clone());
        let tid = list
            .add_task(&pid, &format!("Task {i}"), TaskPosition::End)
            .unwrap();
        task_ids.push(tid);
    }

    // All phase IDs are unique.
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
    // Old-format JSON with counter fields should deserialize cleanly.
    let json = r#"{"phases":[],"next_phase_id":5,"next_task_id":10}"#;
    let mut list: TaskList = serde_json::from_str(json).unwrap();
    assert!(list.is_empty());
    // Should be able to add phases/tasks after loading old format.
    let pid = list.add_phase("New phase");
    let tid = list.add_task(&pid, "New task", TaskPosition::End).unwrap();
    assert_eq!(pid.to_string().len(), 4);
    assert_eq!(tid.to_string().len(), 4);
}

#[rstest::rstest]
#[test]
fn postpone_task_marks_source_and_creates_copy() {
    let mut list = TaskList::new();
    let p1 = list.add_phase("Research");
    let t1 = list.add_task(&p1, "Read docs", TaskPosition::End).unwrap();
    let p2 = list.add_phase("Build");
    let t2 = list.add_task(&p2, "Write code", TaskPosition::End).unwrap();

    let new_tid = list.postpone_task(&t1, TaskPosition::After(t2)).unwrap();

    // Source task should be deferred.
    let source = list.get_task(&t1).unwrap();
    assert_eq!(source.status(), TaskStatus::Postponed);

    // New task should be pending in phase 2.
    let copy = list.get_task(&new_tid).unwrap();
    assert_eq!(copy.status(), TaskStatus::Pending);
    assert_eq!(copy.description(), "Read docs");
    assert_ne!(copy.id(), source.id());

    // Phase 2 should have the original task + the copy.
    let phase2 = list.get_phase(&p2).unwrap();
    assert_eq!(phase2.tasks().len(), 2);
}

#[rstest::rstest]
#[test]
fn postpone_task_same_phase() {
    let mut list = TaskList::new();
    let p1 = list.add_phase("Research");
    let t1 = list.add_task(&p1, "Read docs", TaskPosition::End).unwrap();
    let t2 = list.add_task(&p1, "Call API", TaskPosition::End).unwrap();

    let new_tid = list.postpone_task(&t1, TaskPosition::Before(t2)).unwrap();

    // Source postponed.
    let source = list.get_task(&t1).unwrap();
    assert_eq!(source.status(), TaskStatus::Postponed);

    // Copy is pending with same description.
    let copy = list.get_task(&new_tid).unwrap();
    assert_eq!(copy.status(), TaskStatus::Pending);
    assert_eq!(copy.description(), "Read docs");

    // Same phase has 3 tasks now (postponed + pending copy + original pending).
    let phase = list.get_phase(&p1).unwrap();
    assert_eq!(phase.tasks().len(), 3);
}

#[rstest::rstest]
#[test]
fn postpone_task_error_on_missing_source() {
    let mut list = TaskList::new();
    let p1 = list.add_phase("Build");
    let t1 = list.add_task(&p1, "Write code", TaskPosition::End).unwrap();

    let fake_id = TaskId::new_for_test("t99");
    let result = list.postpone_task(&fake_id, TaskPosition::After(t1));
    assert!(matches!(result, Err(TaskListError::TaskNotFound(_))));
}

#[rstest::rstest]
#[test]
fn postpone_task_error_on_missing_reference() {
    let mut list = TaskList::new();
    let p1 = list.add_phase("Build");
    let t1 = list.add_task(&p1, "Write code", TaskPosition::End).unwrap();

    let fake_ref = TaskId::new_for_test("t99");
    let result = list.postpone_task(&t1, TaskPosition::After(fake_ref));
    assert!(matches!(result, Err(TaskListError::TaskNotFound(_))));
}

#[rstest::rstest]
#[test]
fn postpone_task_error_on_self_reference() {
    let mut list = TaskList::new();
    let p1 = list.add_phase("Build");
    let t1 = list.add_task(&p1, "Write code", TaskPosition::End).unwrap();

    let result = list.postpone_task(&t1, TaskPosition::After(t1.clone()));
    assert!(matches!(result, Err(TaskListError::SelfReference(_))));
}

#[rstest::rstest]
#[test]
fn postpone_task_error_on_already_postponed() {
    let mut list = TaskList::new();
    let p1 = list.add_phase("Research");
    let t1 = list.add_task(&p1, "Read docs", TaskPosition::End).unwrap();
    let p2 = list.add_phase("Build");
    let t2 = list.add_task(&p2, "Write code", TaskPosition::End).unwrap();

    // Postpone once.
    list.postpone_task(&t1, TaskPosition::After(t2.clone()))
        .unwrap();

    // Try to postpone again.
    let result = list.postpone_task(&t1, TaskPosition::After(t2));
    assert!(matches!(result, Err(TaskListError::AlreadyPostponed(_))));
}

#[rstest::rstest]
#[test]
fn render_text_excludes_postponed() {
    let mut list = TaskList::new();
    let p1 = list.add_phase("Research");
    let t1 = list.add_task(&p1, "Read docs", TaskPosition::End).unwrap();
    let p2 = list.add_phase("Build");
    let t2 = list.add_task(&p2, "Write code", TaskPosition::End).unwrap();

    let new_tid = list.postpone_task(&t1, TaskPosition::After(t2)).unwrap();

    let rendered = list.render_text();

    // The deferred source should NOT appear as a task line.
    assert!(
        !rendered.contains(&format!("- [ ] Read docs [{t1}]")),
        "postponed task should not appear in render"
    );
    // The copy should appear.
    assert!(
        rendered.contains(&format!("- [ ] Read docs [{new_tid}]")),
        "copy should appear in render"
    );
}

#[rstest::rstest]
#[test]
fn render_text_shows_no_tasks_when_all_postponed() {
    let mut list = TaskList::new();
    let p1 = list.add_phase("Research");
    let t1 = list.add_task(&p1, "Read docs", TaskPosition::End).unwrap();
    let p2 = list.add_phase("Build");
    let t2 = list.add_task(&p2, "Write code", TaskPosition::End).unwrap();

    // Postpone the only task in phase 1.
    list.postpone_task(&t1, TaskPosition::After(t2)).unwrap();

    let rendered = list.render_text();

    // Phase 1 should show (no tasks) since its only task is deferred.
    let phase1_section = rendered.split("Phase 1").nth(1).unwrap();
    let phase1_text = phase1_section.split("Phase 2").next().unwrap();
    assert!(
        phase1_text.contains("(no tasks)"),
        "phase with all postponed tasks should show (no tasks)"
    );
}

#[rstest::rstest]
#[test]
fn render_phase_text_excludes_postponed() {
    let mut list = TaskList::new();
    let p1 = list.add_phase("Research");
    let t1 = list.add_task(&p1, "Read docs", TaskPosition::End).unwrap();
    let p2 = list.add_phase("Build");
    let t2 = list.add_task(&p2, "Write code", TaskPosition::End).unwrap();

    list.postpone_task(&t1, TaskPosition::After(t2)).unwrap();

    let rendered = list.render_phase_text(&p1).unwrap();
    assert!(
        rendered.contains("(no tasks)"),
        "phase with only postponed task should show (no tasks)"
    );
}

#[rstest::rstest]
#[test]
fn postpone_to_phase_appends_to_existing_phase() {
    let mut list = TaskList::new();
    let p1 = list.add_phase("Research");
    let t1 = list.add_task(&p1, "Read docs", TaskPosition::End).unwrap();
    let p2 = list.add_phase("Build");

    let new_tid = list.postpone_to_phase(&t1, &p2).unwrap();

    // Source postponed.
    let source = list.get_task(&t1).unwrap();
    assert_eq!(source.status(), TaskStatus::Postponed);

    // Copy is pending in phase 2.
    let copy = list.get_task(&new_tid).unwrap();
    assert_eq!(copy.status(), TaskStatus::Pending);
    assert_eq!(copy.description(), "Read docs");

    // Phase 2 has 1 task (the copy).
    let phase2 = list.get_phase(&p2).unwrap();
    assert_eq!(phase2.tasks().len(), 1);
}

#[rstest::rstest]
#[test]
fn postpone_to_phase_errors_on_missing_source() {
    let mut list = TaskList::new();
    let p1 = list.add_phase("Build");

    let fake_id = TaskId::new_for_test("t99");
    let result = list.postpone_to_phase(&fake_id, &p1);
    assert!(matches!(result, Err(TaskListError::TaskNotFound(_))));
}

#[rstest::rstest]
#[test]
fn postpone_to_phase_errors_on_missing_phase() {
    let mut list = TaskList::new();
    let p1 = list.add_phase("Build");
    let t1 = list.add_task(&p1, "Write code", TaskPosition::End).unwrap();

    let fake_phase = PhaseId::new_for_test("p99");
    let result = list.postpone_to_phase(&t1, &fake_phase);
    assert!(matches!(result, Err(TaskListError::PhaseNotFound(_))));
}

#[rstest::rstest]
#[test]
fn postpone_to_phase_errors_on_already_postponed() {
    let mut list = TaskList::new();
    let p1 = list.add_phase("Research");
    let t1 = list.add_task(&p1, "Read docs", TaskPosition::End).unwrap();
    let p2 = list.add_phase("Build");

    // Postpone once.
    list.postpone_to_phase(&t1, &p2).unwrap();

    // Try to postpone again.
    let result = list.postpone_to_phase(&t1, &p2);
    assert!(matches!(result, Err(TaskListError::AlreadyPostponed(_))));
}

#[rstest::rstest]
#[test]
fn set_from_descriptions_creates_phases_and_tasks() {
    let mut list = TaskList::new();
    list.set_from_descriptions(vec![
        (
            "Research".to_owned(),
            vec!["Read docs".to_owned(), "Call API".to_owned()],
        ),
        ("Build".to_owned(), vec!["Write code".to_owned()]),
        ("Deploy".to_owned(), vec![]),
    ])
    .unwrap();

    assert_eq!(list.phases().len(), 3);
    assert_eq!(list.phases()[0].description(), "Research");
    assert_eq!(list.phases()[0].tasks().len(), 2);
    assert_eq!(list.phases()[1].description(), "Build");
    assert_eq!(list.phases()[1].tasks().len(), 1);
    assert_eq!(list.phases()[2].description(), "Deploy");
    assert!(list.phases()[2].is_empty());

    // All tasks are pending.
    for phase in list.phases() {
        for task in phase.tasks() {
            assert_eq!(task.status(), TaskStatus::Pending);
        }
    }

    // All IDs are unique.
    let mut ids: Vec<String> = list
        .phases()
        .iter()
        .flat_map(|p| p.tasks().iter().map(|t| t.id().to_string()))
        .collect();
    ids.sort();
    ids.dedup();
    assert_eq!(
        ids.len(),
        list.phases().iter().map(|p| p.tasks().len()).sum::<usize>()
    );
}

#[rstest::rstest]
#[test]
fn set_from_descriptions_replaces_existing() {
    let mut list = TaskList::new();
    let p1 = list.add_phase("Old");
    list.add_task(&p1, "Old task", TaskPosition::End).unwrap();

    list.set_from_descriptions(vec![("New".to_owned(), vec!["New task".to_owned()])])
        .unwrap();

    assert_eq!(list.phases().len(), 1);
    assert_eq!(list.phases()[0].description(), "New");
    assert_eq!(list.phases()[0].tasks().len(), 1);
    assert_eq!(list.phases()[0].tasks()[0].description(), "New task");
}

#[rstest::rstest]
#[test]
fn set_from_descriptions_errors_on_empty() {
    let mut list = TaskList::new();
    let result = list.set_from_descriptions(vec![]);
    assert!(matches!(result, Err(TaskListError::EmptyPhasesList)));
}

// ---------------------------------------------------------------------------
// clear
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[test]
fn clear_empties_all_phases() {
    // Given a task list with multiple phases and tasks.
    let mut list = TaskList::new();
    list.set_from_descriptions(vec![
        ("Research".to_owned(), vec!["Read docs".to_owned()]),
        ("Build".to_owned(), vec!["Write code".to_owned()]),
    ])
    .unwrap();

    // When clearing the list.
    list.clear();

    // Then no phases remain.
    assert!(list.is_empty());
    // And the phase slice is empty.
    assert!(list.phases().is_empty());
}

// ---------------------------------------------------------------------------
// cancel_task
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[test]
fn cancel_task_marks_as_cancelled() {
    let mut list = TaskList::new();
    let pid = list.add_phase("Build");
    let t1 = list
        .add_task(&pid, "Write code", TaskPosition::End)
        .unwrap();

    list.cancel_task(&t1).unwrap();

    let task = list.get_task(&t1).unwrap();
    assert_eq!(task.status(), TaskStatus::Cancelled);
}

#[rstest::rstest]
#[test]
fn cancel_task_errors_on_unknown_task() {
    let mut list = TaskList::new();
    list.add_phase("Build");
    let bad_id = TaskId::new_for_test("t99");
    let result = list.cancel_task(&bad_id);
    assert!(matches!(result, Err(TaskListError::TaskNotFound(_))));
}

#[rstest::rstest]
#[test]
fn cancel_task_errors_on_already_cancelled() {
    let mut list = TaskList::new();
    let pid = list.add_phase("Build");
    let t1 = list
        .add_task(&pid, "Write code", TaskPosition::End)
        .unwrap();

    list.cancel_task(&t1).unwrap();
    let result = list.cancel_task(&t1);
    assert!(matches!(result, Err(TaskListError::AlreadyCancelled(_))));
}

#[rstest::rstest]
#[test]
fn cancel_task_can_cancel_postponed_task() {
    let mut list = TaskList::new();
    let p1 = list.add_phase("Research");
    let t1 = list.add_task(&p1, "Read docs", TaskPosition::End).unwrap();
    let p2 = list.add_phase("Build");
    let t2 = list.add_task(&p2, "Write code", TaskPosition::End).unwrap();

    list.postpone_task(&t1, TaskPosition::After(t2)).unwrap();

    // Cancel the postponed source task.
    list.cancel_task(&t1).unwrap();
    let task = list.get_task(&t1).unwrap();
    assert_eq!(task.status(), TaskStatus::Cancelled);
}

#[rstest::rstest]
#[test]
fn complete_task_errors_on_cancelled() {
    let mut list = TaskList::new();
    let pid = list.add_phase("Build");
    let t1 = list
        .add_task(&pid, "Write code", TaskPosition::End)
        .unwrap();

    list.cancel_task(&t1).unwrap();
    let result = list.complete_task(&t1);
    assert!(matches!(result, Err(TaskListError::TaskCancelled(_))));
}

#[rstest::rstest]
#[test]
fn postpone_task_errors_on_cancelled() {
    let mut list = TaskList::new();
    let p1 = list.add_phase("Build");
    let t1 = list.add_task(&p1, "Write code", TaskPosition::End).unwrap();
    let t2 = list.add_task(&p1, "Test code", TaskPosition::End).unwrap();

    list.cancel_task(&t1).unwrap();
    let result = list.postpone_task(&t1, TaskPosition::After(t2));
    assert!(matches!(result, Err(TaskListError::TaskCancelled(_))));
}

#[rstest::rstest]
#[test]
fn postpone_to_phase_errors_on_cancelled() {
    let mut list = TaskList::new();
    let p1 = list.add_phase("Research");
    let t1 = list.add_task(&p1, "Read docs", TaskPosition::End).unwrap();
    let p2 = list.add_phase("Build");

    list.cancel_task(&t1).unwrap();
    let result = list.postpone_to_phase(&t1, &p2);
    assert!(matches!(result, Err(TaskListError::TaskCancelled(_))));
}

#[rstest::rstest]
#[test]
fn render_text_shows_cancelled_with_prefix() {
    let mut list = TaskList::new();
    let pid = list.add_phase("Build");
    let t1 = list
        .add_task(&pid, "Write code", TaskPosition::End)
        .unwrap();
    list.cancel_task(&t1).unwrap();

    let rendered = list.render_text();
    assert!(rendered.contains("CANCELLED: Write code"));
    assert!(rendered.contains("[\u{2717}]"));
}

#[rstest::rstest]
#[test]
fn render_text_hides_postponed_shows_cancelled() {
    let mut list = TaskList::new();
    let p1 = list.add_phase("Research");
    let t1 = list.add_task(&p1, "Read docs", TaskPosition::End).unwrap();
    let p2 = list.add_phase("Build");
    let t2 = list.add_task(&p2, "Write code", TaskPosition::End).unwrap();

    list.postpone_task(&t1, TaskPosition::After(t2)).unwrap();

    let t3 = list.add_task(&p1, "Extra task", TaskPosition::End).unwrap();
    list.cancel_task(&t3).unwrap();

    let rendered = list.render_text();
    // Postponed should be hidden.
    assert!(
        !rendered.contains(&format!("- [ ] Read docs [{t1}]")),
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
fn render_phase_text_handles_cancelled() {
    let mut list = TaskList::new();
    let pid = list.add_phase("Build");
    let t1 = list
        .add_task(&pid, "Write code", TaskPosition::End)
        .unwrap();
    list.cancel_task(&t1).unwrap();

    let rendered = list.render_phase_text(&pid).unwrap();
    assert!(rendered.contains("CANCELLED: Write code"));
}

#[rstest::rstest]
#[test]
fn serde_roundtrip_with_cancelled() {
    let mut list = TaskList::new();
    let pid = list.add_phase("Build");
    let t1 = list
        .add_task(&pid, "Write code", TaskPosition::End)
        .unwrap();
    list.cancel_task(&t1).unwrap();

    let json = serde_json::to_string(&list).unwrap();
    let restored: TaskList = serde_json::from_str(&json).unwrap();
    assert_eq!(list, restored);
}

// ---------------------------------------------------------------------------
// has_pending_work
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[test]
fn has_pending_work_true_when_any_pending() {
    let mut list = TaskList::new();
    let pid = list.add_phase("Build");
    let t1 = list
        .add_task(&pid, "Write code", TaskPosition::End)
        .unwrap();
    let _t2 = list
        .add_task(&pid, "Write tests", TaskPosition::End)
        .unwrap();
    list.complete_task(&t1).unwrap();

    let phase = list.get_phase(&pid).unwrap();
    assert!(
        phase.has_pending_work(),
        "phase with at least one pending task has pending work"
    );
}

#[rstest::rstest]
#[test]
fn has_pending_work_false_when_only_completed_cancelled_postponed() {
    let mut list = TaskList::new();
    let pid = list.add_phase("Build");
    let t1 = list
        .add_task(&pid, "Write code", TaskPosition::End)
        .unwrap();
    let t2 = list
        .add_task(&pid, "Write tests", TaskPosition::End)
        .unwrap();
    let t3 = list
        .add_task(&pid, "Write docs", TaskPosition::End)
        .unwrap();
    let other_pid = list.add_phase("Other");
    let t_other = list
        .add_task(&other_pid, "Anchor", TaskPosition::End)
        .unwrap();

    list.complete_task(&t1).unwrap();
    list.cancel_task(&t2).unwrap();
    list.postpone_task(&t3, TaskPosition::After(t_other))
        .unwrap();

    let phase = list.get_phase(&pid).unwrap();
    assert!(
        !phase.has_pending_work(),
        "phase with only completed/cancelled/postponed tasks has no pending work"
    );
}

// ---------------------------------------------------------------------------
// active_phase
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[test]
fn active_phase_returns_none_when_empty() {
    let list = TaskList::new();
    assert!(list.active_phase().is_none());
}

#[rstest::rstest]
#[test]
fn active_phase_returns_earliest_with_pending() {
    let mut list = TaskList::new();
    let p1 = list.add_phase("Done");
    let t1 = list.add_task(&p1, "Done task", TaskPosition::End).unwrap();
    list.complete_task(&t1).unwrap();
    let p2 = list.add_phase("Active");
    list.add_task(&p2, "Pending task", TaskPosition::End)
        .unwrap();
    let p3 = list.add_phase("Later");
    list.add_task(&p3, "Pending task", TaskPosition::End)
        .unwrap();

    let active = list.active_phase().expect("active phase should exist");
    assert_eq!(active.id, p2);
}

#[rstest::rstest]
#[test]
fn active_phase_returns_none_when_all_complete() {
    let mut list = TaskList::new();
    let p1 = list.add_phase("Build");
    let t1 = list.add_task(&p1, "Write code", TaskPosition::End).unwrap();
    list.complete_task(&t1).unwrap();

    assert!(list.active_phase().is_none());
}

#[rstest::rstest]
#[test]
fn active_phase_skips_phase_with_only_postponed_cancelled_completed() {
    let mut list = TaskList::new();
    let p1 = list.add_phase("All-stale");
    let t1 = list.add_task(&p1, "Completed", TaskPosition::End).unwrap();
    let t2 = list.add_task(&p1, "Cancelled", TaskPosition::End).unwrap();
    let t3 = list.add_task(&p1, "Postponed", TaskPosition::End).unwrap();
    let p2 = list.add_phase("Has work");
    let t_anchor = list.add_task(&p2, "Anchor", TaskPosition::End).unwrap();
    let p3 = list.add_phase("Has work 2");
    list.add_task(&p3, "Other", TaskPosition::End).unwrap();

    list.complete_task(&t1).unwrap();
    list.cancel_task(&t2).unwrap();
    list.postpone_task(&t3, TaskPosition::After(t_anchor))
        .unwrap();

    let active = list
        .active_phase()
        .expect("active phase should skip stale phase");
    assert_eq!(active.id, p2);
}

// ---------------------------------------------------------------------------
// completion_counts
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[test]
fn completion_counts_empty_list_is_zero_zero() {
    // Given an empty task list.
    let list = TaskList::new();

    // When counting completion.
    let (completed, total) = list.completion_counts();

    // Then there are no completed tasks and no tasks at all.
    assert_eq!((completed, total), (0, 0));
}

#[rstest::rstest]
#[test]
fn completion_counts_all_pending() {
    // Given a phase with three pending tasks.
    let mut list = TaskList::new();
    let pid = list.add_phase("Research");
    list.add_task(&pid, "Read docs", TaskPosition::End).unwrap();
    list.add_task(&pid, "Call API", TaskPosition::End).unwrap();
    list.add_task(&pid, "Write notes", TaskPosition::End)
        .unwrap();

    // When counting completion.
    let (completed, total) = list.completion_counts();

    // Then nothing is completed but all three are counted.
    assert_eq!((completed, total), (0, 3));
}

#[rstest::rstest]
#[test]
fn completion_counts_all_completed() {
    // Given a phase where every task is completed.
    let mut list = TaskList::new();
    let pid = list.add_phase("Research");
    let t1 = list.add_task(&pid, "Read docs", TaskPosition::End).unwrap();
    let t2 = list.add_task(&pid, "Call API", TaskPosition::End).unwrap();
    let t3 = list
        .add_task(&pid, "Write notes", TaskPosition::End)
        .unwrap();
    list.complete_task(&t1).unwrap();
    list.complete_task(&t2).unwrap();
    list.complete_task(&t3).unwrap();

    // When counting completion.
    let (completed, total) = list.completion_counts();

    // Then all three are completed.
    assert_eq!((completed, total), (3, 3));
}

#[rstest::rstest]
#[test]
fn completion_counts_counts_only_completed() {
    // Given a phase with two completed, one pending, one postponed, one cancelled.
    let mut list = TaskList::new();
    let p1 = list.add_phase("Research");
    let tc1 = list.add_task(&p1, "Done 1", TaskPosition::End).unwrap();
    let tc2 = list.add_task(&p1, "Done 2", TaskPosition::End).unwrap();
    list.add_task(&p1, "Pending", TaskPosition::End).unwrap();
    let tpost = list.add_task(&p1, "Postponed", TaskPosition::End).unwrap();
    let tcan = list.add_task(&p1, "Cancelled", TaskPosition::End).unwrap();
    let p2 = list.add_phase("Build");
    let t_anchor = list.add_task(&p2, "Anchor", TaskPosition::End).unwrap();
    list.complete_task(&tc1).unwrap();
    list.complete_task(&tc2).unwrap();
    list.postpone_task(&tpost, TaskPosition::After(t_anchor))
        .unwrap();
    list.cancel_task(&tcan).unwrap();

    // When counting completion.
    let (completed, total) = list.completion_counts();

    // Then only the two completed count; total is all seven tasks
    // (5 original in p1 + 1 anchor in p2 + 1 postponed copy in p2).
    assert_eq!((completed, total), (2, 7));
}

#[rstest::rstest]
#[test]
fn completion_counts_aggregates_across_phases() {
    // Given tasks split across two phases.
    let mut list = TaskList::new();
    let p1 = list.add_phase("Research");
    let t1 = list.add_task(&p1, "Read docs", TaskPosition::End).unwrap();
    list.add_task(&p1, "Call API", TaskPosition::End).unwrap();
    let p2 = list.add_phase("Build");
    let t3 = list.add_task(&p2, "Write code", TaskPosition::End).unwrap();
    list.complete_task(&t1).unwrap();
    list.complete_task(&t3).unwrap();

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
    list.add_phase("Empty");
    let p2 = list.add_phase("Build");
    let t1 = list.add_task(&p2, "Write code", TaskPosition::End).unwrap();
    let t2 = list
        .add_task(&p2, "Write tests", TaskPosition::End)
        .unwrap();
    list.complete_task(&t1).unwrap();
    list.complete_task(&t2).unwrap();

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
    let list = TaskList::new();
    assert_eq!(list.render_text_with_blockers(), "No phases defined.");
}

#[rstest::rstest]
#[test]
fn render_text_with_blockers_no_prefix_for_single_phase() {
    let mut list = TaskList::new();
    let pid = list.add_phase("Research");
    list.add_task(&pid, "Read docs", TaskPosition::End).unwrap();

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
    let mut list = TaskList::new();
    let p1 = list.add_phase("Done");
    let t1 = list.add_task(&p1, "Done task", TaskPosition::End).unwrap();
    list.complete_task(&t1).unwrap();
    let p2 = list.add_phase("Active");
    list.add_task(&p2, "Pending", TaskPosition::End).unwrap();
    let p3 = list.add_phase("Later");
    list.add_task(&p3, "Pending", TaskPosition::End).unwrap();

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
    let mut list = TaskList::new();
    let p1 = list.add_phase("Build");
    let t1 = list.add_task(&p1, "Write code", TaskPosition::End).unwrap();
    list.complete_task(&t1).unwrap();

    let rendered = list.render_text_with_blockers();
    assert!(rendered.contains("Phase 1: Build"));
    assert!(!rendered.contains("(Blocked by previous phase)"));
}

#[rstest::rstest]
#[test]
fn render_text_with_blockers_no_prefix_for_completed_phase() {
    let mut list = TaskList::new();
    let p1 = list.add_phase("Done");
    let t1 = list.add_task(&p1, "Done task", TaskPosition::End).unwrap();
    list.complete_task(&t1).unwrap();
    let p2 = list.add_phase("Active");
    list.add_task(&p2, "Pending", TaskPosition::End).unwrap();

    let rendered = list.render_text_with_blockers();
    assert!(rendered.contains("Phase 1: Done"));
    assert!(!rendered.contains("(Blocked by previous phase) Phase 1"));
}

#[rstest::rstest]
#[test]
fn render_next_block_empty_when_no_phases() {
    let list = TaskList::new();
    assert_eq!(list.render_next_block(), "");
}

#[rstest::rstest]
#[test]
fn render_next_block_empty_when_phases_have_no_tasks() {
    let mut list = TaskList::new();
    list.add_phase("Empty phase");
    // Phase exists but has zero tasks. No tasks ever created.
    assert_eq!(list.render_next_block(), "");
}

#[rstest::rstest]
#[test]
fn render_next_block_points_at_active_phase_next_task() {
    let mut list = TaskList::new();
    let p1 = list.add_phase("Done");
    let t1 = list.add_task(&p1, "Done task", TaskPosition::End).unwrap();
    list.complete_task(&t1).unwrap();
    let p2 = list.add_phase("Active");
    let t2 = list
        .add_task(&p2, "First pending", TaskPosition::End)
        .unwrap();
    list.add_task(&p2, "Second pending", TaskPosition::End)
        .unwrap();

    let block = list.render_next_block();
    assert_eq!(
        block,
        format!("→ NEXT: {} — First pending (2 pending in phase {})", t2, p2)
    );
}

#[rstest::rstest]
#[test]
fn render_next_block_skips_cancelled_tasks() {
    // Cancelled tasks are filtered out; the next Pending task is found.
    // Postponed is implicitly covered by the same TaskStatus::Pending filter
    // (and exercising postpone_task's Pending-copy semantics would conflate
    // two behaviors).
    let mut list = TaskList::new();
    let p1 = list.add_phase("Active");
    let t1 = list.add_task(&p1, "Cancelled", TaskPosition::End).unwrap();
    list.cancel_task(&t1).unwrap();
    let t2 = list.add_task(&p1, "Real next", TaskPosition::End).unwrap();

    let block = list.render_next_block();
    // t1 (Cancelled) should be skipped; t2 is the next.
    assert!(
        block.starts_with(&format!("→ NEXT: {} — Real next", t2)),
        "got: {block}"
    );
}

#[rstest::rstest]
#[test]
fn render_next_block_includes_remaining_count() {
    let mut list = TaskList::new();
    let p1 = list.add_phase("Active");
    let t_first = list.add_task(&p1, "A", TaskPosition::End).unwrap();
    list.add_task(&p1, "B", TaskPosition::End).unwrap();
    list.add_task(&p1, "C", TaskPosition::End).unwrap();

    let block = list.render_next_block();
    assert!(block.contains("3 pending in phase"));
    // First pending task is identified.
    assert!(block.starts_with(&format!("→ NEXT: {} — A", t_first)));
}

#[rstest::rstest]
#[test]
fn render_next_block_all_complete_message() {
    let mut list = TaskList::new();
    let p1 = list.add_phase("One");
    let t1 = list.add_task(&p1, "Do", TaskPosition::End).unwrap();
    list.complete_task(&t1).unwrap();

    let block = list.render_next_block();
    assert_eq!(block, "→ All phases complete — stop.");
}

#[rstest::rstest]
#[test]
fn render_next_block_all_complete_with_multiple_phases() {
    let mut list = TaskList::new();
    let p1 = list.add_phase("One");
    let t1 = list.add_task(&p1, "Do", TaskPosition::End).unwrap();
    list.complete_task(&t1).unwrap();
    let p2 = list.add_phase("Two");
    // p2 exists but has no tasks. Counts as "no tasks ever in this phase".
    // But p1 has had a task, so list is fully done.
    let block = list.render_next_block();
    assert_eq!(block, "→ All phases complete — stop.");
    drop(p2);
}

#[rstest::rstest]
#[test]
fn render_next_block_empty_for_phase_with_no_tasks_anywhere() {
    let mut list = TaskList::new();
    list.add_phase("Empty");
    // No tasks ever added to any phase.
    assert_eq!(list.render_next_block(), "");
}

#[rstest::rstest]
#[test]
fn render_next_block_after_completion_same_phase_remaining() {
    let mut list = TaskList::new();
    let p1 = list.add_phase("Build");
    let t1 = list.add_task(&p1, "First", TaskPosition::End).unwrap();
    let t2 = list.add_task(&p1, "Second", TaskPosition::End).unwrap();

    list.complete_task(&t1).unwrap();

    let block = list.render_next_block_after_completion(&p1);
    assert!(
        block.starts_with(&format!("→ NEXT: {} — Second", t2)),
        "got: {block}"
    );
    assert!(block.contains("1 pending in phase"));
}

#[rstest::rstest]
#[test]
fn render_next_block_after_completion_phase_done_no_later() {
    let mut list = TaskList::new();
    let p1 = list.add_phase("Build");
    let t1 = list.add_task(&p1, "Only", TaskPosition::End).unwrap();
    list.complete_task(&t1).unwrap();

    let block = list.render_next_block_after_completion(&p1);
    assert_eq!(
        block,
        format!("→ Phase {} complete — proceed to verify.", p1)
    );
}

#[rstest::rstest]
#[test]
fn render_next_block_after_completion_phase_done_with_later_blocked() {
    let mut list = TaskList::new();
    let p1 = list.add_phase("Build");
    let t1 = list.add_task(&p1, "Only", TaskPosition::End).unwrap();
    list.complete_task(&t1).unwrap();
    let p2 = list.add_phase("Test");
    list.add_task(&p2, "Later", TaskPosition::End).unwrap();

    let block = list.render_next_block_after_completion(&p1);
    assert_eq!(
        block,
        format!(
            "→ Phase {} complete — proceed to verify. Later phases are blocked until then.",
            p1
        )
    );
}

#[rstest::rstest]
#[test]
fn render_next_block_after_completion_falls_back_when_phase_missing() {
    let mut list = TaskList::new();
    let p1 = list.add_phase("Build");
    list.add_task(&p1, "Task", TaskPosition::End).unwrap();

    let bogus = PhaseId::new(&[]);
    let block = list.render_next_block_after_completion(&bogus);
    // Falls back to global next-task.
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
    let old_pid = list.add_phase("Old");
    list.add_task(&old_pid, "Old task", TaskPosition::End).unwrap();

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
    let p1 = list.add_phase("Build");
    list.add_task(&p1, "Old", TaskPosition::End).unwrap();
    list.add_phase("Build");

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
    assert!(list.phases()[1].tasks().is_empty());
}

#[rstest::rstest]
#[test]
fn set_phase_from_input_appends_when_no_description_matches() {
    // Given a list with one phase.
    let mut list = TaskList::new();
    list.add_phase("Existing");

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
    let p1 = list.add_phase("Alpha");
    list.add_task(&p1, "Keep me", TaskPosition::End).unwrap();
    let p2 = list.add_phase("Beta");
    list.add_task(&p2, "Replace target", TaskPosition::End).unwrap();

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
    let p1 = list.add_phase("Alpha");
    let old_tid = list.add_task(&p1, "Keep", TaskPosition::End).unwrap();

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
    list.add_phase("  Build  ");

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
