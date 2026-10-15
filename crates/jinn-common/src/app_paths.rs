//! Application filesystem paths - single source of truth for all directory locations.
//!
//! [`AppPaths`] holds every base directory the application needs. It is computed
//! once at init time: [`AppPaths::default()`] for production (from `dirs::*`,
//! honoring the [`PATHS_ROOT_ENV`] override), [`AppPaths::new_in()`] for tests
//! (from a temp root).
//!
//! All domain code that needs a filesystem path must read it from the injected
//! `AppPaths` instance - never from `dirs::*` free functions directly.

use std::path::{Path, PathBuf};

use crate::app_info::APP_NAME;

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
    config_dir: PathBuf,
    /// `~/.local/share/jinn` - sessions.db
    data_dir: PathBuf,
    /// `~/.cache/jinn` - model_cache.json
    cache_dir: PathBuf,
    /// `~/.local/state/jinn` - jinn.log
    ///
    /// Falls back to `data_dir` on platforms where `dirs::state_dir()` returns
    /// `None` (e.g. macOS, Windows).
    state_dir: PathBuf,
    /// `~/` - home directory (skills live at `~/.agents/skills`)
    home_dir: PathBuf,
    /// `/usr/share/jinn` - system-wide defaults (themes, personas, prompts).
    ///
    /// User files in `~/.config/jinn/` override system files of the same name.
    system_data_dir: PathBuf,
}

/// Environment variable that, when set, rebinds every [`AppPaths`] base
/// under one root directory.
///
/// Read once, at construction of [`AppPaths::default()`]; no other code in
/// the workspace reads the environment for path derivation. When set, the
/// layout matches [`AppPaths::new_in`] exactly: `config/`, `data/`,
/// `cache/`, `state/` subdirectories, `home` is the root itself, and
/// system data lives at `root/share`.
///
/// Its purpose is test isolation: integration tests that spawn the `jinn`
/// binary point this at a temp root so no run ever writes to the real
/// user config, data, or home directories. On Linux, where `dirs`
/// already honors `XDG_CONFIG_HOME`/`XDG_DATA_HOME`, the override wins
/// there too, keeping one behavior across platforms.
pub const PATHS_ROOT_ENV: &str = "JINN_PATHS_ROOT";

impl Default for AppPaths {
    fn default() -> Self {
        match std::env::var_os(PATHS_ROOT_ENV) {
            Some(root) => Self::new_in(Path::new(&root)),
            None => Self::from_platform_dirs(),
        }
    }
}

impl AppPaths {
    /// Creates paths derived from the platform's per-user directories.
    fn from_platform_dirs() -> Self {
        Self {
            config_dir: dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")),
            data_dir: dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")),
            cache_dir: dirs::cache_dir().unwrap_or_else(|| PathBuf::from(".")),
            state_dir: dirs::state_dir()
                .or_else(dirs::data_dir)
                .unwrap_or_else(|| PathBuf::from(".")),
            home_dir: dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")),
            system_data_dir: PathBuf::from("/usr/share/jinn"),
        }
    }

    /// Creates paths derived from a single root directory (for tests).
    ///
    /// All subdirectories are nested under `root/` so that a single
    /// `TempDir` cleans everything up.
    #[must_use]
    pub fn new_in(root: &Path) -> Self {
        Self {
            config_dir: root.join("config"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
            state_dir: root.join("state"),
            home_dir: root.to_path_buf(),
            system_data_dir: root.join("share"),
        }
    }

    /// Sets the home directory for tests (bounds the project walk).
    #[cfg(feature = "testing")]
    pub fn set_home_dir_for_test(&mut self, home: PathBuf) {
        self.home_dir = home;
    }

    // -- Derived paths -------------------------------------------------------

    /// Session database parent directory (`~/.local/share/jinn`).
    ///
    /// Pass to `SqliteSessionStore::new_in()`.
    #[must_use]
    pub fn sessions_dir(&self) -> PathBuf {
        self.data_dir.join(APP_NAME)
    }

    /// Base directory for persistent browser profiles (`~/.local/share/jinn/browser-profile`).
    ///
    /// Per-mode profiles live under subdirs (`browser-profile/headless`,
    /// `browser-profile/headed`) so a single warmed headed profile is shared by
    /// both `web-fetch` and `web-search` when they select `headed-chrome`.
    /// Use [`Self::browser_profile_dir_for_mode`] to resolve a concrete path.
    #[must_use]
    pub fn browser_profile_base_dir(&self) -> PathBuf {
        self.data_dir.join(APP_NAME).join("browser-profile")
    }

    /// Persistent browser profile for a given mode (`headless` or `headed`).
    #[must_use]
    pub fn browser_profile_dir_for_mode(&self, mode: BrowserProfileMode) -> PathBuf {
        self.browser_profile_base_dir().join(mode.as_str())
    }

    /// Prompt templates directory (`~/.config/jinn/prompts`).
    #[must_use]
    pub fn prompts_dir(&self) -> PathBuf {
        self.config_dir.join(APP_NAME).join("prompts")
    }

    /// Agent skills directory (`~/.agents/skills`).
    #[must_use]
    pub fn skills_dir(&self) -> PathBuf {
        self.home_dir.join(".agents").join("skills")
    }

    /// User home directory (`~/`). Bounds the project walk (exclusive of home).
    #[must_use]
    pub fn home_dir(&self) -> &Path {
        &self.home_dir
    }

    /// Personas directory (`~/.config/jinn/personas`).
    #[must_use]
    pub fn personas_dir(&self) -> PathBuf {
        self.config_dir.join(APP_NAME).join("personas")
    }

    /// Workflows directory (`~/.config/jinn/workflows`).
    ///
    /// One directory per workflow, each holding a `workflow.md`. Launch-global:
    /// unlike skills and prompts, workflows are not project-scoped, so the
    /// discovery scan reads this one directory regardless of session cwd.
    #[must_use]
    pub fn workflows_dir(&self) -> PathBuf {
        self.config_dir.join(APP_NAME).join("workflows")
    }

    /// Model cache file (`~/.cache/jinn/model_cache.json`).
    #[must_use]
    pub fn cache_path(&self) -> PathBuf {
        self.cache_dir.join(APP_NAME).join("model_cache.json")
    }

    /// Provider config file (`~/.config/jinn/providers.toml`).
    #[must_use]
    pub fn config_path(&self) -> PathBuf {
        self.config_dir.join(APP_NAME).join("providers.toml")
    }

    /// Log file path (`~/.local/state/jinn/jinn.log` on Linux).
    #[must_use]
    pub fn log_path(&self) -> PathBuf {
        self.state_dir
            .join(APP_NAME)
            .join(format!("{APP_NAME}.log"))
    }

    /// App state file (`~/.local/state/jinn/state.toml` on Linux).
    #[must_use]
    pub fn state_file_path(&self) -> PathBuf {
        self.state_dir.join(APP_NAME).join("state.toml")
    }

    /// Themes directory (`~/.config/jinn/themes`).
    #[must_use]
    pub fn themes_dir(&self) -> PathBuf {
        self.config_dir.join(APP_NAME).join("themes")
    }

    /// User preferences file (`~/.config/jinn/jinn.toml`).
    #[must_use]
    pub fn preferences_path(&self) -> PathBuf {
        self.config_dir
            .join(APP_NAME)
            .join(crate::app_info::PREFS_FILE_NAME)
    }

    // -- System data directories (XDG fallback) -----------------------------

    /// System data directory (`/usr/share/jinn`).
    ///
    /// Used as a fallback source for themes, personas, and prompts
    /// when user-specific files are not found.
    #[must_use]
    pub fn system_data_dir(&self) -> &Path {
        &self.system_data_dir
    }

    /// Config directory (`~/.config/jinn`).
    #[must_use]
    pub fn app_config_dir(&self) -> PathBuf {
        self.config_dir.join(APP_NAME)
    }

    /// Base config directory (`~/.config`) — parent of all jinn config.
    #[must_use]
    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    /// Base data directory (`~/.local/share`) — parent of all jinn data.
    #[must_use]
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// App data directory (`~/.local/share/jinn`) — caches, scratch
    /// dirs, and other jinn-owned data live here.
    #[must_use]
    pub fn app_data_dir(&self) -> PathBuf {
        self.data_dir.join(APP_NAME)
    }

    /// System themes directory (`/usr/share/jinn/themes`).
    #[must_use]
    pub fn system_themes_dir(&self) -> PathBuf {
        self.system_data_dir.join("themes")
    }

    /// System prompts directory (`/usr/share/jinn/prompts`).
    #[must_use]
    pub fn system_prompts_dir(&self) -> PathBuf {
        self.system_data_dir.join("prompts")
    }
    /// System skills directory (`/usr/share/jinn/skills`).
    ///
    /// Installed by the system package. Used as a fallback source for
    /// skills when user-global skills are not found.
    #[must_use]
    pub fn system_skills_dir(&self) -> PathBuf {
        self.system_data_dir.join("skills")
    }

    /// User's models.dev reference file (`~/.cache/jinn/models.dev.json`).
    ///
    /// Written by `jinn fetch models`. Contains the full models.dev API
    /// dump used as a fallback for context window lookup.
    #[must_use]
    pub fn models_dev_user_path(&self) -> PathBuf {
        self.cache_dir.join(APP_NAME).join("models.dev.json")
    }

    /// System models.dev reference file (`/usr/share/jinn/models.dev.json`).
    ///
    /// Installed by the system package. Used as a fallback when no
    /// user-level file exists.
    #[must_use]
    pub fn models_dev_system_path(&self) -> PathBuf {
        self.system_data_dir.join("models.dev.json")
    }

    // -- Merged resource paths (system + user) ---------------------------------

    /// Returns merged theme file paths from system and user directories.
    ///
    /// System themes are listed first. If a user theme has the same filename
    /// as a system theme, the user version replaces the system one in the result.
    /// The returned list is sorted by filename stem.
    #[must_use]
    pub fn resolve_theme_paths(&self) -> Vec<(String, PathBuf)> {
        resolve_resource_paths(&self.system_themes_dir(), &self.themes_dir(), "toml")
    }

    /// Returns merged prompt template file paths from system and user directories.
    ///
    /// Same merge semantics as [`resolve_theme_paths`].
    #[must_use]
    pub fn resolve_prompt_paths(&self) -> Vec<(String, PathBuf)> {
        resolve_resource_paths(&self.system_prompts_dir(), &self.prompts_dir(), "md")
    }
}

/// Scans two directories for files with the given extension, merging by filename stem.
///
/// System files are listed first. User files with the same stem replace system ones.
/// Results are sorted by stem.
fn resolve_resource_paths(
    system_dir: &Path,
    user_dir: &Path,
    extension: &str,
) -> Vec<(String, PathBuf)> {
    use std::collections::BTreeMap;

    let mut map = BTreeMap::new();

    // System files first (lower priority).
    scan_dir_into(&mut map, system_dir, extension);
    // User files override system files of the same name.
    scan_dir_into(&mut map, user_dir, extension);

    map.into_iter().collect()
}

/// Scans a directory for files with the given extension and inserts into the map by stem.
fn scan_dir_into(
    map: &mut std::collections::BTreeMap<String, PathBuf>,
    dir: &Path,
    extension: &str,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == extension)
            && let Some(name) = path.file_stem().and_then(|s| s.to_str())
        {
            map.insert(name.to_owned(), path);
        }
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
        fn set(root: &Path) -> Self {
            let lock = PATHS_ROOT_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let previous = std::env::var_os(PATHS_ROOT_ENV);
            // SAFETY: the lock is held before any env mutation, so no other
            // test mutates or observes the environment concurrently.
            unsafe { std::env::set_var(PATHS_ROOT_ENV, root) };
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
        let root = tempfile::TempDir::new().expect("temp dir");
        let _guard = PathsRootEnvGuard::set(root.path());

        // When constructing the default AppPaths.
        let paths = AppPaths::default();

        // Then every base rebinds under the root with the new_in layout:
        // config/data/cache/state subdirs, home as the root, share for
        // system data.
        assert_eq!(paths.config_dir(), root.path().join("config"));
        assert_eq!(paths.data_dir(), root.path().join("data"));
        let cache_parent = root.path().join("cache").join("jinn");
        assert_eq!(paths.cache_path().parent(), Some(cache_parent.as_path()));
        let state_parent = root.path().join("state").join("jinn");
        assert_eq!(
            paths.state_file_path().parent(),
            Some(state_parent.as_path())
        );
        assert_eq!(paths.home_dir(), root.path());
        assert_eq!(paths.system_data_dir(), root.path().join("share"));
    }

    #[rstest::rstest]
    fn default_with_paths_root_env_matches_new_in() {
        // Given a set JINN_PATHS_ROOT.
        let root = tempfile::TempDir::new().expect("temp dir");
        let _guard = PathsRootEnvGuard::set(root.path());

        // When constructing the default and a new_in AppPaths from the
        // same root.
        let via_env = AppPaths::default();
        let via_new_in = AppPaths::new_in(root.path());

        // Then they are identical — the override is exactly the new_in
        // layout, not a variant of it.
        assert_eq!(via_env, via_new_in);
    }

    #[rstest::rstest]
    fn default_without_paths_root_env_uses_platform_dirs() {
        // Given no JINN_PATHS_ROOT in the environment (set then clear, both
        // under the lock, so the test is hermetic against ambient env).
        let root = tempfile::TempDir::new().expect("temp dir");
        let _guard = PathsRootEnvGuard::set(root.path());
        let _cleared = ClearPathsRootGuard::new();

        // When constructing the default AppPaths.
        let paths = AppPaths::default();

        // Then the system data dir keeps its platform default (the marker
        // that distinguishes the platform layout from the override layout).
        assert_eq!(paths.system_data_dir(), Path::new("/usr/share/jinn"));
    }

    /// Guard that clears [`PATHS_ROOT_ENV`] for its lifetime, restoring on
    /// drop. The PATHS_ROOT lock must already be held by the caller.
    struct ClearPathsRootGuard {
        /// The value to restore (None leaves the variable cleared).
        previous: Option<std::ffi::OsString>,
    }

    impl ClearPathsRootGuard {
        fn new() -> Self {
            let previous = std::env::var_os(PATHS_ROOT_ENV);
            // SAFETY: the caller holds PATHS_ROOT_LOCK across this guard's
            // lifetime, serializing env mutation against other tests.
            unsafe { std::env::remove_var(PATHS_ROOT_ENV) };
            Self { previous }
        }
    }

    impl Drop for ClearPathsRootGuard {
        fn drop(&mut self) {
            // The outer PathsRootEnvGuard's lock is still held here.
            restore_paths_root_env(self.previous.as_deref());
        }
    }

    #[rstest::rstest]
    fn workflows_dir_sits_beside_personas_under_config() {
        // Given an AppPaths rooted at a temp tree.
        let root = tempfile::TempDir::new().expect("temp dir");
        let paths = AppPaths::new_in(root.path());

        // When resolving the workflows dir.
        let dir = paths.workflows_dir();

        // Then it is the config dir's workflows folder, beside personas.
        assert_eq!(
            dir,
            root.path().join("config").join("jinn").join("workflows")
        );
        // And it is a sibling of the personas dir.
        assert_eq!(
            dir.parent().map(std::path::Path::to_path_buf),
            Some(
                paths
                    .personas_dir()
                    .parent()
                    .map(std::path::Path::to_path_buf)
                    .expect("parent")
            )
        );
    }

    #[rstest::rstest]
    fn resolve_theme_paths_returns_empty_when_both_dirs_missing() {
        // Given an AppPaths with nonexistent system and user dirs.
        let root = tempfile::TempDir::new().expect("temp dir");
        let paths = AppPaths::new_in(root.path());

        // When resolving theme paths.
        let result = paths.resolve_theme_paths();

        // Then the result is empty.
        assert_eq!(result, [] as [(std::string::String, std::path::PathBuf); 0]);
    }

    #[rstest::rstest]
    fn resolve_theme_paths_finds_system_files() {
        // Given an AppPaths with a system themes dir containing one theme.
        let root = tempfile::TempDir::new().expect("temp dir");
        let system_dir = root.path().join("share/themes");
        std::fs::create_dir_all(&system_dir).expect("create dir");
        std::fs::write(system_dir.join("ocean.toml"), "focus_accent = \"blue\"").expect("write");

        let paths = AppPaths::new_in(root.path());

        // When resolving theme paths.
        let result = paths.resolve_theme_paths();

        // Then the system theme is found.
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "ocean");
    }

    #[rstest::rstest]
    fn resolve_theme_paths_finds_user_files() {
        // Given an AppPaths with a user themes dir containing one theme.
        let root = tempfile::TempDir::new().expect("temp dir");
        let user_dir = root.path().join("config/jinn/themes");
        std::fs::create_dir_all(&user_dir).expect("create dir");
        std::fs::write(user_dir.join("forest.toml"), "focus_accent = \"green\"").expect("write");

        let paths = AppPaths::new_in(root.path());

        // When resolving theme paths.
        let result = paths.resolve_theme_paths();

        // Then the user theme is found.
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "forest");
    }

    #[rstest::rstest]
    fn resolve_theme_paths_user_overrides_system() {
        // Given system and user dirs both with a theme named "ocean".
        let root = tempfile::TempDir::new().expect("temp dir");

        let system_dir = root.path().join("share/themes");
        std::fs::create_dir_all(&system_dir).expect("create dir");
        std::fs::write(system_dir.join("ocean.toml"), "focus_accent = \"blue\"").expect("write");

        let user_dir = root.path().join("config/jinn/themes");
        std::fs::create_dir_all(&user_dir).expect("create dir");
        std::fs::write(user_dir.join("ocean.toml"), "focus_accent = \"red\"").expect("write");

        let paths = AppPaths::new_in(root.path());

        // When resolving theme paths.
        let result = paths.resolve_theme_paths();

        // Then only one "ocean" entry exists (user version).
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "ocean");
        // And it points to the user directory.
        assert!(result[0].1.starts_with(user_dir));
    }

    #[rstest::rstest]
    fn resolve_theme_paths_merges_both_dirs() {
        // Given system has "ocean", user has "forest".
        let root = tempfile::TempDir::new().expect("temp dir");

        let system_dir = root.path().join("share/themes");
        std::fs::create_dir_all(&system_dir).expect("create dir");
        std::fs::write(system_dir.join("ocean.toml"), "focus_accent = \"blue\"").expect("write");

        let user_dir = root.path().join("config/jinn/themes");
        std::fs::create_dir_all(&user_dir).expect("create dir");
        std::fs::write(user_dir.join("forest.toml"), "focus_accent = \"green\"").expect("write");

        let paths = AppPaths::new_in(root.path());

        // When resolving theme paths.
        let result = paths.resolve_theme_paths();

        // Then both themes are found, sorted by name.
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].0, "forest");
        assert_eq!(result[1].0, "ocean");
    }

    #[rstest::rstest]
    fn resolve_prompt_paths_uses_md_extension() {
        // Given a system prompts dir with a .md file.
        let root = tempfile::TempDir::new().expect("temp dir");
        let system_dir = root.path().join("share/prompts");
        std::fs::create_dir_all(&system_dir).expect("create dir");
        std::fs::write(system_dir.join("example.md"), "content").expect("write");

        let paths = AppPaths::new_in(root.path());

        // When resolving prompt paths.
        let result = paths.resolve_prompt_paths();

        // Then the .md file is found.
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "example");
    }

    #[rstest::rstest]
    fn default_system_data_dir_is_usr_share() {
        // Given a default AppPaths, constructed with no ambient override —
        // the lock is held so a parallel test's transient JINN_PATHS_ROOT
        // cannot leak into this construction.
        let root = tempfile::TempDir::new().expect("temp dir");
        let _guard = PathsRootEnvGuard::set(root.path());
        let _cleared = ClearPathsRootGuard::new();
        let paths = AppPaths::default();

        // When reading the system data dir.
        let system_data_dir = paths.system_data_dir();

        // Then it is /usr/share/jinn.
        assert_eq!(system_data_dir, Path::new("/usr/share/jinn"));
    }

    #[rstest::rstest]
    fn new_in_system_data_dir_is_under_root() {
        // Given an AppPaths created with new_in.
        let root = tempfile::TempDir::new().expect("temp dir");
        let paths = AppPaths::new_in(root.path());

        // When reading the system data dir.
        let system_data_dir = paths.system_data_dir();

        // Then it is root/share.
        assert_eq!(system_data_dir, root.path().join("share"));
    }

    #[rstest::rstest]
    fn default_system_skills_dir_is_usr_share() {
        // Given a default AppPaths, constructed with no ambient override —
        // the lock is held so a parallel test's transient JINN_PATHS_ROOT
        // cannot leak into this construction.
        let root = tempfile::TempDir::new().expect("temp dir");
        let _guard = PathsRootEnvGuard::set(root.path());
        let _cleared = ClearPathsRootGuard::new();
        let paths = AppPaths::default();

        // When reading the system skills dir.
        let system_skills_dir = paths.system_skills_dir();

        // Then it is /usr/share/jinn/skills.
        assert_eq!(system_skills_dir, Path::new("/usr/share/jinn/skills"));
    }

    #[rstest::rstest]
    fn new_in_system_skills_dir_is_under_root() {
        // Given an AppPaths created with new_in.
        let root = tempfile::TempDir::new().expect("temp dir");
        let paths = AppPaths::new_in(root.path());

        // When reading the system skills dir.
        let system_skills_dir = paths.system_skills_dir();

        // Then it is root/share/skills.
        assert_eq!(system_skills_dir, root.path().join("share/skills"));
    }

    #[rstest::rstest]
    fn log_path_under_state_dir_in_new_in() {
        // Given an AppPaths rooted in a temp dir.
        let root = tempfile::TempDir::new().expect("temp dir");
        let paths = AppPaths::new_in(root.path());

        // When reading the log path.
        let log_path = paths.log_path();

        // Then it is root/state/jinn/jinn.log.
        assert_eq!(
            log_path,
            root.path().join("state").join("jinn").join("jinn.log")
        );
    }

    #[rstest::rstest]
    fn state_file_path_under_state_dir_in_new_in() {
        // Given an AppPaths rooted in a temp dir.
        let root = tempfile::TempDir::new().expect("temp dir");
        let paths = AppPaths::new_in(root.path());

        // When reading the state file path.
        let state_file_path = paths.state_file_path();

        // Then it is root/state/jinn/state.toml.
        assert_eq!(
            state_file_path,
            root.path().join("state").join("jinn").join("state.toml")
        );
    }

    #[rstest::rstest]
    fn log_path_filename_is_app_name_dot_log() {
        // Given an AppPaths.
        let root = tempfile::TempDir::new().expect("temp dir");
        let paths = AppPaths::new_in(root.path());

        // When reading the filename component of the log path.
        let log_path = paths.log_path();
        let filename = log_path.file_name().and_then(|s| s.to_str());

        // Then it is jinn.log.
        assert_eq!(filename, Some("jinn.log"));
    }

    #[rstest::rstest]
    fn browser_profile_dir_for_mode_lands_under_data_base() {
        // Given an AppPaths rooted at a temp dir.
        let root = tempfile::TempDir::new().expect("temp dir");
        let paths = AppPaths::new_in(root.path());

        // When resolving the headed profile dir.
        let headed = paths.browser_profile_dir_for_mode(BrowserProfileMode::Headed);
        let headless = paths.browser_profile_dir_for_mode(BrowserProfileMode::Headless);

        // Then both land under data/jinn/browser-profile/<mode>.
        assert_eq!(headed, root.path().join("data/jinn/browser-profile/headed"));
        assert_eq!(
            headless,
            root.path().join("data/jinn/browser-profile/headless")
        );
    }
}
