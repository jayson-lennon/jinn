#![expect(clippy::expect_used, reason = "infallible static regex initialization")]
//! `@path` attachment scanning — detects `@path` tokens in user text,
//! rewrites them to `file://` URIs, and collects the resolved paths.
//!
//! This runs as a second expansion pass after `#token` expansion. Each `@path`
//! reference is resolved against the session CWD and replaced in place by
//! `(file:///resolved/absolute/path)` so terminals with OSC-8 link support
//! auto-link the original file.
//!
//! Accepted forms (each at a word boundary — start of buffer or preceded by
//! space/newline):
//! - `@/abs/path` — absolute path, used as-is.
//! - `@~/rel/path` — tilde-expanded against the home directory.
//! - `@rel/path` — resolved against the session CWD.
//!
//! `foo@path` (no boundary) is never matched.
//!
//! **This module performs NO filesystem I/O.** [`scan_at_paths`] is a
//! pure-text transform: it rewrites tokens and collects resolved paths into
//! [`ScanResult::pending_paths`]. Reading bytes and classifying the image
//! format ([`classify_image_bytes`]) is the async session actor's job, so that
//! blocking I/O happens inside `spawn_blocking`.
//!
//! **Spaces in filenames are not supported.** The path runs to the next
//! whitespace, so `@my file.png` attaches only `@my` (which fails) and leaves
//! `file.png` as plain text.

use std::path::{Path, PathBuf};

use std::sync::LazyLock;

use regex::Regex;

/// Resolver context for turning raw `@path` tokens into absolute paths.
///
/// Relative paths resolve against `cwd`; `~`-prefixed paths resolve against
/// `home`. Absolute paths (`/...`) are used as-is.
#[derive(Debug, Clone, Copy)]
pub struct PathResolveContext<'a> {
    /// The session's working directory — base for relative `@rel/path` tokens.
    pub cwd: &'a Path,
    /// The user's home directory — base for `@~/path` (tilde) tokens.
    pub home: &'a Path,
}

impl<'a> PathResolveContext<'a> {
    /// Builds a resolver from a cwd and home directory.
    #[must_use]
    pub fn new(cwd: &'a Path, home: &'a Path) -> Self {
        Self { cwd, home }
    }

    /// Resolves a raw `@path` token body (everything after `@`) into an
    /// absolute path.
    ///
    /// - `~` or `~/...` → joined against `home`.
    /// - `/...` → used as-is (absolute).
    /// - anything else → joined against `cwd` (relative).
    #[must_use]
    pub fn resolve(&self, raw: &str) -> PathBuf {
        if raw == "~" {
            self.home.to_path_buf()
        } else if let Some(rest) = raw.strip_prefix("~/") {
            self.home.join(rest)
        } else if raw.starts_with('/') {
            PathBuf::from(raw)
        } else {
            self.cwd.join(raw)
        }
    }
}

/// A `@` followed by a path token at a word boundary (start of buffer or
/// preceded by space/newline). The path runs to the next whitespace.
///
/// Capture group 1 is the boundary (`^` or a space/newline); group 2 is the
/// raw path text (everything after `@` up to whitespace). `foo@path` does not
/// match because the char before `@` is not a boundary.
static AT_PATH_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(^|[ \n])@([^\s]+)").expect("valid @path regex"));

/// The literal body of a scanned `@path` token paired with its resolved
/// absolute path.
///
// This carries both "what the user typed" (the render-time key into the
// display text) and "where it points" (the absolute path the actor reads).
// Resolution happens exactly once at enqueue; downstream consumers (the
// image resolver, the render layer) read this frozen result and never touch
// the filesystem or re-resolve against cwd/home.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PendingPath {
    /// The literal token body as it appears in the display text (after the `@`).
    pub raw: String,
    /// The absolute path the token resolved to against the session cwd/home.
    pub abs: PathBuf,
}

/// Result of scanning text for `@path` references.
///
/// This is a **pure-text** scan: it rewrites tokens to `file://` URIs and
/// collects the resolved paths, but it does **not** read any files. The
/// caller (the async session actor) is responsible for reading the bytes at
/// each [`pending_path`](ScanResult::pending_paths) and building attachments,
/// so that blocking I/O happens inside `spawn_blocking` instead of on the
/// async runtime.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ScanResult {
    /// The text with each `@path` replaced by `(file:///abs/path)`.
    pub rewritten_text: String,
    /// Scanned `@path` tokens (literal body + resolved absolute path), in order.
    ///
    /// The caller reads and classifies each (native image → attach as-is;
    /// other image → convert; not an image → error). Path existence is
    /// **not** checked here — a path appears here even if the file is
    /// missing.
    pub pending_paths: Vec<PendingPath>,
}

/// Scans `text` for `@path` tokens, resolving each against `ctx`, rewriting
/// attachable tokens to `(file:///resolved/abs/path)` form and collecting
/// their resolved paths for later byte-reading.
///
/// Tokens whose literal body (the text after `@`) is in `degraded_raw` are
/// left as **literal text** (the original `@raw` token) and excluded from
/// `pending_paths`. This is how the actor marks a `@path` that turned out to
/// be missing or not-a-recognized-image: on re-expansion, the token stays
/// literal so the model sees exactly what the user typed. Matching is by the
/// raw token body (what the user typed), not the resolved path — this keeps
/// the skip check environment-independent and immune to cwd/home changes.
///
/// This is a **pure-text, I/O-free** operation: it does not touch the
/// filesystem. File existence and image-format classification happen in the
/// async session actor via [`classify_image_bytes`].
///
/// `foo@path` (no boundary) is never matched.
#[must_use]
pub fn scan_at_paths_with_degraded(
    text: &str,
    ctx: &PathResolveContext<'_>,
    degraded_raw: &[String],
) -> ScanResult {
    let mut pending_paths: Vec<PendingPath> = Vec::new();
    let rewritten_text = AT_PATH_RE
        .replace_all(text, |caps: &regex::Captures<'_>| {
            let boundary = &caps[1];
            let raw_path = &caps[2];
            let resolved = ctx.resolve(raw_path);
            if degraded_raw.iter().any(|r| r == raw_path) {
                // Degraded: leave the token as the original literal text.
                format!("{boundary}@{raw_path}")
            } else {
                // Use the resolved absolute path so terminals link to the real file.
                let rewrite = format!("{boundary}(file://{})", resolved.display());
                // Collect the resolved path for the actor's byte-reading phase.
                pending_paths.push(PendingPath {
                    raw: raw_path.to_owned(),
                    abs: resolved,
                });
                rewrite
            }
        })
        .into_owned();
    ScanResult {
        rewritten_text,
        pending_paths,
    }
}

/// Convenience wrapper for [`scan_at_paths_with_degraded`] with no degraded
/// tokens — the common case for first-time expansion.
#[must_use]
pub fn scan_at_paths(text: &str, ctx: &PathResolveContext<'_>) -> ScanResult {
    scan_at_paths_with_degraded(text, ctx, &[])
}

/// Classification of a file's bytes by image-format family.
///
/// Returned by [`classify_image_bytes`]; drives the actor's attach-or-convert
/// decision. Native formats attach directly; non-native formats need
/// conversion; `NotAnImage` is an error.
#[must_use]
pub enum ImageKind {
    /// A natively-supported image format (PNG/JPEG/GIF/WEBP) — attach as-is.
    Native { media_type: &'static str },
    /// A recognizable image that is not natively supported — needs conversion.
    NeedsConversion,
    /// The bytes are not a recognizable image format.
    NotAnImage,
}

/// Classifies raw bytes by image-format family without doing any I/O.
///
/// This is a pure function over bytes. The actor reads the file in
/// `spawn_blocking`, then calls this to decide: attach natively, convert, or
/// error.
pub fn classify_image_bytes(bytes: &[u8]) -> ImageKind {
    if let Some(media_type) = sniff_media_type(bytes) {
        ImageKind::Native { media_type }
    } else if is_recognizable_image(bytes) {
        ImageKind::NeedsConversion
    } else {
        ImageKind::NotAnImage
    }
}

/// Returns the MIME type for natively-supported image magic-byte signatures,
/// or `None` if the bytes do not match a known native format.
///
/// Native formats are the ones vision providers accept directly without
/// conversion: PNG, JPEG, GIF, WEBP.
#[must_use]
pub fn sniff_media_type(bytes: &[u8]) -> Option<&'static str> {
    // Order matters only for prefixes that are prefixes of others; none here overlap.
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12
        && bytes.starts_with(b"RIFF")
        && bytes.get(8..12) == Some(&b"WEBP"[..])
    {
        Some("image/webp")
    } else {
        None
    }
}

/// Heuristic detection of recognizable-but-non-native image formats that
/// ImageMagick can convert. Returns `true` for formats whose magic bytes we
/// recognize even though they are not natively accepted by vision providers.
///
/// This is intentionally permissive: a `false` here means "we have no idea
/// what this is," which becomes a hard error. A `true` means "try conversion."
#[must_use]
fn is_recognizable_image(bytes: &[u8]) -> bool {
    // HEIC / HEIF / AVIF (ISO Base Media File Format family).
    // Structure: [4-byte size]["ftyp"][4-byte brand]... so bytes[4..8]="ftyp",
    // bytes[8..12]=brand (heic/heix/mif1/msf1/avif/avis).
    bytes.len() >= 12
        && bytes.get(4..8) == Some(&b"ftyp"[..])
        && matches!(
            bytes.get(8..12),
            Some(b"heic" | b"heix" | b"mif1" | b"msf1" | b"avif" | b"avis")
        )
    // BMP
    || bytes.starts_with(b"BM")
    // TIFF (little-endian and big-endian).
    || bytes.starts_with(b"II*\x00")
    || bytes.starts_with(b"MM\x00*")
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]

    use super::*;

    /// Context with arbitrary cwd/home — absolute paths ignore both, so this
    /// suffices for any test that doesn't exercise relative/tilde resolution.
    fn dummy_ctx() -> PathResolveContext<'static> {
        PathResolveContext::new(std::path::Path::new("/"), std::path::Path::new("/"))
    }

    #[rstest::rstest]
    fn scan_collects_resolved_path_from_at_path() {
        // Given text referencing an absolute path (no file on disk — scan is I/O-free).
        let text = "describe @/abs/img.png";

        // When scanning.
        let result = scan_at_paths(text, &dummy_ctx());

        // Then exactly one resolved path was collected.
        assert_eq!(result.pending_paths.len(), 1);
        assert_eq!(result.pending_paths[0].abs, PathBuf::from("/abs/img.png"));
    }

    #[rstest::rstest]
    fn scan_rewrites_at_path_to_file_uri() {
        // Given text referencing an absolute path.
        let abs = "/abs/img.png";
        let text = format!("describe @{abs}");

        // When scanning.
        let result = scan_at_paths(&text, &dummy_ctx());

        // Then the @path was rewritten to the file:// URI form.
        assert!(
            result.rewritten_text.contains(&format!("(file://{abs})")),
            "rewritten text should contain the file:// URI, got: {}",
            result.rewritten_text
        );
        assert!(
            !result.rewritten_text.contains(&format!("@{abs}")),
            "original @path token should be gone"
        );
    }

    #[rstest::rstest]
    fn scan_does_not_match_email_at() {
        // Given text containing an email-style @ not followed by a path.
        let text = "contact foo@bar.com please";

        // When scanning.
        let result = scan_at_paths(text, &dummy_ctx());

        // Then no paths were collected and text is unchanged.
        assert_eq!(result.pending_paths, [] as [PendingPath; 0]);
        assert_eq!(result.rewritten_text, text);
    }

    #[rstest::rstest]
    fn scan_collects_path_even_when_file_missing() {
        // Given a reference to a nonexistent path — scan is pure-text, so the
        // path is still collected (existence is checked later by the actor).
        let text = "see @/nonexistent/path.png here";

        // When scanning.
        let result = scan_at_paths(text, &dummy_ctx());

        // Then the path was collected and the token was rewritten.
        assert_eq!(result.pending_paths.len(), 1);
        assert!(
            result
                .rewritten_text
                .contains("(file:///nonexistent/path.png)"),
            "token should still be rewritten even for missing files"
        );
    }

    #[rstest::rstest]
    fn scan_at_path_at_start_of_string() {
        // Given a path referenced at the very start of the text.
        let text = "@/abs/img.png what is this";

        // When scanning.
        let result = scan_at_paths(text, &dummy_ctx());

        // Then the path was collected and rewrite happens at string start.
        assert_eq!(result.pending_paths.len(), 1);
        assert!(result.rewritten_text.starts_with("(file://"));
    }

    #[rstest::rstest]
    fn scan_resolves_relative_path_against_cwd() {
        // Given a cwd-rooted temp dir and a relative path token.
        let dir = tempfile::tempdir().expect("temp dir");
        let text = "describe @img.png here";

        // When scanning with a context rooted at the temp dir.
        let ctx = PathResolveContext::new(dir.path(), std::path::Path::new("/"));
        let result = scan_at_paths(text, &ctx);

        // Then the resolved path is under the cwd.
        assert_eq!(result.pending_paths.len(), 1);
        assert_eq!(result.pending_paths[0].abs, dir.path().join("img.png"));
    }

    #[rstest::rstest]
    fn scan_resolves_tilde_path_against_home() {
        // Given a home-rooted temp dir and a tilde path token.
        let dir = tempfile::tempdir().expect("temp dir");
        let text = "describe @~/photo.png here";

        // When scanning with home pointing at the temp dir.
        let ctx = PathResolveContext::new(std::path::Path::new("/"), dir.path());
        let result = scan_at_paths(text, &ctx);

        // Then the resolved path is under home.
        assert_eq!(result.pending_paths.len(), 1);
        assert_eq!(result.pending_paths[0].abs, dir.path().join("photo.png"));
    }

    #[rstest::rstest]
    fn scan_tilde_alone_resolves_to_home() {
        // Given a tilde token with no subpath.
        let dir = tempfile::tempdir().expect("temp dir");
        let text = "@~/bare.png";

        // When scanning with home at the temp dir.
        let ctx = PathResolveContext::new(std::path::Path::new("/"), dir.path());
        let result = scan_at_paths(text, &ctx);

        // Then the resolved path is home/bare.png.
        assert_eq!(result.pending_paths.len(), 1);
        assert_eq!(result.pending_paths[0].abs, dir.path().join("bare.png"));
    }

    #[rstest::rstest]
    fn scan_space_in_filename_not_supported() {
        // Given text with a space in the filename. The @ token runs to the
        // next whitespace, so `@my` is the whole token.
        let text = "describe @my file.png here";

        // When scanning with cwd at a temp dir.
        let dir = tempfile::tempdir().expect("temp dir");
        let ctx = PathResolveContext::new(dir.path(), std::path::Path::new("/"));
        let result = scan_at_paths(text, &ctx);

        // Then only `@my` was collected (the space terminated the token).
        assert_eq!(result.pending_paths.len(), 1);
        assert_eq!(result.pending_paths[0].abs, dir.path().join("my"));
    }

    #[rstest::rstest]
    fn scan_relative_path_descends_into_subdirectory() {
        // Given a relative path into a subdirectory.
        let dir = tempfile::tempdir().expect("temp dir");
        let text = "see @sub/img.png";

        // When scanning with cwd at the temp dir root.
        let ctx = PathResolveContext::new(dir.path(), std::path::Path::new("/"));
        let result = scan_at_paths(text, &ctx);

        // Then the resolved path descends into the subdirectory.
        assert_eq!(result.pending_paths.len(), 1);
        assert_eq!(result.pending_paths[0].abs, dir.path().join("sub/img.png"));
    }

    #[rstest::rstest]
    fn scan_makes_no_filesystem_calls() {
        // Given a path that definitely does not exist.
        let text = "see @/this/does/not/exist.png";

        // When scanning.
        let result = scan_at_paths(text, &dummy_ctx());

        // Then the path is still collected — proving scan did not touch the
        // filesystem to check existence.
        assert_eq!(
            result
                .pending_paths
                .iter()
                .map(|p| p.abs.clone())
                .collect::<Vec<_>>(),
            vec![PathBuf::from("/this/does/not/exist.png")]
        );
    }

    #[rstest::rstest]
    fn classify_returns_native_for_png_bytes() {
        // Given PNG magic bytes.
        let png = b"\x89PNG\r\n\x1a\n\x00\x00";

        // When classifying.
        // Then it is native image/png.
        assert!(matches!(
            classify_image_bytes(png),
            ImageKind::Native {
                media_type: "image/png"
            }
        ));
    }

    #[rstest::rstest]
    fn classify_returns_needs_conversion_for_heic() {
        // Given HEIC ftyp box bytes (brand "heic").
        let heic = [
            0, 0, 0, 0x18, b'f', b't', b'y', b'p', b'h', b'e', b'i', b'c',
        ];

        // When classifying.
        // Then it needs conversion.
        assert!(matches!(
            classify_image_bytes(&heic),
            ImageKind::NeedsConversion
        ));
    }

    #[rstest::rstest]
    fn classify_returns_not_an_image_for_text() {
        // Given plain text bytes.
        // When classifying.
        // Then it is not an image.
        assert!(matches!(
            classify_image_bytes(b"plain text"),
            ImageKind::NotAnImage
        ));
    }

    #[rstest::rstest]
    fn sniff_png_from_magic_bytes() {
        // Given PNG magic bytes.
        let bytes = b"\x89PNG\r\n\x1a\n";

        // When sniffing the media type.
        // Then the sniffer identifies it as image/png.
        assert_eq!(sniff_media_type(bytes), Some("image/png"));
    }

    #[rstest::rstest]
    fn sniff_jpeg_from_magic_bytes() {
        // Given JPEG magic bytes.
        let bytes = b"\xff\xd8\xff\xe0";

        // When sniffing the media type.
        // Then the sniffer identifies it as image/jpeg.
        assert_eq!(sniff_media_type(bytes), Some("image/jpeg"));
    }

    #[rstest::rstest]
    fn sniff_returns_none_for_unknown() {
        // Given non-image bytes.
        let bytes = b"plain text";

        // When sniffing the media type.
        // Then the sniffer returns None.
        assert_eq!(sniff_media_type(bytes), None);
    }

    #[rstest::rstest]
    fn degraded_set_leaves_token_literal_and_excludes_from_pending() {
        // Given text with a boundary @path and a degraded set naming its raw token.
        let raw = "/abs/whatever";
        let text = format!("describe @{raw}");
        let degraded = vec![raw.to_owned()];

        // When scanning with the degraded set.
        let result = scan_at_paths_with_degraded(&text, &dummy_ctx(), &degraded);

        // Then the token stays literal (no file:// rewrite) and is not collected.
        assert!(
            result.rewritten_text.contains(&format!("@{raw}")),
            "degraded token should stay literal, got: {}",
            result.rewritten_text
        );
        assert!(
            !result.rewritten_text.contains("file://"),
            "degraded token must not be rewritten to file://"
        );
        assert!(
            result.pending_paths.is_empty(),
            "degraded token must not be collected as a pending path"
        );
    }

    #[rstest::rstest]
    fn degraded_re_scan_is_idempotent() {
        // Given text whose degraded token was reverted to literal on a first pass.
        let raw = "/abs/whatever";
        let text = format!("describe @{raw}");
        let degraded = vec![raw.to_owned()];
        let first = scan_at_paths_with_degraded(&text, &dummy_ctx(), &degraded);

        // When scanning the already-reverted text again with the same degraded set.
        let second = scan_at_paths_with_degraded(&first.rewritten_text, &dummy_ctx(), &degraded);

        // Then the literal token survives unchanged (no further rewrite).
        assert_eq!(
            second.rewritten_text, first.rewritten_text,
            "re-scanning reverted text must be a no-op"
        );
        assert_eq!(second.pending_paths, [] as [PendingPath; 0]);
    }

    #[rstest::rstest]
    fn mixed_degraded_and_attachable_tokens_split_correctly() {
        // Given text with one attachable @path and one degraded @path.
        let attachable = "/abs/real.png";
        let degraded_raw = "/abs/whatever";
        let text = format!("see @{attachable} and @{degraded_raw}");
        let degraded = vec![degraded_raw.to_owned()];

        // When scanning with the degraded set.
        let result = scan_at_paths_with_degraded(&text, &dummy_ctx(), &degraded);

        // Then only the attachable token is rewritten and collected.
        assert!(
            result
                .rewritten_text
                .contains(&format!("(file://{attachable})")),
            "attachable token should be rewritten: {}",
            result.rewritten_text
        );
        assert!(
            result.rewritten_text.contains(&format!("@{degraded_raw}")),
            "degraded token should stay literal: {}",
            result.rewritten_text
        );
        assert_eq!(
            result
                .pending_paths
                .iter()
                .map(|p| p.abs.clone())
                .collect::<Vec<_>>(),
            vec![PathBuf::from(attachable)]
        );
    }

    #[rstest::rstest]
    fn degraded_set_does_not_reenable_email_matching() {
        // Given email-style text (no boundary before @) and a non-empty degraded set.
        let text = "contact foo@bar.com";
        let degraded = vec!["bar.com".to_owned()];

        // When scanning with the degraded set.
        let result = scan_at_paths_with_degraded(text, &dummy_ctx(), &degraded);

        // Then the email is still not matched (text unchanged, nothing collected).
        assert_eq!(result.rewritten_text, text);
        assert_eq!(result.pending_paths, [] as [PendingPath; 0]);
    }
}
