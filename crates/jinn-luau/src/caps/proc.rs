//! The `jinn.proc` capability: run a child process, no shell involved.
//!
//! `jinn.proc.run{program=…, args={…}}` spawns the program directly via
//! `std::process::Command` — no shell expansion, no `cmd /c`, nothing but
//! the named binary — with the child's stdin closed, and returns
//! `{status, stdout, stderr}`. stdout/stderr are lossy-UTF8 (a script
//! capturing binary output gets a readable approximation); a child killed
//! by a signal (no exit code exists) reports `status = -1`.
//!
//! `current_dir` optionally runs the child in another directory.

use marked_path::MarkedPathAccess as _;
use std::rc::Rc;

use mlua::Lua;
use mlua::Table;

use crate::caps::Caps;

/// Registers `jinn.proc.run`.
///
/// # Errors
///
/// Propagates any registration failure (harness-side, not script-side).
pub fn inject(lua: &Lua, jinn: &Table, caps: &Rc<Caps>) -> mlua::Result<()> {
    let run_caps = Rc::clone(caps);
    let family = lua.create_table()?;
    family.raw_set(
        "run",
        lua.create_function(move |lua, request: Table| run_process(lua, &run_caps, &request))?,
    )?;
    jinn.raw_set("proc", family)?;
    Ok(())
}

/// Runs one process request table.
fn run_process(lua: &Lua, caps: &Rc<Caps>, request: &Table) -> mlua::Result<Table> {
    let program: String = request.get("program")?;
    let args: Vec<String> = match request.get::<Option<Table>>("args")? {
        Some(args) => args
            .sequence_values::<String>()
            .collect::<mlua::Result<_>>()
            .map_err(|error| {
                mlua::Error::runtime(format!("`args` must be an array of strings: {error}"))
            })?,
        None => Vec::new(),
    };
    let cwd: Option<String> = request.get("current_dir").ok();

    let mut command = std::process::Command::new(&program);
    command.args(args).stdin(std::process::Stdio::null());
    if let Some(cwd) = cwd {
        let resolved = caps.resolve_path(&cwd);
        command.current_dir(resolved.as_path());
    }

    let output = command
        .output()
        .map_err(|error| mlua::Error::runtime(format!("cannot run `{program}`: {error}")))?;

    let result = lua.create_table()?;
    result.raw_set(
        "status",
        mlua::Integer::from(output.status.code().unwrap_or(-1)),
    )?;
    result.raw_set(
        "stdout",
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )?;
    result.raw_set(
        "stderr",
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unwrap_used,
        reason = "test assertions need expect/panic to state what went wrong"
    )]

    use std::rc::Rc;

    use super::*;
    use crate::caps::Caps;

    /// A VM with the proc family injected, against a fresh temp cwd. The
    /// family lands on `globals().proc` here (the test passes `globals()`
    /// as the registration table) so `run_fn` can fetch `proc.run`.
    fn proc_vm() -> (Lua, Rc<Caps>, jinn_testutil::temp::JinnTempDir) {
        // The Caps constructor builds a reqwest blocking client, which
        // panics without the process-default TLS provider.
        crate::install_crypto_provider();
        let dir = jinn_testutil::tempdir();
        let caps = Rc::new(Caps::with_http(
            Vec::new(),
            dir.path.clone(),
            None,
            reqwest::blocking::Client::new(),
        ));
        let lua = Lua::new();
        inject(&lua, &lua.globals(), &caps).expect("injects");
        (lua, caps, dir)
    }

    /// A request table for one program.
    fn request_table(lua: &Lua, program: &str, args: &[&str]) -> Table {
        let table = lua.create_table().expect("table");
        table.raw_set("program", program).expect("program");
        let args_table = lua.create_table().expect("args");
        for (index, arg) in args.iter().enumerate() {
            args_table
                .raw_set(index as mlua::Integer + 1, *arg)
                .expect("arg");
        }
        table.raw_set("args", args_table).expect("args");
        table
    }

    /// The `jinn.proc.run` function out of a proc VM. (The `proc_vm` table
    /// registered the family under globals `proc` in this test world — see
    /// `proc_vm`, which injects into `globals()`.)
    fn run_fn(lua: &Lua) -> mlua::Function {
        lua.globals()
            .get::<mlua::Table>("proc")
            .expect("proc family")
            .get("run")
            .expect("run")
    }

    /// The platform shell invocation for one command line, portably:
    /// `sh -c` on Unix, `cmd /C` on Windows.
    fn shell_invocation(command: &str) -> (&'static str, Vec<String>) {
        #[cfg(windows)]
        {
            ("cmd", vec!["/C".to_owned(), command.to_owned()])
        }
        #[cfg(not(windows))]
        {
            ("sh", vec!["-c".to_owned(), command.to_owned()])
        }
    }

    #[rstest::rstest]
    #[test]
    fn run_captures_stdout_and_exit_code() {
        // Given a VM with the proc family.
        let (lua, _caps, _dir) = proc_vm();
        let run = run_fn(&lua);

        // When echoing a fixed word through the platform shell.
        let (program, args) = shell_invocation("echo jinn-proc-probe");
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let table = request_table(&lua, program, &arg_refs);
        let result: Table = run.call(table).expect("runs");

        // Then stdout carries the word and the exit code is zero.
        let status: mlua::Integer = result.get("status").expect("status");
        let stdout: String = result.get("stdout").expect("stdout");
        assert_eq!(status, 0);
        assert!(stdout.contains("jinn-proc-probe"), "got {stdout:?}");
    }

    #[rstest::rstest]
    #[test]
    fn run_reports_a_nonzero_exit_code() {
        // Given a VM with the proc family.
        let (lua, _caps, _dir) = proc_vm();
        let run = run_fn(&lua);

        // When running the shell with a deliberately failing command.
        let (program, args) = shell_invocation("exit 7");
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let table = request_table(&lua, program, &arg_refs);
        let result: Table = run.call(table).expect("runs");

        // Then the child's code comes back.
        let status: mlua::Integer = result.get("status").expect("status");
        assert_eq!(status, 7);
    }

    #[rstest::rstest]
    #[test]
    fn run_captures_stderr() {
        // Given a VM with the proc family.
        let (lua, _caps, _dir) = proc_vm();
        let run = run_fn(&lua);

        // When running a command that writes to stderr.
        let (program, args) = shell_invocation("echo jinn-proc-err 1>&2");
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let table = request_table(&lua, program, &arg_refs);
        let result: Table = run.call(table).expect("runs");

        // Then stderr carries the word.
        let stderr: String = result.get("stderr").expect("stderr");
        assert!(stderr.contains("jinn-proc-err"), "got {stderr:?}");
    }

    #[rstest::rstest]
    #[test]
    fn an_unspawnable_program_is_a_script_error() {
        // Given a VM with the proc family.
        let (lua, _caps, _dir) = proc_vm();
        let run = run_fn(&lua);

        // When running a program name that cannot exist.
        let table = request_table(&lua, "jinn-no-such-binary-xyz", &[]);
        let result: mlua::Result<Table> = run.call(table);

        // Then the error names the program, without a panic.
        let error = result.expect_err("unspawnable");
        assert!(
            error.to_string().contains("jinn-no-such-binary-xyz"),
            "got {error}"
        );
    }
}
