//! The `jinn.stdio` capability: stdin read and stderr write.
//!
//! `print` stays the vendored Luau builtin (stdout, newline-terminated);
//! `jinn.stdio.read` slurps piped stdin to EOF (scripts are batch
//! programs — there is no TTY detection), and `jinn.stdio.eprint` writes
//! one line to stderr with a flush.
//!
//! The functions read and write through the context's [`StdinSource`] and
//! `std::io::stderr` respectively, so tests can exercise them with a
//! caller-owned byte source; production gets the same process streams the
//! `jinn luau` process owns.

use std::io::Write as _;

use mlua::Lua;
use mlua::Table;

use super::Caps;

/// Registers `jinn.stdio.read/eprint`.
///
/// # Errors
///
/// Propagates any registration failure (harness-side, not script-side).
pub fn inject(lua: &Lua, jinn: &Table, caps: &Caps) -> mlua::Result<()> {
    let family = lua.create_table()?;

    let stdin = caps.stdin.clone();
    family.raw_set(
        "read",
        lua.create_function(move |_lua, ()| {
            let buffer = stdin
                .read()
                .map_err(|error| mlua::Error::runtime(format!("cannot read stdin: {error}")))?;
            Ok(String::from_utf8_lossy(&buffer).into_owned())
        })?,
    )?;

    family.raw_set(
        "eprint",
        lua.create_function(|_lua, message: String| {
            let mut stderr = std::io::stderr();
            writeln!(stderr, "{message}")
                .and_then(|()| stderr.flush())
                .map_err(|error| mlua::Error::runtime(format!("cannot write stderr: {error}")))
        })?,
    )?;

    jinn.raw_set("stdio", family)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        reason = "test assertions need expect/panic to state what went wrong"
    )]

    use super::*;
    use crate::caps::StdinSource;

    /// A VM with the stdio family wired to the given stdin bytes.
    fn stdio_vm(stdin: &str) -> Lua {
        crate::install_crypto_provider();
        let dir = jinn_testutil::tempdir();
        let caps = Caps::with_stdio(
            Vec::new(),
            dir.path.clone(),
            None,
            reqwest::blocking::Client::new(),
            StdinSource::from_bytes(stdin),
        );
        let lua = Lua::new();
        inject(&lua, &lua.globals(), &caps).expect("injects");
        lua
    }

    fn family_function(lua: &Lua, name: &str) -> mlua::Function {
        lua.globals()
            .get::<mlua::Table>("stdio")
            .expect("stdio")
            .get(name)
            .expect("family function")
    }

    #[rstest::rstest]
    #[test]
    fn read_returns_empty_string_when_nothing_is_piped() {
        // Given a VM whose stdin carries no bytes.
        let lua = stdio_vm("");

        // When calling read.
        let result: mlua::Result<String> = family_function(&lua, "read").call(());

        // Then it completes with the empty string.
        assert_eq!(result.expect("reads"), "");
    }

    #[rstest::rstest]
    #[test]
    fn read_returns_the_piped_bytes() {
        // Given a VM whose stdin carries two lines.
        let lua = stdio_vm("first\nsecond\n");

        // When calling read.
        let result: mlua::Result<String> = family_function(&lua, "read").call(());

        // Then it returns them verbatim.
        assert_eq!(result.expect("reads"), "first\nsecond\n");
    }

    #[rstest::rstest]
    #[test]
    fn eprint_writes_one_line_to_stderr() {
        // Given a VM with the stdio family.
        let lua = stdio_vm("");
        let eprint = family_function(&lua, "eprint");

        // When calling eprint (writes to the process stderr; the observable
        // contract is that it succeeds and terminates the line).
        eprint.call::<()>("visible on stderr").expect("writes");

        // Then the call succeeded — the writeln+flush path completed.
        // (Capturing the process stderr from inside the test process is
        // not portable; the integration test asserts the bytes end to end.)
    }
}
