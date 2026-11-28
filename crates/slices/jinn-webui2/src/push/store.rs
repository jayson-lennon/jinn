//! The push store: this instance's key pair and every browser's
//! subscription, in one file.
//!
//! Held in memory and rewritten whole on every change. The file is the
//! instance's own secret — the endpoints let anyone who holds one send to
//! that browser — so it is created readable by the owner only. A write that
//! fails is logged and leaves the in-memory copy intact: a push that cannot
//! be persisted is still a push this run can send, and the next successful
//! write carries it.
//!
//! A file that is missing gets a fresh key pair. A file that cannot be
//! parsed is replaced by a fresh one rather than refusing to load: the
//! alternative is a web UI that boots without notifications with no way
//! back, and the cost of replacing is that every browser subscribes again
//! on its next load.

use std::{io::Write as _, path::Path};

use jinn_webui2_msg::PushKeysBody;
use marked_path::{MarkedPathAccess as _, MarkedPathBuf};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use super::keys::KeyPair;

/// The file's name under the app data dir.
const FILE_NAME: &str = "webui-push.toml";

/// One browser's subscription, as its push manager issued it. Held by the
/// server so a push can be encrypted for, and posted to, that browser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscription {
    /// The push service's address for this subscription, verbatim.
    pub endpoint: String,
    /// The keys the payload is encrypted with.
    pub keys: PushKeysBody,
}

/// The file's whole content.
#[derive(Debug, Default, Serialize, Deserialize)]
struct FileBody {
    /// The instance's key pair.
    vapid: VapidRow,
    /// Every subscription, one row per endpoint.
    #[serde(default, rename = "subscription")]
    subscriptions: Vec<Subscription>,
}

/// The key pair as the file keeps it.
#[derive(Debug, Default, Serialize, Deserialize)]
struct VapidRow {
    /// The private scalar: raw 32 bytes, base64url.
    private_key: String,
}

/// What the store holds, under one lock.
#[derive(Debug, Default)]
struct Inner {
    /// The signing pair. `None` only for a store that could not get one —
    /// an unwritable data dir — which reports push as unsupported.
    keys: Option<KeyPair>,
    /// Every subscription, keyed by endpoint. One row per endpoint: a page
    /// re-sending the subscription it already holds replaces the row rather
    /// than adding a second, so a stale row never outlives the load that
    /// replaced it.
    subscriptions: Vec<Subscription>,
    /// Where the file lives; `None` for an in-memory store that persists nothing.
    path: Option<MarkedPathBuf<marked_path::Absolute>>,
}

/// The key pair and subscriptions the server currently holds.
#[derive(Debug, Clone, Default)]
pub struct PushStore {
    inner: std::sync::Arc<Mutex<Inner>>,
}

/// Why the store could not be opened.
#[derive(Debug, wherror::Error)]
#[error(debug)]
pub enum PushStoreError {
    /// The data dir could not be created or the file could not be read.
    Io,
}

impl PushStore {
    /// An in-memory store that persists nothing and holds no key pair. Every
    /// push would go nowhere until a pair and subscriptions are set.
    ///
    /// The test seam: a test that needs no file gets one, and a test that
    /// needs persistence opens a real one under an `AppPaths` root.
    #[must_use]
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// Opens the store at `app_data_dir`: loads the file when it parses,
    /// and otherwise generates a key pair and writes a fresh one.
    ///
    /// # Errors
    ///
    /// Returns [`PushStoreError::Io`] when the directory cannot be created or
    /// the file cannot be read for a reason other than being absent.
    pub fn open(
        app_data_dir: &marked_path::MarkedPath<'_, marked_path::Absolute>,
    ) -> Result<Self, error_stack::Report<PushStoreError>> {
        use error_stack::ResultExt as _;
        let path = app_data_dir
            .join(FILE_NAME)
            .change_context(PushStoreError::Io)
            .attach("the push store path leaves the data dir")?;
        std::fs::create_dir_all(app_data_dir.as_path())
            .change_context(PushStoreError::Io)
            .attach("creating the data dir for the push store")?;
        let body = read_body(path.as_path());
        let keys = body
            .as_ref()
            .and_then(|body| KeyPair::from_stored(&body.vapid.private_key));
        let store = Self {
            inner: std::sync::Arc::new(Mutex::new(Inner {
                keys,
                subscriptions: body.map_or_else(Vec::new, |body| body.subscriptions),
                path: Some(path),
            })),
        };
        // A missing or unparseable file has no pair yet: make one and write
        // it, so the file exists from the first run and the pair is stable.
        if store.inner.lock().keys.is_none() {
            store.inner.lock().keys = Some(KeyPair::generate());
            store.persist();
        }
        Ok(store)
    }

    /// The public key the page subscribes with; `None` when the store holds
    /// no pair, which the page reports as push being unavailable.
    #[must_use]
    pub fn public_key(&self) -> Option<String> {
        self.inner
            .lock()
            .keys
            .as_ref()
            .map(|keys| keys.public_key().as_str().to_owned())
    }

    /// The signing pair, for the VAPID signature.
    pub(crate) fn key_pair(&self) -> Option<KeyPair> {
        self.inner.lock().keys.clone()
    }

    /// Adds a subscription, replacing any row with the same endpoint, and
    /// rewrites the file.
    pub fn add(&self, subscription: Subscription) {
        {
            let mut inner = self.inner.lock();
            inner
                .subscriptions
                .retain(|held| held.endpoint != subscription.endpoint);
            inner.subscriptions.push(subscription);
        }
        self.persist();
    }

    /// Removes the subscription with this endpoint, when held, and rewrites
    /// the file.
    pub fn remove(&self, endpoint: &str) {
        {
            let mut inner = self.inner.lock();
            inner.subscriptions.retain(|held| held.endpoint != endpoint);
        }
        self.persist();
    }

    /// Every subscription, as a snapshot the caller can send to without
    /// holding the lock across a network request.
    #[must_use]
    pub fn subscriptions(&self) -> Vec<Subscription> {
        self.inner.lock().subscriptions.clone()
    }

    /// Rewrites the file whole from the in-memory copy. A store with no path
    /// persists nothing; a write that fails is logged and the in-memory copy
    /// stands.
    fn persist(&self) {
        let inner = self.inner.lock();
        let Some(path) = inner.path.as_ref() else {
            return;
        };
        let Some(keys) = inner.keys.as_ref() else {
            return;
        };
        let body = FileBody {
            vapid: VapidRow {
                private_key: keys.stored_form(),
            },
            subscriptions: inner.subscriptions.clone(),
        };
        let rendered = toml::to_string(&body).unwrap_or_default();
        // Write to a sibling and rename over: a crash mid-write leaves the
        // previous whole file, never a torn one.
        let mut temp = path.clone();
        temp.set_extension("toml.tmp");
        let write = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(temp.as_path())
            .map_err(std::io::Error::other)
            .and_then(|file| write_private(file, rendered.as_bytes()));
        if let Err(error) = write {
            tracing::warn!(%error, "push store: could not write the file");
            return;
        }
        if let Err(error) = std::fs::rename(temp.as_path(), path.as_path()) {
            tracing::warn!(%error, "push store: could not replace the file");
        }
    }
}

/// Writes `bytes` to a freshly created file and makes it readable by its
/// owner only. The mode is set before the rename so the file never exists
/// at its final path with wider permissions than it should have.
fn write_private(mut file: std::fs::File, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(bytes)?;
    file.flush()
}

/// The file's content, or `None` when there is none that parses. A file
/// that exists but does not parse is logged and treated as absent.
fn read_body(path: &Path) -> Option<FileBody> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            tracing::warn!(%error, "push store: could not read the file");
            return None;
        }
    };
    match toml::from_str::<FileBody>(&text) {
        Ok(body) => Some(body),
        Err(error) => {
            tracing::warn!(%error, "push store: the file does not parse; replacing it");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test code")]

    use jinn_testutil::paths::TestRoot;
    use marked_path::MarkedPathAccess as _;

    use super::{PushStore, Subscription};

    fn subscription(endpoint: &str, auth: &str) -> Subscription {
        Subscription {
            endpoint: endpoint.to_owned(),
            keys: jinn_webui2_msg::PushKeysBody {
                p256dh: "key".to_owned(),
                auth: auth.to_owned(),
            },
        }
    }

    /// A store persisted under its own temp root, through `AppPaths` — the
    /// same route production takes, so no test builds a path by hand.
    fn open_store(root: &TestRoot) -> PushStore {
        let paths = jinn_kernel::AppPaths::new_in(&root.path().as_marked_path());
        PushStore::open(&paths.app_data_dir().as_marked_path()).expect("the store opens")
    }

    #[rstest::rstest]
    #[test]
    fn an_in_memory_store_has_no_public_key() {
        // Given a store with no key pair.
        let store = PushStore::in_memory();

        // Then it reports no public key: push is unavailable.
        assert!(store.public_key().is_none());
    }

    #[rstest::rstest]
    #[test]
    fn an_opened_store_generates_a_public_key() {
        // Given a fresh data dir.
        let root = TestRoot::new();

        // When the store is opened over it.
        let store = open_store(&root);

        // Then it holds a public key.
        assert!(store.public_key().is_some());
    }

    #[rstest::rstest]
    #[test]
    fn a_reopened_store_keeps_its_public_key() {
        // Given a store that was opened once over a data dir.
        let root = TestRoot::new();
        let first = open_store(&root);
        let key = first.public_key().expect("a key");

        // When the store is opened again over the same dir.
        let second = open_store(&root);

        // Then the key is the one from before.
        assert_eq!(second.public_key().as_deref(), Some(key.as_str()));
    }

    #[rstest::rstest]
    #[test]
    fn a_corrupt_file_is_replaced_by_a_fresh_key() {
        // Given a data dir holding a push file that does not parse.
        let root = TestRoot::new();
        let paths = jinn_kernel::AppPaths::new_in(&root.path().as_marked_path());
        let dir = paths.app_data_dir();
        std::fs::create_dir_all(dir.as_path()).expect("mkdir");
        let file = root
            .join_str("data/jinn/webui-push.toml")
            .expect("a relative path under the root");
        std::fs::write(file.as_path(), "not [ valid toml").expect("write the bad file");

        // When the store is opened over it.
        let store = PushStore::open(&dir.as_marked_path()).expect("the store opens");

        // Then it holds a fresh key, and reopening keeps that one.
        let fresh = store.public_key().expect("a key");
        assert_eq!(
            open_store(&root).public_key().as_deref(),
            Some(fresh.as_str())
        );
    }

    #[rstest::rstest]
    #[test]
    fn an_added_subscription_survives_a_reopen() {
        // Given a store with one subscription.
        let root = TestRoot::new();
        let store = open_store(&root);
        store.add(subscription("https://push.example/a", "one"));

        // When the store is reopened over the same dir.
        let reopened = open_store(&root);

        // Then the subscription is still there.
        let listed = reopened.subscriptions();
        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed.first().map(|row| row.endpoint.as_str()),
            Some("https://push.example/a")
        );
    }

    #[rstest::rstest]
    #[test]
    fn the_same_endpoint_replaces_the_held_row() {
        // Given a store holding one subscription.
        let store = PushStore::in_memory();
        store.add(subscription("https://push.example/a", "old"));

        // When the same endpoint arrives with new keys.
        store.add(subscription("https://push.example/a", "new"));

        // Then there is one row, and it carries the new keys.
        let listed = store.subscriptions();
        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed.first().map(|row| row.keys.auth.as_str()),
            Some("new")
        );
    }

    #[rstest::rstest]
    #[test]
    fn a_removed_subscription_is_gone_after_a_reopen() {
        // Given a store with two subscriptions.
        let root = TestRoot::new();
        let store = open_store(&root);
        store.add(subscription("https://push.example/a", "one"));
        store.add(subscription("https://push.example/b", "two"));

        // When one is removed and the store is reopened.
        store.remove("https://push.example/a");
        let reopened = open_store(&root);

        // Then only the other is held.
        let listed = reopened.subscriptions();
        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed.first().map(|row| row.endpoint.as_str()),
            Some("https://push.example/b")
        );
    }

    #[rstest::rstest]
    #[test]
    fn removing_an_absent_endpoint_changes_nothing() {
        // Given a store holding one subscription.
        let store = PushStore::in_memory();
        store.add(subscription("https://push.example/a", "one"));

        // When another endpoint is removed.
        store.remove("https://push.example/other");

        // Then the held subscription is still there.
        assert_eq!(store.subscriptions().len(), 1);
    }

    #[rstest::rstest]
    #[test]
    fn a_clone_shares_the_held_subscriptions() {
        // Given a store and a clone of it.
        let store = PushStore::in_memory();
        let clone = store.clone();

        // When the store adds a subscription.
        store.add(subscription("https://push.example/a", "one"));

        // Then the clone lists it.
        assert_eq!(clone.subscriptions().len(), 1);
    }

    #[rstest::rstest]
    #[test]
    fn the_stored_file_is_readable_by_the_owner_only() {
        // Given a store opened over a data dir.
        let root = TestRoot::new();
        open_store(&root);

        // Then the file's mode carries no group or other bits.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let file = root
                .join_str("data/jinn/webui-push.toml")
                .expect("a relative path under the root");
            let mode = std::fs::metadata(file.as_path())
                .expect("metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0);
        }
    }
}
