//! Per-call tool-result dumping — the reserved `dump_to_disk` argument.
//!
//! A tool call that carries a `dump_to_disk` path argument has its successful
//! text result written to that path and replaced in-context by a small stub:
//! a pointer to the file plus a short head preview. The model keeps triage
//! information; the full text lives on disk where it can be `read` or
//! grepped on demand instead of being carried in context.
//!
//! The argument is a *reserved* argument name, peeked from the call's JSON
//! the same way `max_duration_secs` is — no per-tool code, no configuration.
//! [`apply_dump`] is the single interception point shared by both producer
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
pub use jinn_core_types::tool_types::DUMP_TO_DISK_ARG;

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

/// Peek the reserved dump path from a tool call's JSON arguments.
///
/// Mirrors the reserved-`max_duration_secs` peek: a malformed arguments
/// string or a missing argument yields `None`, and the caller proceeds
/// exactly as if the feature did not exist.
#[must_use]
pub fn extract_dump_path(arguments: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(arguments).unwrap_or_default();
    value
        .get(DUMP_TO_DISK_ARG)
        .and_then(serde_json::Value::as_str)
        .filter(|path| !path.is_empty())
        .map(ToOwned::to_owned)
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
            "dump_to_disk: could not create parent directory; result left in context"
        );
        return DumpApplied::NotApplied;
    }
    if let Err(error) = std::fs::write(target, original) {
        tracing::warn!(
            %error,
            path = %target.display(),
            "dump_to_disk: could not write dump file; result left in context"
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
        assert!(result.content.contains("out/page.md"));
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
    fn extract_dump_path_reads_the_reserved_argument() {
        // Given call arguments carrying dump_to_disk among other fields.
        let arguments = r#"{"url":"https://example.com","dump_to_disk":"research/page.md"}"#;

        // When extracting.
        let path = extract_dump_path(arguments);

        // Then the dump path comes back.
        assert_eq!(path.as_deref(), Some("research/page.md"));
    }

    #[rstest::rstest]
    fn extract_dump_path_missing_returns_none() {
        // Given call arguments without the reserved argument.
        let arguments = r#"{"url":"https://example.com"}"#;

        // When extracting.
        let path = extract_dump_path(arguments);

        // Then there is no dump path.
        assert_eq!(path, None);
    }

    #[rstest::rstest]
    fn extract_dump_path_empty_string_is_not_a_path() {
        // Given an empty dump_to_disk value.
        let arguments = r#"{"dump_to_disk":""}"#;

        // When extracting.
        let path = extract_dump_path(arguments);

        // Then it is treated as absent.
        assert_eq!(path, None);
    }

    #[rstest::rstest]
    fn extract_dump_path_non_string_is_ignored() {
        // Given a non-string dump_to_disk value.
        let arguments = r#"{"dump_to_disk":42}"#;

        // When extracting.
        let path = extract_dump_path(arguments);

        // Then it is ignored.
        assert_eq!(path, None);
    }

    #[rstest::rstest]
    fn extract_dump_path_malformed_json_yields_none() {
        // Given an arguments string that is not valid JSON.
        // When extracting.
        let path = extract_dump_path("not json");

        // Then there is no dump path and no panic.
        assert_eq!(path, None);
    }
}
