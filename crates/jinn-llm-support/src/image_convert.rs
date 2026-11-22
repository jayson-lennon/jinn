//! Image conversion for non-native `@path` attachments.
//!
//! Native image formats (PNG/JPEG/GIF/WEBP) attach directly without
//! conversion. Other image formats (HEIC, AVIF, TIFF, BMP, …) are transcoded
//! to PNG by [`ImageConverterService`]: the `image` crate decodes in-process
//! where it can (BMP, TIFF today), and the ImageMagick CLI takes whatever it
//! cannot (HEIC-family), invoked from the session actor inside
//! `spawn_blocking` so the blocking decode or process spawn never stalls the
//! async runtime.
//!
//! Binary discovery mirrors the browser-binary resolver: a testable
//! [`binary_locator::ImageMagickLocator`] seam with a production
//! [`binary_locator::SystemImageMagickLocator`] that probes `PATH`.

mod binary_locator;
mod converter;
mod image_crate;
mod resolve;

pub use binary_locator::{ImageMagickLocator, SystemImageMagickLocator};
pub use converter::{
    FallbackConverter, ImageConversionError, ImageConverter, ImageConverterService,
    ImageMagickConverter, UnavailableConverter,
};
pub use image_crate::ImageCrateConverter;
pub use resolve::{
    ImageBytesError, ImageFileError, ImageResolveError, ResolveOutcome, ResolvedImage,
    format_attachment_error, ingest_image_bytes_blocking, resolve_attachments_blocking,
    resolve_image_file_blocking,
};

#[cfg(test)]
mod tests;
