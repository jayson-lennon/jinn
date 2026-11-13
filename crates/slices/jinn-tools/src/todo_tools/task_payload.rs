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
//!
//! Models do not always emit an array where the schema asks for one; they
//! send a single object, wrap the array in a `{"item": ...}` envelope, or
//! JSON-encode the whole array into a string. [`normalize_array`] absorbs
//! those shapes before parsing, so a mis-encoded payload costs the caller a
//! retried tool call instead of a wiped task list.
//!
//! Status vocabulary drifts the same way: a model trained on other task tooling
//! says `in_progress` or `done` where jinn declares `pending` and `completed`.
//! `parse_status` resolves those through a synonym table, so a routine
//! progress update costs nothing. Coercion is silent — the rendered list shows
//! the status that was stored, so there is nothing to report back.

use error_stack::Report;
use jinn_tools_msg::{PhaseInput, TaskStatus};

/// Why a todo payload could not be parsed.
///
/// Variants carry everything the model needs in their message: the tool
/// boundary renders a failure as tool-result prose, and a `Report`'s
/// `Display` prints only the outermost context.
#[derive(Debug, wherror::Error)]
pub enum TaskPayloadError {
    /// A field held a number, boolean, or otherwise unusable shape.
    #[error("{message}")]
    NotAnArray {
        /// The ready-to-render complaint naming `field`.
        message: String,
    },
    /// A task entry carried a `status` that was not a string.
    #[error("{label} has a 'status' but it must be a string")]
    StatusNotAString {
        /// The entry's position label.
        label: String,
    },
    /// A status word matched none of the three declarable statuses.
    #[error("{label}: unknown status \"{key}\" (expected pending, completed, or cancelled)")]
    UnknownStatus {
        /// The entry's position label.
        label: String,
        /// The normalized, unrecognized word.
        key: String,
    },
    /// A task entry was neither a string nor a description object.
    #[error("{label} must be a string or an object with 'description'")]
    MalformedEntry {
        /// The entry's position label.
        label: String,
    },
    /// A task entry named no description.
    #[error("{label} is missing 'description'")]
    MissingDescription {
        /// The entry's position label.
        label: String,
    },
    /// A phase description was present but empty after trimming.
    #[error("{label} must have a non-empty description")]
    EmptyDescription {
        /// The phase's position label.
        label: String,
    },
    /// A nested failure, prefixed with the position it occurred at.
    #[error("{label}: {detail}")]
    At {
        /// The phase's or task's position label.
        label: String,
        /// The nested payload error's message.
        detail: String,
    },
}

/// Keys a model reaches for when it wraps an array payload in a single
/// envelope object instead of emitting the array directly. `item`/`items` are
/// generic; the field's own name only counts as a wrapper for itself, because
/// a phase entry legitimately carries a `tasks` key and would otherwise be
/// unwrapped as an envelope and lose everything but its tasks.
const GENERIC_WRAPPER_KEYS: [&str; 2] = ["item", "items"];

/// Returns the wrapper keys valid for `field`: the generic keys plus the
/// field's own name.
fn wrapper_keys(field: &str) -> impl Iterator<Item = &str> {
    GENERIC_WRAPPER_KEYS.into_iter().chain([field])
}

/// Builds the "this must be an array" error for `field`, naming the
/// expectation and showing one worked example of the shape.
fn not_an_array_error(field: &str) -> String {
    match field {
        "tasks" => "'tasks' must be an array of task objects, e.g. \
             {\"tasks\": [{\"description\": \"Read the docs\", \"status\": \"pending\"}]}"
            .to_owned(),
        "phases" => "'phases' must be an array of phase objects, e.g. \
             {\"phases\": [{\"description\": \"Probe\", \"tasks\": []}]}"
            .to_owned(),
        other => format!("'{other}' must be an array, e.g. {{\"{other}\": []}}"),
    }
}

/// Returns the value hiding under the first recognised wrapper key.
fn wrapper_value<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Option<&'a serde_json::Value> {
    wrapper_keys(field).find_map(|key| object.get(key))
}

/// Coerces a model-supplied array field into an array, accepting the shapes
/// models actually emit: a single entry, an envelope object, or a
/// JSON-encoded array. An empty string, `null`, or `{}` all mean "nothing
/// here", matching how an absent array is treated.
///
/// # Errors
///
/// Returns a shape error naming `field` when the value is a number, a
/// boolean, or otherwise carries no recoverable array.
pub fn normalize_array(
    value: &serde_json::Value,
    field: &str,
) -> Result<Vec<serde_json::Value>, Report<TaskPayloadError>> {
    match value {
        serde_json::Value::Array(items) => Ok(items.clone()),
        serde_json::Value::Null => Ok(Vec::new()),
        serde_json::Value::String(text) => normalize_text(text, field),
        // The wrapper check comes first: `{"item": "first task"}` must unwrap
        // to `["first task"]` rather than be read as a single entry named
        // "item" with the real task silently dropped.
        serde_json::Value::Object(object) => match wrapper_value(object, field) {
            Some(inner) => normalize_array(inner, field),
            None if object.is_empty() => Ok(Vec::new()),
            None => Ok(vec![value.clone()]),
        },
        _ => Err(Report::new(TaskPayloadError::NotAnArray {
            message: not_an_array_error(field),
        })),
    }
}

/// Normalizes a string field: empty means nothing here, a JSON-encoded
/// payload is decoded once, anything else is a single entry.
fn normalize_text(
    text: &str,
    field: &str,
) -> Result<Vec<serde_json::Value>, Report<TaskPayloadError>> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        // A decoded string stays a single entry rather than being decoded
        // again, so a payload can never re-parse itself.
        Ok(serde_json::Value::String(inner)) => Ok(vec![serde_json::Value::String(inner)]),
        Ok(decoded) => normalize_array(&decoded, field),
        Err(_) => Ok(vec![serde_json::Value::String(text.to_owned())]),
    }
}

/// Resolves a model-supplied status word to one of the three declarable
/// statuses.
///
/// A model trained on other task tooling reaches for a much wider vocabulary:
/// `in_progress` is the status name in the Codex plan tool, in Claude Code's
/// `TodoWrite`/`TaskUpdate`, and in pi-agent's todo extension, yet jinn has no
/// state to hold it. Every entry below is a true synonym of its target — the
/// same intent spelled differently or in a different register — so a routine
/// progress update resolves instead of failing the whole write.
///
/// Values that name a *different* intent jinn cannot represent are
/// deliberately absent and continue to fail as unknown: `blocked`, `waiting`,
/// `on_hold`, `paused`, `partial`, `failed`, `error`, `in_review`, and the
/// non-declarable `postponed` / `deferred`. The declared values are present
/// too, so every accepted spelling is resolved through this one table.
const STATUS_ALIASES: &[(&str, TaskStatus)] = &[
    ("pending", TaskStatus::Pending),
    ("in_progress", TaskStatus::Pending),
    ("not_started", TaskStatus::Pending),
    ("new", TaskStatus::Pending),
    ("open", TaskStatus::Pending),
    ("todo", TaskStatus::Pending),
    ("to_do", TaskStatus::Pending),
    ("queued", TaskStatus::Pending),
    ("backlog", TaskStatus::Pending),
    ("ready", TaskStatus::Pending),
    ("doing", TaskStatus::Pending),
    ("started", TaskStatus::Pending),
    ("active", TaskStatus::Pending),
    ("working", TaskStatus::Pending),
    ("wip", TaskStatus::Pending),
    ("completed", TaskStatus::Completed),
    ("complete", TaskStatus::Completed),
    ("done", TaskStatus::Completed),
    ("finished", TaskStatus::Completed),
    ("resolved", TaskStatus::Completed),
    ("closed", TaskStatus::Completed),
    ("fixed", TaskStatus::Completed),
    ("shipped", TaskStatus::Completed),
    ("landed", TaskStatus::Completed),
    ("cancelled", TaskStatus::Cancelled),
    ("canceled", TaskStatus::Cancelled),
    ("skipped", TaskStatus::Cancelled),
    ("dropped", TaskStatus::Cancelled),
    ("abandoned", TaskStatus::Cancelled),
    ("obsolete", TaskStatus::Cancelled),
    ("wontfix", TaskStatus::Cancelled),
    ("not_needed", TaskStatus::Cancelled),
    ("rejected", TaskStatus::Cancelled),
    ("removed", TaskStatus::Cancelled),
    ("superseded", TaskStatus::Cancelled),
    ("invalid", TaskStatus::Cancelled),
    ("moot", TaskStatus::Cancelled),
    ("deprecated", TaskStatus::Cancelled),
    ("deleted", TaskStatus::Cancelled),
];

/// Folds a status word to its lookup key: trimmed, lowercased, with `-` and
/// space written as `_`, so `"In Progress"` and `"in-progress"` are one entry.
fn normalize_status_key(text: &str) -> String {
    text.trim().to_ascii_lowercase().replace(['-', ' '], "_")
}

/// Parses the `status` field of one task entry.
///
/// Omitted or empty means [`TaskStatus::Pending`]. A value is resolved through
/// [`STATUS_ALIASES`], so the model vocabulary around `pending`, `completed`,
/// and `cancelled` is accepted and coerced silently; anything else is rejected
/// with the accepted vocabulary named.
fn parse_status(
    raw: &serde_json::Value,
    label: &str,
) -> Result<TaskStatus, Report<TaskPayloadError>> {
    let Some(text) = raw.as_str() else {
        return Err(TaskPayloadError::StatusNotAString {
            label: label.to_owned(),
        }
        .into());
    };
    let key = normalize_status_key(text);
    if key.is_empty() {
        return Ok(TaskStatus::Pending);
    }
    STATUS_ALIASES
        .iter()
        .find(|(alias, _)| *alias == key)
        .map(|(_, status)| *status)
        .ok_or_else(|| {
            TaskPayloadError::UnknownStatus {
                label: label.to_owned(),
                key: key.clone(),
            }
            .into()
        })
}

/// Parses one task entry: a bare string or `{description, status?}`.
///
/// `label` names the entry's position (e.g. `"phase 1, task 2"`) so errors
/// point the caller at the exact payload location.
fn parse_task_entry(
    value: &serde_json::Value,
    label: &str,
) -> Result<(String, TaskStatus), Report<TaskPayloadError>> {
    match value {
        serde_json::Value::String(text) => Ok((text.trim().to_owned(), TaskStatus::Pending)),
        serde_json::Value::Object(_) => {
            let description = match value.get("description").and_then(serde_json::Value::as_str) {
                Some(text) => text.trim().to_owned(),
                None => {
                    return Err(TaskPayloadError::MissingDescription {
                        label: label.to_owned(),
                    }
                    .into());
                }
            };
            let status = match value.get("status") {
                Some(v) if !v.is_null() => parse_status(v, label)?,
                _ => TaskStatus::Pending,
            };
            Ok((description, status))
        }
        _ => Err(TaskPayloadError::MalformedEntry {
            label: label.to_owned(),
        }
        .into()),
    }
}

/// Parses one phase payload — `{description, tasks?}` — into its trimmed
/// description and parsed task entries.
///
/// `label` names the phase in error messages (e.g. `"phase at index 0"` for
/// `todo_set_list`, `"phase"` for `todo_set_phase`). An empty-after-trim
/// description is rejected: it could never be matched by description again.
///
/// An absent, `null`, or empty `tasks` field yields a task-less phase. A
/// `tasks` value in any other non-array shape is run through
/// [`normalize_array`] first.
///
/// # Errors
///
/// Returns the payload error message when the phase lacks a usable
/// `description`, or when any task entry is malformed.
pub fn parse_phase_body(
    value: &serde_json::Value,
    label: &str,
) -> Result<PhaseInput, Report<TaskPayloadError>> {
    let Some(description) = value.get("description").and_then(serde_json::Value::as_str) else {
        return Err(TaskPayloadError::MissingDescription {
            label: label.to_owned(),
        }
        .into());
    };
    let description = description.trim().to_owned();
    if description.is_empty() {
        return Err(TaskPayloadError::EmptyDescription {
            label: label.to_owned(),
        }
        .into());
    }

    let tasks = match value.get("tasks") {
        Some(entries_val) => {
            let entries =
                normalize_array(entries_val, "tasks").map_err(|report| TaskPayloadError::At {
                    label: label.to_owned(),
                    detail: report.to_string(),
                })?;
            entries
                .iter()
                .enumerate()
                .map(|(i, entry)| parse_task_entry(entry, &format!("{label}, task at index {i}")))
                .collect::<Result<Vec<_>, _>>()?
        }
        None => Vec::new(),
    };
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
pub fn parse_phases_array(
    entries: &[serde_json::Value],
) -> Result<Vec<PhaseInput>, Report<TaskPayloadError>> {
    let mut phases = Vec::with_capacity(entries.len());
    for (i, value) in entries.iter().enumerate() {
        let label = format!("phase at index {i}");
        phases.push(parse_phase_body(value, &label).map_err(|report| {
            error_stack::Report::new(TaskPayloadError::At {
                label,
                detail: report.to_string(),
            })
        })?);
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
    #[case::array(json!([{ "description": "A" }]), 1)]
    #[case::null_value(json!(null), 0)]
    #[case::empty_object(json!({}), 0)]
    #[case::empty_string(json!(""), 0)]
    #[case::blank_string(json!("   "), 0)]
    #[case::single_object(json!({ "description": "A" }), 1)]
    #[case::wrapped_array(json!({ "items": [{ "description": "A" }] }), 1)]
    #[case::wrapped_single_object(json!({ "item": { "description": "A" } }), 1)]
    #[case::wrapped_string(json!({ "item": "a task" }), 1)]
    #[case::encoded_array(json!("[{\"description\": \"A\"}]"), 1)]
    #[test]
    fn normalize_array_recovers_recoverable_shapes(
        #[case] value: serde_json::Value,
        #[case] expected_len: usize,
    ) {
        // Given a model-supplied value in a non-array shape.
        // When normalizing it to an array.
        let result = normalize_array(&value, "phases");
        // Then it recovers the entries it can.
        let entries = result.expect("normalizes");
        assert_eq!(entries.len(), expected_len, "for value: {value}");
    }

    #[rstest::rstest]
    #[test]
    fn normalize_array_passes_arrays_through_verbatim() {
        // Given a well-formed array payload.
        let value = json!([{ "description": "A" }, { "description": "B" }]);

        // When normalizing it.
        let entries = normalize_array(&value, "phases").expect("normalizes");

        // Then the entries are unchanged and in order.
        assert_eq!(entries, vec![value[0].clone(), value[1].clone()]);
    }

    #[rstest::rstest]
    #[test]
    fn normalize_array_prefers_wrapper_over_single_entry() {
        // Given an envelope object that also carries other keys.
        let value = json!({ "item": "first task", "description": "y" });

        // When normalizing it.
        let entries = normalize_array(&value, "tasks").expect("normalizes");

        // Then the wrapped entry wins, rather than the object being read as
        // a single entry named "item" with the real task dropped.
        assert_eq!(entries, vec![json!("first task")]);
    }

    #[rstest::rstest]
    #[test]
    fn normalize_array_does_not_treat_a_phase_entry_as_an_envelope() {
        // Given a phase entry, which legitimately carries its own 'tasks' key.
        let value = json!({ "tasks": "", "description": "Probe" });

        // When normalizing it as a phases array.
        let entries = normalize_array(&value, "phases").expect("normalizes");

        // Then it is one phase, not an envelope whose body is that phase's tasks.
        assert_eq!(entries, vec![value.clone()]);
    }

    #[rstest::rstest]
    #[test]
    fn normalize_array_does_not_reparse_a_decoded_string() {
        // Given a string that decodes to another string.
        let value = json!("\"still a string\"");

        // When normalizing it.
        let entries = normalize_array(&value, "tasks").expect("normalizes");

        // Then it settles as a single entry instead of decoding again.
        assert_eq!(entries, vec![json!("still a string")]);
    }

    #[rstest::rstest]
    #[case::number(json!(42))]
    #[case::boolean(json!(true))]
    #[test]
    fn normalize_array_rejects_unrecoverable_shapes(#[case] value: serde_json::Value) {
        // Given a value carrying no recoverable array.
        // When normalizing it.
        let result = normalize_array(&value, "phases");
        // Then it is rejected.
        assert!(result.is_err(), "expected rejection for {value}");
    }

    #[rstest::rstest]
    #[test]
    fn normalize_array_shape_error_shows_worked_example() {
        // Given a number where an array was expected.
        // When normalizing it.
        let result = normalize_array(&json!(42), "tasks");
        // Then the error names the expectation and shows an example.
        let msg = result.expect_err("rejected").to_string();
        assert!(
            msg.contains("must be an array of task objects"),
            "got: {msg}"
        );
        assert!(msg.contains("\"description\""), "got: {msg}");
    }

    #[rstest::rstest]
    #[test]
    fn parse_phase_body_null_tasks_yields_no_tasks() {
        // Given a phase whose tasks are explicitly null.
        let payload = json!({ "description": "P", "tasks": null });

        // When parsing the phase.
        let phase = parse_phase_body(&payload, "phase").expect("parses");

        // Then the phase holds no tasks rather than erroring.
        assert_eq!(
            phase.tasks,
            [] as [(std::string::String, jinn_tools_msg::TaskStatus); 0]
        );
    }

    #[rstest::rstest]
    #[test]
    fn parse_phase_body_unrecoverable_tasks_names_the_phase() {
        // Given a phase whose tasks are a number.
        let payload = json!({ "description": "P", "tasks": 42 });

        // When parsing it.
        let result = parse_phase_body(&payload, "phase at index 0");

        // Then the error locates the offending phase.
        let msg = result.expect_err("rejected").to_string();
        assert!(msg.starts_with("phase at index 0"), "got: {msg}");
    }

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
        // Given a task entry declaring a status jinn cannot hold.
        let payload = json!({ "description": "P", "tasks": [
            { "description": "t", "status": status }
        ]});

        // When parsing the phase.
        let result = parse_phase_body(&payload, "phase at index 0");

        // Then it is rejected as an unknown status, like any unrecognised value.
        let msg = result.expect_err("rejected").to_string();
        assert!(
            msg.contains(&format!("unknown status \"{status}\"")),
            "got: {msg}"
        );
        // And the message names the accepted vocabulary.
        assert!(
            msg.contains("expected pending, completed, or cancelled"),
            "got: {msg}"
        );
    }

    #[rstest::rstest]
    #[case("postponed")]
    #[case("deferred")]
    #[case("blocked")]
    #[case("on_hold")]
    #[case("failed")]
    #[case("in_review")]
    #[case("partial")]
    #[test]
    fn parse_status_rejects_intent_jinn_cannot_hold(#[case] status: &str) {
        // Given a status naming an intent with no declarable counterpart.
        let payload = json!({ "description": "P", "tasks": [
            { "description": "t", "status": status }
        ]});

        // When parsing the phase.
        let result = parse_phase_body(&payload, "phase");

        // Then it is rejected rather than coerced to the nearest status.
        let msg = result.expect_err("rejected").to_string();
        assert!(
            msg.contains(&format!("unknown status \"{status}\"")),
            "got: {msg}"
        );
    }

    #[rstest::rstest]
    #[case("in_progress", TaskStatus::Pending)]
    #[case("not started", TaskStatus::Pending)]
    #[case("IN-PROGRESS", TaskStatus::Pending)]
    #[case("ToDo", TaskStatus::Pending)]
    #[case("wip", TaskStatus::Pending)]
    #[case("done", TaskStatus::Completed)]
    #[case("DONE", TaskStatus::Completed)]
    #[case("finished", TaskStatus::Completed)]
    #[case("shipped", TaskStatus::Completed)]
    #[case("canceled", TaskStatus::Cancelled)]
    #[case("skipped", TaskStatus::Cancelled)]
    #[case("wontfix", TaskStatus::Cancelled)]
    #[case("deprecated", TaskStatus::Cancelled)]
    #[case("pending", TaskStatus::Pending)]
    #[case("completed", TaskStatus::Completed)]
    #[case("cancelled", TaskStatus::Cancelled)]
    #[test]
    fn parse_status_resolves_model_vocabulary(#[case] status: &str, #[case] expected: TaskStatus) {
        // Given a task entry using a status a model reaches for.
        let payload = json!({ "description": "P", "tasks": [
            { "description": "t", "status": status }
        ]});

        // When parsing the phase.
        let phase = parse_phase_body(&payload, "phase").expect("parses");

        // Then it resolves to the declarable status it stands for.
        assert_eq!(phase.tasks[0].1, expected, "for status: {status}");
    }

    #[rstest::rstest]
    #[case::hyphen(json!("in-progress"))]
    #[case::space(json!("in progress"))]
    #[case::mixed_case(json!("In_Progress"))]
    #[case::padded(json!("  DONE  "))]
    #[test]
    fn parse_status_normalizes_separators_and_case(#[case] raw: serde_json::Value) {
        // Given a status word written with hyphens, spaces, or mixed case.
        // When parsing a task entry carrying it.
        let phase = parse_phase_body(
            &json!({ "description": "P", "tasks": [{ "description": "t", "status": raw }] }),
            "phase",
        )
        .expect("parses");

        // Then it resolves to the same status as its underscore form.
        let key = normalize_status_key(raw.as_str().expect("string"));
        let expected = STATUS_ALIASES
            .iter()
            .find(|(alias, _)| *alias == key)
            .map(|(_, status)| *status)
            .expect("alias present");
        assert_eq!(phase.tasks[0].1, expected);
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

    #[rstest::rstest]
    #[case::omitted(json!({ "description": "t" }))]
    #[case::null(json!({ "description": "t", "status": null }))]
    #[case::empty_string(json!({ "description": "t", "status": "" }))]
    #[case::blank_string(json!({ "description": "t", "status": "   " }))]
    #[test]
    fn parse_status_absent_or_empty_means_pending(#[case] entry: serde_json::Value) {
        // Given a task entry with no usable status.
        let payload = json!({ "description": "P", "tasks": [entry] });

        // When parsing the phase.
        let phase = parse_phase_body(&payload, "phase").expect("parses");

        // Then the task is Pending.
        assert_eq!(phase.tasks[0].1, TaskStatus::Pending);
    }

    #[rstest::rstest]
    #[test]
    fn parse_status_unknown_value_names_the_accepted_vocabulary() {
        // Given a status word that is not in the table.
        let payload = json!({ "description": "P", "tasks": [
            { "description": "t", "status": "nonsense" }
        ]});

        // When parsing the phase.
        let result = parse_phase_body(&payload, "phase at index 0, task at index 0");

        // Then the error quotes the word and names the accepted statuses.
        let msg = result.expect_err("rejected").to_string();
        assert!(msg.contains("unknown status \"nonsense\""), "got: {msg}");
        assert!(
            msg.contains("expected pending, completed, or cancelled"),
            "got: {msg}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn parse_status_alias_table_holds_only_synonyms() {
        // Given the alias table.
        // When scanning it for intent jinn cannot represent.
        // Then those spellings stay out, so they keep failing as unknown.
        let keys: Vec<&str> = STATUS_ALIASES.iter().map(|(alias, _)| *alias).collect();
        for excluded in [
            "postponed",
            "deferred",
            "blocked",
            "waiting",
            "on_hold",
            "paused",
            "partial",
            "failed",
            "error",
            "needs_review",
            "in_review",
        ] {
            assert!(!keys.contains(&excluded), "{excluded} must not be aliased");
        }
    }
}
