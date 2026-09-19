//! Blocking image-attachment resolution: read + classify + convert.
//!
//! These free functions run inside `spawn_blocking` (off the async runtime) and
//! turn a list of resolved `@path` file paths into [`Attachment`]s:
//!
//! 1. Read the file bytes.
//! 2. Classify via [`classify_image_bytes`] as native / needs-conversion /
//!    not-an-image.
//! 3. Native formats (PNG/JPEG/GIF/WEBP) attach directly; non-native
//!    recognizable images are transcoded to PNG via the
//!    [`ImageConverterService`] (ImageMagick); anything else is an error.
//!
//! On any failure the whole resolution fails — the actor surfaces a single
//! `Error` entry rather than a partial attachment set.

use std::path::Path;

use error_stack::{Report, ResultExt};

use crate::feat::context::prompt_template::{ImageKind, PendingPath, classify_image_bytes};
use crate::feat::image_convert::ImageConverterService;
use crate::protocol::ResolvedToken;
use jinn_provider::Attachment;

/// Errors that can occur while resolving `@path` image attachments.
#[derive(Debug, wherror::Error)]
#[error(debug)]
pub struct ImageResolveError;

/// The result of resolving a batch of `@path` image attachments.
///
/// Attachable images (native or successfully converted) end up in
/// [`attachments`]. Paths that are **not** attachable — the file is missing,
/// or it exists but is not a recognized image — end up in [`degraded`]
/// and are left as literal text in the message. A conversion failure on a
/// *recognizable* image does **not** degrade; it is a hard error (see the
/// `Result`-returning entry point).
///
/// [`attachments`]: ResolveOutcome::attachments
/// [`degraded`]: ResolveOutcome::degraded
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct ResolveOutcome {
    /// Successfully resolved image attachments, in path order.
    pub attachments: Vec<Attachment>,
    /// Resolved tokens that attached successfully as images, in order.
    ///
    /// Pairs positionally with [`attachments`]; used to build the per-token
    /// outcome marker that colors attached `@path` tokens in the render.
    ///
    /// [`attachments`]: ResolveOutcome::attachments
    pub attached: Vec<ResolvedToken>,
    /// Resolved tokens that degraded (missing file or not-an-image), in order.
    ///
    /// The caller leaves these tokens as literal text instead of attaching.
    pub degraded: Vec<ResolvedToken>,
}

/// Reads, classifies, and (if needed) converts each path into an
/// [`Attachment`]. Missing files and non-image files **degrade** (they land in
/// [`ResolveOutcome::degraded`] and do not abort the batch); a
/// conversion failure on a *recognizable* image is a hard error (`Err`),
/// since the user clearly intended an image attachment there.
///
/// This is a blocking function — callers must run it inside `spawn_blocking`.
pub(super) fn resolve_attachments_blocking(
    paths: &[PendingPath],
    converter: &ImageConverterService,
) -> Result<ResolveOutcome, Report<ImageResolveError>> {
    let mut attachments = Vec::with_capacity(paths.len());
    let mut attached = Vec::with_capacity(paths.len());
    let mut degraded = Vec::new();
    for p in paths {
        match resolve_one_blocking(&p.abs, converter)? {
            OneOutcome::Attached(attachment) => {
                attachments.push(attachment);
                attached.push(ResolvedToken {
                    raw: p.raw.clone(),
                    abs: p.abs.clone(),
                });
            }
            OneOutcome::Degraded => degraded.push(ResolvedToken {
                raw: p.raw.clone(),
                abs: p.abs.clone(),
            }),
        }
    }
    Ok(ResolveOutcome {
        attachments,
        attached,
        degraded,
    })
}

/// Outcome of resolving a single `@path`.
///
/// `Attached` adds an attachment; `Degraded` leaves the token as literal
/// text (the file is missing or not a recognized image). Conversion failure
/// on a *recognizable* image does not produce either — it bubbles as `Err`
/// from [`resolve_one_blocking`] so the whole batch aborts.
enum OneOutcome {
    Attached(Attachment),
    Degraded,
}

/// Resolves a single path to an attachment, or degrades it.
///
/// - Missing file → [`OneOutcome::Degraded`] (no error).
/// - Existing file, not a recognized image → [`OneOutcome::Degraded`].
/// - Recognizable image needing conversion → conversion runs; on success
///   attaches, on failure returns `Err` (hard error — the user meant an image).
fn resolve_one_blocking(
    path: &Path,
    converter: &ImageConverterService,
) -> Result<OneOutcome, Report<ImageResolveError>> {
    // Missing file: degrade rather than error. A `@word` that isn't a file is
    // almost certainly prose, not a broken attachment attempt.
    let Ok(bytes) = std::fs::read(path) else {
        return Ok(OneOutcome::Degraded);
    };
    match classify_image_bytes(&bytes) {
        ImageKind::Native { media_type } => Ok(OneOutcome::Attached(Attachment::image(
            media_type.to_owned(),
            bytes,
        ))),
        ImageKind::NeedsConversion => {
            convert_via_imagemagick(path, converter).map(OneOutcome::Attached)
        }
        ImageKind::NotAnImage => Ok(OneOutcome::Degraded),
    }
}

/// Transcodes a non-native image to PNG via ImageMagick. Errors when the
/// converter is unavailable (ImageMagick not found) or the conversion fails
/// (non-zero exit, invalid output).
fn convert_via_imagemagick(
    path: &Path,
    converter: &ImageConverterService,
) -> Result<Attachment, Report<ImageResolveError>> {
    let png_bytes = converter
        .convert_to_png(path)
        .change_context(ImageResolveError)
        .attach(path.to_string_lossy().to_string())?;
    Ok(Attachment::image(String::from("image/png"), png_bytes))
}

/// Renders a resolution-failure [`Report`] into a user-facing message.
///
/// Joins the error's context chain (the top-level message plus attached
/// context frames) into a single string.
#[must_use]
pub(super) fn format_attachment_error(report: &Report<ImageResolveError>) -> String {
    use std::fmt::Write as _;
    let mut out = String::from("Could not attach image: ");
    // The Report's Display already includes the error and all attachments.
    let _ = write!(out, "{report:}");
    out
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        reason = "test code"
    )]

    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    use error_stack::Report;

    use super::*;
    use crate::feat::image_convert::{ImageConversionError, ImageConverter};

    /// A fake converter that returns canned PNG bytes or fails, recording its
    /// calls so tests can assert conversion was/wasn't attempted.
    #[derive(Default)]
    struct FakeConverter {
        available: bool,
        fail: bool,
        calls: Mutex<Vec<PathBuf>>,
    }

    impl ImageConverter for FakeConverter {
        fn name(&self) -> &'static str {
            "Fake"
        }
        fn convert_to_png(&self, input: &Path) -> Result<Vec<u8>, Report<ImageConversionError>> {
            self.calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(input.to_path_buf());
            if !self.available {
                return Err(Report::new(ImageConversionError));
            }
            if self.fail {
                Err(Report::new(ImageConversionError))
            } else {
                Ok(b"\x89PNG\r\n\x1a\nfake".to_vec())
            }
        }
        fn is_available(&self) -> bool {
            self.available
        }
    }

    fn service(available: bool, fail: bool) -> ImageConverterService {
        ImageConverterService::new(Arc::new(FakeConverter {
            available,
            fail,
            calls: Mutex::new(Vec::new()),
        }))
    }

    fn png_bytes() -> Vec<u8> {
        b"\x89PNG\r\n\x1a\nbody".to_vec()
    }

    /// Wraps a resolved path in a `PendingPath` with the basename as the raw
    /// token body — sufficient for resolver tests (only `abs` drives I/O).
    fn pending(path: impl Into<PathBuf>) -> PendingPath {
        let abs = path.into();
        let raw = abs
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        PendingPath { raw, abs }
    }
    #[rstest::rstest]
    #[test]
    fn native_png_attaches_without_conversion() {
        // Given a native PNG file.
        let tmp = tempfile::tempdir().expect("tmp");
        let path = tmp.path().join("native.png");
        std::fs::write(&path, png_bytes()).expect("write");
        let converter = service(true, false);

        // When resolving.
        let result = resolve_attachments_blocking(&[pending(path)], &converter);

        // Then exactly one image/png attachment is returned (none degraded).
        let outcome = result.expect("resolve");
        assert_eq!(outcome.attachments.len(), 1);
        assert_eq!(outcome.attachments[0].media_type(), "image/png");
        assert!(outcome.degraded.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn non_native_image_is_converted_when_converter_available() {
        // Given a recognizable-but-non-native image (HEIC magic bytes).
        let tmp = tempfile::tempdir().expect("tmp");
        let path = tmp.path().join("photo.heic");
        // ftyp box: [size][ftyp][brand=heic]
        let mut heic = vec![0x00, 0x00, 0x00, 0x08];
        heic.extend_from_slice(b"ftyp");
        heic.extend_from_slice(b"heic");
        heic.extend_from_slice(b"payload");
        std::fs::write(&path, &heic).expect("write");
        let converter = service(true, false);

        // When resolving.
        let result = resolve_attachments_blocking(&[pending(path)], &converter);

        // Then a converted image/png attachment is returned (none degraded).
        let outcome = result.expect("resolve");
        assert_eq!(outcome.attachments.len(), 1);
        assert_eq!(outcome.attachments[0].media_type(), "image/png");
        assert!(outcome.degraded.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn non_native_image_errors_when_converter_unavailable() {
        // Given a non-native image and an unavailable converter.
        let tmp = tempfile::tempdir().expect("tmp");
        let path = tmp.path().join("photo.heic");
        let mut heic = vec![0x00, 0x00, 0x00, 0x08];
        heic.extend_from_slice(b"ftypheicpayload");
        std::fs::write(&path, &heic).expect("write");
        let converter = service(false, false);

        // When resolving.
        let result = resolve_attachments_blocking(&[pending(path)], &converter);

        // Then it errors.
        assert!(result.is_err());
    }

    #[rstest::rstest]
    #[test]
    fn non_native_image_errors_when_conversion_fails() {
        // Given a non-native image and a failing converter.
        let tmp = tempfile::tempdir().expect("tmp");
        let path = tmp.path().join("photo.heic");
        let mut heic = vec![0x00, 0x00, 0x00, 0x08];
        heic.extend_from_slice(b"ftypheicpayload");
        std::fs::write(&path, &heic).expect("write");
        let converter = service(true, true);

        // When resolving.
        let result = resolve_attachments_blocking(&[pending(path)], &converter);

        // Then it errors.
        assert!(result.is_err());
    }

    #[rstest::rstest]
    #[test]
    fn not_an_image_degrades() {
        // Given a non-image file.
        let tmp = tempfile::tempdir().expect("tmp");
        let path = tmp.path().join("notes.txt");
        std::fs::write(&path, b"hello world").expect("write");
        let converter = service(true, false);

        // When resolving.
        let result = resolve_attachments_blocking(&[pending(&path)], &converter);

        // Then it degrades: no error, no attachment, path in degraded set.
        let outcome = result.expect("resolve");
        assert!(outcome.attachments.is_empty());
        assert_eq!(outcome.degraded.len(), 1);
        assert_eq!(outcome.degraded[0].abs, path);
    }

    #[rstest::rstest]
    #[test]
    fn missing_file_degrades() {
        // Given a path to a nonexistent file.
        let path = PathBuf::from("/nonexistent/x.png");
        let converter = service(true, false);

        // When resolving.
        let result = resolve_attachments_blocking(&[pending(&path)], &converter);

        // Then it degrades: no error, no attachment, path in degraded set.
        let outcome = result.expect("resolve");
        assert!(outcome.attachments.is_empty());
        assert_eq!(outcome.degraded.len(), 1);
        assert_eq!(outcome.degraded[0].abs, path);
    }

    #[rstest::rstest]
    #[test]
    fn mixed_native_and_non_native_all_resolve() {
        // Given one native PNG and one non-native HEIC.
        let tmp = tempfile::tempdir().expect("tmp");
        let png_path = tmp.path().join("a.png");
        std::fs::write(&png_path, png_bytes()).expect("write");
        let heic_path = tmp.path().join("b.heic");
        let mut heic = vec![0x00, 0x00, 0x00, 0x08];
        heic.extend_from_slice(b"ftypheicpayload");
        std::fs::write(&heic_path, &heic).expect("write");
        let converter = service(true, false);

        // When resolving both.
        let result =
            resolve_attachments_blocking(&[pending(png_path), pending(heic_path)], &converter);

        // Then two image/png attachments are returned (none degraded).
        let outcome = result.expect("resolve");
        assert_eq!(outcome.attachments.len(), 2);
        assert_eq!(outcome.attachments[0].media_type(), "image/png");
        assert_eq!(outcome.attachments[1].media_type(), "image/png");
        assert!(outcome.degraded.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn mixed_native_png_and_missing_path_attaches_and_degrades() {
        // Given one native PNG and one nonexistent path in the same batch.
        let tmp = tempfile::tempdir().expect("tmp");
        let png_path = tmp.path().join("a.png");
        std::fs::write(&png_path, png_bytes()).expect("write");
        let missing_path = PathBuf::from("/nonexistent/b.png");
        let converter = service(true, false);

        // When resolving both.
        let result = resolve_attachments_blocking(
            &[pending(png_path), pending(missing_path.clone())],
            &converter,
        );

        // Then one attachment and one degraded path; no error.
        let outcome = result.expect("resolve");
        assert_eq!(outcome.attachments.len(), 1);
        assert_eq!(outcome.attachments[0].media_type(), "image/png");
        assert_eq!(outcome.degraded.len(), 1);
        assert_eq!(outcome.degraded[0].abs, missing_path);
    }
}
