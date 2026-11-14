//! The script-facing capability context and its injection.
//!
//! [`Caps`] is the context every `jinn.*` family reads from: the argv the
//! caller passed after `--`, the process cwd captured once at startup (a
//! relative `jinn.fs` path resolves against it), and an optional
//! wall-clock deadline (mirrored into the VM interrupt by
//! [`crate::runtime::build`]).
//!
//! Each family module below defines one `inject` function registering its
//! closures on the `jinn` table; [`inject_all`] runs them in one pass.
//! Registration failures are harness errors, never script errors — the
//! surface is fixed at build time.
//!
//! The closures share the context by cloning the `Rc<Caps>`: script
//! execution is single-threaded and mlua's `create_function` accepts
//! non-`Send` captures, so no locking exists anywhere on this path.

pub mod env;
pub mod fs;
pub mod http;
pub mod json;
pub mod proc;
pub mod stdio;
pub mod time;
pub mod toml;

use std::path::Path;
use std::rc::Rc;
use std::time::Instant;

use marked_path::{Absolute, MarkedPathBuf};
use mlua::Lua;

/// The script-facing execution context.
#[derive(Debug)]
pub struct Caps {
    /// The argv after `--`, verbatim.
    pub args: Vec<String>,
    /// The process cwd captured once; relative fs paths join against it.
    pub cwd: MarkedPathBuf<Absolute>,
    /// The wall-clock deadline, when a timeout applies.
    pub deadline: Option<Instant>,
    /// The blocking HTTP client the `jinn.http` family shares.
    pub http: reqwest::blocking::Client,
}

impl Caps {
    /// Captures the context for one script run, building the HTTP client
    /// behind a shared-once wrapper (reqwest's blocking client spawns its
    /// event-loop thread in the constructor, which panics without a TLS
    /// provider installed — the provider is installed by `main` in the
    /// binary and by each test before any `Caps::new`).
    #[must_use]
    pub fn new(args: Vec<String>, cwd: MarkedPathBuf<Absolute>, deadline: Option<Instant>) -> Self {
        Self {
            args,
            cwd,
            deadline,
            http: reqwest::blocking::Client::new(),
        }
    }

    /// Captures the context with a caller-supplied HTTP client (tests).
    #[must_use]
    pub fn with_http(
        args: Vec<String>,
        cwd: MarkedPathBuf<Absolute>,
        deadline: Option<Instant>,
        http: reqwest::blocking::Client,
    ) -> Self {
        Self {
            args,
            cwd,
            deadline,
            http,
        }
    }

    /// Resolves a script-supplied path against the captured cwd.
    ///
    /// An absolute path is taken as-is (the script may name any file — the
    /// surface is deliberately maximal); a relative path joins the cwd.
    #[must_use]
    pub fn resolve_path(&self, supplied: &str) -> MarkedPathBuf<Absolute> {
        resolve_path_against(&self.cwd, supplied)
    }
}

/// Resolves a script-supplied path string against a base directory.
///
/// Split from [`Caps::resolve_path`] so the conversion seam stays testable
/// without a full context. A path that cannot be typed as absolute (an
/// impossible encoding on the current platform) falls back to the base
/// rather than panicking mid-script.
#[must_use]
pub fn resolve_path_against(
    base: &MarkedPathBuf<Absolute>,
    supplied: &str,
) -> MarkedPathBuf<Absolute> {
    let raw = Path::new(supplied);
    if raw.is_absolute() {
        return MarkedPathBuf::<Absolute>::new(raw).unwrap_or_else(|_| base.clone());
    }
    base.join(supplied).unwrap_or_else(|_| base.clone())
}

/// Registers every `jinn.*` capability family on a fresh `jinn` global.
///
/// The table is created here and passed to each family's `inject`; the
/// families never touch the global environment directly, so the sandbox's
/// globals proxy stays the only writable surface.
///
/// # Errors
///
/// Propagates any registration failure; the caller turns it into a
/// [`crate::LuauError::Harness`].
pub fn inject_all(lua: &Lua, caps: &Rc<Caps>) -> mlua::Result<()> {
    let jinn = lua.create_table()?;

    // jinn.args: the argv after `--`, as a 1-indexed array of strings.
    let args = lua.create_table()?;
    for (index, arg) in caps.args.iter().enumerate() {
        args.raw_set(index as mlua::Integer + 1, arg.as_str())?;
    }
    jinn.raw_set("args", args)?;

    fs::inject(lua, &jinn, caps)?;
    env::inject(lua, &jinn)?;
    http::inject(lua, &jinn, caps)?;
    proc::inject(lua, &jinn, caps)?;
    stdio::inject(lua, &jinn)?;
    json::inject(lua, &jinn)?;
    toml::inject(lua, &jinn)?;
    time::inject(lua, &jinn)?;

    lua.globals().raw_set("jinn", jinn)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        reason = "test assertions need expect/panic to state what went wrong"
    )]

    use marked_path::MarkedPathAccess as _;

    use super::*;

    #[rstest::rstest]
    #[test]
    fn absolute_paths_resolve_verbatim() {
        // Given a base directory and a script-supplied absolute path
        // (built by joining onto the temp volume root, never by literal).
        let dir = jinn_testutil::tempdir();
        let base = dir.path.clone();
        let absolute = base.join("data.txt").expect("temp root joins a name");

        // When resolving it.
        let resolved = resolve_path_against(&base, absolute.as_path().to_str().expect("utf-8"));

        // Then it comes back verbatim.
        assert_eq!(resolved, absolute);
    }

    #[rstest::rstest]
    #[test]
    fn relative_paths_join_the_base() {
        // Given a base directory.
        let dir = jinn_testutil::tempdir();
        let base = dir.path.clone();

        // When resolving a bare relative name.
        let resolved = resolve_path_against(&base, "out.txt");

        // Then it lands directly under the base.
        assert_eq!(
            resolved,
            base.join("out.txt").expect("temp root joins a name")
        );
    }

    #[rstest::rstest]
    #[test]
    fn inject_all_builds_the_jinn_table_with_every_family() {
        // Given a context and a fresh unsandboxed VM.
        crate::install_crypto_provider();
        let dir = jinn_testutil::tempdir();
        let caps = Rc::new(Caps::with_http(
            vec!["one".to_owned(), "two".to_owned()],
            dir.path.clone(),
            None,
            reqwest::blocking::Client::new(),
        ));
        let lua = Lua::new();

        // When injecting every family.
        inject_all(&lua, &caps).expect("injects");

        // Then the jinn table carries the args array and every family key.
        let args: Vec<String> = lua
            .globals()
            .get::<mlua::Table>("jinn")
            .expect("jinn table")
            .get::<mlua::Table>("args")
            .expect("args")
            .sequence_values()
            .collect::<mlua::Result<_>>()
            .expect("sequence");
        assert_eq!(args, vec!["one".to_owned(), "two".to_owned()]);
    }
}
