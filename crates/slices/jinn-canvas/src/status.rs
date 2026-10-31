//! The dashboard row's status vocabulary.
//!
//! The row's Notes column reads exactly two shapes: offline before the
//! first serve, and the bind URL plus served path while serving. The
//! format lives here, beside the actor that publishes it, so the
//! strings are testable without an actor and the tool result's URL (the
//! same shape) and the row can never drift apart.

use std::path::Path;

/// The row's status before any serve: no server is live.
#[must_use]
pub fn offline_message() -> String {
    "server offline".to_owned()
}

/// The row's status while serving: scheme'd bind URL and the served
/// path — the scheme makes terminals render the address as a link.
#[must_use]
pub fn serving_message(url: &str, artifact: &Path) -> String {
    format!("{url} ({})", artifact.display())
}
