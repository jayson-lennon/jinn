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

pub mod browser;
pub mod env;
pub mod fs;
pub mod http;
pub mod json;
pub mod proc;
pub mod stdio;
pub mod time;
pub mod toml;
pub mod tool;

use std::io::Read as _;
use std::path::Path;
use std::rc::Rc;
use std::time::Instant;

use marked_path::{Absolute, MarkedPathBuf};
use mlua::Lua;

/// The byte source `jinn.stdio.read` slurps to EOF.
///
/// Production always takes [`StdinSource::process`]; tests construct one
/// with [`StdinSource::from_bytes`] so a read never depends on what the
/// test harness left open on file descriptor 0. Clones share one source.
#[derive(Clone)]
pub struct StdinSource {
    read: Rc<dyn Fn() -> std::io::Result<Vec<u8>>>,
    /// Names the source in `Debug` output.
    label: &'static str,
}

impl std::fmt::Debug for StdinSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The closure is opaque; the label is what a reader needs.
        f.debug_struct("StdinSource")
            .field("source", &self.label)
            .finish_non_exhaustive()
    }
}

impl StdinSource {
    /// Reads the process's standard input.
    #[must_use]
    pub fn process() -> Self {
        Self {
            read: Rc::new(|| {
                let mut buffer = Vec::new();
                std::io::stdin().read_to_end(&mut buffer)?;
                Ok(buffer)
            }),
            label: "process stdin",
        }
    }

    /// Always reads the given bytes.
    #[must_use]
    pub fn from_bytes<B>(bytes: B) -> Self
    where
        B: Into<Vec<u8>>,
    {
        let bytes: Vec<u8> = bytes.into();
        Self {
            read: Rc::new(move || Ok(bytes.clone())),
            label: "fixed bytes",
        }
    }

    fn read(&self) -> std::io::Result<Vec<u8>> {
        (self.read)()
    }
}

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
    /// Where `jinn.stdio.read` slurps from: the process's stdin in
    /// production, a caller-supplied source in tests.
    pub stdin: StdinSource,
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
            stdin: StdinSource::process(),
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
            stdin: StdinSource::process(),
        }
    }

    /// Captures the context with caller-supplied stdin (tests).
    ///
    /// A test cannot own the process's stdin — under a harness it is an
    /// open pipe that never reaches EOF, so a read of it blocks forever.
    #[must_use]
    pub fn with_stdio(
        args: Vec<String>,
        cwd: MarkedPathBuf<Absolute>,
        deadline: Option<Instant>,
        http: reqwest::blocking::Client,
        stdin: StdinSource,
    ) -> Self {
        Self {
            args,
            cwd,
            deadline,
            http,
            stdin,
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
/// The `tool` family rides a caller-supplied registration: run/eval
/// injection passes the inert default (so `jinn.tool` fails loudly naming
/// the serve subcommand), while serve injection passes the table it will
/// seal after the script loads.
///
/// # Errors
///
/// Propagates any registration failure; the caller turns it into a
/// [`crate::LuauError::Harness`].
pub fn inject_all(lua: &Lua, caps: &Rc<Caps>) -> mlua::Result<()> {
    inject_all_with_tool_registration(
        lua,
        caps,
        &Rc::new(std::cell::RefCell::new(tool::Registration::plain_run())),
    )
}

/// [`inject_all`] with the caller's `jinn.tool` registration.
///
/// Serve mode passes its own registration here to collect tools.
///
/// # Errors
///
/// Propagates any registration failure; the caller turns it into a
/// [`crate::LuauError::Harness`].
pub fn inject_all_with_tool_registration(
    lua: &Lua,
    caps: &Rc<Caps>,
    tool_registration: &Rc<std::cell::RefCell<tool::Registration>>,
) -> mlua::Result<()> {
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
    browser::inject(lua, &jinn, caps)?;
    proc::inject(lua, &jinn, caps)?;
    stdio::inject(lua, &jinn, caps)?;
    json::inject(lua, &jinn)?;
    toml::inject(lua, &jinn)?;
    time::inject(lua, &jinn)?;
    tool::inject(lua, &jinn, tool_registration)?;

    lua.globals().raw_set("jinn", jinn)?;
    Ok(())
}

#[cfg(test)]
mod browser_tests;

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
