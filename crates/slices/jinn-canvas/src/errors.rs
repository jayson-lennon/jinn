//! Serve-path errors for the canvas slice.
//!
//! Every variant is stop-and-report: the canvas actor never degrades a
//! configuration error into a fallback silently. The failure sentence a
//! serve reply carries is the variant's detail line plus its [`fix`]
//! remedy, so the tool result that surfaces it names both the cause and
//! the way out.

use wherror::Error;

/// Failures raised by the canvas serve path.
#[derive(Debug, Error)]
#[error(debug)]
pub enum CanvasServeError {
    /// The configured `[canvas] bind` is not a host:port socket address.
    InvalidBind,
    /// The requested artifact file does not exist or is a directory.
    ArtifactMissing,
    /// The artifact could not be read from disk (canonicalize failed).
    ArtifactRead,
    /// Binding the listener failed (port taken, permission denied).
    Bind,
    /// The directory watcher could not be established: the artifact
    /// serves, but live reload cannot be armed for it.
    WatcherEstablish,
}

impl CanvasServeError {
    /// The one-line fix sentence the serve reply carries beneath the
    /// cause, so every failure surfaces its remedy with it.
    #[must_use]
    pub fn fix(&self) -> &'static str {
        match self {
            Self::InvalidBind => {
                "set [canvas] bind to a host:port socket address, e.g. bind = \"127.0.0.1:8899\""
            }
            Self::ArtifactMissing => "pass the path of an existing HTML file to serve",
            Self::ArtifactRead => "check the file's permissions and try again",
            Self::Bind => "choose another [canvas] bind port, then call the canvas tool again",
            Self::WatcherEstablish => {
                "check the artifact directory's permissions; serving continues without live reload"
            }
        }
    }
}
