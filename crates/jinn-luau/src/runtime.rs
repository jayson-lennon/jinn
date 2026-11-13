//! VM construction for the `jinn luau` runtime.
//!
//! One function, [`build`], assembles the sandboxed Luau state a script
//! runs in: mlua's Luau sandbox (standard library minus `io` and the
//! mutating `os` calls), the fixed memory cap, the wall-clock interrupt,
//! and the `jinn` capability table. The chunk is then loaded under the
//! caller's chunk name — a script's absolute path, so file-based `require`
//! resolves siblings from the script's directory.
//!
//! The interrupt is the only budget mechanism mlua's Luau build exposes
//! (debug hooks are compiled out under the `luau` feature): it fires on the
//! VM's instruction count, and returning an error from the callback unwinds
//! the script.

use std::rc::Rc;
use std::time::Instant;

use mlua::Lua;
use mlua::VmState;

use crate::LuauError;
use crate::caps::Caps;

/// The fixed memory cap: 512 MiB of Luau heap.
///
/// Generous by design — it kills runaway allocation, not legitimate work.
/// Not configurable: the guardrail exists so an agent-written script cannot
/// take the host down, and a knob would only invite tuning what nothing
/// should tune.
const MEMORY_CAP_BYTES: usize = 512 * 1024 * 1024;

/// The interrupt sentinel. The interrupt callback raises this message on
/// budget expiry and the exit mapper matches it — never string-match a user
/// error against it.
pub(crate) const TIMEOUT_SENTINEL: &str = "jinn-luau-timeout";

/// Builds the sandboxed VM with every capability injected.
///
/// # Errors
///
/// Returns [`LuauError::Harness`] when the sandbox, the memory cap, the
/// interrupt, or any capability registration fails — all setup, never the
/// script.
pub fn build(caps: &Rc<Caps>) -> Result<Lua, LuauError> {
    let lua = Lua::new();

    lua.sandbox(true)
        .map_err(|error| harness(format!("cannot sandbox the Luau VM: {error}")))?;
    lua.set_memory_limit(MEMORY_CAP_BYTES)
        .map_err(|error| harness(format!("cannot set the memory cap: {error}")))?;

    if let Some(deadline) = caps.deadline {
        install_deadline_interrupt(&lua, deadline);
    }

    crate::caps::inject_all(&lua, caps)
        .map_err(|error| harness(format!("cannot register the script capabilities: {error}")))?;

    Ok(lua)
}

/// Loads a chunk under its name and executes it, returning the chunk's
/// return value.
///
/// The chunk name decides both where error messages point and where
/// file-based `require` resolves, so it must be the absolute script path
/// (with mlua's `@` file prefix) or the `=(eval)` sentinel. Loading uses
/// the eval-style path (`Chunk::eval`), so an expression chunk (`return
/// 3`) yields its value and a statement chunk yields its returns.
///
/// # Errors
///
/// Propagates [`mlua::Error`] variants; the caller maps them through
/// [`crate::commands::script_error`].
pub fn execute(lua: &Lua, chunk: &crate::Chunk) -> Result<mlua::Value, mlua::Error> {
    lua.load(chunk.source.as_str())
        .set_name(chunk.name.as_str())
        .eval::<mlua::Value>()
}

/// The harness error constructor: every `build` failure is the harness's,
/// and one constructor keeps the variant's `sentence` the only wording path.
fn harness(sentence: String) -> LuauError {
    LuauError::Harness { sentence }
}

/// Installs the wall-clock interrupt closure.
///
/// The callback runs on the VM's instruction-count tick; past the deadline
/// it returns a runtime error carrying [`TIMEOUT_SENTINEL`], which unwinds
/// the script and surfaces as [`mlua::Error::RuntimeError`]. Before the
/// deadline it lets the VM continue.
fn install_deadline_interrupt(lua: &Lua, deadline: Instant) {
    lua.set_interrupt(move |_lua| {
        if Instant::now() >= deadline {
            Err(mlua::Error::runtime(TIMEOUT_SENTINEL))
        } else {
            Ok(VmState::Continue)
        }
    });
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        reason = "test assertions need expect/panic to state what went wrong"
    )]

    use std::time::Duration;

    use super::*;
    use crate::Timeout;

    /// A `Caps` with no deadline, built against a temp root cwd.
    fn caps_no_deadline() -> Rc<Caps> {
        Rc::new(Caps::with_http(
            Vec::new(),
            jinn_testutil::tempdir().path.clone(),
            None,
            reqwest::blocking::Client::new(),
        ))
    }

    #[rstest::rstest]
    #[test]
    fn build_produces_a_sandboxed_vm_with_the_jinn_table() {
        // Given a fresh Caps.
        let caps = caps_no_deadline();

        // When building the VM.
        let lua = build(&caps).expect("builds");

        // Then the sandbox stripped `io` and the `jinn` table is present.
        assert!(
            lua.globals()
                .get::<Option<mlua::Value>>("io")
                .expect("get")
                .is_none()
        );
        assert!(
            lua.globals()
                .get::<Option<mlua::Value>>("jinn")
                .expect("get")
                .is_some()
        );
    }

    #[rstest::rstest]
    #[test]
    fn sandbox_keeps_print_and_require_available() {
        // Given a built VM.
        let caps = caps_no_deadline();
        let lua = build(&caps).expect("builds");

        // When reading the standard-library globals the contract promises.
        // Then `print` and `require` are still there.
        assert!(
            lua.globals()
                .get::<Option<mlua::Value>>("print")
                .expect("get")
                .is_some()
        );
        assert!(
            lua.globals()
                .get::<Option<mlua::Value>>("require")
                .expect("get")
                .is_some()
        );
    }

    #[rstest::rstest]
    #[test]
    fn a_runaway_loop_dies_at_the_deadline() {
        // Given a VM built with a one-second deadline.
        let dir = jinn_testutil::tempdir();
        let caps = Rc::new(Caps::with_http(
            Vec::new(),
            dir.path.clone(),
            Timeout::Secs(1)
                .seconds()
                .map(|s| Instant::now() + Duration::from_secs(s)),
            reqwest::blocking::Client::new(),
        ));
        let lua = build(&caps).expect("builds");
        let chunk = crate::Chunk {
            source: "while true do end".to_owned(),
            name: "=(eval)".to_owned(),
        };

        // When executing an infinite loop.
        let result = execute(&lua, &chunk);

        // Then it unwinds with the timeout sentinel, not a hang.
        let error = result.expect_err("loop must die");
        let mlua::Error::RuntimeError(message) = error else {
            panic!("expected RuntimeError, got {error:?}");
        };
        assert!(message.contains(TIMEOUT_SENTINEL), "got {message:?}");
    }

    #[rstest::rstest]
    #[test]
    fn an_allocation_hog_dies_at_the_memory_cap() {
        // Given a VM at the fixed cap.
        let caps = caps_no_deadline();
        let lua = build(&caps).expect("builds");
        let chunk = crate::Chunk {
            // Integer pushes grow the table fast without multi-megabyte
            // memcpy churn, so the cap fires in about a second.
            source: "local t = {} while true do t[#t+1] = 1 end".to_owned(),
            name: "=(eval)".to_owned(),
        };

        // When executing an unbounded allocation loop.
        let result = execute(&lua, &chunk);

        // Then it dies with the memory error.
        assert!(matches!(result, Err(mlua::Error::MemoryError(_))));
    }

    #[rstest::rstest]
    #[test]
    fn execute_returns_the_chunk_value() {
        // Given a built VM and a chunk returning an integer.
        let caps = caps_no_deadline();
        let lua = build(&caps).expect("builds");
        let chunk = crate::Chunk {
            source: "return 7".to_owned(),
            name: "=(eval)".to_owned(),
        };

        // When executing it.
        let value = execute(&lua, &chunk).expect("executes");

        // Then the integer comes back.
        assert_eq!(integer_from_lossy(&value), Some(7));
    }

    /// Reads an integer out of a returned value, or nothing.
    fn integer_from_lossy(value: &mlua::Value) -> Option<i64> {
        match value {
            mlua::Value::Integer(n) => Some(*n),
            mlua::Value::Number(f) if f.fract() == 0.0 => Some(*f as i64),
            _ => None,
        }
    }
}
