//! The `jinn.stdio` capability: stdin read and stderr write.
//!
//! `print` stays the vendored Luau builtin (stdout, newline-terminated);
//! `jinn.stdio.read` slurps piped stdin to EOF (scripts are batch
//! programs — there is no TTY detection), and `jinn.stdio.eprint` writes
//! one line to stderr with a flush.
//!
//! The functions go through `std::io::stdin`/`stderr` so tests can
//! exercise them; production callers get the same process streams the
//! `jinn luau` process owns.

use std::io::Read as _;
use std::io::Write as _;

use mlua::Lua;
use mlua::Table;

/// Registers `jinn.stdio.read/eprint`.
///
/// # Errors
///
/// Propagates any registration failure (harness-side, not script-side).
pub fn inject(lua: &Lua, jinn: &Table) -> mlua::Result<()> {
    let family = lua.create_table()?;

    family.raw_set(
        "read",
        lua.create_function(|_lua, ()| {
            let mut buffer = Vec::new();
            std::io::stdin()
                .read_to_end(&mut buffer)
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

    #[rstest::rstest]
    #[test]
    fn eprint_writes_one_line_to_stderr() {
        // Given a VM with the stdio family.
        let lua = Lua::new();
        inject(&lua, &lua.globals()).expect("injects");
        let eprint: mlua::Function = lua
            .globals()
            .get::<mlua::Table>("stdio")
            .expect("stdio")
            .get("eprint")
            .expect("eprint");

        // When calling eprint (writes to the process stderr; the observable
        // contract is that it succeeds and terminates the line).
        eprint.call::<()>("visible on stderr").expect("writes");

        // Then the call succeeded — the writeln+flush path completed.
        // (Capturing the process stderr from inside the test process is
        // not portable; the integration test asserts the bytes end to end.)
    }

    #[rstest::rstest]
    #[test]
    fn stdio_read_is_registered_and_callable_with_no_piped_input() {
        // Given a VM with the stdio family and a test process whose stdin
        // is whatever the harness provides.
        let lua = Lua::new();
        inject(&lua, &lua.globals()).expect("injects");
        let read: mlua::Function = lua
            .globals()
            .get::<mlua::Table>("stdio")
            .expect("stdio")
            .get("read")
            .expect("read");

        // When calling read.
        let result: mlua::Result<String> = read.call(());

        // Then it completes (an empty or harness-fed read), never panics.
        let _ = result;
    }
}
