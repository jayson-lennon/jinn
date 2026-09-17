// Copyright (C) 2026 Jayson Lennon
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as
// published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Shared payload parsing for the todo write tools.
//!
//! [`todo_set_list`](super::set_list) and
//! [`todo_set_phase`](super::set_phase) accept the same task-entry grammar:
//! a task is either a bare string (created as `Pending`) or an object with a
//! `description` and an optional declarative `status`. Parsing is pure — a
//! payload is fully validated here before any task list state is touched, so
//! the writers can build their replacement atomically.

use jinn_tools_msg::{PhaseInput, TaskStatus};

/// Parses the `status` field of one task entry.
///
/// Omitted or empty means [`TaskStatus::Pending`]. Accepted values are
/// exactly `pending`, `completed`, or `cancelled` (trimmed, case-insensitive).
/// `postponed` and `deferred` are rejected with guidance — postponement is
/// not a declarable status; restructure the phase or cancel the task instead.
fn parse_status(raw: &serde_json::Value, label: &str) -> Result<TaskStatus, String> {
    let Some(text) = raw.as_str() else {
        return Err(format!("{label} has a 'status' but it must be a string"));
    };
    match text.trim().to_ascii_lowercase().as_str() {
        "" | "pending" => Ok(TaskStatus::Pending),
        "completed" => Ok(TaskStatus::Completed),
        "cancelled" => Ok(TaskStatus::Cancelled),
        "postponed" | "deferred" => Err(format!(
            "{label}: 'postponed' is not a declarable status; \
             move the task to a later phase or cancel it instead"
        )),
        other => Err(format!(
            "{label}: unknown status \"{other}\" (expected pending, completed, or cancelled)"
        )),
    }
}

/// Parses one task entry: a bare string or `{description, status?}`.
///
/// `label` names the entry's position (e.g. `"phase 1, task 2"`) so errors
/// point the caller at the exact payload location.
fn parse_task_entry(
    value: &serde_json::Value,
    label: &str,
) -> Result<(String, TaskStatus), String> {
    match value {
        serde_json::Value::String(text) => Ok((text.trim().to_owned(), TaskStatus::Pending)),
        serde_json::Value::Object(_) => {
            let description = match value.get("description").and_then(serde_json::Value::as_str) {
                Some(text) => text.trim().to_owned(),
                None => return Err(format!("{label} is missing 'description'")),
            };
            let status = match value.get("status") {
                Some(v) if !v.is_null() => parse_status(v, label)?,
                _ => TaskStatus::Pending,
            };
            Ok((description, status))
        }
        _ => Err(format!(
            "{label} must be a string or an object with 'description'"
        )),
    }
}

/// Parses one phase payload — `{description, tasks?}` — into its trimmed
/// description and parsed task entries.
///
/// `label` names the phase in error messages (e.g. `"phase at index 0"` for
/// `todo_set_list`, `"phase"` for `todo_set_phase`). An empty-after-trim
/// description is rejected: it could never be matched by description again.
///
/// # Errors
///
/// Returns the payload error message when the phase lacks a usable
/// `description`, has a non-array `tasks`, or any task entry is malformed.
pub fn parse_phase_body(value: &serde_json::Value, label: &str) -> Result<PhaseInput, String> {
    let Some(description) = value.get("description").and_then(serde_json::Value::as_str) else {
        return Err(format!("{label} is missing 'description'"));
    };
    let description = description.trim().to_owned();
    if description.is_empty() {
        return Err(format!("{label} must have a non-empty description"));
    }

    let mut tasks = Vec::new();
    if let Some(entries_val) = value.get("tasks").filter(|v| !v.is_null()) {
        let Some(entries) = entries_val.as_array() else {
            return Err(format!("{label} has 'tasks' but it must be an array"));
        };
        for (i, entry) in entries.iter().enumerate() {
            let task_label = format!("{label}, task at index {i}");
            tasks.push(parse_task_entry(entry, &task_label)?);
        }
    }
    Ok(PhaseInput { description, tasks })
}

/// Parses the `phases` array of a whole-list payload.
///
/// Each element is parsed by [`parse_phase_body`]; the first failure aborts
/// with that phase's error message.
///
/// # Errors
///
/// Returns the payload error message of the first malformed phase.
pub fn parse_phases_array(entries: &[serde_json::Value]) -> Result<Vec<PhaseInput>, String> {
    let mut phases = Vec::with_capacity(entries.len());
    for (i, value) in entries.iter().enumerate() {
        let label = format!("phase at index {i}");
        phases.push(parse_phase_body(value, &label)?);
    }
    Ok(phases)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        clippy::uninlined_format_args,
        reason = "test code"
    )]
    use serde_json::json;

    use super::*;

    #[rstest::rstest]
    #[test]
    fn parse_phases_array_bare_strings_default_to_pending() {
        // Given a phases payload with bare-string task entries.
        let payload = json!([
            { "description": "Research", "tasks": ["Read docs", "Call API"] }
        ]);
        let entries = payload.as_array().expect("array");

        // When parsing the array.
        let phases = parse_phases_array(entries).expect("parses");

        // Then the tasks carry Pending status.
        assert_eq!(phases[0].description, "Research");
        assert_eq!(
            phases[0].tasks,
            vec![
                ("Read docs".to_owned(), TaskStatus::Pending),
                ("Call API".to_owned(), TaskStatus::Pending)
            ]
        );
    }

    #[rstest::rstest]
    #[test]
    fn parse_phases_array_declared_statuses_round_trip() {
        // Given a payload where tasks declare completed and cancelled statuses.
        let payload = json!([
            { "description": "P", "tasks": [
                { "description": "done", "status": "completed" },
                { "description": "dropped", "status": "cancelled" },
                { "description": "todo" }
            ]}
        ]);
        let entries = payload.as_array().expect("array");

        // When parsing the array.
        let phases = parse_phases_array(entries).expect("parses");

        // Then the declared statuses are preserved.
        assert_eq!(phases[0].tasks[0].1, TaskStatus::Completed);
        // And the third task defaults to Pending.
        assert_eq!(phases[0].tasks[2].1, TaskStatus::Pending);
    }

    #[rstest::rstest]
    #[case("postponed")]
    #[case("deferred")]
    #[test]
    fn parse_status_rejects_postponed_and_deferred(#[case] status: &str) {
        // Given a task entry declaring the non-declarable status.
        let payload = json!({ "description": "P", "tasks": [
            { "description": "t", "status": status }
        ]});

        // When parsing the phase.
        let result = parse_phase_body(&payload, "phase at index 0");

        // Then it is rejected with guided messaging.
        let msg = result.expect_err("rejected");
        assert!(
            msg.contains("not a declarable status"),
            "expected guided error, got: {msg}"
        );
        // And the message names the alternatives.
        assert!(msg.contains("cancel it instead"), "got: {msg}");
    }

    #[rstest::rstest]
    #[test]
    fn parse_status_accepts_capitalized_values() {
        // Given a payload with capitalized status values.
        let payload = json!({ "description": "P", "tasks": [
            { "description": "a", "status": "Completed" },
            { "description": "b", "status": "PENDING" },
            { "description": "c", "status": "Cancelled" }
        ]});

        // When parsing the phase.
        let phases = parse_phase_body(&payload, "phase").expect("parses");

        // Then statuses normalize case-insensitively.
        assert_eq!(phases.tasks[0].1, TaskStatus::Completed);
        assert_eq!(phases.tasks[1].1, TaskStatus::Pending);
        assert_eq!(phases.tasks[2].1, TaskStatus::Cancelled);
    }
}
