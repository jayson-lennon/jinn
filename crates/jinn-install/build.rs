//! Build script: makes `include_dir!("$CARGO_MANIFEST_DIR/../../res")`
//! re-expand whenever the embedded resource tree changes *in shape*.
//!
//! `include_dir!` expands each file to a real `include_bytes!`, so editing a
//! file already in the tree re-triggers a rebuild on its own — but *adding*,
//! *moving*, or *deleting* one does not. Without this script a file dropped
//! into `res/` would never ship: the macro's expansion is cached and the
//! binary keeps the old tree. Cargo has no "watch this directory recursively"
//! directive, so every path currently under `res/` is declared individually;
//! a new file under a watched directory is a path cargo has not seen, so the
//! enclosing directories are declared too.
//!
//! (`include_dir`'s `nightly` feature solves this with `track_path`, but the
//! repo builds on stable and stays there.)
//!
//! Declared paths are relative to this crate's manifest directory, never to the
//! cwd. A rerun-if-changed path that does not exist makes cargo treat the
//! build script as always-dirty — recompiling on every build, silently and
//! permanently — so the build aborts instead.

#![allow(warnings, reason = "want to fail fast")]

use marked_path::{Absolute, MarkedPath, MarkedPathAccess, MarkedPathBuf};

/// The resource tree embedded by `jinn-install`.
const RES: &str = "../../res";

fn main() {
    let res = manifest_dir()
        .join(RES)
        .expect("RES is a relative constant, so the join stays absolute");

    assert!(
        res.is_dir(),
        "build script rerun path is not a directory: {} (resolved from CARGO_MANIFEST_DIR)",
        res.to_string_lossy()
    );

    watch_tree(&res.as_marked_path());
}

/// Emits `cargo:rerun-if-changed` for every path under `root`, files and
/// directories alike.
fn watch_tree(root: &MarkedPath<Absolute>) {
    let entries = std::fs::read_dir(root.as_path())
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", root.to_string_lossy()));

    let mut paths: Vec<MarkedPathBuf<Absolute>> = entries
        .map(|entry| {
            let path = entry
                .unwrap_or_else(|e| {
                    panic!("failed to read entry in {}: {e}", root.to_string_lossy())
                })
                .path();
            MarkedPathBuf::<Absolute>::new(&path).unwrap_or_else(|e| {
                panic!(
                    "build-script entry is not absolute: {}: {e}",
                    path.to_string_lossy()
                )
            })
        })
        .collect();
    paths.sort_by(|a, b| a.as_path().cmp(b.as_path()));

    for path in &paths {
        if path.is_dir() {
            watch_tree(&path.as_marked_path());
        }
        println!("cargo:rerun-if-changed={}", path.to_string_lossy());
    }
}

fn manifest_dir() -> MarkedPathBuf<Absolute> {
    let dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    MarkedPathBuf::<Absolute>::new(dir).expect("CARGO_MANIFEST_DIR is absolute")
}
