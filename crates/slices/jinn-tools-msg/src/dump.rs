//! Per-call tool-result dumping — the reserved `dump_to_path` argument.
//!
//! A tool call that carries a `dump_to_path` path argument has its successful
//! text result written to that path and replaced in-context by a small stub:
//! a pointer to the file plus a short head preview. The model keeps triage
//! information; the full text lives on disk where it can be `read` or
//! grepped on demand instead of being carried in context.
//!
//! The argument is a *reserved* argument name, peeked from the call's JSON
//! the same way `max_duration_secs` is — no per-tool code, no configuration.
//! Its contract is *either pass in a path or omit the field entirely*;
//! [`classify_dump_argument`] sorts each call into dump / absent / corrective
//! and [`apply_dump`] is the single interception point shared by both producer
//! paths (the builtin dispatch task and the MCP actor), applied after the
//! tool finishes and before the result enters context.
//!
//! Dumping is truncation with a disk copy: the stub lands in `content` (the
//! only field prompt assembly sends to the model), the original lands in
//! `full_content` (what clipboard yank, audit popups, and storage already
//! read), and the metadata records the fact via
//! [`TruncatedBy::DumpedToDisk`](jinn_core_types::tool_types::TruncatedBy::DumpedToDisk).

use std::path::Path;

use jinn_core_types::tool_types::{ToolResult, TruncatedBy, TruncationMeta};
use marked_path::{Absolute, MarkedPathAccess as _, MarkedPathBuf};

/// The reserved tool-call argument that opts a call into dumping its result.
///
/// Defined in the layer-0 vocabulary (`jinn-core-types`) and re-exported
/// here, where the peeking and applying happen.
pub use jinn_core_types::tool_types::DUMP_TO_PATH_ARG;

/// Maximum bytes of the original carried into the in-context stub preview.
pub const DUMP_PREVIEW_BYTES: usize = 2048;

/// Outcome of applying a dump decision to a finished tool result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DumpApplied {
    /// The original was written to disk and the result stubbed in place.
    Dumped,
    /// The result was left untouched — not eligible, or the write failed.
    NotApplied,
}

/// What a tool call's reserved dump argument asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DumpDecision {
    /// The argument names a real path — dump the result there.
    Dump(String),
    /// The argument is absent or carries no meaningful value — no dump.
    Absent,
    /// The argument names true/false instead of a path — the boolean-flag
    /// misreading. The caller rejects the call with [`DUMP_CORRECTION`]
    /// before the tool executes.
    Corrective,
}

/// The corrective failure text a tool call receives when its dump argument
/// carries a true/false spelling. Names the field's contract — *pass in a
/// path or omit the field entirely* — so the model can correct on the retry.
pub const DUMP_CORRECTION: &str = "dump_to_path takes a file path or must be omitted \
entirely — true/false is not accepted. Pass a path to write the full result to disk \
(relative paths resolve against the working directory), or omit the field to keep the \
full result in context.";

/// Sort a tool call's reserved dump argument into a [`DumpDecision`].
///
/// Mirrors the reserved-`max_duration_secs` peek: a malformed arguments
/// string, a missing argument, a non-string value (including the JSON
/// booleans `true`/`false`, which already fall out of `as_str`), or a
/// blank string all yield [`DumpDecision::Absent`] — the caller proceeds
/// exactly as if the feature did not exist. Only the *string* spellings
/// `true`/`false` (case-insensitive, surrounding whitespace tolerated)
/// yield [`DumpDecision::Corrective`]: that is the observed boolean-flag
/// misreading, and a file named `false` is never the model's intent.
/// Any other string is [`DumpDecision::Dump`], carrying the untrimmed
/// original spelling — path resolution semantics are unchanged.
#[must_use]
pub fn classify_dump_argument(arguments: &str) -> DumpDecision {
    let value: serde_json::Value = serde_json::from_str(arguments).unwrap_or_default();
    let Some(raw) = value.get(DUMP_TO_PATH_ARG) else {
        return DumpDecision::Absent;
    };
    let Some(text) = raw.as_str() else {
        return DumpDecision::Absent;
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return DumpDecision::Absent;
    }
    if trimmed.eq_ignore_ascii_case("true") || trimmed.eq_ignore_ascii_case("false") {
        return DumpDecision::Corrective;
    }
    DumpDecision::Dump(text.to_owned())
}

/// Apply a per-call dump to a finished tool result, in place.
///
/// Eligible results are successful, attachment-free, non-empty text results
/// with a dump path. The original text — [`ToolResult::full_content`] when
/// the tool already truncated, else [`ToolResult::content`] — is written to
/// `dump_path` (resolved against `cwd` when relative, parent directories
/// created first), then the result is stubbed: a pointer naming the path and
/// size, a head preview of up to `preview_bytes`, the original preserved in
/// `full_content`, and the metadata stamped with
/// [`TruncatedBy::DumpedToDisk`].
///
/// An ineligible or failed dump leaves the result untouched and returns
/// [`DumpApplied::NotApplied`] — a disk error must never surface as a tool
/// error to the model.
pub fn apply_dump(
    result: &mut ToolResult,
    dump_path: &str,
    cwd: &Path,
    preview_bytes: usize,
) -> DumpApplied {
    // Failed results are never dumped: the failure narrative is the result.
    if !result.success {
        return DumpApplied::NotApplied;
    }
    // Attachment-bearing results ride the multimodal path; dumping their
    // text companion would mislead about where the images live.
    if !result.attachments.is_empty() {
        return DumpApplied::NotApplied;
    }
    // Clone the original out of the result so nothing below aliases it: the
    // already-truncated full copy wins over the truncated tail in `content`,
    // and every NotApplied path leaves the result untouched.
    let original = result
        .full_content
        .clone()
        .unwrap_or_else(|| result.content.clone());
    if original.is_empty() {
        return DumpApplied::NotApplied;
    }

    let Some(target) = resolve_dump_path(dump_path, cwd) else {
        return DumpApplied::NotApplied;
    };
    if write_original(&target, &original) == DumpApplied::NotApplied {
        return DumpApplied::NotApplied;
    }

    let preview = head_preview(&original, preview_bytes);
    result.content = stub_content(target.as_path(), original.len(), &preview);
    result.full_content = Some(original.clone());
    result.truncation = Some(TruncationMeta {
        truncated_by: TruncatedBy::DumpedToDisk,
        total_lines: original.split('\n').count(),
        total_bytes: original.len(),
        output_lines: result.content.split('\n').count(),
        output_bytes: result.content.len(),
    });
    tracing::debug!(
        path = %target.as_path().display(),
        bytes = original.len(),
        "tool result dumped to disk; stub published in its place"
    );
    DumpApplied::Dumped
}

/// Resolve the dump path the way every tool path resolves: absolute as-is,
/// relative joined onto `cwd`. Purely lexical — no canonicalization.
///
/// # Errors
///
/// A path that resolves non-absolute (only possible for a malformed absolute
/// form on Windows) returns [`DumpApplied::NotApplied`] — the dump degrades
/// to a no-op rather than guessing at a path spelling.
fn resolve_dump_path(dump_path: &str, cwd: &Path) -> Option<MarkedPathBuf<Absolute>> {
    let path = Path::new(dump_path);
    if path.is_absolute() {
        MarkedPathBuf::<Absolute>::new(path).ok()
    } else {
        MarkedPathBuf::<Absolute>::new(cwd).ok()?.join(path).ok()
    }
}

/// Create the target's parent directories first, then write the original.
///
/// Parent creation comes before the write because a dump path usually names
/// a file in directories that do not exist yet. Any failure logs a warning
/// and returns [`DumpApplied::NotApplied`]; the caller's result stays
/// untouched and degrades to today's behavior.
fn write_original(target: &MarkedPathBuf<Absolute>, original: &str) -> DumpApplied {
    let target = target.as_path();
    if let Some(parent) = target.parent().filter(|p| !p.as_os_str().is_empty())
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        tracing::warn!(
            %error,
            parent = %parent.display(),
            "dump_to_path: could not create parent directory; result left in context"
        );
        return DumpApplied::NotApplied;
    }
    if let Err(error) = std::fs::write(target, original) {
        tracing::warn!(
            %error,
            path = %target.display(),
            "dump_to_path: could not write dump file; result left in context"
        );
        return DumpApplied::NotApplied;
    }
    DumpApplied::Dumped
}

/// The first `max_bytes` of `content`, cut at a char boundary.
///
/// A cut preview ends with `…` so the model can tell the head from the
/// whole. Content within the limit is returned whole, unmarked.
fn head_preview(content: &str, max_bytes: usize) -> String {
    if content.len() <= max_bytes {
        return content.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !content.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", content.get(..end).unwrap_or_default())
}

/// The in-context stub: pointer naming the file and its size, then the preview.
fn stub_content(target: &Path, total_bytes: usize, preview: &str) -> String {
    format!(
        "[Tool result written to {} ({}). First {} bytes of the content:]\n\n{}",
        target.display(),
        format_size(total_bytes),
        preview.len(),
        preview
    )
}

/// Format a byte count as a human-readable size string.
#[expect(
    clippy::cast_precision_loss,
    reason = "loss is negligible for a display label"
)]
fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unwrap_used,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::*;
    use jinn_testutil::{MarkedPathAccess, tempdir};

    fn text_result(content: &str) -> ToolResult {
        ToolResult {
            tool_call_id: "call_1".into(),
            name: "webReader".to_owned(),
            content: content.to_owned(),
            success: true,
            full_content: None,
            truncation: None,
            pin_position: None,
            attachments: Vec::new(),
        }
    }

    /// The stub spells the target path with native separators (the typed
    /// path join normalizes), so assertions on stub content must spell the
    /// dump path the same way.
    fn native(path: &str) -> String {
        path.replace('/', std::path::MAIN_SEPARATOR_STR)
    }

    #[rstest::rstest]
    fn dump_writes_original_and_stubs_content() {
        // Given a successful text result and a dump path.
        let dir = tempdir();
        let mut result = text_result("the full page text");

        // When applying the dump.
        let applied = apply_dump(&mut result, "out/page.md", dir.path.as_path(), 2048);

        // Then the original is on disk.
        assert_eq!(applied, DumpApplied::Dumped);
        let written =
            std::fs::read_to_string(dir.path.as_path().join("out/page.md")).expect("dump file");
        assert_eq!(written, "the full page text");

        // And the in-context result names the file and preserves the original.
        assert!(result.content.contains(&native("out/page.md")));
        assert!(result.content.contains("the full page text"));
        assert_eq!(result.full_content.as_deref(), Some("the full page text"));
        let meta = result.truncation.expect("dump meta");
        assert_eq!(meta.truncated_by, TruncatedBy::DumpedToDisk);
        assert_eq!(meta.total_bytes, "the full page text".len());
    }

    #[rstest::rstest]
    fn dump_creates_missing_parent_directories() {
        // Given a dump path whose parent directories do not exist.
        let dir = tempdir();
        let mut result = text_result("content");

        // When applying the dump to a nested path.
        let applied = apply_dump(
            &mut result,
            "research/deep/page.md",
            dir.path.as_path(),
            2048,
        );

        // Then the whole chain of parents was created and the file written.
        assert_eq!(applied, DumpApplied::Dumped);
        assert!(
            dir.path.as_path().join("research/deep/page.md").exists(),
            "nested parents must be created before the write"
        );
    }

    #[rstest::rstest]
    fn dump_prefers_full_content_over_truncated_tail() {
        // Given a result already truncated: a short tail in content, the
        // long original in full_content.
        let dir = tempdir();
        let original = "line1\nline2\nline3";
        let mut result = text_result("line3");
        result.full_content = Some(original.to_owned());

        // When applying the dump.
        let applied = apply_dump(&mut result, "page.md", dir.path.as_path(), 2048);

        // Then the disk file holds the untruncated original, not the tail.
        assert_eq!(applied, DumpApplied::Dumped);
        let written = std::fs::read_to_string(dir.path.as_path().join("page.md")).expect("dump");
        assert_eq!(written, original);

        // And full_content still carries the original, never the discarded tail.
        assert_eq!(result.full_content.as_deref(), Some(original));
    }

    #[rstest::rstest]
    fn dump_honors_absolute_paths() {
        // Given an absolute dump path under the temp root.
        let dir = tempdir();
        let target = dir.path.join("abs/page.md").expect("temp root joins");
        let mut result = text_result("content");

        // When applying the dump with that absolute path.
        let applied = apply_dump(
            &mut result,
            &target.as_path().to_string_lossy(),
            dir.path.as_path(),
            2048,
        );

        // Then the file lands exactly at the absolute path.
        assert_eq!(applied, DumpApplied::Dumped);
        assert!(target.as_path().exists());
    }

    #[rstest::rstest]
    fn failed_results_are_never_dumped() {
        // Given a failed result with a dump path.
        let dir = tempdir();
        let mut result = text_result("boom");
        result.success = false;

        // When applying the dump.
        let applied = apply_dump(&mut result, "out.md", dir.path.as_path(), 2048);

        // Then nothing was written and the result is untouched.
        assert_eq!(applied, DumpApplied::NotApplied);
        assert!(!dir.path.as_path().join("out.md").exists());
        assert_eq!(result.content, "boom");
        assert!(result.full_content.is_none());
        assert!(result.truncation.is_none());
    }

    #[rstest::rstest]
    fn results_with_attachments_are_skipped() {
        // Given a successful result carrying an image attachment.
        let dir = tempdir();
        let mut result = text_result("see attached");
        result.attachments = vec![jinn_core_types::attachment::Attachment::Image {
            media_type: "image/png".to_owned(),
            data: b"png-bytes".to_vec(),
        }];

        // When applying the dump.
        let applied = apply_dump(&mut result, "out.md", dir.path.as_path(), 2048);

        // Then the result passes through untouched.
        assert_eq!(applied, DumpApplied::NotApplied);
        assert!(!dir.path.as_path().join("out.md").exists());
        assert_eq!(result.content, "see attached");
    }

    #[rstest::rstest]
    fn empty_results_are_left_untouched() {
        // Given a successful result with empty content and no full_content.
        let dir = tempdir();
        let mut result = text_result("");

        // When applying the dump.
        let applied = apply_dump(&mut result, "out.md", dir.path.as_path(), 2048);

        // Then nothing is written — a zero-byte file helps no one.
        assert_eq!(applied, DumpApplied::NotApplied);
        assert!(!dir.path.as_path().join("out.md").exists());
        assert_eq!(result.content, "");
    }

    #[rstest::rstest]
    fn write_failure_leaves_result_untouched() {
        // Given a dump path whose parent is an existing file, so both the
        // parent-directory creation and the write must fail.
        let dir = tempdir();
        let occupied = dir.path.as_path().join("occupied.txt");
        std::fs::write(&occupied, "not a directory").expect("seed file");
        let mut result = text_result("content");

        // When applying the dump through that blocked path.
        let applied = apply_dump(
            &mut result,
            &occupied.join("sub.md").to_string_lossy(),
            dir.path.as_path(),
            2048,
        );

        // Then the dump reports NotApplied and the result is byte-identical.
        assert_eq!(applied, DumpApplied::NotApplied);
        assert_eq!(result.content, "content");
        assert!(result.full_content.is_none());
        assert!(result.truncation.is_none());
    }

    #[rstest::rstest]
    fn preview_is_cut_at_a_char_boundary() {
        // Given multi-byte content longer than the preview limit.
        let dir = tempdir();
        let content = "あ".repeat(1000); // 3 bytes per char, 3000 bytes total
        let mut result = text_result(&content);

        // When applying the dump with a 100-byte preview limit.
        let applied = apply_dump(&mut result, "out.md", dir.path.as_path(), 100);

        // Then the stub's preview is valid UTF-8 within the cut, ending in an
        // ellipsis, and the disk file still holds the whole content.
        assert_eq!(applied, DumpApplied::Dumped);
        assert!(result.content.ends_with('…'));
        let preview = result
            .content
            .lines()
            .last()
            .expect("stub has a preview line");
        assert!(preview.len() <= 100 + "…".len());
        let written = std::fs::read_to_string(dir.path.as_path().join("out.md")).expect("dump");
        assert_eq!(written, content);
    }

    #[rstest::rstest]
    fn stub_carries_pointer_head_and_meta() {
        // Given a successful result within the preview limit.
        let dir = tempdir();
        let mut result = text_result("small");

        // When applying the dump.
        apply_dump(&mut result, "out.md", dir.path.as_path(), 2048);

        // Then the stub names the path and size, carries the whole content,
        // and the metadata reports the stub's own dimensions.
        assert!(result.content.contains("[Tool result written to"));
        assert!(result.content.contains("(5B)"));
        assert!(result.content.contains("small"));
        let meta = result.truncation.expect("meta");
        assert_eq!(meta.total_bytes, 5);
        assert_eq!(meta.total_lines, 1);
        assert_eq!(meta.output_bytes, result.content.len());
    }

    #[rstest::rstest]
    fn classify_dump_argument_reads_the_reserved_argument() {
        // Given call arguments carrying dump_to_path among other fields.
        let arguments = r#"{"url":"https://example.com","dump_to_path":"research/page.md"}"#;

        // When classifying.
        let decision = classify_dump_argument(arguments);

        // Then the dump path comes back.
        assert_eq!(decision, DumpDecision::Dump("research/page.md".to_owned()));
    }

    #[rstest::rstest]
    fn classify_dump_argument_missing_returns_absent() {
        // Given call arguments without the reserved argument.
        let arguments = r#"{"url":"https://example.com"}"#;

        // When classifying.
        let decision = classify_dump_argument(arguments);

        // Then the field is absent.
        assert_eq!(decision, DumpDecision::Absent);
    }

    #[rstest::rstest]
    fn classify_dump_argument_empty_string_is_not_a_path() {
        // Given an empty dump_to_path value.
        let arguments = r#"{"dump_to_path":""}"#;

        // When classifying.
        let decision = classify_dump_argument(arguments);

        // Then it is treated as absent.
        assert_eq!(decision, DumpDecision::Absent);
    }

    #[rstest::rstest]
    fn classify_dump_argument_non_string_is_ignored() {
        // Given a non-string dump_to_path value.
        let arguments = r#"{"dump_to_path":42}"#;

        // When classifying.
        let decision = classify_dump_argument(arguments);

        // Then it is ignored.
        assert_eq!(decision, DumpDecision::Absent);
    }

    #[rstest::rstest]
    fn classify_dump_argument_malformed_json_yields_absent() {
        // Given an arguments string that is not valid JSON.
        // When classifying.
        let decision = classify_dump_argument("not json");

        // Then the field is absent and there is no panic.
        assert_eq!(decision, DumpDecision::Absent);
    }

    #[rstest::rstest]
    #[case("false")]
    #[case("true")]
    #[case("False")]
    #[case("TRUE")]
    #[case("  false  ")]
    fn classify_dump_argument_flags_boolean_spellings_as_corrective(#[case] value: &str) {
        // Given a dump_to_path value spelling true/false as a string.
        let arguments = format!(r#"{{"dump_to_path":"{value}"}}"#);

        // When classifying.
        let decision = classify_dump_argument(&arguments);

        // Then the call is corrective, not a path named true or false.
        assert_eq!(decision, DumpDecision::Corrective);
    }

    #[rstest::rstest]
    fn classify_dump_argument_json_boolean_false_is_absent_not_corrective() {
        // Given a JSON boolean false (not the string spelling).
        let arguments = r#"{"dump_to_path":false}"#;

        // When classifying.
        let decision = classify_dump_argument(arguments);

        // Then it is absent — non-string values never carry a path, and the
        // corrective message is reserved for the string misreading.
        assert_eq!(decision, DumpDecision::Absent);
    }

    #[rstest::rstest]
    fn classify_dump_argument_whitespace_only_is_absent() {
        // Given a whitespace-only dump_to_path value.
        let arguments = r#"{"dump_to_path":"   "}"#;

        // When classifying.
        let decision = classify_dump_argument(arguments);

        // Then it is treated as absent.
        assert_eq!(decision, DumpDecision::Absent);
    }

    #[rstest::rstest]
    fn classify_dump_argument_keeps_the_untrimmed_original_for_dump() {
        // Given a real path with surrounding whitespace.
        let arguments = r#"{"dump_to_path":" out/page.md "}"#;

        // When classifying.
        let decision = classify_dump_argument(arguments);

        // Then the path comes back untrimmed — trim only served the
        // boolean check, and path resolution semantics are unchanged.
        assert_eq!(decision, DumpDecision::Dump(" out/page.md ".to_owned()));
    }

    #[rstest::rstest]
    fn dump_correction_names_the_pass_a_path_or_omit_contract() {
        // Given the corrective message constant.
        // When reading it.
        let message = DUMP_CORRECTION;

        // Then it names the field, the path alternative, and the omit
        // alternative — the three words the model needs to correct.
        assert!(message.contains("dump_to_path"), "{message}");
        assert!(message.contains("path"), "{message}");
        assert!(message.contains("omit"), "{message}");
    }
}
