//! The browser-daemon registry — how the running daemon advertises itself
//! and how any client finds it.
//!
//! The daemon registers one [`DaemonEntry`] in a JSON file under the
//! AppPaths state dir: pid, bound port, start time. The file is shared
//! state between concurrent processes (two jinn instances auto-starting
//! the daemon at once, a luau script and an MCP server racing), so every
//! write is temp-file + rename (never corrupt) and every *reader* prunes
//! entries whose port no longer accepts a TCP connection. The pid is
//! carried for display and the start lock's staleness check; it is never
//! probed (pid syscalls are not cross-platform).
//!
//! Liveness is the port probe: a dead daemon frees its port, so its entry
//! fails the probe and is invisible to every reader. By the design's
//! single-window guarantee there is at most one live daemon;
//! [`read_live`] returns it or nothing.

use serde::{Deserialize, Serialize};
use std::io::Write as _;
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::time::Duration;

/// The registry file name, joined onto the AppPaths state dir.
pub const REGISTRY_FILE_NAME: &str = "browser-daemons.json";

/// How long the liveness probe waits before declaring a port dead.
pub const LIVENESS_TIMEOUT: Duration = Duration::from_millis(250);

/// The running browser daemon's advertisement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonEntry {
    /// The daemon process's pid (display + lock staleness; never probed).
    pid: u32,
    /// The port the control API bound (already resolved: never 0).
    port: u16,
    /// Epoch milliseconds at registration.
    started_at: u64,
}

impl DaemonEntry {
    #[must_use]
    pub const fn new(pid: u32, port: u16, started_at: u64) -> Self {
        Self {
            pid,
            port,
            started_at,
        }
    }

    #[must_use]
    pub const fn pid(&self) -> u32 {
        self.pid
    }

    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
    }

    #[must_use]
    pub const fn started_at(&self) -> u64 {
        self.started_at
    }
}

/// Probes the daemon's port: an open TCP connection means a listener is
/// alive on it.
#[must_use]
pub fn is_live(entry: &DaemonEntry) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], entry.port()));
    TcpStream::connect_timeout(&addr, LIVENESS_TIMEOUT).is_ok()
}

/// Reads the registry and returns only the live entries.
///
/// Stale entries are filtered, never rewritten: a client is a reader,
/// and staleness is bounded by the next daemon start (each new daemon
/// prunes the file as it registers).
#[must_use]
pub fn read_live(path: &Path) -> Vec<DaemonEntry> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(entries) = serde_json::from_str::<Vec<DaemonEntry>>(&raw) else {
        return Vec::new();
    };
    entries.into_iter().filter(is_live).collect()
}

/// Writes `entries` to `path` atomically: temp file in the same
/// directory, then rename. A concurrent reader sees either the old file
/// or the new one, never a partial write.
///
/// # Errors
///
/// Returns the underlying io error when the temp file cannot be written
/// or the rename fails (the parent directory not existing is the common
/// cause — callers create it first).
pub fn write_entries(path: &Path, entries: &[DaemonEntry]) -> std::io::Result<()> {
    let serialized = serde_json::to_string_pretty(entries).map_err(std::io::Error::other)?;
    let temp = path.with_extension("json.tmp");
    {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(serialized.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::rename(&temp, path)
}

/// Registers `entry` in the registry at `path`.
///
/// The write prunes every entry whose port is dead — each new daemon
/// start cleans the file for everyone — then replaces or appends this
/// daemon's own entry, keyed by pid.
///
/// # Errors
///
/// Returns the underlying io error when the file cannot be written
/// (read failures are not errors: a missing or corrupt file reads as
/// empty).
pub fn register(path: &Path, entry: DaemonEntry) -> std::io::Result<()> {
    let mut entries = read_all(path);
    entries.retain(|existing| is_live(existing) && existing.pid() != entry.pid());
    entries.push(entry);
    write_entries(path, &entries)
}

/// Removes the entry carrying `pid`, if any. Best effort by design: the
/// graceful shutdown path calls this, but correctness never depends on
/// it — a dead daemon is invisible to every reader via the port probe.
///
/// # Errors
///
/// Returns the underlying io error when the write fails; a missing or
/// corrupt file is not an error.
pub fn remove_entry(path: &Path, pid: u32) -> std::io::Result<()> {
    let entries = read_all(path);
    let remaining: Vec<DaemonEntry> = entries.into_iter().filter(|e| e.pid() != pid).collect();
    if remaining.is_empty() {
        // An empty registry is the file's normal post-prune state; drop
        // the file rather than leaving `[ ]` behind.
        let _ = std::fs::remove_file(path);
        return Ok(());
    }
    write_entries(path, &remaining)
}

fn read_all(path: &Path) -> Vec<DaemonEntry> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        reason = "test assertions need expect to state what went wrong"
    )]

    use super::{DaemonEntry, REGISTRY_FILE_NAME, is_live, read_live, register, remove_entry};
    use jinn_testutil::TestRoot;
    use marked_path::MarkedPathAccess as _;

    fn entry(pid: u32, port: u16, started_at: u64) -> DaemonEntry {
        DaemonEntry::new(pid, port, started_at)
    }

    /// Binds an OS-assigned loopback port and returns the listener. The
    /// caller holds it for the test's lifetime: the probe only succeeds
    /// while the listener is open, so tests bind first and build the
    /// entry from the bound port.
    fn live_listener() -> std::net::TcpListener {
        std::net::TcpListener::bind("127.0.0.1:0").expect("ephemeral bind succeeds")
    }

    /// The port an open listener holds.
    fn port_of(listener: &std::net::TcpListener) -> u16 {
        listener.local_addr().expect("bound addr resolves").port()
    }

    /// Binds an ephemeral port and immediately drops the listener, so the
    /// port is (near-certainly) closed. This is the deterministic dead
    /// fixture: the probe must fail.
    fn dead_port() -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("ephemeral bind succeeds");
        let port = listener.local_addr().expect("bound addr resolves").port();
        drop(listener);
        port
    }

    #[rstest::rstest]
    #[test]
    fn entry_json_round_trips() {
        // Given an entry with every field set.
        let original = entry(4242, 8899, 1_700_000_000_000);

        // When serializing then re-parsing.
        let raw = serde_json::to_string(&original).expect("serialize");
        let parsed: DaemonEntry = serde_json::from_str(&raw).expect("parse");

        // Then it equals the original.
        assert_eq!(parsed, original);
    }

    #[rstest::rstest]
    #[test]
    fn liveness_probe_rejects_a_dead_port() {
        // Given an entry whose port was bound and released.
        let dead = entry(1, dead_port(), 100);

        // When probing liveness.
        // Then the probe fails.
        assert!(!is_live(&dead));
    }

    #[rstest::rstest]
    #[test]
    fn liveness_probe_accepts_a_live_port() {
        // Given an entry whose port holds an open listener.
        let listener = live_listener();
        let live = entry(1, port_of(&listener), 100);

        // When probing liveness.
        // Then the probe succeeds.
        assert!(is_live(&live));
    }

    #[rstest::rstest]
    #[test]
    fn read_live_prunes_entries_whose_port_is_dead() {
        // Given a registry file holding one live and one dead entry.
        let root = TestRoot::new();
        let path = root.join_str(REGISTRY_FILE_NAME).expect("name joins");
        let listener = live_listener();
        let live = entry(1, port_of(&listener), 300);
        let dead = entry(2, dead_port(), 100);
        super::write_entries(path.as_path(), &[live.clone(), dead]).expect("write");

        // When reading the live entries.
        let entries = read_live(path.as_path());

        // Then only the live one comes back.
        assert_eq!(entries, vec![live]);
    }

    #[rstest::rstest]
    #[test]
    fn read_live_on_a_missing_file_is_empty() {
        // Given a path with no registry file behind it.
        let root = TestRoot::new();
        let path = root.join_str(REGISTRY_FILE_NAME).expect("name joins");

        // When reading the live entries.
        let entries = read_live(path.as_path());

        // Then nothing comes back and no file was created.
        assert_eq!(entries.len(), 0, "no entries should come back");
        assert!(!path.as_path().exists());
    }

    #[rstest::rstest]
    #[test]
    fn read_live_on_a_corrupt_file_is_empty() {
        // Given a registry file that is not valid JSON.
        let root = TestRoot::new();
        let path = root.join_str(REGISTRY_FILE_NAME).expect("name joins");
        std::fs::write(path.as_path(), b"not json at all").expect("write");

        // When reading the live entries.
        let entries = read_live(path.as_path());

        // Then nothing comes back.
        assert_eq!(entries.len(), 0, "no entries should come back");
    }

    #[rstest::rstest]
    #[test]
    fn register_appends_the_own_entry_to_an_empty_registry() {
        // Given no registry file.
        let root = TestRoot::new();
        let path = root.join_str(REGISTRY_FILE_NAME).expect("name joins");
        let listener = live_listener();
        let own = entry(42, port_of(&listener), 500);

        // When registering.
        register(path.as_path(), own.clone()).expect("register");

        // Then the file holds exactly the own entry.
        let raw = std::fs::read_to_string(path.as_path()).expect("read");
        let parsed: Vec<DaemonEntry> = serde_json::from_str(&raw).expect("valid JSON");
        assert_eq!(parsed, vec![own]);
    }

    #[rstest::rstest]
    #[test]
    fn register_replaces_a_stale_entry_of_the_same_pid() {
        // Given a registry carrying an old entry for pid 42 on a dead port.
        let root = TestRoot::new();
        let path = root.join_str(REGISTRY_FILE_NAME).expect("name joins");
        super::write_entries(path.as_path(), &[entry(42, dead_port(), 100)]).expect("write");

        // When registering a fresh entry for the same pid.
        let listener = live_listener();
        let fresh = entry(42, port_of(&listener), 900);
        register(path.as_path(), fresh.clone()).expect("register");

        // Then the old entry is replaced, not duplicated.
        let raw = std::fs::read_to_string(path.as_path()).expect("read");
        let parsed: Vec<DaemonEntry> = serde_json::from_str(&raw).expect("valid JSON");
        assert_eq!(parsed, vec![fresh]);
    }

    #[rstest::rstest]
    #[test]
    fn register_prunes_dead_siblings_and_keeps_live_ones() {
        // Given a registry holding one live sibling and one dead sibling.
        let root = TestRoot::new();
        let path = root.join_str(REGISTRY_FILE_NAME).expect("name joins");
        let sibling = live_listener();
        let live = entry(1, port_of(&sibling), 100);
        let dead = entry(2, dead_port(), 200);
        super::write_entries(path.as_path(), &[live.clone(), dead]).expect("write");

        // When registering a third daemon.
        let own_listener = live_listener();
        let own = entry(3, port_of(&own_listener), 300);
        register(path.as_path(), own.clone()).expect("register");

        // Then the dead sibling is gone and the live one survives.
        let raw = std::fs::read_to_string(path.as_path()).expect("read");
        let mut parsed: Vec<DaemonEntry> = serde_json::from_str(&raw).expect("valid JSON");
        parsed.sort_by_key(super::DaemonEntry::started_at);
        let mut expected = vec![live, own];
        expected.sort_by_key(super::DaemonEntry::started_at);
        assert_eq!(parsed, expected);
    }

    #[rstest::rstest]
    #[test]
    fn remove_entry_drops_only_the_matching_pid() {
        // Given a registry carrying entries for two pids.
        let root = TestRoot::new();
        let path = root.join_str(REGISTRY_FILE_NAME).expect("name joins");
        let survivor = entry(1, dead_port(), 100);
        let removed = entry(2, dead_port(), 200);
        super::write_entries(path.as_path(), &[survivor.clone(), removed]).expect("write");

        // When removing pid 2.
        remove_entry(path.as_path(), 2).expect("remove");

        // Then only pid 1 remains (dead entries survive removal — removal
        // does not prune).
        let raw = std::fs::read_to_string(path.as_path()).expect("read");
        let parsed: Vec<DaemonEntry> = serde_json::from_str(&raw).expect("valid JSON");
        assert_eq!(parsed, vec![survivor]);
    }

    #[rstest::rstest]
    #[test]
    fn removing_the_last_entry_leaves_no_registry_file() {
        // Given a registry carrying a single entry.
        let root = TestRoot::new();
        let path = root.join_str(REGISTRY_FILE_NAME).expect("name joins");
        super::write_entries(path.as_path(), &[entry(2, dead_port(), 100)]).expect("write");

        // When removing that entry.
        remove_entry(path.as_path(), 2).expect("remove");

        // Then the file is gone entirely.
        assert!(!path.as_path().exists());
    }
}
