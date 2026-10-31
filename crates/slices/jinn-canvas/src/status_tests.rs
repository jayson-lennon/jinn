//! Tests for the status vocabulary and the serve refusals.
//!
//! The status strings are the dashboard contract (end state 7): exact
//! shapes, asserted verbatim. The refusal renderings are the tool
//! result contract: each names the cause and carries the fix.

#![allow(clippy::expect_used, reason = "test assertions need expect")]

use std::path::Path;

use marked_path::MarkedPathAccess as _;

use crate::errors::CanvasServeError;
use crate::status::{offline_message, serving_message};

#[rstest::rstest]
#[test]
fn offline_message_reads_server_offline() {
    // Given no server has ever served.
    // When rendering the offline status.
    let message = offline_message();

    // Then the exact contract string comes back.
    assert_eq!(message, "server offline");
}

#[rstest::rstest]
#[test]
fn serving_message_is_scheme_url_space_path_in_parens() {
    // Given a resolved bind URL and the served artifact.
    let artifact = std::path::PathBuf::from("/tmp/proj/chart.html");

    // When rendering the serving status.
    let message = serving_message("http://127.0.0.1:43121", artifact.as_path());

    // Then the status is the link-shaped URL, a space, and the path in
    // parentheses.
    assert_eq!(message, "http://127.0.0.1:43121 (/tmp/proj/chart.html)");
}

#[rstest::rstest]
#[test]
fn artifact_missing_sentence_names_the_path_and_the_fix() {
    // Given a missing artifact path.
    let path = Path::new("/tmp/nope/chart.html");

    // When rendering the refusal.
    // Then the sentence names the file and the remedy.
    let sentence = format!(
        "canvas artifact must be an existing HTML file: {} — fix: {}",
        path.display(),
        CanvasServeError::ArtifactMissing.fix()
    );
    assert!(sentence.contains("/tmp/nope/chart.html"));
    assert!(sentence.contains("pass the path of an existing HTML file"));
}

#[rstest::rstest]
#[test]
fn bind_not_loopback_sentence_carries_the_loopback_remedy() {
    // Given the non-loopback refusal.
    // When rendering it.
    let sentence = format!(
        "{}: {} — fix: {}",
        "BindNotLoopback",
        "the bind must be loopback",
        CanvasServeError::BindNotLoopback.fix()
    );

    // Then the remedy points at a loopback bind value.
    assert!(sentence.contains("127.0.0.1:8899"));
}

#[rstest::rstest]
#[test]
fn temp_dir_rendering_is_absolute_for_the_status_path() {
    // Given a testutil temp dir artifact, on disk.
    let dir = jinn_testutil::tempdir();
    let artifact = dir.path.join("chart.html").expect("name join");
    std::fs::write(artifact.as_path(), "<html></html>").expect("seed artifact");

    // When rendering the serving status against its canonical path.
    let canonical = artifact.as_path().canonicalize().expect("canonical");
    let message = serving_message("http://127.0.0.1:40000", canonical.as_path());

    // Then the absolute path rides in the message — the row shows where
    // the served file lives, not the spelling the caller used. The tail
    // names the file separator-neutrally: the canonical spelling is the
    // platform's own.
    assert!(
        message.starts_with("http://127.0.0.1:40000 (") && message.ends_with("chart.html)"),
        "{message}"
    );
}
