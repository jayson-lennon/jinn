//! Shared application constants and utilities.
//!
//! Foundational helpers used across multiple jinn crates:
//!
//! - **`app_info`** — application identity constants (`APP_NAME`, `PREFS_FILE_NAME`).
//! - **`app_paths`** — well-known filesystem paths (config dir, data dir, etc.).
//! - **`fs_paths`** — canonicalization that strips Windows verbatim `\\?\`
//!   prefixes, so canonicalized paths stay comparable, storable, and
//!   displayable.
//! - **`shell`** — the one shared shell resolver (Git Bash preferred on
//!   Windows, bare `bash` via `PATH` on Unix) used by the bash tool and the
//!   session-lifecycle command runner.
//! - **`toml_patch`** — comment-preserving TOML document patcher for user-editable
//!   config files.
//! - **`process_isolation` / `process_kill`** — spawning children in their own
//!   process group, and terminating a child with its whole descendant tree.
//! - **`system_resource`** — loading a system resource file from a user-override
//!   directory with a system-default fallback.

pub mod app_info;
pub mod app_paths;
pub mod fs_paths;
pub mod path_display;
pub mod process_isolation;

pub use fs_paths::{canonicalize_display, strip_verbatim_prefix};
pub use path_display::shorten_path;
pub mod process_kill;
pub mod shell;
pub mod system_resource;
pub mod template_check;
pub mod toml_patch;
