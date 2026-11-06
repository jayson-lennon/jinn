//! Content signatures — what turns "the file changed" into "the *content*
//! changed".
//!
//! The watcher debounces notify's event bursts (editors write temp-file +
//! rename, which lands as several events), but debounce alone cannot tell
//! a real edit from jinn's own no-op write: a picker confirm that rewrites
//! the identical value still produces write-shaped events. The dedupe is
//! a content hash per watched file and a recursive fingerprint per
//! resource directory; a settled burst whose hashes match the last seen
//! publishes nothing.

use std::collections::HashMap;
use std::path::Path;

use marked_path::Absolute;
use marked_path::MarkedPathAccess as _;
use marked_path::MarkedPathBuf;
use sha2::Digest as _;

/// The SHA-256 of a file's bytes; a missing file hashes as empty.
///
/// A missing file hashing as empty makes deletion and emptying equivalent
/// at the signature level — both are content changes, which is the fact
/// the watcher reports.
#[must_use]
pub fn file_signature(path: &Path) -> [u8; 32] {
    let bytes = std::fs::read(path).unwrap_or_default();
    sha2::Sha256::digest(&bytes).into()
}

/// The last-seen signatures of the watched files.
#[derive(Debug, Default)]
pub struct SignatureLedger {
    files: HashMap<MarkedPathBuf<Absolute>, [u8; 32]>,
    resources: [u8; 32],
}

impl SignatureLedger {
    /// A ledger seeded with the current on-disk state.
    #[must_use]
    pub fn capture(
        files: &[MarkedPathBuf<Absolute>],
        resource_dirs: &[MarkedPathBuf<Absolute>],
    ) -> Self {
        let files = files
            .iter()
            .map(|path| (path.clone(), file_signature(path.as_path())))
            .collect();
        Self {
            files,
            resources: resource_signature(resource_dirs),
        }
    }

    /// Whether the file's content differs from the last seen signature,
    /// recording the new signature when it does.
    pub fn file_changed(&mut self, path: MarkedPathBuf<Absolute>) -> bool {
        let now = file_signature(path.as_path());
        let changed = self.files.get(&path).is_none_or(|last| *last != now);
        if changed {
            self.files.insert(path, now);
        }
        changed
    }

    /// Whether any resource directory's fingerprint differs from the last
    /// seen one, recording the new fingerprint when it does.
    pub fn resources_changed(&mut self, resource_dirs: &[MarkedPathBuf<Absolute>]) -> bool {
        let now = resource_signature(resource_dirs);
        let changed = self.resources != now;
        if changed {
            self.resources = now;
        }
        changed
    }
}

/// A stable fingerprint over a set of directories: every regular file's
/// relative path and size, hashed in sorted order.
///
/// Size and path, not mtime or content: an mtime fingerprint reports a
/// touch as a change, and content-hashing every file on every settle is
/// the cost the fingerprint exists to avoid. Size plus path catches the
/// cases that matter — a file added, removed, or rewritten to a different
/// length — and misses only a same-length rewrite, which a resource
/// re-scan would treat as a no-op anyway.
#[must_use]
pub fn resource_signature(dirs: &[MarkedPathBuf<Absolute>]) -> [u8; 32] {
    let mut hasher = sha2::Sha256::new();
    let mut entries: Vec<String> = Vec::new();
    for dir in dirs {
        // Viewing borrows are fine: only *owning* raw paths is banned.
        collect_entries(dir.as_path(), dir.as_path(), &mut entries);
    }
    entries.sort();
    for entry in &entries {
        sha2::Digest::update(&mut hasher, entry.as_bytes());
    }
    hasher.finalize().into()
}

/// Collects `rel:size` lines for every regular file under `root`.
fn collect_entries(root: &Path, dir: &Path, entries: &mut Vec<String>) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_dir() {
            collect_entries(root, &path, entries);
        } else {
            // Regular files and symlinks alike: the size is what the
            // fingerprint needs, not the kind.
            let size = entry.metadata().map_or(0, |meta| meta.len());
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .display()
                .to_string();
            entries.push(format!("{rel}:{size}"));
        }
    }
}
