//! The `jinn.sleep`/`jinn.exit` capability: wait and terminate.
//!
//! `sleep` blocks the (single-threaded) script world for the given
//! milliseconds. `exit` flushes stdout and stderr and terminates the
//! process immediately — it does not unwind back into the script, even
//! from inside a `pcall`; that is documented behavior, the one capability
//! that never returns.
//!
//! The exit-code mapping itself lives in [`exit_code_from_result`] (and
//! is tested there), so the process-terminating closure stays a thin
//! flush-and-exit.

use std::io::Write as _;
use std::time::Duration;

use mlua::Lua;
use mlua::Table;
use mlua::Value;

/// Registers `jinn.sleep` and `jinn.exit`.
///
/// # Errors
///
/// Propagates any registration failure (harness-side, not script-side).
pub fn inject(lua: &Lua, jinn: &Table) -> mlua::Result<()> {
    jinn.raw_set(
        "sleep",
        lua.create_function(|_lua, millis: u64| {
            std::thread::sleep(Duration::from_millis(millis));
            Ok(())
        })?,
    )?;

    #[expect(
        clippy::exit,
        reason = "jinn.exit IS the script's process exit: the code must reach the shell, and no unwinding back into the script is wanted (documented behavior)"
    )]
    jinn.raw_set(
        "exit",
        lua.create_function(|_lua, code: Option<i32>| -> mlua::Result<()> {
            let _ = std::io::stdout().flush();
            let _ = std::io::stderr().flush();
            std::process::exit(code.unwrap_or_default());
        })?,
    )?;

    Ok(())
}

/// The exit code a completed script maps to.
///
/// An integer chunk return is the script's chosen code; any other return
/// (including no return) is success.
#[must_use]
pub fn exit_code_from_result(value: &Value) -> i32 {
    match value {
        Value::Integer(code) => i32::try_from(*code).unwrap_or_default(),
        _ => 0,
    }
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
    fn sleep_returns_after_the_requested_interval() {
        // Given a VM with the time family registered.
        let lua = Lua::new();
        let jinn = lua.create_table().expect("table");
        inject(&lua, &jinn).expect("injects");
        let sleep: mlua::Function = jinn.get("sleep").expect("sleep");
        let started = std::time::Instant::now();

        // When sleeping 20ms.
        sleep.call::<()>(20).expect("sleeps");

        // Then at least that interval elapsed.
        assert!(started.elapsed() >= Duration::from_millis(20));
    }

    #[rstest::rstest]
    #[test]
    fn integer_chunk_return_maps_to_its_code() {
        // Given a chunk returning 3.
        let value = Value::Integer(3);

        // When mapping the exit code.
        // Then it is 3.
        assert_eq!(exit_code_from_result(&value), 3);
    }

    #[rstest::rstest]
    #[test]
    fn nil_and_non_integer_returns_map_to_success() {
        // Given a nil return and a string return.
        let nil = Value::Nil;
        let string = Value::String(lua_string("done"));

        // When mapping each exit code.
        // Then both map to success.
        assert_eq!(exit_code_from_result(&nil), 0);
        assert_eq!(exit_code_from_result(&string), 0);
    }

    #[rstest::rstest]
    #[test]
    fn an_out_of_range_integer_return_floors_at_the_default() {
        // Given a chunk returning a value wider than an i32 exit code.
        let value = Value::Integer(i64::from(i32::MAX) + 1);

        // When mapping the exit code.
        // Then it saturates to the default rather than panicking.
        assert_eq!(exit_code_from_result(&value), 0);
    }

    fn lua_string(text: &str) -> mlua::LuaString {
        let lua = Lua::new();
        lua.create_string(text).expect("string")
    }
}
