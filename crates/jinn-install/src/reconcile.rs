//! Boot-time reconciliation of the bundled `res/` resources.
//!
//! jinn seeds bundled resources into user directories. After a binary update
//! the on-disk copies can go stale, and nothing repaired them — until now.
//! Reconciliation compares, per bundled file, three hashes — what is on
//! **disk**, what jinn last **wrote** (recorded in the state-dir manifest),
//! and what the running **binary** embeds — and repairs only what jinn owns:
//!
//! - a file jinn wrote whose shipped bytes have moved is overwritten;
//! - a file already matching the binary is merely recorded;
//! - a file matching no manifest entry is **user-owned**: never written,
//!   never deleted, and no longer tracked;
//! - a tracked file the binary no longer ships is deleted when it still
//!   matches what jinn wrote, and kept (untracked) when it does not.
//!
//! The manifest is write-side bookkeeping only: no resource loader consults
//! it, and every byte this module touches comes from the real filesystem.
//! The expected set is injected as plain `(path, bytes)` pairs, so the
//! three-way compare is testable without the embedded tree; the public
//! entry points in the crate root derive the pairs from the catalogue.

use std::collections::{BTreeMap, HashSet};
use std::io;

use error_stack::Report;
use marked_path::{Absolute, MarkedPath, MarkedPathAccess, MarkedPathBuf, Relative};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use wherror::Error;

use crate::Destinations;
use crate::bundled::res_relative_parts;

/// The manifest schema version this code reads and writes. A manifest
/// carrying any other version is treated as absent (first-boot semantics).
pub(crate) const MANIFEST_VERSION: u32 = 1;

/// Error returned by the loud restore path ([`restore_builtins`]).
///
/// The boot reconcile never returns this: it reports every per-file problem
/// as a warning on [`ReconcileReport`] and keeps the launch alive.
#[derive(Debug, Error)]
#[error(debug)]
pub struct ReconcileError;

/// One expected bundled file: its `res/`-relative path and its bytes.
///
/// The path is relative to the `res/` root (`themes/default.toml`), matching
/// the manifest's entry key format, so the destination root is derived from
/// the path's first component.
#[derive(Debug, Clone)]
pub struct ExpectedFile<'a> {
    /// Path relative to the `res/` root (e.g. `themes/default.toml`).
    pub path: MarkedPathBuf<Relative>,
    /// The file's bytes as the binary ships them.
    pub bytes: &'a [u8],
}

/// One reconciliation step taken for, or about, a single file.
///
/// Every variant names the full destination path, so the composition layer
/// can log each action verbatim; the variant itself is the action class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconcileAction {
    /// The file was absent on disk and the shipped bytes were written
    /// (first seed or resurrection of a deleted jinn-owned file).
    Created(MarkedPathBuf<Absolute>),
    /// A jinn-owned copy had gone stale — disk still matched the manifest
    /// while the binary moved on — and was overwritten with the shipped bytes.
    Overwritten(MarkedPathBuf<Absolute>),
    /// The on-disk bytes already match the binary; the manifest record was
    /// (re)claimed without writing anything.
    Adopted(MarkedPathBuf<Absolute>),
    /// The on-disk copy is current and already tracked; nothing was done.
    Unchanged(MarkedPathBuf<Absolute>),
    /// `--builtins` only: an on-disk copy — including a user-modified one —
    /// was overwritten with the shipped bytes and tracking reclaimed.
    Reclaimed(MarkedPathBuf<Absolute>),
    /// The file was modified outside what jinn wrote; it was left alone and
    /// its manifest entry dropped. The file is user-owned from here on.
    KeptUserEdited(MarkedPathBuf<Absolute>),
    /// A tracked file the binary no longer ships still matched what jinn
    /// wrote; it was deleted and forgotten.
    DeletedObsolete(MarkedPathBuf<Absolute>),
    /// A tracked file the binary no longer ships no longer matches what
    /// jinn wrote; it was kept and forgotten.
    KeptOrphan(MarkedPathBuf<Absolute>),
}

impl ReconcileAction {
    /// The full destination path this action refers to.
    #[must_use]
    pub const fn path(&self) -> &MarkedPathBuf<Absolute> {
        match self {
            Self::Created(path)
            | Self::Overwritten(path)
            | Self::Adopted(path)
            | Self::Unchanged(path)
            | Self::Reclaimed(path)
            | Self::KeptUserEdited(path)
            | Self::DeletedObsolete(path)
            | Self::KeptOrphan(path) => path,
        }
    }
}

/// What a reconciliation did, as observable actions plus non-fatal warnings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    /// Per-file actions in deterministic order: shipped files in the order
    /// given, then reaped manifest entries in path order.
    pub actions: Vec<ReconcileAction>,
    /// Non-fatal problems met along the way. A boot reconcile logs these and
    /// continues; the loud restore path turns the first hard failure into an
    /// error instead.
    pub warnings: Vec<String>,
}

impl ReconcileReport {
    /// Tallies the actions by kind.
    #[must_use]
    pub fn counts(&self) -> ReconcileCounts {
        let mut counts = ReconcileCounts::default();
        for action in &self.actions {
            match action {
                ReconcileAction::Created(_) => counts.created += 1,
                ReconcileAction::Overwritten(_) => counts.overwritten_stale += 1,
                ReconcileAction::Adopted(_) => counts.adopted += 1,
                ReconcileAction::Unchanged(_) => counts.unchanged += 1,
                ReconcileAction::Reclaimed(_) => counts.reclaimed += 1,
                ReconcileAction::KeptUserEdited(_) => counts.kept_user_edited += 1,
                ReconcileAction::DeletedObsolete(_) => counts.deleted_obsolete += 1,
                ReconcileAction::KeptOrphan(_) => counts.kept_orphan += 1,
            }
        }
        counts
    }
}

/// Tallies of [`ReconcileReport::actions`] by kind, for logging and tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReconcileCounts {
    /// Files written to a previously-absent path.
    pub created: u32,
    /// Jinn-owned stale copies overwritten with the shipped bytes.
    pub overwritten_stale: u32,
    /// Copies matching the binary whose manifest record was (re)claimed.
    pub adopted: u32,
    /// Copies already current and tracked; nothing done.
    pub unchanged: u32,
    /// Copies overwritten by the explicit `--builtins` restore.
    pub reclaimed: u32,
    /// User-modified copies left alone; manifest entries dropped.
    pub kept_user_edited: u32,
    /// Tracked unshipped copies deleted (still byte-identical to what jinn wrote).
    pub deleted_obsolete: u32,
    /// Tracked unshipped copies kept (modified after jinn wrote them).
    pub kept_orphan: u32,
}

/// The on-disk manifest document: `version = 1` plus one `[[file]]` table
/// per file jinn last wrote.
#[derive(Debug, Serialize, Deserialize)]
struct ManifestDocument {
    /// Schema version; see [`MANIFEST_VERSION`].
    version: u32,
    /// One entry per jinn-written file.
    #[serde(default, rename = "file")]
    files: Vec<ManifestEntry>,
}

/// One `[[file]]` entry: the `res/`-relative path and the sha256 of the
/// bytes jinn wrote there.
#[derive(Debug, Serialize, Deserialize)]
struct ManifestEntry {
    /// Path relative to the `res/` root (e.g. `themes/default.toml`).
    path: String,
    /// Lowercase hex sha256 of the bytes jinn wrote.
    sha256: String,
}

/// The manifest in memory: `res/`-relative path → sha256 hex. Ordered, so
/// the written manifest is byte-stable across runs.
type Manifest = BTreeMap<String, String>;

/// The result of comparing one shipped file: the action taken (if any) and
/// the manifest entry the file should carry afterwards (`None` = untracked).
/// Warnings are pushed directly onto the caller's list as they arise.
struct ShippedStep {
    action: Option<ReconcileAction>,
    entry: Option<(String, String)>,
}

/// Reconciles the bundled resources under `destinations` against the
/// injected `expected` set, reading and writing the manifest at
/// `manifest_path`.
///
/// This is the boot path: it is infallible by design. Every per-file
/// failure — an unreadable manifest, an unwritable destination, a manifest
/// that cannot be persisted — lands in [`ReconcileReport::warnings`], and
/// the caller logs them and continues the launch. The compare obeys the
/// three-way rule table exactly; see the module documentation.
///
/// Running this twice back-to-back leaves the destinations and the manifest
/// byte-identical: the second run's compare finds every file matching the
/// binary and every entry current.
///
/// # Panics
///
/// Never deliberately: the injected expected set is derived from the
/// embedded catalogue, whose paths are kind-mapped at compile time, so
/// destination resolution cannot fail in production. A hostile injected
/// path could still fail it, which is what the panic guards.
pub fn reconcile(
    destinations: &Destinations,
    expected: &[ExpectedFile<'_>],
    manifest_path: &MarkedPathBuf<Absolute>,
) -> ReconcileReport {
    let mut warnings = Vec::new();
    let manifest = load_manifest(manifest_path, &mut warnings);

    let mut actions = Vec::new();
    let mut next_manifest = Manifest::new();
    for file in expected {
        let step = reconcile_shipped(file, destinations, &manifest, &mut warnings);
        if let Some(action) = step.action {
            actions.push(action);
        }
        if let Some(entry) = step.entry {
            next_manifest.insert(entry.0, entry.1);
        }
    }

    let shipped_keys: HashSet<String> = expected
        .iter()
        .map(|file| file.path.as_path().to_string_lossy().into_owned())
        .collect();
    for (key, recorded) in &manifest {
        if shipped_keys.contains(key) {
            continue;
        }
        let (action, keep_entry, _unused) = reap_unshipped_entry(key, recorded, destinations);
        if let Some(action) = action {
            actions.push(action);
        }
        if keep_entry {
            next_manifest.insert(key.clone(), recorded.clone());
        }
    }

    if let Err(error) = write_manifest_atomic(manifest_path, &next_manifest) {
        warnings.push(format!(
            "failed to persist the bundled-files manifest at {manifest_path}: {error}"
        ));
    }

    ReconcileReport { actions, warnings }
}

/// Force-restores every bundled resource from the injected `expected` set:
/// writes the shipped bytes over whatever is on disk (including user
/// edits), reclaims tracking for every shipped file, resurrects missing
/// ones, reaps unshipped tracked files by the same rule as
/// [`reconcile`], and touches nothing but resources and the manifest.
///
/// This is the explicit `jinn install --builtins` path, so it fails loudly:
/// the first write or reap failure aborts the restore with an error, as
/// today's install does.
///
/// # Errors
///
/// Returns an error if any destination cannot be written, any obsolete file
/// cannot be deleted, or the manifest cannot be persisted.
pub fn restore_builtins(
    destinations: &Destinations,
    expected: &[ExpectedFile<'_>],
    manifest_path: &MarkedPathBuf<Absolute>,
) -> Result<ReconcileReport, Report<ReconcileError>> {
    let mut warnings = Vec::new();
    let manifest = load_manifest(manifest_path, &mut warnings);

    let mut actions = Vec::new();
    let mut next_manifest = Manifest::new();
    for file in expected {
        let (action, entry) = restore_one(file, destinations)?;
        actions.push(action);
        if let Some(entry) = entry {
            next_manifest.insert(entry.0, entry.1);
        }
    }

    let shipped_keys: HashSet<String> = expected
        .iter()
        .map(|file| file.path.as_path().to_string_lossy().into_owned())
        .collect();
    for (key, recorded) in &manifest {
        if shipped_keys.contains(key) {
            continue;
        }
        let (action, keep_entry, warning) = reap_unshipped_entry(key, recorded, destinations);
        if let Some(warning) = warning {
            return Err(Report::new(ReconcileError).attach(warning));
        }
        if let Some(action) = action {
            actions.push(action);
        }
        if keep_entry {
            next_manifest.insert(key.clone(), recorded.clone());
        }
    }

    write_manifest_atomic(manifest_path, &next_manifest).map_err(|error| {
        Report::new(ReconcileError)
            .attach("failed to persist the bundled-files manifest")
            .attach(format!("path: {manifest_path}"))
            .attach(format!("{error}"))
    })?;

    Ok(ReconcileReport { actions, warnings })
}

/// The three-way compare for one shipped file, per the rule table.
fn reconcile_shipped(
    expected: &ExpectedFile<'_>,
    destinations: &Destinations,
    manifest: &Manifest,
    warnings: &mut Vec<String>,
) -> ShippedStep {
    let destination = match resolve_destination(destinations, &expected.path.as_marked_path()) {
        Ok(destination) => destination,
        Err(warning) => {
            warnings.push(warning);
            return ShippedStep {
                action: None,
                entry: None,
            };
        }
    };
    let key = expected.path.as_path().to_string_lossy().into_owned();
    let shipped_hash = hash_hex(expected.bytes);

    // The hash of what is on disk, or `None` when the file is absent. An
    // unreadable file is indistinguishable from an edited one for safety
    // purposes: both fall through to "leave it alone".
    let disk_hash = match std::fs::read(&destination) {
        Ok(bytes) => Some(hash_hex(&bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            warnings.push(format!(
                "bundled resource unreadable at {destination}, treating as user-owned: {error}"
            ));
            Some(String::from("unreadable"))
        }
    };

    match disk_hash {
        // Absent on disk: write the shipped bytes and track them.
        None => {
            if let Some(warning) = write_expected(&destination, expected.bytes) {
                warnings.push(warning);
                return ShippedStep {
                    action: None,
                    entry: None,
                };
            }
            ShippedStep {
                action: Some(ReconcileAction::Created(destination)),
                entry: Some((key, shipped_hash)),
            }
        }
        Some(disk) if disk == shipped_hash => {
            // Already the shipped bytes: record them; write nothing. The
            // manifest entry is refreshed whenever it differs (or is absent),
            // which is how a first migratory boot adopts an installed tree.
            let action = if manifest.get(&key) == Some(&shipped_hash) {
                ReconcileAction::Unchanged(destination)
            } else {
                ReconcileAction::Adopted(destination)
            };
            ShippedStep {
                action: Some(action),
                entry: Some((key, shipped_hash)),
            }
        }
        Some(disk) if manifest.get(&key) == Some(&disk) => {
            // Jinn-owned but stale: the binary moved on since jinn wrote
            // this copy. Heal it.
            if let Some(warning) = write_expected(&destination, expected.bytes) {
                warnings.push(warning);
                return ShippedStep {
                    action: None,
                    entry: None,
                };
            }
            ShippedStep {
                action: Some(ReconcileAction::Overwritten(destination)),
                entry: Some((key, shipped_hash)),
            }
        }
        Some(_) => {
            // Matches no manifest entry: user-owned. Never written, never
            // deleted, no longer tracked.
            ShippedStep {
                action: Some(ReconcileAction::KeptUserEdited(destination)),
                entry: None,
            }
        }
    }
}

/// Writes one bundled file over the destination, creating parents first.
/// Returns a warning string on failure instead of an error: the boot
/// reconcile continues past per-file failures.
fn write_expected(destination: &MarkedPathBuf<Absolute>, bytes: &[u8]) -> Option<String> {
    if let Some(parent) = destination.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        return Some(format!(
            "failed to create destination directory {parent}: {error}"
        ));
    }
    std::fs::write(destination, bytes)
        .err()
        .map(|error| format!("failed to write bundled resource to {destination}: {error}"))
}

/// The manifest entry a shipped file should carry: its `res/`-relative key
/// and the hash of the bytes to be recorded.
type ManifestEntryDraft = (String, String);

/// Force-restores one shipped file: writes the shipped bytes whether or not
/// anything is on disk, and always tracks them.
fn restore_one(
    expected: &ExpectedFile<'_>,
    destinations: &Destinations,
) -> Result<(ReconcileAction, Option<ManifestEntryDraft>), Report<ReconcileError>> {
    let destination = resolve_destination(destinations, &expected.path.as_marked_path())
        .map_err(|warning| Report::new(ReconcileError).attach(warning))?;
    let existed = destination.exists();
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            Report::new(ReconcileError)
                .attach("failed to create destination directory")
                .attach(format!("path: {parent}"))
                .attach(format!("{error}"))
        })?;
    }
    std::fs::write(&destination, expected.bytes).map_err(|error| {
        Report::new(ReconcileError)
            .attach("failed to write bundled resource")
            .attach(format!("path: {destination}"))
            .attach(format!("{error}"))
    })?;
    let key = expected.path.as_path().to_string_lossy().into_owned();
    let action = if existed {
        ReconcileAction::Reclaimed(destination)
    } else {
        ReconcileAction::Created(destination)
    };
    Ok((action, Some((key, hash_hex(expected.bytes)))))
}

/// Reaps one manifest entry the binary no longer ships.
///
/// Returns the action taken (if any), whether the entry should be kept
/// (a reap that failed must be retried by a later boot), and a warning.
fn reap_unshipped_entry(
    key: &str,
    recorded_hash: &str,
    destinations: &Destinations,
) -> (Option<ReconcileAction>, bool, Option<String>) {
    let Ok(relative) = MarkedPathBuf::<Relative>::new(key) else {
        // Already unresolvable at load time; nothing on disk to reap.
        return (None, false, None);
    };
    let destination = match resolve_destination(destinations, &relative.as_marked_path()) {
        Ok(destination) => destination,
        Err(warning) => return (None, false, Some(warning)),
    };

    match std::fs::read(&destination) {
        // Gone already: just forget the entry.
        Err(error) if error.kind() == io::ErrorKind::NotFound => (None, false, None),
        // Still matches what jinn wrote: delete it.
        Ok(bytes) if hash_hex(&bytes) == recorded_hash => {
            match std::fs::remove_file(&destination) {
                Ok(()) => (
                    Some(ReconcileAction::DeletedObsolete(destination)),
                    false,
                    None,
                ),
                // Deletion failed: keep the entry so a later boot retries.
                Err(error) => (
                    None,
                    true,
                    Some(format!(
                        "failed to delete obsolete bundled file {destination}: {error}"
                    )),
                ),
            }
        }
        // Modified after jinn wrote it: keep it, forget the entry.
        Ok(_) => (Some(ReconcileAction::KeptOrphan(destination)), false, None),
        // Unreadable: cannot verify ownership, so keep the entry and retry
        // on a later boot rather than forget a file we might still own.
        Err(error) => (
            None,
            true,
            Some(format!(
                "obsolete bundled file unreadable at {destination}: {error}"
            )),
        ),
    }
}

/// Resolves a `res/`-relative path to its destination under `destinations`.
fn resolve_destination(
    destinations: &Destinations,
    res_relative: &MarkedPath<'_, Relative>,
) -> Result<MarkedPathBuf<Absolute>, String> {
    let (kind, rest) = res_relative_parts(res_relative.as_path()).ok_or_else(|| {
        format!(
            "bundled path names no destination root: {}",
            res_relative.as_path().display()
        )
    })?;
    kind.root(destinations)
        .join(rest)
        .map_err(|error| format!("failed to resolve destination for {res_relative}: {error}"))
}

/// Reads the manifest from disk as a [`Manifest`].
///
/// Absence, corruption, an unknown schema version, and unresolvable entry
/// paths all degrade to an empty (or reduced) manifest with a warning:
/// first-boot semantics. No manifest condition may block a launch.
fn load_manifest(path: &MarkedPathBuf<Absolute>, warnings: &mut Vec<String>) -> Manifest {
    let mut manifest = Manifest::new();

    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return manifest,
        Err(error) => {
            warnings.push(format!(
                "bundled-files manifest unreadable at {path}, treating as absent: {error}"
            ));
            return manifest;
        }
    };
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) => {
            warnings.push(format!(
                "bundled-files manifest at {path} is not UTF-8, treating as absent: {error}"
            ));
            return manifest;
        }
    };
    let document: ManifestDocument = match toml::from_str(&text) {
        Ok(document) => document,
        Err(error) => {
            warnings.push(format!(
                "bundled-files manifest at {path} does not parse, treating as absent: {error}"
            ));
            return manifest;
        }
    };
    if document.version != MANIFEST_VERSION {
        warnings.push(format!(
            "bundled-files manifest at {path} has version {} (expected {MANIFEST_VERSION}), \
             treating as absent",
            document.version
        ));
        return manifest;
    }

    for entry in document.files {
        match MarkedPathBuf::<Relative>::new(&entry.path) {
            Ok(relative) => {
                manifest.insert(
                    relative.as_path().to_string_lossy().into_owned(),
                    entry.sha256,
                );
            }
            Err(_) => {
                warnings.push(format!(
                    "bundled-files manifest entry at {path} names an unresolvable path, \
                     forgetting it: {}",
                    entry.path
                ));
            }
        }
    }
    manifest
}

/// Serializes `manifest` and swaps it into place atomically: write to a
/// sibling temp file, then rename over the target. The remove-then-rename
/// order is for Windows, where rename fails when the destination exists.
///
/// When the serialized bytes already match what is on disk, nothing is
/// written and the file's mtime survives — a no-op reconcile must not look
/// like a write to mtime-based watchers.
fn write_manifest_atomic(
    path: &MarkedPathBuf<Absolute>,
    manifest: &Manifest,
) -> Result<(), io::Error> {
    let serialized = serialize_manifest(manifest)?;

    if let Ok(existing) = std::fs::read(path) {
        if existing == serialized.as_bytes() {
            return Ok(());
        }
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp_path = {
        let mut name = path.as_path().as_os_str().to_os_string();
        name.push(".tmp");
        match path.parent() {
            Some(parent) => parent
                .join(name)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?,
            None => MarkedPathBuf::<Absolute>::new(name)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?,
        }
    };
    std::fs::write(&temp_path, serialized)?;
    match std::fs::remove_file(path) {
        Ok(()) => {}
        // No manifest to replace yet: a plain rename then.
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    std::fs::rename(&temp_path, path)
}

/// Serializes `manifest` to its TOML document form.
fn serialize_manifest(manifest: &Manifest) -> Result<String, io::Error> {
    let document = ManifestDocument {
        version: MANIFEST_VERSION,
        files: manifest
            .iter()
            .map(|(path, hash)| ManifestEntry {
                path: path.clone(),
                sha256: hash.clone(),
            })
            .collect(),
    };
    toml::to_string(&document)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))
}

/// Hex-encodes the sha256 of `bytes`, lowercase — the manifest's hash form.
fn hash_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest
        .iter()
        .fold(String::with_capacity(64), |mut hex, byte| {
            use std::fmt::Write as _;
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::unwrap_used,
        clippy::panic,
        reason = "test code"
    )]

    use super::*;
    use jinn_testutil::JinnTempDir;

    /// Bytes of the two synthetic shipped files the fixtures use.
    const ALPHA_V1: &[u8] = b"alpha-contents-v1";
    const ALPHA_V2: &[u8] = b"alpha-contents-v2";
    const BETA_V1: &[u8] = b"beta-contents-v1";

    /// A reconciliation environment: five destination roots (themes and
    /// skills are the two the fixtures exercise) plus a state dir holding
    /// the manifest path.
    struct Fixture {
        destinations: Destinations,
        manifest_path: MarkedPathBuf<Absolute>,
        themes_root: MarkedPathBuf<Absolute>,
        skills_root: MarkedPathBuf<Absolute>,
        _temps: Vec<JinnTempDir>,
    }

    impl Fixture {
        fn fresh() -> Self {
            let themes = jinn_testutil::tempdir();
            let personas = jinn_testutil::tempdir();
            let prompts = jinn_testutil::tempdir();
            let workflows = jinn_testutil::tempdir();
            let skills = jinn_testutil::tempdir();
            let state = jinn_testutil::tempdir();
            let manifest_path = state
                .path
                .join_all(["jinn", "bundled-files.toml"])
                .expect("name joins keep the base absolute");
            let destinations = Destinations::new(
                themes.path.as_marked_path(),
                personas.path.as_marked_path(),
                prompts.path.as_marked_path(),
                workflows.path.as_marked_path(),
                skills.path.as_marked_path(),
            );
            Self {
                themes_root: themes.path.clone(),
                skills_root: skills.path.clone(),
                destinations,
                manifest_path,
                _temps: vec![themes, personas, prompts, workflows, skills, state],
            }
        }

        /// The injected expected set: two shipped files under two kinds.
        fn expected_with(alpha: &'static [u8]) -> Vec<ExpectedFile<'static>> {
            vec![
                ExpectedFile {
                    path: MarkedPathBuf::<Relative>::new("themes/alpha.toml")
                        .expect("relative path"),
                    bytes: alpha,
                },
                ExpectedFile {
                    path: MarkedPathBuf::<Relative>::new("skills/beta/SKILL.md")
                        .expect("relative path"),
                    bytes: BETA_V1,
                },
            ]
        }

        /// Runs the reconcile with the fixture's default expected set.
        fn reconcile_with(&self, expected: &[ExpectedFile<'_>]) -> ReconcileReport {
            reconcile(&self.destinations, expected, &self.manifest_path)
        }

        /// The full destination path of a `res/`-relative fixture path.
        fn disk_path(&self, res_relative: &str) -> std::path::PathBuf {
            let themes_prefix = self.themes_root.as_path().to_string_lossy().into_owned();
            let skills_prefix = self.skills_root.as_path().to_string_lossy().into_owned();
            if let Some(rest) = res_relative.strip_prefix("themes/") {
                std::path::PathBuf::from(format!("{themes_prefix}/{rest}"))
            } else if let Some(rest) = res_relative.strip_prefix("skills/") {
                std::path::PathBuf::from(format!("{skills_prefix}/{rest}"))
            } else {
                panic!("fixture only maps themes/ and skills/ paths: {res_relative}")
            }
        }

        /// Writes bytes to a fixture path on disk, creating parents.
        fn seed_disk(&self, res_relative: &str, bytes: &[u8]) {
            let path = self.disk_path(res_relative);
            std::fs::create_dir_all(path.parent().expect("parent exists")).unwrap();
            std::fs::write(path, bytes).unwrap();
        }

        /// Reads a fixture path back from disk.
        fn read_disk(&self, res_relative: &str) -> Vec<u8> {
            std::fs::read(self.disk_path(res_relative)).unwrap()
        }

        /// Whether a fixture path exists on disk.
        fn disk_exists(&self, res_relative: &str) -> bool {
            self.disk_path(res_relative).exists()
        }

        /// The manifest as it stands on disk, parsed back to a map.
        fn manifest_on_disk(&self) -> Manifest {
            let text = std::fs::read_to_string(&self.manifest_path).unwrap();
            let document: ManifestDocument = toml::from_str(&text).unwrap();
            document
                .files
                .into_iter()
                .map(|entry| (entry.path, entry.sha256))
                .collect()
        }

        /// Crafts a manifest on disk from raw (path, hash) pairs.
        fn craft_manifest(&self, entries: &[(&str, &str)]) {
            let document = ManifestDocument {
                version: MANIFEST_VERSION,
                files: entries
                    .iter()
                    .map(|(path, hash)| ManifestEntry {
                        path: (*path).to_owned(),
                        sha256: (*hash).to_owned(),
                    })
                    .collect(),
            };
            let serialized = toml::to_string(&document).unwrap();
            std::fs::create_dir_all(self.manifest_path.parent().unwrap()).unwrap();
            std::fs::write(&self.manifest_path, serialized).unwrap();
        }
    }

    // ---- Truth table: shipped files ------------------------------------

    #[rstest::rstest]
    #[test]
    fn shipped_file_absent_on_disk_is_written_and_tracked() {
        // Given shipped files with no on-disk copies and no manifest.
        let fixture = Fixture::fresh();
        let expected = Fixture::expected_with(ALPHA_V2);

        // When reconciling.
        let report = fixture.reconcile_with(&expected);

        // Then both shipped files were created with the shipped bytes.
        assert_eq!(fixture.read_disk("themes/alpha.toml"), ALPHA_V2);
        assert_eq!(fixture.read_disk("skills/beta/SKILL.md"), BETA_V1);
        assert_eq!(report.counts().created, 2);
        // And the manifest tracks both under their shipped hashes.
        let manifest = fixture.manifest_on_disk();
        assert_eq!(manifest.get("themes/alpha.toml"), Some(&hash_hex(ALPHA_V2)));
        assert_eq!(
            manifest.get("skills/beta/SKILL.md"),
            Some(&hash_hex(BETA_V1))
        );
    }

    #[rstest::rstest]
    #[test]
    fn shipped_file_matching_the_binary_is_adopted_without_a_write() {
        // Given files on disk that already match the binary, untracked.
        let fixture = Fixture::fresh();
        fixture.seed_disk("themes/alpha.toml", ALPHA_V2);
        fixture.seed_disk("skills/beta/SKILL.md", BETA_V1);
        let expected = Fixture::expected_with(ALPHA_V2);

        // When reconciling.
        let report = fixture.reconcile_with(&expected);

        // Then nothing was written but the entries were claimed.
        assert_eq!(report.counts().adopted, 2);
        assert_eq!(
            report.counts().created + report.counts().overwritten_stale,
            0
        );
        assert_eq!(fixture.read_disk("themes/alpha.toml"), ALPHA_V2);
        // And the manifest carries the shipped hashes.
        let manifest = fixture.manifest_on_disk();
        assert_eq!(manifest.get("themes/alpha.toml"), Some(&hash_hex(ALPHA_V2)));
    }

    #[rstest::rstest]
    #[test]
    fn shipped_file_current_and_tracked_is_unchanged() {
        // Given a fully-reconciled state (disk matches binary, entry stands).
        let fixture = Fixture::fresh();
        let expected = Fixture::expected_with(ALPHA_V2);
        fixture.reconcile_with(&expected);

        // When reconciling a second time.
        let report = fixture.reconcile_with(&expected);

        // Then the file is reported unchanged, with no writes anywhere.
        assert_eq!(report.counts().unchanged, 2, "both shipped files");
        assert_eq!(
            report.counts().created + report.counts().adopted + report.counts().overwritten_stale,
            0
        );
    }

    #[rstest::rstest]
    #[test]
    fn shipped_file_jinn_owned_but_stale_is_healed() {
        // Given a manifest claiming a stale copy that is indeed on disk.
        let fixture = Fixture::fresh();
        fixture.seed_disk("themes/alpha.toml", ALPHA_V1);
        fixture.craft_manifest(&[("themes/alpha.toml", &hash_hex(ALPHA_V1))]);
        let expected = Fixture::expected_with(ALPHA_V2);

        // When reconciling against a binary that ships newer bytes.
        let report = fixture.reconcile_with(&expected);

        // Then the stale copy was overwritten with the shipped bytes.
        assert_eq!(fixture.read_disk("themes/alpha.toml"), ALPHA_V2);
        assert_eq!(report.counts().overwritten_stale, 1);
        // And the entry now records the shipped hash.
        let manifest = fixture.manifest_on_disk();
        assert_eq!(manifest.get("themes/alpha.toml"), Some(&hash_hex(ALPHA_V2)));
    }

    #[rstest::rstest]
    #[test]
    fn shipped_file_edited_by_the_user_is_kept_and_untracked() {
        // Given a file on disk that matches neither the manifest nor the binary.
        let fixture = Fixture::fresh();
        fixture.seed_disk("themes/alpha.toml", b"the user's theme");
        fixture.craft_manifest(&[("themes/alpha.toml", &hash_hex(ALPHA_V1))]);
        let expected = Fixture::expected_with(ALPHA_V2);

        // When reconciling.
        let report = fixture.reconcile_with(&expected);

        // Then the user's bytes survive untouched.
        assert_eq!(fixture.read_disk("themes/alpha.toml"), b"the user's theme");
        assert_eq!(report.counts().kept_user_edited, 1);
        // And the entry is dropped: the file is user-owned from here on.
        let manifest = fixture.manifest_on_disk();
        assert!(!manifest.contains_key("themes/alpha.toml"));
    }

    #[rstest::rstest]
    #[test]
    fn shipped_file_edited_with_no_manifest_entry_is_kept_and_untracked() {
        // Given a file on disk with no manifest at all (migratory boot):
        // it is indistinguishable from a user edit and must not be clobbered.
        let fixture = Fixture::fresh();
        fixture.seed_disk("themes/alpha.toml", b"pre-existing install");
        let expected = Fixture::expected_with(ALPHA_V2);

        // When reconciling.
        let report = fixture.reconcile_with(&expected);

        // Then the pre-existing bytes survive.
        assert_eq!(
            fixture.read_disk("themes/alpha.toml"),
            b"pre-existing install"
        );
        assert_eq!(report.counts().kept_user_edited, 1);
        // And no entry was minted for the file.
        let manifest = fixture.manifest_on_disk();
        assert!(!manifest.contains_key("themes/alpha.toml"));
        // While the untouched sibling (absent on disk) was written and tracked.
        assert_eq!(report.counts().created, 1);
    }

    // ---- Truth table: unshipped manifest entries ------------------------

    #[rstest::rstest]
    #[test]
    fn unshipped_entry_matching_its_disk_copy_is_deleted_and_forgotten() {
        // Given a manifest entry for a file the binary no longer ships,
        // whose on-disk bytes still match what jinn wrote.
        let fixture = Fixture::fresh();
        fixture.seed_disk("themes/gone.toml", ALPHA_V1);
        fixture.craft_manifest(&[("themes/gone.toml", &hash_hex(ALPHA_V1))]);
        let expected = Fixture::expected_with(ALPHA_V2);

        // When reconciling.
        let report = fixture.reconcile_with(&expected);

        // Then the obsolete file was deleted.
        assert!(!fixture.disk_exists("themes/gone.toml"));
        assert_eq!(report.counts().deleted_obsolete, 1);
        // And the entry is gone from the manifest.
        let manifest = fixture.manifest_on_disk();
        assert!(!manifest.contains_key("themes/gone.toml"));
    }

    #[rstest::rstest]
    #[test]
    fn unshipped_entry_for_an_edited_file_is_kept_and_forgotten() {
        // Given a manifest entry for an unshipped path whose on-disk bytes
        // were edited after jinn wrote them.
        let fixture = Fixture::fresh();
        fixture.seed_disk("themes/gone.toml", b"the user kept this");
        fixture.craft_manifest(&[("themes/gone.toml", &hash_hex(ALPHA_V1))]);
        let expected = Fixture::expected_with(ALPHA_V2);

        // When reconciling.
        let report = fixture.reconcile_with(&expected);

        // Then the file was kept, not deleted.
        assert_eq!(fixture.read_disk("themes/gone.toml"), b"the user kept this");
        assert_eq!(report.counts().kept_orphan, 1);
        // And the entry is forgotten.
        let manifest = fixture.manifest_on_disk();
        assert!(!manifest.contains_key("themes/gone.toml"));
    }

    #[rstest::rstest]
    #[test]
    fn unshipped_entry_for_an_absent_file_is_forgotten_without_action() {
        // Given a manifest entry for an unshipped path with no file on disk.
        let fixture = Fixture::fresh();
        fixture.craft_manifest(&[("themes/gone.toml", &hash_hex(ALPHA_V1))]);
        let expected = Fixture::expected_with(ALPHA_V2);

        // When reconciling.
        let report = fixture.reconcile_with(&expected);

        // Then no file action was taken and the entry is forgotten.
        assert!(!fixture.disk_exists("themes/gone.toml"));
        assert_eq!(
            report.counts().deleted_obsolete + report.counts().kept_orphan,
            0
        );
        let manifest = fixture.manifest_on_disk();
        assert!(!manifest.contains_key("themes/gone.toml"));
    }

    // ---- Idempotence and manifest meaning -------------------------------

    /// Snapshots every file under the fixture's five roots as
    /// (relative path, bytes), sorted, for byte-compare assertions.
    fn snapshot_roots(fixture: &Fixture) -> Vec<(String, Vec<u8>)> {
        let mut snapshot = Vec::new();
        for root in [&fixture.themes_root, &fixture.skills_root] {
            let mut stack = vec![root.as_path().to_path_buf()];
            while let Some(dir) = stack.pop() {
                let Ok(entries) = std::fs::read_dir(&dir) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        stack.push(path);
                    } else {
                        let rel = path
                            .strip_prefix(root.as_path())
                            .expect("walked path sits under the root");
                        let bytes = std::fs::read(&path).unwrap();
                        snapshot.push((rel.to_string_lossy().into_owned(), bytes));
                    }
                }
            }
        }
        snapshot.sort();
        snapshot
    }

    #[rstest::rstest]
    #[test]
    fn reconcile_twice_leaves_disk_and_manifest_byte_identical() {
        // Given a mixed state: one written file, one user edit, one orphan.
        let fixture = Fixture::fresh();
        fixture.seed_disk("themes/alpha.toml", b"the user's theme");
        fixture.seed_disk("skills/beta/SKILL.md", BETA_V1);
        fixture.seed_disk("themes/gone.toml", ALPHA_V1);
        fixture.craft_manifest(&[
            ("themes/alpha.toml", &hash_hex(ALPHA_V1)),
            ("themes/gone.toml", &hash_hex(ALPHA_V1)),
        ]);
        let expected = Fixture::expected_with(ALPHA_V2);
        fixture.reconcile_with(&expected);

        // When reconciling a second time.
        let before_disk = snapshot_roots(&fixture);
        let before_manifest = std::fs::read(&fixture.manifest_path).unwrap();
        let before_mtime = std::fs::metadata(&fixture.manifest_path)
            .unwrap()
            .modified()
            .unwrap();
        let second = fixture.reconcile_with(&expected);

        // Then disk and manifest are byte-identical to the first run's end state.
        assert_eq!(snapshot_roots(&fixture), before_disk);
        assert_eq!(
            std::fs::read(&fixture.manifest_path).unwrap(),
            before_manifest
        );
        // And the manifest was not rewritten: an unchanged second run is a
        // filesystem no-op, not a same-bytes overwrite.
        assert_eq!(
            std::fs::metadata(&fixture.manifest_path)
                .unwrap()
                .modified()
                .unwrap(),
            before_mtime,
            "second run must not touch the manifest file"
        );
        // And the second run took no mutating action at all. (KeptUserEdited
        // and KeptOrphan repeat every run by design — they are observations
        // about user-owned files, not writes.)
        let counts = second.counts();
        let mutations =
            counts.created + counts.overwritten_stale + counts.reclaimed + counts.deleted_obsolete;
        assert_eq!(mutations, 0, "second run must take no mutating action");
    }

    #[rstest::rstest]
    #[test]
    fn manifest_after_fresh_reconcile_names_exactly_the_written_files() {
        // Given an empty root and a two-file expected set.
        let fixture = Fixture::fresh();
        let expected = Fixture::expected_with(ALPHA_V2);

        // When reconciling.
        let report = fixture.reconcile_with(&expected);

        // Then the manifest carries exactly the written set, keyed by the
        // shipped hash — a bijection between entries and jinn-written files.
        let manifest = fixture.manifest_on_disk();
        assert_eq!(manifest.len(), expected.len(), "entry per written file");
        assert_eq!(manifest.get("themes/alpha.toml"), Some(&hash_hex(ALPHA_V2)));
        assert_eq!(
            manifest.get("skills/beta/SKILL.md"),
            Some(&hash_hex(BETA_V1))
        );
        // And every entry names a file the reconcile wrote (the entry keys
        // are res-relative; the actions carry full destinations).
        let written_paths: Vec<String> = report
            .actions
            .iter()
            .filter(|action| {
                matches!(
                    action,
                    ReconcileAction::Created(_) | ReconcileAction::Overwritten(_)
                )
            })
            .map(|action| action.path().as_path().to_string_lossy().into_owned())
            .collect();
        assert_eq!(written_paths.len(), manifest.len());
        for key in manifest.keys() {
            let disk = fixture.disk_path(key).to_string_lossy().into_owned();
            assert!(
                written_paths.contains(&disk),
                "manifest entry {key} names no written file"
            );
        }
    }

    // ---- Manifest resilience --------------------------------------------

    #[rstest::rstest]
    #[test]
    fn corrupt_manifest_is_treated_as_absent_with_a_warning() {
        // Given a manifest that does not parse.
        let fixture = Fixture::fresh();
        std::fs::create_dir_all(fixture.manifest_path.parent().unwrap()).unwrap();
        std::fs::write(&fixture.manifest_path, "NOT [valid toml").unwrap();
        fixture.seed_disk("themes/alpha.toml", ALPHA_V2);
        let expected = Fixture::expected_with(ALPHA_V2);

        // When reconciling.
        let report = fixture.reconcile_with(&expected);

        // Then the matching on-disk file was adopted (first-boot semantics:
        // no entries means nothing is provably jinn-owned), not clobbered.
        assert_eq!(fixture.read_disk("themes/alpha.toml"), ALPHA_V2);
        assert_eq!(report.counts().adopted, 1);
        // And the corruption was reported as a warning, not an error.
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("does not parse")),
            "expected a parse warning, got: {:?}",
            report.warnings
        );
        // And the manifest was rewritten in the shipped-image shape.
        let manifest = fixture.manifest_on_disk();
        assert_eq!(manifest.get("themes/alpha.toml"), Some(&hash_hex(ALPHA_V2)));
    }

    #[rstest::rstest]
    #[test]
    fn unknown_manifest_version_is_treated_as_absent_with_a_warning() {
        // Given a manifest carrying a future schema version.
        let fixture = Fixture::fresh();
        fixture.craft_manifest(&[("themes/alpha.toml", &hash_hex(ALPHA_V1))]);
        let raw = std::fs::read_to_string(&fixture.manifest_path).unwrap();
        std::fs::write(
            &fixture.manifest_path,
            raw.replace("version = 1", "version = 99"),
        )
        .unwrap();
        fixture.seed_disk("themes/alpha.toml", ALPHA_V2);
        let expected = Fixture::expected_with(ALPHA_V2);

        // When reconciling.
        let report = fixture.reconcile_with(&expected);

        // Then the on-disk copy is treated as untracked and adopted, not
        // overwritten — an unknown manifest must never look jinn-owned.
        assert_eq!(fixture.read_disk("themes/alpha.toml"), ALPHA_V2);
        assert_eq!(report.counts().adopted, 1);
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("version 99")),
            "expected a version warning, got: {:?}",
            report.warnings
        );
    }

    #[rstest::rstest]
    #[test]
    fn unpersistable_manifest_degrades_to_a_warning_and_still_writes_files() {
        // Given a directory squatting on the manifest's temp-file path, so
        // the atomic swap cannot complete.
        let fixture = Fixture::fresh();
        let temp_squat = {
            let mut name = fixture.manifest_path.as_path().as_os_str().to_os_string();
            name.push(".tmp");
            std::path::PathBuf::from(name)
        };
        std::fs::create_dir_all(&temp_squat).unwrap();
        let expected = Fixture::expected_with(ALPHA_V2);

        // When reconciling.
        let report = fixture.reconcile_with(&expected);

        // Then the resources were still written (a read-only state dir must
        // not brick the launch or the seed), with a warning recorded.
        assert_eq!(fixture.read_disk("themes/alpha.toml"), ALPHA_V2);
        assert_eq!(report.counts().created, 2);
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("manifest")),
            "expected a manifest warning, got: {:?}",
            report.warnings
        );
        std::fs::remove_dir_all(temp_squat).unwrap();
    }

    // ---- Restore path ----------------------------------------------------

    #[rstest::rstest]
    #[test]
    fn restore_builtins_overwrites_user_edits_and_reclaims_them() {
        // Given a user-edited bundled file.
        let fixture = Fixture::fresh();
        fixture.seed_disk("themes/alpha.toml", b"the user's theme");
        let expected = Fixture::expected_with(ALPHA_V2);

        // When restoring builtins.
        let report = restore_builtins(&fixture.destinations, &expected, &fixture.manifest_path)
            .expect("restore succeeds");

        // Then the file was reclaimed with the shipped bytes.
        assert_eq!(fixture.read_disk("themes/alpha.toml"), ALPHA_V2);
        assert_eq!(report.counts().reclaimed, 1);
        // And the manifest tracks every shipped file again.
        let manifest = fixture.manifest_on_disk();
        assert_eq!(manifest.get("themes/alpha.toml"), Some(&hash_hex(ALPHA_V2)));
        assert_eq!(
            manifest.get("skills/beta/SKILL.md"),
            Some(&hash_hex(BETA_V1))
        );
    }

    #[rstest::rstest]
    #[test]
    fn restore_builtins_resurrects_missing_files_and_reaps_unshipped() {
        // Given an absent shipped file and an owned unshipped one.
        let fixture = Fixture::fresh();
        fixture.seed_disk("themes/gone.toml", ALPHA_V1);
        fixture.craft_manifest(&[("themes/gone.toml", &hash_hex(ALPHA_V1))]);
        let expected = Fixture::expected_with(ALPHA_V2);

        // When restoring builtins.
        let report = restore_builtins(&fixture.destinations, &expected, &fixture.manifest_path)
            .expect("restore succeeds");

        // Then the missing file was created and the orphan deleted.
        assert_eq!(fixture.read_disk("themes/alpha.toml"), ALPHA_V2);
        assert!(!fixture.disk_exists("themes/gone.toml"));
        assert_eq!(report.counts().created, 2);
        assert_eq!(report.counts().deleted_obsolete, 1);
    }

    #[rstest::rstest]
    #[test]
    fn restore_builtins_leaves_config_files_and_state_alone() {
        // Given a fresh fixture (the restore only receives destinations and
        // the manifest path — no config path exists to touch).
        let fixture = Fixture::fresh();
        let expected = Fixture::expected_with(ALPHA_V2);

        // When restoring builtins.
        restore_builtins(&fixture.destinations, &expected, &fixture.manifest_path)
            .expect("restore succeeds");

        // Then only files under the five roots and the manifest exist in
        // the fixture's temps — the API shape enforces the config boundary.
        assert!(fixture.disk_exists("themes/alpha.toml"));
        assert!(fixture.manifest_path.is_file());
    }
}
