//! Image-conversion binary discovery.
//!
//! A
//! testable filesystem seam (`ImageMagickLocator`) with a production
//! implementation (`SystemImageMagickLocator`) that probes `PATH` for the
//! ImageMagick binary, and injectable fakes for unit tests.
//!
//! ImageMagick v7 ships `magick`; v6 ships `convert`. The locator prefers
//! `magick` (current) and falls back to `convert` (legacy). Both accept the
//! identical `magick <input> png:-` / `convert <input> png:-` CLI shape used
//! by the converter to write a PNG to stdout.

use std::path::PathBuf;

/// Filesystem seam for locating the ImageMagick binary.
///
/// Production uses [`SystemImageMagickLocator`]; tests inject fakes that
/// report a fixed (or absent) resolved path without touching the disk.
pub trait ImageMagickLocator: Send + Sync {
    /// Resolve the ImageMagick executable to a concrete path, preferring
    /// `magick` (v7) over `convert` (v6). Returns `None` when neither is on
    /// `PATH`.
    fn find_binary(&self) -> Option<PathBuf>;
}

/// Production locator: probes `PATH` via the `which` crate.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemImageMagickLocator;

impl ImageMagickLocator for SystemImageMagickLocator {
    fn find_binary(&self) -> Option<PathBuf> {
        // Prefer v7's unified `magick` entry point; fall back to v6 `convert`.
        which::which("magick")
            .or_else(|_| which::which("convert"))
            .ok()
    }
}
