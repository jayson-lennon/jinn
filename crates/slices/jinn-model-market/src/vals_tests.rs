//! Tolerant-parse tests over synthetic fixtures.
//!
//! No fixture here resembles the live roster: every payload is 2–3
//! hand-built entries. The contract's tolerances (unknown fields
//! ignored, task map read as data, only `overall` consumed) are proven
//! by mutating a base fixture.

#![allow(clippy::expect_used, reason = "test code")]

use crate::vals::parse_vals_index;

/// The base fixture: the observed payload shape — `metadata` (version,
/// updated, models) plus a top-level `tasks` map whose `overall` bucket
/// carries two model records and whose `finance_agent` sector bucket
/// carries a DIFFERENT accuracy for the same slug — the poison a
/// scan-every-bucket parser would drink. The index value must win.
fn base_payload() -> serde_json::Value {
    serde_json::json!({
        "metadata": {
            "version": "2.2",
            "updated": "2026-11-18",
            "models": ["anthropic/claude-sonnet-5-5", "zai/glm-5.3"],
            "tasks": {"overall": "Overall", "finance_agent": "Finance Agent"}
        },
        "tasks": {
            "overall": {
                "anthropic/claude-sonnet-5-5": {
                    "accuracy": 71.309,
                    "stderr": 0.956,
                    "latency": 2621.8,
                    "cost_per_test": 17.9,
                    "token_totals": {"input_tokens": 1},
                    "provider": "Anthropic",
                    "harness": "vals-harness"
                },
                "zai/glm-5.3": {
                    "accuracy": 58.4,
                    "stderr": 1.1
                }
            },
            "finance_agent": {
                "anthropic/claude-sonnet-5-5": {"accuracy": 64.0},
                "zai/glm-5.3": {"accuracy": 12.0}
            },
            "vibe_code_bench": {
                "anthropic/claude-sonnet-5-5": {"accuracy": 99.9},
                "zai/glm-5.3": {"accuracy": 7.7}
            }
        }
    })
}

#[rstest::rstest]
fn parse_reads_the_overall_bucket_into_the_index() {
    // Given the base payload.
    let json = base_payload().to_string();

    // When parsing.
    let index = parse_vals_index(json.as_bytes()).expect("parses");

    // Then both overall accuracies land, keyed by slug — and neither
    // sector bucket's value leaked in over them (the live-data bug this
    // test pins: a scan-every-bucket parser put vibe-code-bench scores
    // in the index column).
    assert_eq!(
        index.get("anthropic/claude-sonnet-5-5").unwrap().accuracy,
        71.309
    );
    assert_eq!(index.get("zai/glm-5.3").unwrap().accuracy, 58.4);
    // And the metadata rides for diagnostics.
    assert_eq!(index.version.as_deref(), Some("2.2"));
    assert_eq!(index.updated.as_deref(), Some("2026-11-18"));
}

#[rstest::rstest]
fn parse_ignores_unknown_record_fields() {
    // Given a payload whose records carry fields the schema has never
    // heard of.
    let mut payload = base_payload();
    payload
        .get_mut("tasks")
        .and_then(|t| t.get_mut("overall"))
        .and_then(|t| t.get_mut("zai/glm-5.3"))
        .expect("fixture shape")
        .as_object_mut()
        .expect("record object")
        .insert(
            "brand_new_field".to_owned(),
            serde_json::json!({"nested": [1, 2, 3]}),
        );
    let json = payload.to_string();

    // When parsing.
    let index = parse_vals_index(json.as_bytes()).expect("parses");

    // Then the accuracy still lands — unknown fields are ignored.
    assert_eq!(index.get("zai/glm-5.3").unwrap().accuracy, 58.4);
}

#[rstest::rstest]
fn parse_survives_an_added_task_bucket() {
    // Given a payload with a task bucket the schema never named.
    let mut payload = base_payload();
    payload
        .get_mut("tasks")
        .expect("fixture shape")
        .as_object_mut()
        .expect("tasks object")
        .insert(
            "brand_new_sector".to_owned(),
            serde_json::json!({"anthropic/claude-sonnet-5-5": {"accuracy": 99.0}}),
        );
    let json = payload.to_string();

    // When parsing.
    let index = parse_vals_index(json.as_bytes()).expect("parses");

    // Then the overall values are unchanged — the bucket map is data.
    assert_eq!(
        index.get("anthropic/claude-sonnet-5-5").unwrap().accuracy,
        71.309
    );
}

#[rstest::rstest]
fn parse_fails_clean_when_the_overall_bucket_is_gone() {
    // Given the `overall` bucket renamed (a later deploy re-labels it)
    // — the index's records sit under `overall_v3` where the parser
    // must not look.
    let mut payload = base_payload();
    let overall = payload
        .get("tasks")
        .and_then(|t| t.get("overall"))
        .cloned()
        .expect("fixture shape");
    *payload
        .get_mut("tasks")
        .expect("fixture shape")
        .as_object_mut()
        .expect("tasks object") = std::iter::once(("overall_v3".to_owned(), overall)).collect();
    let json = payload.to_string();

    // When parsing.
    let result = parse_vals_index(json.as_bytes());

    // Then the parse degrades with a legible error — a blank VALS
    // column beats silently showing a different benchmark's score.
    assert!(
        result.is_err(),
        "a payload without the overall bucket is a clean failure"
    );
}

#[rstest::rstest]
fn parse_skips_records_without_a_numeric_accuracy() {
    // Given a payload carrying one record whose accuracy went missing.
    let mut payload = base_payload();
    payload
        .get_mut("tasks")
        .and_then(|t| t.get_mut("overall"))
        .and_then(|t| t.get_mut("zai/glm-5.3"))
        .expect("fixture shape")
        .as_object_mut()
        .expect("record object")
        .clear();
    payload
        .get_mut("tasks")
        .and_then(|t| t.get_mut("overall"))
        .and_then(|t| t.get_mut("zai/glm-5.3"))
        .expect("fixture shape")
        .as_object_mut()
        .expect("record object")
        .insert("accuracy".to_owned(), serde_json::Value::Null);
    payload
        .get_mut("tasks")
        .and_then(|t| t.get_mut("overall"))
        .and_then(|t| t.get_mut("zai/glm-5.3"))
        .expect("fixture shape")
        .as_object_mut()
        .expect("record object")
        .insert("stderr".to_owned(), serde_json::json!(1.0));
    let json = payload.to_string();

    // When parsing.
    let index = parse_vals_index(json.as_bytes()).expect("parses");

    // Then the healthy record lands and the broken one is skipped, not
    // fatal.
    assert!(index.get("anthropic/claude-sonnet-5-5").is_some());
    assert!(index.get("zai/glm-5.3").is_none());
}

#[rstest::rstest]
fn parse_reads_a_metadata_nested_tasks_map_as_fallback() {
    // Given a payload that nests the record maps under `metadata.tasks`
    // instead of the top level (schema drift).
    let payload = serde_json::json!({
        "metadata": {
            "version": "3.0",
            "tasks": {
                "overall": {
                    "zai/glm-5.3": {"accuracy": 58.4}
                }
            }
        }
    });
    let json = payload.to_string();

    // When parsing.
    let index = parse_vals_index(json.as_bytes()).expect("parses");

    // Then the fallback map is read.
    assert_eq!(index.get("zai/glm-5.3").unwrap().accuracy, 58.4);
    assert_eq!(index.version.as_deref(), Some("3.0"));
}

#[rstest::rstest]
fn parse_fails_on_non_json_bytes() {
    // Given bytes that are not JSON.
    let bytes = b"<html>not the index</html>";

    // When parsing.
    let result = parse_vals_index(bytes);

    // Then the parse names the failure for the status line.
    let report = result.expect_err("non-JSON is a parse failure");
    assert!(
        report.to_string().contains("not JSON") || report.to_string().contains("JSON"),
        "the error names the JSON problem, got: {report}"
    );
}

#[rstest::rstest]
fn parse_fails_when_no_records_exist_anywhere() {
    // Given JSON with no tasks map at all.
    let json = serde_json::json!({"metadata": {"version": "9.9"}}).to_string();

    // When parsing.
    let result = parse_vals_index(json.as_bytes());

    // Then the parse fails — an empty index must be a reported
    // degraded state, never a silent blank column.
    assert!(result.is_err());
}
