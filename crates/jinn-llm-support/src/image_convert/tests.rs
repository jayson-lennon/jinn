#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "test code"
)]
// The fakes mirror the locator trait, whose resolved values ride PATH
// entries that may be relative.
#![expect(
    clippy::disallowed_types,
    reason = "test fakes mirror the PATH-scanner locator trait; the trait's resolved values may be relative"
)]

use std::path::PathBuf;
use std::sync::Arc;

use error_stack::Report;
use marked_path::MarkedPathAccess;
use marked_path::{Absolute, MarkedPathBuf};
use rstest::rstest;

use super::binary_locator::ImageMagickLocator;
use super::converter::{
    FallbackConverter, ImageConversionError, ImageConverter, ImageConverterService,
    ImageMagickConverter,
};

// ---------------------------------------------------------------------------
// Test fakes
// ---------------------------------------------------------------------------

/// Fake locator returning a fixed (or absent) resolved path.
struct FakeLocator {
    resolved: Option<PathBuf>,
}

impl ImageMagickLocator for FakeLocator {
    fn find_binary(&self) -> Option<PathBuf> {
        self.resolved.clone()
    }
}

/// Records whether `convert_to_png` / `is_available` were exercised. Lets
/// tests assert "converter not called for native formats" behaviorally.
#[derive(Default, Debug)]
struct SpyConverter {
    available: bool,
    convert_calls: std::sync::Mutex<Vec<MarkedPathBuf<Absolute>>>,
    /// Canned bytes returned on convert (when `available` and non-empty).
    canned: Vec<u8>,
    /// When true, convert returns an error instead of canned bytes.
    fail: bool,
}

impl ImageConverter for SpyConverter {
    fn name(&self) -> &'static str {
        "Spy"
    }

    fn is_available(&self) -> bool {
        self.available
    }

    fn convert_to_png(
        &self,
        input: &MarkedPathBuf<Absolute>,
    ) -> Result<Vec<u8>, Report<ImageConversionError>> {
        self.convert_calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(input.clone());
        if self.fail {
            Err(Report::new(ImageConversionError))
        } else {
            Ok(self.canned.clone())
        }
    }
}

// ---------------------------------------------------------------------------
// Locator tests
// ---------------------------------------------------------------------------

#[rstest]
fn locator_prefers_magick_over_convert() {
    // Given an ImageMagickConverter built via the system locator.
    // (Structural: SystemImageMagickLocator tries `magick` then `convert`.
    // We assert the converter reflects whatever is on this machine's PATH —
    // the only observable without mocking `which`.)
    let converter = ImageMagickConverter::new();

    // When querying availability.
    // Then it does not panic and reports a stable boolean.
    let _ = converter.is_available();
}

#[rstest]
fn converter_is_unavailable_when_locator_finds_nothing() {
    // Given a converter whose locator resolves no binary.
    let converter = ImageMagickConverter::with_locator(&FakeLocator { resolved: None });

    // When querying availability.
    // Then it reports unavailable.
    assert!(!converter.is_available());
}

#[rstest]
fn converter_is_available_when_locator_resolves() {
    // Given a converter whose locator resolves a binary.
    let dir = jinn_testutil::tempdir();
    let converter = ImageMagickConverter::with_locator(&FakeLocator {
        resolved: Some(
            dir.path
                .join_all(["bin", "magick"])
                .expect("name joins keep the base absolute")
                .to_path_buf(),
        ),
    });

    // When querying availability.
    // Then it reports available.
    assert!(converter.is_available());
}

// ---------------------------------------------------------------------------
// Converter behavior
// ---------------------------------------------------------------------------

#[rstest]
fn convert_errors_when_binary_unavailable() {
    // Given a converter with no resolved binary.
    let converter = ImageMagickConverter::with_locator(&FakeLocator { resolved: None });

    // When converting a path.
    let dir = jinn_testutil::tempdir();
    let input = dir
        .path
        .join("x.heic")
        .expect("temp root joins a bare name");
    let result = converter.convert_to_png(&input);

    // Then it errors without spawning a process.
    assert!(result.is_err());
}

#[rstest]
fn service_reports_unavailable_when_backend_is() {
    // Given a service wrapping an unavailable spy backend.
    let service = ImageConverterService::new(Arc::new(SpyConverter {
        available: false,
        ..SpyConverter::default()
    }));

    // When querying availability.
    // Then the service reports unavailable.
    assert!(!service.is_available());
}

#[rstest]
fn service_reports_available_when_backend_is() {
    // Given a service wrapping an available spy backend.
    let service = ImageConverterService::new(Arc::new(SpyConverter {
        available: true,
        ..SpyConverter::default()
    }));

    // When querying availability.
    // Then the service reports available.
    assert!(service.is_available());
}

#[rstest]
fn service_convert_returns_canned_bytes_when_backend_available() {
    // Given a service wrapping an available spy returning canned PNG bytes.
    let png = b"\x89PNG\r\n\x1a\nDATA".to_vec();
    let service = ImageConverterService::new(Arc::new(SpyConverter {
        available: true,
        canned: png.clone(),
        ..SpyConverter::default()
    }));

    // When converting.
    let dir = jinn_testutil::tempdir();
    let input = dir
        .path
        .join("x.heic")
        .expect("temp root joins a bare name");
    let result = service.convert_to_png(&input);

    // Then the canned bytes are returned.
    assert_eq!(result.expect("convert"), png);
}

#[rstest]
fn service_convert_errors_when_backend_fails() {
    // Given a service wrapping an available-but-failing spy.
    let service = ImageConverterService::new(Arc::new(SpyConverter {
        available: true,
        fail: true,
        ..SpyConverter::default()
    }));

    // When converting.
    // Then it errors.
    let dir = jinn_testutil::tempdir();
    let input = dir
        .path
        .join("x.heic")
        .expect("temp root joins a bare name");
    assert!(service.convert_to_png(&input).is_err());
}

#[rstest]
fn service_name_reflects_backend() {
    // Given a service wrapping a spy.
    let service = ImageConverterService::new(Arc::new(SpyConverter::default()));

    // When querying the name.
    // Then it reports the spy's name.
    assert_eq!(service.name(), "Spy");
}

// ---------------------------------------------------------------------------
// Fallback chain (ImageCrateConverter → fallback)
// ---------------------------------------------------------------------------

/// A minimal valid 1x1 24-bit BMP (BITMAPINFOHEADER, bottom-up, no
/// compression), generated once with `magick -size 1x1 xc:lime -depth 8
/// BMP3:` and inlined so tests never shell out. 58 bytes, matching the
/// header's declared file size (bytes 2–6) and pixel-data offset 54; the
/// 4-byte pixel row is one lime pixel in BGR (`00 ff 00`) plus its pad.
/// The `image` crate decodes BMP in-process, so a BMP input exercises the
/// chain's primary stage without ImageMagick.
const BMP1X1: &[u8] = &[
    0x42, 0x4d, 0x3a, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x36, 0x00, 0x00, 0x00, 0x28, 0x00,
    0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x18, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0x00, 0x00,
];

#[rstest]
fn fallback_converter_converts_bmp_without_touching_the_fallback() {
    // Given a fallback chain whose fallback stage is an available spy.
    let spy = Arc::new(SpyConverter {
        available: true,
        ..SpyConverter::default()
    });
    let converter = FallbackConverter::with_fallback(spy.clone());
    let dir = jinn_testutil::tempdir();
    let input = dir
        .path
        .join("tiny.bmp")
        .expect("temp root joins a bare name");
    std::fs::write(&input, BMP1X1).expect("write bmp");

    // When converting the BMP (decodable by the primary Rust stage).
    let result = converter.convert_to_png(&input);

    // Then it succeeds with PNG-magic output.
    let png = result.expect("converts in-process");
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    // And the fallback stage was never called.
    assert!(
        spy.convert_calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
    );
}

#[rstest]
fn fallback_converter_uses_the_fallback_when_the_primary_cannot_decode() {
    // Given a fallback chain whose fallback stage returns canned PNG bytes.
    let canned = b"\x89PNG\r\n\x1a\nfrom-fallback".to_vec();
    let spy = Arc::new(SpyConverter {
        available: true,
        canned: canned.clone(),
        ..SpyConverter::default()
    });
    let converter = FallbackConverter::with_fallback(spy);
    let dir = jinn_testutil::tempdir();
    let input = dir
        .path
        .join("photo.heic")
        .expect("temp root joins a bare name");
    // ftyp box with the heic brand — undecodable by the `image` crate.
    let mut heic = vec![0x00, 0x00, 0x00, 0x08];
    heic.extend_from_slice(b"ftypheicpayload");
    std::fs::write(&input, &heic).expect("write heic");

    // When converting the HEIC.
    let result = converter.convert_to_png(&input);

    // Then the fallback's bytes come back.
    assert_eq!(result.expect("converts via fallback"), canned);
}

#[rstest]
fn fallback_converter_errors_when_both_stages_fail() {
    // Given a fallback chain whose fallback stage errors.
    let converter = FallbackConverter::with_fallback(Arc::new(SpyConverter {
        fail: true,
        ..SpyConverter::default()
    }));
    let dir = jinn_testutil::tempdir();
    let input = dir
        .path
        .join("photo.heic")
        .expect("temp root joins a bare name");
    let mut heic = vec![0x00, 0x00, 0x00, 0x08];
    heic.extend_from_slice(b"ftypheicpayload");
    std::fs::write(&input, &heic).expect("write heic");

    // When converting the HEIC (primary cannot decode, fallback errors).
    let result = converter.convert_to_png(&input);

    // Then it errors.
    assert!(result.is_err());
}
