//! The instance registry — how a running jinn advertises itself and how
//! a client finds one.
//!
//! A jinn instance that serves the control surface registers one
//! [`InstanceEntry`] in a JSON file under the AppPaths state dir: pid,
//! bound port, working directory, launch time. The file is shared state
//! between concurrent jinn instances, so every write is temp-file +
//! rename (never corrupt) and every *reader* prunes entries whose port
//! no longer accepts a TCP connection. The pid is carried for
//! `--instance` matching and display; it is never probed (pid syscalls
//! are not cross-platform).
//!
//! Liveness is the port probe: a dead jinn frees its port, so its entry
//! fails the probe and is invisible to every reader. Pid reuse cannot
//! fake liveness because the pid is never tested.

use serde::{Deserialize, Serialize};
use std::io::Write as _;
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::time::Duration;

/// The registry file name, joined onto the AppPaths state dir.
pub const REGISTRY_FILE_NAME: &str = "remote-instances.json";

/// How long the liveness probe waits before declaring a port dead.
pub const LIVENESS_TIMEOUT: Duration = Duration::from_millis(250);

/// One running jinn instance's advertisement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceEntry {
    /// The serving process's pid.
    pid: u32,
    /// The port the control surface bound (already resolved: never 0).
    port: u16,
    /// The instance's working directory at launch.
    cwd: String,
    /// Epoch milliseconds at registration.
    started_at: u64,
}

impl InstanceEntry {
    #[must_use]
    pub fn new(pid: u32, port: u16, cwd: String, started_at: u64) -> Self {
        Self {
            pid,
            port,
            cwd,
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
    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    #[must_use]
    pub const fn started_at(&self) -> u64 {
        self.started_at
    }
}

/// Instance selection failed: the registry is empty, or the requested
/// pid is not in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectError {
    /// The registry lists no live instance.
    NoInstances,
    /// An explicit pid was requested and no live entry carries it.
    UnknownInstance(u32),
}

/// Selects the instance a client talks to.
///
/// An explicit pid selects the entry carrying it — unambiguous by
/// construction, since one process registers one listener. With no pid,
/// the most recently launched instance wins (ties broken by higher pid,
/// a deterministic tiebreak that also prefers a recycled pid over a
/// stale one).
///
/// # Errors
///
/// Returns [`SelectError::NoInstances`] when the slice is empty and
/// [`SelectError::UnknownInstance`] when the requested pid is absent.
pub fn select(entries: &[InstanceEntry], pid: Option<u32>) -> Result<InstanceEntry, SelectError> {
    match pid {
        Some(pid) => entries
            .iter()
            .find(|entry| entry.pid() == pid)
            .cloned()
            .ok_or(SelectError::UnknownInstance(pid)),
        None => entries
            .iter()
            .max_by_key(|entry| (entry.started_at(), entry.pid()))
            .cloned()
            .ok_or(SelectError::NoInstances),
    }
}

/// Probes the instance's port: an open TCP connection means a listener
/// is alive on it.
#[must_use]
pub fn is_live(entry: &InstanceEntry) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], entry.port()));
    TcpStream::connect_timeout(&addr, LIVENESS_TIMEOUT).is_ok()
}

/// Reads the registry and returns only the live entries.
///
/// Stale entries are filtered, never rewritten: the CLI is a reader,
/// and staleness is bounded by the next instance start (each new jinn
/// prunes the file as it registers).
#[must_use]
pub fn read_live(path: &Path) -> Vec<InstanceEntry> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(entries) = serde_json::from_str::<Vec<InstanceEntry>>(&raw) else {
        return Vec::new();
    };
    entries.into_iter().filter(is_live).collect()
}

/// Writes `entries` to `path` atomically: temp file in the same
/// directory, then rename. A concurrent reader sees either the old
/// file or the new one, never a partial write.
///
/// # Errors
///
/// Returns the underlying io error when the temp file cannot be
/// written or the rename fails (the parent directory not existing is
/// the common cause — callers create it first).
pub fn write_entries(path: &Path, entries: &[InstanceEntry]) -> std::io::Result<()> {
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
/// The write prunes every entry whose port is dead — each new jinn
/// start cleans the file for everyone — then replaces or appends this
/// instance's own entry, keyed by pid.
///
/// # Errors
///
/// Returns the underlying io error when the file cannot be written
/// (read failures are not errors: a missing or corrupt file reads as
/// empty).
pub fn register(path: &Path, entry: InstanceEntry) -> std::io::Result<()> {
    let mut entries = read_all(path);
    entries.retain(|existing| is_live(existing) && existing.pid() != entry.pid());
    entries.push(entry);
    write_entries(path, &entries)
}

/// Removes the entry carrying `pid`, if any. Best effort by design:
/// the graceful shutdown path calls this, but correctness never depends
/// on it — a dead instance is invisible to every reader via the port
/// probe.
///
/// # Errors
///
/// Returns the underlying io error when the write fails; a missing or
/// corrupt file is not an error.
pub fn remove_entry(path: &Path, pid: u32) -> std::io::Result<()> {
    let entries = read_all(path);
    let remaining: Vec<InstanceEntry> = entries.into_iter().filter(|e| e.pid() != pid).collect();
    if remaining.is_empty() {
        // An empty registry is the file's normal post-prune state; drop
        // the file rather than leaving `[ ]` behind.
        let _ = std::fs::remove_file(path);
        return Ok(());
    }
    write_entries(path, &remaining)
}

/// Reads every entry, live or not. Registry-internal: callers outside
/// this module use [`read_live`].
#[cfg(test)]
pub(crate) fn read_all_for_test(path: &Path) -> Vec<InstanceEntry> {
    read_all(path)
}

fn read_all(path: &Path) -> Vec<InstanceEntry> {
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

    use super::{
        InstanceEntry, REGISTRY_FILE_NAME, SelectError, is_live, read_live, register, remove_entry,
        select,
    };
    use jinn_testutil::TestRoot;
    use marked_path::MarkedPathAccess as _;

    fn entry(pid: u32, port: u16, started_at: u64) -> InstanceEntry {
        InstanceEntry::new(pid, port, "/tmp/work".to_owned(), started_at)
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
        let parsed: InstanceEntry = serde_json::from_str(&raw).expect("parse");

        // Then it equals the original.
        assert_eq!(parsed, original);
    }

    #[rstest::rstest]
    #[test]
    fn select_without_pid_prefers_the_most_recently_launched_instance() {
        // Given three live entries launched at different times.
        let entries = vec![
            entry(1, 9001, 100),
            entry(2, 9002, 300),
            entry(3, 9003, 200),
        ];

        // When selecting with no pid.
        let picked = select(&entries, None).expect("entries exist");

        // Then the highest started_at wins.
        assert_eq!(picked.pid(), 2);
    }

    #[rstest::rstest]
    #[test]
    fn select_ties_on_started_at_break_toward_the_higher_pid() {
        // Given two entries launched at the same instant.
        let entries = vec![entry(7, 9001, 100), entry(11, 9002, 100)];

        // When selecting with no pid.
        let picked = select(&entries, None).expect("entries exist");

        // Then the higher pid wins.
        assert_eq!(picked.pid(), 11);
    }

    #[rstest::rstest]
    #[test]
    fn select_with_pid_picks_the_entry_carrying_it() {
        // Given entries from several instances.
        let entries = vec![entry(1, 9001, 100), entry(2, 9002, 200)];

        // When selecting by explicit pid.
        let picked = select(&entries, Some(1)).expect("pid present");

        // Then the entry with that pid wins, regardless of launch order.
        assert_eq!(picked.port(), 9001);
    }

    #[rstest::rstest]
    #[test]
    fn select_with_unknown_pid_is_a_named_error() {
        // Given entries from several instances.
        let entries = vec![entry(1, 9001, 100)];

        // When selecting a pid the registry does not carry.
        let result = select(&entries, Some(99));

        // Then the error names the missing pid.
        assert_eq!(result, Err(SelectError::UnknownInstance(99)));
    }

    #[rstest::rstest]
    #[test]
    fn select_on_an_empty_registry_is_a_named_error() {
        // Given no entries.
        let entries: Vec<InstanceEntry> = Vec::new();

        // When selecting.
        let result = select(&entries, None);

        // Then the error is the no-instances one.
        assert_eq!(result, Err(SelectError::NoInstances));
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
        assert_eq!(super::read_all_for_test(path.as_path()), vec![own]);
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
        assert_eq!(super::read_all_for_test(path.as_path()), vec![fresh]);
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

        // When registering a third instance.
        let own_listener = live_listener();
        let own = entry(3, port_of(&own_listener), 300);
        register(path.as_path(), own.clone()).expect("register");

        // Then the dead sibling is gone and the live one survives.
        let mut expected = vec![live, own];
        expected.sort_by_key(super::InstanceEntry::started_at);
        let mut actual = super::read_all_for_test(path.as_path());
        actual.sort_by_key(super::InstanceEntry::started_at);
        assert_eq!(actual, expected);
    }

    #[rstest::rstest]
    #[test]
    fn the_written_file_is_valid_json_for_an_outside_reader() {
        // Given a registry written through register.
        let root = TestRoot::new();
        let path = root.join_str(REGISTRY_FILE_NAME).expect("name joins");
        let listener = live_listener();
        register(path.as_path(), entry(42, port_of(&listener), 500)).expect("register");

        // When reading the file back as raw JSON.
        let raw = std::fs::read_to_string(path.as_path()).expect("read");

        // Then it parses as an array of entries.
        let parsed: Vec<InstanceEntry> =
            serde_json::from_str(&raw).expect("registry file is valid JSON");
        assert_eq!(parsed.len(), 1);
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
        assert_eq!(super::read_all_for_test(path.as_path()), vec![survivor]);
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
