//! The `jinn.env` capability: environment variable get/set.
//!
//! Absent variables read back `nil` (the Lua representation of "not
//! set"); `set` writes into the process environment. `std::env::set_var`
//! is `unsafe` in edition 2024 — the safety contract is that the script
//! world runs single-threaded on the main thread and nothing else reads
//! the environment concurrently during script execution.

use mlua::Lua;
use mlua::Table;

/// Registers `jinn.env.get/set`.
///
/// # Errors
///
/// Propagates any registration failure (harness-side, not script-side).
pub fn inject(lua: &Lua, jinn: &Table) -> mlua::Result<()> {
    let family = lua.create_table()?;

    family.raw_set(
        "get",
        lua.create_function(|_lua, key: String| Ok(std::env::var(key).ok()))?,
    )?;

    family.raw_set(
        "set",
        lua.create_function(|_lua, (key, value): (String, String)| {
            // SAFETY: the script world is single-threaded on the main
            // thread and no other thread reads the environment while a
            // script executes (see the module doc).
            unsafe { std::env::set_var(key, value) };
            Ok(())
        })?,
    )?;

    jinn.raw_set("env", family)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unwrap_used,
        reason = "test assertions need expect/panic to state what went wrong"
    )]

    use super::*;

    /// A VM with the env family injected.
    fn env_vm() -> Lua {
        let lua = Lua::new();
        inject(&lua, &lua.globals()).expect("injects");
        lua
    }

    fn env_family(lua: &Lua, name: &str) -> mlua::Function {
        lua.globals()
            .get::<mlua::Table>("env")
            .expect("env")
            .get(name)
            .expect("family function")
    }

    #[rstest::rstest]
    #[test]
    fn get_returns_a_set_variable() {
        // Given a VM with the env family and a variable this test owns.
        let lua = env_vm();
        let key = "JINN_LUAU_TEST_SET_VAR";
        // SAFETY: single-threaded test process; the key name is test-unique.
        unsafe { std::env::set_var(key, "present") };
        let get = env_family(&lua, "get");

        // When getting it.
        let value: String = get.call(key).expect("gets");

        // Then the value comes back.
        assert_eq!(value, "present");
    }

    #[rstest::rstest]
    #[test]
    fn get_returns_nil_for_an_absent_variable() {
        // Given a VM with the env family.
        let lua = env_vm();
        let get = env_family(&lua, "get");

        // When getting a variable no test sets.
        let value: Option<String> = get.call("JINN_LUAU_TEST_DEFINITELY_ABSENT").expect("gets");

        // Then nothing comes back.
        assert!(value.is_none());
    }

    #[rstest::rstest]
    #[test]
    fn set_then_get_roundtrips() {
        // Given a VM with the env family.
        let lua = env_vm();
        let key = "JINN_LUAU_TEST_ROUNDTRIP";

        // When setting then getting a variable.
        let set = env_family(&lua, "set");
        set.call::<()>((key, "written-by-luau")).expect("sets");
        let get = env_family(&lua, "get");
        let value: String = get.call(key).expect("gets");

        // Then the written value reads back.
        assert_eq!(value, "written-by-luau");
    }
}
