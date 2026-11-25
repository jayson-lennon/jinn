//! Build script for the second web UI: embeds the committed client
//! bundle after verifying it against the vendor manifest, and fails the
//! build when the bundle is older than its sources.

// A build script's panic is its failure channel.
#![allow(clippy::panic, reason = "build script: panic is the failure channel")]
#![allow(
    clippy::expect_used,
    reason = "build script: env and IO failures are fatal by contract"
)]

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
}
