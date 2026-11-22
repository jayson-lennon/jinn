//! In-process PNG transcoding via the `image` crate — the first stage of the
//! conversion chain.
//!
//! [`ImageCrateConverter`] decodes a non-native image file in-process (no
//! subprocess) and re-encodes it as PNG. Formats the `image` crate cannot
//! decode (HEIC-family) error out — the signal the caller
//! ([`super::converter::FallbackConverter`]) uses to try the ImageMagick
//! fallback. The content-guessing reader decides the format from the file's
//! bytes, so no pre-gating by extension is needed: a decodable format decodes,
//! anything else errors.
//!
//! Default `image::Limits` apply (512 MiB max allocation per decode), so the
//! in-process path is as guarded against decompression bombs as the
//! subprocess path was.

use std::io::Cursor;

use error_stack::{Report, ResultExt};
use image::ImageFormat;
use marked_path::{Absolute, MarkedPathAccess, MarkedPathBuf};

use super::converter::{ImageConversionError, ImageConverter};

/// Converts a non-native image file to PNG in-process, via the `image` crate.
///
/// The first stage of the production conversion chain
/// ([`super::converter::FallbackConverter`]): formats the crate decodes
/// (BMP, TIFF today) convert without spawning a process, and every decode
/// failure propagates as `Err` so the fallback stage gets its chance.
#[derive(Debug, Default)]
pub struct ImageCrateConverter;

impl ImageCrateConverter {
    /// Construct the converter. The `image` crate is compiled in, so there is
    /// nothing to locate — every instance behaves identically.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl ImageConverter for ImageCrateConverter {
    fn name(&self) -> &'static str {
        "ImageCrate"
    }

    fn is_available(&self) -> bool {
        // The decoder is a compiled-in library dependency, not a located
        // binary: availability is a property of the build, not the machine.
        true
    }

    fn convert_to_png(
        &self,
        input: &MarkedPathBuf<Absolute>,
    ) -> Result<Vec<u8>, Report<ImageConversionError>> {
        // Content-sniffing replaces the extension-derived guess when the bytes
        // match a known format, so a `.heic`-named text file never decodes as
        // one. An undecodable format errors `Unsupported` here — the fallback
        // stage's signal.
        let decoded = image::ImageReader::open(input.as_path())
            .change_context(ImageConversionError)
            .attach(input.to_string())
            .attach("failed to open image for in-process decoding")?
            .with_guessed_format()
            .change_context(ImageConversionError)
            .attach(input.to_string())
            .attach("failed to sniff image format")?
            .decode()
            .change_context(ImageConversionError)
            .attach(input.to_string())
            .attach(
                "the image crate cannot decode this format (unsupported, truncated, or corrupt)",
            )?;

        encode_png(&decoded, input.to_string())
    }

    /// The in-memory override: decodes straight from the bytes, no temp file.
    ///
    /// Same sniffing and same error shape as the path-based stage — the bytes
    /// carry their own format, so a paste's PNG decodes regardless of what
    /// name (none) it arrived under.
    fn convert_to_png_bytes(&self, bytes: &[u8]) -> Result<Vec<u8>, Report<ImageConversionError>> {
        let decoded = image::ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .change_context(ImageConversionError)
            .attach("failed to sniff the image format from the bytes")?
            .decode()
            .change_context(ImageConversionError)
            .attach(
                "the image crate cannot decode these bytes (unsupported, truncated, or corrupt)",
            )?;

        encode_png(&decoded, "in-memory bytes".to_owned())
    }
}

/// Re-encodes a decoded image as PNG and validates the magic signature —
/// the tail both conversion stages share.
fn encode_png(
    decoded: &image::DynamicImage,
    context: String,
) -> Result<Vec<u8>, Report<ImageConversionError>> {
    let png = {
        let mut out = Cursor::new(Vec::new());
        decoded
            .write_to(&mut out, ImageFormat::Png)
            .change_context(ImageConversionError)
            .attach(context.clone())
            .attach("failed to re-encode decoded image as PNG")?;
        out.into_inner()
    };

    // Trait-contract parity: every backend returns PNG-magic-prefixed
    // bytes, regardless of how it produced them.
    if !png.starts_with(super::converter::PNG_MAGIC) {
        return Err(Report::new(ImageConversionError)
            .change_context(ImageConversionError)
            .attach(context)
            .attach("image crate output is not a valid PNG"));
    }

    Ok(png)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        reason = "test code"
    )]

    use super::*;

    /// A minimal valid 1x1 24-bit BMP (BITMAPINFOHEADER, bottom-up, no
    /// compression), generated once with `magick -size 1x1 xc:lime -depth 8
    /// BMP3:` and inlined so tests never shell out. 58 bytes, matching the
    /// header's declared file size (bytes 2–6) and pixel-data offset 54; the
    /// 4-byte pixel row is one lime pixel in BGR (`00 ff 00`) plus its pad.
    const BMP1X1: &[u8] = &[
        0x42, 0x4d, 0x3a, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x36, 0x00, 0x00, 0x00, 0x28,
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x18, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0x00, 0x00,
    ];

    /// PNG magic signature, duplicated from `converter` (private there) for
    /// test assertions.
    const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

    /// HEIC-shaped bytes: ftyp box with the heic brand — a format the
    /// `image` crate (without `avif-native`) cannot decode.
    fn heic_bytes() -> Vec<u8> {
        let mut v = vec![0x00, 0x00, 0x00, 0x08];
        v.extend_from_slice(b"ftypheicpayload");
        v
    }

    #[rstest::rstest]
    #[test]
    fn bmp_fixture_decodes_to_png_in_process() {
        // Given a converter and a file holding a minimal BMP.
        let converter = ImageCrateConverter::new();
        let dir = jinn_testutil::tempdir();
        let path = dir
            .path
            .join("tiny.bmp")
            .expect("temp root joins a bare name");
        std::fs::write(&path, BMP1X1).expect("write bmp");

        // When converting it to PNG.
        let png = converter.convert_to_png(&path).expect("converts");

        // Then the output starts with the PNG magic signature.
        assert!(png.starts_with(PNG_MAGIC));
    }

    #[rstest::rstest]
    #[test]
    fn heic_bytes_error_in_the_image_crate_converter() {
        // Given a converter and a file holding HEIC bytes.
        let converter = ImageCrateConverter::new();
        let dir = jinn_testutil::tempdir();
        let path = dir
            .path
            .join("photo.heic")
            .expect("temp root joins a bare name");
        std::fs::write(&path, heic_bytes()).expect("write heic");

        // When converting.
        let result = converter.convert_to_png(&path);

        // Then it errors (the fallback stage's signal).
        assert!(result.is_err());
    }

    #[rstest::rstest]
    #[test]
    fn missing_file_errors_in_the_image_crate_converter() {
        // Given a converter and a path under a temp root that does not exist.
        let converter = ImageCrateConverter::new();
        let dir = jinn_testutil::tempdir();
        let path = dir
            .path
            .join_all(["nonexistent", "x.tiff"])
            .expect("name joins keep the base absolute");

        // When converting.
        let result = converter.convert_to_png(&path);

        // Then it errors.
        assert!(result.is_err());
    }
}
