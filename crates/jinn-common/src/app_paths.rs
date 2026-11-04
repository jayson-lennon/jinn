//! Application filesystem paths - single source of truth for all directory locations.
//!
//! [`AppPaths`] holds every base directory the application needs. It is computed
//! once at init time: [`AppPaths::default()`] for production (from `dirs::*`,
//! honoring the [`PATHS_ROOT_ENV`] override), [`AppPaths::new_in()`] for tests
//! (from a temp root).
//!
//! All domain code that needs a filesystem path must read it from the injected
//! `AppPaths` instance - never from `dirs::*` free functions directly.

use crate::app_info::APP_NAME;
use marked_path::{Absolute, MarkedPath, MarkedPathBuf};

/// Browser profile mode: one persistent profile per mode.
///
/// `headless` and `headed` each get their own profile subdir so a single
/// warmed headed profile can be shared by both `web-fetch` and `web-search`
/// without colliding on Chromium's singleton lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BrowserProfileMode {
    /// Headless profile (no visible window).
    Headless,
    /// Headed profile (visible window, manually warmable).
    Headed,
}

impl BrowserProfileMode {
    /// Returns the subdirectory name for this mode.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Headless => "headless",
            Self::Headed => "headed",
        }
    }
}

/// Application filesystem paths.
///
/// Stores app-specific directories (not platform directories). For production,
/// these are derived from `dirs::*` — or from a single root directory when
/// [`PATHS_ROOT_ENV`] is set (see [`AppPaths::default()`]). For tests, from a
/// single temp root via [`AppPaths::new_in`].
///
/// Construct once at init and share via `Services.paths`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(clippy::struct_field_names, reason = "names match domain terminology")]
pub struct AppPaths {
    /// `~/.config/jinn` - providers.toml, prompts/, personas/, themes/, jinn.toml
    config_dir: MarkedPathBuf<Absolute>,
    /// `~/.local/share/jinn` - sessions.db
    data_dir: MarkedPathBuf<Absolute>,
    /// `~/.cache/jinn` - model_cache.json
    cache_dir: MarkedPathBuf<Absolute>,
    /// `~/.local/state/jinn` - jinn.log
    ///
    /// Falls back to `data_dir` on platforms where `dirs::state_dir()` returns
    /// `None` (e.g. macOS, Windows).
    state_dir: MarkedPathBuf<Absolute>,
    /// `~/` - home directory (skills live at `~/.agents/skills`)
    home_dir: MarkedPathBuf<Absolute>,
}

/// Environment variable that, when set, rebinds every [`AppPaths`] base
/// under one root directory.
///
/// Read once, at construction of [`AppPaths::default()`]; no other code in
/// the workspace reads the environment for path derivation. When set, the
/// layout matches [`AppPaths::new_in`] exactly: `config/`, `data/`,
/// `cache/`, `state/` subdirectories, with `home` as the root itself.
///
/// Its purpose is test isolation: integration tests that spawn the `jinn`
/// binary point this at a temp root so no run ever writes to the real
/// user config, data, or home directories. On Linux, where `dirs`
/// already honors `XDG_CONFIG_HOME`/`XDG_DATA_HOME`, the override wins
/// there too, keeping one behavior across platforms.
pub const PATHS_ROOT_ENV: &str = "JINN_PATHS_ROOT";

impl Default for AppPaths {
    // The env override is a test hook read once at startup; a malformed value
    // is a programming error in the harness, not runtime state.
    #![allow(
        clippy::expect_used,
        reason = "test-hook env var, malformed value is a harness bug"
    )]
    fn default() -> Self {
        match std::env::var_os(PATHS_ROOT_ENV) {
            Some(root) => {
                let typed_root = MarkedPathBuf::<Absolute>::new(&root)
                    .expect("PATHS_ROOT_ENV override root is absolute");
                Self::new_in(&typed_root.as_marked_path())
            }
            None => Self::from_platform_dirs(),
        }
    }
}

/// The current working directory in absolute form.
///
/// The last-resort base when the platform cannot name a directory. If even
/// the working directory cannot be read (it was deleted), the temp directory
/// stands in; both candidates are absolute by construction.
///
/// # Panics
///
/// Panics if the candidate directory is not absolute, which cannot happen on
/// supported platforms (the OS returns absolute cwds and temp paths).
#[expect(
    clippy::expect_used,
    reason = "platform-impossible: OS cwds and temp paths are absolute"
)]
fn cwd_absolute() -> MarkedPathBuf<Absolute> {
    let candidate = std::env::current_dir().unwrap_or_else(|_| std::env::temp_dir());
    MarkedPathBuf::<Absolute>::new(candidate).expect("cwd and temp dir are absolute")
}

impl AppPaths {
    // Bare-name joins onto bases already typed `Absolute` cannot introduce a
    // drive prefix or absolute segment, so those `join`/`join_all` results are
    // infallible by construction; the panics guard programmer error, not
    // runtime state, and the invariant is documented here once for the family.
    #![allow(
        clippy::expect_used,
        clippy::missing_panics_doc,
        reason = "bare-name joins on typed bases are infallible by construction"
    )]

    /// Creates paths derived from the platform's per-user directories.
    ///
    /// Every base is absolute: when the platform cannot name a directory,
    /// the current working directory stands in, resolved to its absolute
    /// form so no base is ever relative.
    ///
    /// # Panics
    ///
    /// Panics if a platform directory is not absolute, which cannot happen
    /// on supported platforms.
    fn from_platform_dirs() -> Self {
        /// Types one platform directory, falling back to the cwd.
        ///
        /// `dirs` hands its values over as raw owning paths; they are typed
        /// immediately and never escape raw.
        #[expect(
            clippy::disallowed_types,
            reason = "dirs-crate seam: platform directory values arrive as raw PathBuf and are typed on arrival"
        )]
        fn typed(
            raw: Option<std::path::PathBuf>,
            fallback: &MarkedPathBuf<Absolute>,
        ) -> MarkedPathBuf<Absolute> {
            match raw {
                Some(raw) => {
                    MarkedPathBuf::<Absolute>::new(raw).expect("platform dirs and cwd are absolute")
                }
                None => fallback.clone(),
            }
        }

        let fallback = cwd_absolute();
        Self {
            config_dir: typed(dirs::config_dir(), &fallback),
            data_dir: typed(dirs::data_dir(), &fallback),
            cache_dir: typed(dirs::cache_dir(), &fallback),
            state_dir: typed(dirs::state_dir().or_else(dirs::data_dir), &fallback),
            home_dir: typed(dirs::home_dir(), &fallback),
        }
    }

    /// Creates paths derived from a single root directory (for tests).
    ///
    /// All subdirectories are nested under `root/` so that a single
    /// `TempDir` cleans everything up.
    #[must_use]
    pub fn new_in(root: &MarkedPath<'_, Absolute>) -> Self {
        // One `Result` for the whole fixed layout: every dir is a bare
        // name join onto the validated root.
        let dir = |name| {
            root.join_all([name])
                .expect("joining a name keeps the root absolute")
        };
        Self {
            config_dir: dir("config"),
            data_dir: dir("data"),
            cache_dir: dir("cache"),
            state_dir: dir("state"),
            home_dir: root.to_owned(),
        }
    }

    /// Sets the home directory for tests (bounds the project walk).
    #[cfg(feature = "testing")]
    pub fn set_home_dir_for_test(&mut self, home: MarkedPathBuf<Absolute>) {
        self.home_dir = home;
    }

    // -- Derived paths -------------------------------------------------------

    /// Session database parent directory (`~/.local/share/jinn`).
    ///
    /// Pass to `SqliteSessionStore::new_in()`.
    #[must_use]
    pub fn sessions_dir(&self) -> MarkedPathBuf<Absolute> {
        self.data_dir
            .join(APP_NAME)
            .expect("name join keeps the base absolute")
    }

    /// Parent directory for builtin graph databases (`~/.local/share/jinn/graph`).
    ///
    /// The builtin overgraph MCP server opens one database directory per
    /// configured server entry beneath this dir (default: `graph/default`).
    #[must_use]
    pub fn graph_db_dir(&self) -> MarkedPathBuf<Absolute> {
        self.data_dir
            .join_all([APP_NAME, "graph"])
            .expect("name joins keep the base absolute")
    }

    /// Base directory for persistent browser profiles (`~/.local/share/jinn/browser-profile`).
    ///
    /// Per-mode profiles live under subdirs (`browser-profile/headless`,
    /// `browser-profile/headed`) so a single warmed headed profile is shared by
    /// both `web-fetch` and `web-search` when they select `headed-chrome`.
    /// Use [`Self::browser_profile_dir_for_mode`] to resolve a concrete path.
    #[must_use]
    pub fn browser_profile_base_dir(&self) -> MarkedPathBuf<Absolute> {
        self.data_dir
            .join_all([APP_NAME, "browser-profile"])
            .expect("name joins keep the base absolute")
    }

    /// Persistent browser profile for a given mode (`headless` or `headed`).
    #[must_use]
    pub fn browser_profile_dir_for_mode(
        &self,
        mode: BrowserProfileMode,
    ) -> MarkedPathBuf<Absolute> {
        self.browser_profile_base_dir()
            .join(mode.as_str())
            .expect("name join keeps the base absolute")
    }

    /// Prompt templates directory (`~/.config/jinn/prompts`).
    #[must_use]
    pub fn prompts_dir(&self) -> MarkedPathBuf<Absolute> {
        self.config_dir
            .join_all([APP_NAME, "prompts"])
            .expect("name joins keep the base absolute")
    }

    /// Agent skills directory (`~/.agents/skills`).
    #[must_use]
    pub fn skills_dir(&self) -> MarkedPathBuf<Absolute> {
        self.home_dir
            .join_all([".agents", "skills"])
            .expect("name joins keep the base absolute")
    }

    /// User home directory (`~/`). Bounds the project walk (exclusive of home).
    #[must_use]
    pub fn home_dir(&self) -> &MarkedPathBuf<Absolute> {
        &self.home_dir
    }

    /// Personas directory (`~/.config/jinn/personas`).
    #[must_use]
    pub fn personas_dir(&self) -> MarkedPathBuf<Absolute> {
        self.config_dir
            .join_all([APP_NAME, "personas"])
            .expect("name joins keep the base absolute")
    }

    /// Workflows directory (`~/.config/jinn/workflows`).
    ///
    /// One directory per workflow, each holding a `workflow.md`. Launch-global:
    /// unlike skills and prompts, workflows are not project-scoped, so the
    /// discovery scan reads this one directory regardless of session cwd.
    #[must_use]
    pub fn workflows_dir(&self) -> MarkedPathBuf<Absolute> {
        self.config_dir
            .join_all([APP_NAME, "workflows"])
            .expect("name joins keep the base absolute")
    }

    /// Model cache file (`~/.cache/jinn/model_cache.json`).
    #[must_use]
    pub fn cache_path(&self) -> MarkedPathBuf<Absolute> {
        self.cache_dir
            .join_all([APP_NAME, "model_cache.json"])
            .expect("name joins keep the base absolute")
    }

    /// Provider config file (`~/.config/jinn/providers.toml`).
    #[must_use]
    pub fn config_path(&self) -> MarkedPathBuf<Absolute> {
        self.config_dir
            .join_all([APP_NAME, "providers.toml"])
            .expect("name joins keep the base absolute")
    }

    /// Log file path (`~/.local/state/jinn/jinn.log` on Linux).
    #[must_use]
    pub fn log_path(&self) -> MarkedPathBuf<Absolute> {
        self.state_dir
            .join(APP_NAME)
            .and_then(|j| j.join(format!("{APP_NAME}.log")))
            .expect("name joins keep the base absolute")
    }

    /// App state file (`~/.local/state/jinn/state.toml` on Linux).
    #[must_use]
    pub fn state_file_path(&self) -> MarkedPathBuf<Absolute> {
        self.state_dir
            .join_all([APP_NAME, "state.toml"])
            .expect("name joins keep the base absolute")
    }

    /// Bundled-files manifest (`~/.local/state/jinn/bundled-files.toml` on
    /// Linux) — the record of what the reconciler last wrote where.
    ///
    /// Lives in the state dir beside `state.toml` and `jinn.log`: it is
    /// machine-local bookkeeping the binary owns, not user data and not
    /// configuration.
    #[must_use]
    pub fn bundled_files_manifest_path(&self) -> MarkedPathBuf<Absolute> {
        self.state_dir
            .join_all([APP_NAME, "bundled-files.toml"])
            .expect("name joins keep the base absolute")
    }

    /// Instance registry (`~/.local/state/jinn/remote-instances.json` on
    /// Linux) — where each running jinn advertises the external control
    /// surface it serves (pid, bound port, working directory).
    ///
    /// Machine-local bookkeeping like the manifest above: readers prune
    /// dead entries, so the file never needs manual curation. The file
    /// name is owned by `jinn-remote-msg`'s registry module; keep the
    /// two in step.
    #[must_use]
    pub fn remote_instances_path(&self) -> MarkedPathBuf<Absolute> {
        self.state_dir
            .join_all([APP_NAME, "remote-instances.json"])
            .expect("name joins keep the base absolute")
    }

    /// Themes directory (`~/.config/jinn/themes`).
    #[must_use]
    pub fn themes_dir(&self) -> MarkedPathBuf<Absolute> {
        self.config_dir
            .join_all([APP_NAME, "themes"])
            .expect("name joins keep the base absolute")
    }

    /// User preferences file (`~/.config/jinn/jinn.toml`).
    #[must_use]
    pub fn preferences_path(&self) -> MarkedPathBuf<Absolute> {
        self.config_dir
            .join_all([APP_NAME, crate::app_info::PREFS_FILE_NAME])
            .expect("name joins keep the base absolute")
    }

    /// Config directory (`~/.config/jinn`).
    #[must_use]
    pub fn app_config_dir(&self) -> MarkedPathBuf<Absolute> {
        self.config_dir
            .join(APP_NAME)
            .expect("name join keeps the base absolute")
    }

    /// Base config directory (`~/.config`) — parent of all jinn config.
    #[must_use]
    pub fn config_dir(&self) -> &MarkedPathBuf<Absolute> {
        &self.config_dir
    }

    /// Base data directory (`~/.local/share`) — parent of all jinn data.
    #[must_use]
    pub fn data_dir(&self) -> &MarkedPathBuf<Absolute> {
        &self.data_dir
    }

    /// App data directory (`~/.local/share/jinn`) — caches, scratch
    /// dirs, and other jinn-owned data live here.
    #[must_use]
    pub fn app_data_dir(&self) -> MarkedPathBuf<Absolute> {
        self.data_dir
            .join(APP_NAME)
            .expect("name join keeps the base absolute")
    }

    /// User's models.dev reference file (`~/.cache/jinn/models.dev.json`).
    ///
    /// Written by `jinn fetch models`. Contains the full models.dev API
    /// dump used as a fallback for context window lookup.
    #[must_use]
    pub fn models_dev_user_path(&self) -> MarkedPathBuf<Absolute> {
        self.cache_dir
            .join_all([APP_NAME, "models.dev.json"])
            .expect("name joins keep the base absolute")
    }

    /// Epoch benchmark cache directory (`~/.cache/jinn/epoch`).
    ///
    /// Holds the three CSVs `<c-r>` extracts from the downloaded epoch
    /// zip; absent on first run, where the compiled-in seed stands in.
    #[must_use]
    pub fn epoch_cache_dir(&self) -> MarkedPathBuf<Absolute> {
        self.cache_dir
            .join_all([APP_NAME, "epoch"])
            .expect("name joins keep the base absolute")
    }

    /// Saved apps directory (`~/.local/share/jinn/apps`).
    ///
    /// One directory per saved app (an agent-built HTML artifact the user
    /// pinned), with the `apps.json` catalog at the root. User data the
    /// user expects to survive jinn and its sessions, so it lives under
    /// the data dir, not cache or state.
    #[must_use]
    pub fn apps_dir(&self) -> MarkedPathBuf<Absolute> {
        self.data_dir
            .join_all([APP_NAME, "apps"])
            .expect("name joins keep the base absolute")
    }

    /// OpenRouter market cache directory (`~/.cache/jinn/openrouter`).
    ///
    /// Holds the model-market's TTL disk caches: the models listing and
    /// the per-model endpoint detail files.
    #[must_use]
    pub fn openrouter_cache_dir(&self) -> MarkedPathBuf<Absolute> {
        self.cache_dir
            .join_all([APP_NAME, "openrouter"])
            .expect("name joins keep the base absolute")
    }

    /// Embedding-model cache directory (`~/.cache/jinn/embed-models`).
    ///
    /// Holds the overgraph server's local embedding model weights,
    /// downloaded from HuggingFace on first use and reused offline after
    /// that. Re-downloadable content, so it lives under the cache dir.
    #[must_use]
    pub fn embed_models_dir(&self) -> MarkedPathBuf<Absolute> {
        self.cache_dir
            .join_all([APP_NAME, "embed-models"])
            .expect("name joins keep the base absolute")
    }

    /// Dictation-model cache directory (`~/.cache/jinn/dictation-models`).
    ///
    /// Holds the dictation backend's model weights, downloaded from
    /// HuggingFace on first use and reused offline after that.
    /// Re-downloadable content, so it lives under the cache dir. The
    /// HuggingFace client's default cache location is overridden to
    /// this directory — nothing lands in `~/.cache/huggingface`.
    #[must_use]
    pub fn dictation_models_dir(&self) -> MarkedPathBuf<Absolute> {
        self.cache_dir
            .join_all([APP_NAME, "dictation-models"])
            .expect("name joins keep the base absolute")
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::*;
    use marked_path::MarkedPathAccess;

    /// Serializes every test that mutates [`PATHS_ROOT_ENV`], and restores the
    /// prior value on drop so one test's override never leaks into another.
    ///
    /// The lock must be held before the env is touched (edition 2024 makes
    /// `set_var`/`remove_var` unsafe because the environment is process-global).
    static PATHS_ROOT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Restores [`PATHS_ROOT_ENV`] to `value`, clearing it when `None`.
    ///
    /// Must only be called while `PATHS_ROOT_LOCK` is held.
    fn restore_paths_root_env(value: Option<&std::ffi::OsStr>) {
        match value {
            Some(value) => {
                // SAFETY: PATHS_ROOT_LOCK is held by the caller, so no other
                // test mutates or observes the environment mid-restore.
                unsafe { std::env::set_var(PATHS_ROOT_ENV, value) };
            }
            None => {
                // SAFETY: PATHS_ROOT_LOCK is held by the caller (see above).
                unsafe { std::env::remove_var(PATHS_ROOT_ENV) };
            }
        }
    }

    /// Guard that sets [`PATHS_ROOT_ENV`] and restores the prior value on drop.
    struct PathsRootEnvGuard {
        /// Held for the lifetime of the guard, serializing env mutation.
        _lock: std::sync::MutexGuard<'static, ()>,
        /// The value to restore (None clears the variable).
        previous: Option<std::ffi::OsString>,
    }

    impl PathsRootEnvGuard {
        /// Sets `JINN_PATHS_ROOT` to `root`, remembering what was there.
        fn set(root: &MarkedPathBuf<Absolute>) -> Self {
            let lock = PATHS_ROOT_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let previous = std::env::var_os(PATHS_ROOT_ENV);
            // SAFETY: the lock is held before any env mutation, so no other
            // test mutates or observes the environment concurrently.
            unsafe { std::env::set_var(PATHS_ROOT_ENV, root.as_path()) };
            Self {
                _lock: lock,
                previous,
            }
        }
    }

    impl Drop for PathsRootEnvGuard {
        fn drop(&mut self) {
            // The lock guard outlives this body (field drop order), so the
            // restore stays serialized.
            restore_paths_root_env(self.previous.as_deref());
        }
    }

    #[rstest::rstest]
    fn default_honors_paths_root_env_for_every_base() {
        // Given a set JINN_PATHS_ROOT.
        let root = jinn_testutil::tempdir();
        let _guard = PathsRootEnvGuard::set(&root.path);

        // When constructing the default AppPaths.
        let paths = AppPaths::default();

        // Then every base rebinds under the root with the new_in layout:
        // config/data/cache/state subdirs, home as the root.
        assert_eq!(
            paths.config_dir(),
            &root.path.join("config").expect("root joins a bare name")
        );
        assert_eq!(
            paths.data_dir(),
            &root.path.join("data").expect("root joins a bare name")
        );
        let cache_parent = root
            .path
            .join_all(["cache", "jinn"])
            .expect("root joins bare names");
        assert_eq!(
            paths.cache_path().parent(),
            Some(cache_parent.as_marked_path())
        );
        let state_parent = root
            .path
            .join_all(["state", "jinn"])
            .expect("root joins bare names");
        assert_eq!(
            paths.state_file_path().parent(),
            Some(state_parent.as_marked_path())
        );
        assert_eq!(paths.home_dir(), &root.path);
    }

    #[rstest::rstest]
    fn default_with_paths_root_env_matches_new_in() {
        // Given a set JINN_PATHS_ROOT.
        let root = jinn_testutil::tempdir();
        let _guard = PathsRootEnvGuard::set(&root.path);

        // When constructing the default and a new_in AppPaths from the
        // same root.
        let via_env = AppPaths::default();
        let via_new_in = AppPaths::new_in(&root.path.as_marked_path());

        // Then they are identical — the override is exactly the new_in
        // layout, not a variant of it.
        assert_eq!(via_env, via_new_in);
    }

    #[rstest::rstest]
    fn workflows_dir_sits_beside_personas_under_config() {
        // Given an AppPaths rooted at a temp tree.
        let root = jinn_testutil::tempdir();
        let paths = AppPaths::new_in(&root.path.as_marked_path());

        // When resolving the workflows dir.
        let dir = paths.workflows_dir();

        // Then it is the config dir's workflows folder, beside personas.
        assert_eq!(
            dir,
            root.path
                .join_all(["config", "jinn", "workflows"])
                .expect("root joins bare names")
        );
        // And it is a sibling of the personas dir.
        assert_eq!(dir.parent(), paths.personas_dir().parent());
    }

    #[rstest::rstest]
    fn new_in_epoch_cache_dir_is_under_root_cache() {
        // Given an AppPaths created with new_in.
        let root = jinn_testutil::tempdir();
        let paths = AppPaths::new_in(&root.path.as_marked_path());

        // When reading the epoch cache dir.
        let epoch_dir = paths.epoch_cache_dir();

        // Then it is root/cache/jinn/epoch.
        assert_eq!(
            epoch_dir,
            root.path
                .join_all(["cache", "jinn"])
                .expect("root joins bare names")
                .join("epoch")
                .expect("typed parent joins a bare name"),
        );
    }

    #[rstest::rstest]
    fn new_in_openrouter_cache_dir_is_under_root_cache() {
        // Given an AppPaths created with new_in.
        let root = jinn_testutil::tempdir();
        let paths = AppPaths::new_in(&root.path.as_marked_path());

        // When reading the OpenRouter market cache dir.
        let or_dir = paths.openrouter_cache_dir();

        // Then it is root/cache/jinn/openrouter.
        assert_eq!(
            or_dir,
            root.path
                .join_all(["cache", "jinn"])
                .expect("root joins bare names")
                .join("openrouter")
                .expect("typed parent joins a bare name"),
        );
    }

    #[rstest::rstest]
    fn new_in_embed_models_dir_is_under_root_cache() {
        // Given an AppPaths created with new_in.
        let root = jinn_testutil::tempdir();
        let paths = AppPaths::new_in(&root.path.as_marked_path());

        // When reading the embedding-model cache dir.
        let dir = paths.embed_models_dir();

        // Then it is root/cache/jinn/embed-models.
        assert_eq!(
            dir,
            root.path
                .join_all(["cache", "jinn"])
                .expect("root joins bare names")
                .join("embed-models")
                .expect("typed parent joins a bare name"),
        );
    }

    #[rstest::rstest]
    fn log_path_under_state_dir_in_new_in() {
        // Given an AppPaths rooted in a temp dir.
        let root = jinn_testutil::tempdir();
        let paths = AppPaths::new_in(&root.path.as_marked_path());

        // When reading the log path.
        let log_path = paths.log_path();

        // Then it is root/state/jinn/jinn.log.
        assert_eq!(
            log_path,
            root.path
                .join_all(["state", "jinn"])
                .expect("root joins bare names")
                .join("jinn.log")
                .expect("typed parent joins a bare name"),
        );
    }

    #[rstest::rstest]
    fn state_file_path_under_state_dir_in_new_in() {
        // Given an AppPaths rooted in a temp dir.
        let root = jinn_testutil::tempdir();
        let paths = AppPaths::new_in(&root.path.as_marked_path());

        // When reading the state file path.
        let state_file_path = paths.state_file_path();

        // Then it is root/state/jinn/state.toml.
        assert_eq!(
            state_file_path,
            root.path
                .join_all(["state", "jinn"])
                .expect("root joins bare names")
                .join("state.toml")
                .expect("typed parent joins a bare name"),
        );
    }

    #[rstest::rstest]
    fn bundled_files_manifest_path_sits_beside_the_state_file() {
        // Given an AppPaths rooted in a temp dir.
        let root = jinn_testutil::tempdir();
        let paths = AppPaths::new_in(&root.path.as_marked_path());

        // When reading the bundled-files manifest path.
        let manifest_path = paths.bundled_files_manifest_path();

        // Then it is root/state/jinn/bundled-files.toml — the state dir,
        // the same home as state.toml and jinn.log.
        assert_eq!(
            manifest_path,
            root.path
                .join_all(["state", "jinn"])
                .expect("root joins bare names")
                .join("bundled-files.toml")
                .expect("typed parent joins a bare name"),
        );
    }

    #[rstest::rstest]
    fn remote_instances_path_sits_beside_the_state_file() {
        // Given an AppPaths rooted in a temp dir.
        let root = jinn_testutil::tempdir();
        let paths = AppPaths::new_in(&root.path.as_marked_path());

        // When reading the remote instance registry path.
        let registry_path = paths.remote_instances_path();

        // Then it is root/state/jinn/remote-instances.json — the state
        // dir, the same home as state.toml and jinn.log.
        assert_eq!(
            registry_path,
            root.path
                .join_all(["state", "jinn"])
                .expect("root joins bare names")
                .join("remote-instances.json")
                .expect("typed parent joins a bare name"),
        );
    }

    #[rstest::rstest]
    fn log_path_filename_is_app_name_dot_log() {
        // Given an AppPaths.
        let root = jinn_testutil::tempdir();
        let paths = AppPaths::new_in(&root.path.as_marked_path());

        // When reading the filename component of the log path.
        let log_path = paths.log_path();
        let filename = log_path.file_name().and_then(|s| s.to_str());

        // Then it is jinn.log.
        assert_eq!(filename, Some("jinn.log"));
    }

    #[rstest::rstest]
    fn browser_profile_dir_for_mode_lands_under_data_base() {
        // Given an AppPaths rooted at a temp dir.
        let root = jinn_testutil::tempdir();
        let paths = AppPaths::new_in(&root.path.as_marked_path());

        // When resolving the headed profile dir.
        let headed = paths.browser_profile_dir_for_mode(BrowserProfileMode::Headed);
        let headless = paths.browser_profile_dir_for_mode(BrowserProfileMode::Headless);

        // Then both land under data/jinn/browser-profile/<mode>.
        assert_eq!(
            headed,
            root.path
                .join("data/jinn/browser-profile/headed")
                .expect("temp root joins a bare name")
        );
        assert_eq!(
            headless,
            root.path
                .join("data/jinn/browser-profile/headless")
                .expect("temp root joins a bare name")
        );
    }

    #[rstest::rstest]
    fn apps_dir_lands_under_data_base() {
        // Given an AppPaths rooted at a temp dir.
        let root = jinn_testutil::tempdir();
        let paths = AppPaths::new_in(&root.path.as_marked_path());

        // When resolving the saved-apps directory.
        let dir = paths.apps_dir();

        // Then it is data/jinn/apps — the data dir, the same base as the
        // session store and the graph databases.
        assert_eq!(
            dir,
            root.path
                .join("data/jinn/apps")
                .expect("temp root joins a bare name")
        );
    }

    #[rstest::rstest]
    fn graph_db_dir_lands_under_data_base() {
        // Given an AppPaths rooted at a temp dir.
        let root = jinn_testutil::tempdir();
        let paths = AppPaths::new_in(&root.path.as_marked_path());

        // When resolving the graph database dir.
        let dir = paths.graph_db_dir();

        // Then it is data/jinn/graph.
        assert_eq!(
            dir,
            root.path
                .join("data/jinn/graph")
                .expect("temp root joins a bare name")
        );
    }
}
