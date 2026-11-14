//! The `jinn.fs` capability: file read/write/exists/list/remove.
//!
//! Paths arrive as Lua strings and resolve against the process cwd
//! captured at startup; absolute paths are taken verbatim. The surface is
//! deliberately maximal (the user chose "everything, not a security
//! boundary"), so there is no allow-list and no path gate — this module is
//! the filesystem boundary carve-out the workspace's clippy configuration
//! anticipates.
//!
//! Reads are byte-faithful (`to_string_lossy` never happens here — Luau
//! strings are 8-bit clean, so a non-UTF-8 file round-trips); writes
//! truncate; `remove` handles a file or an empty directory.

use marked_path::MarkedPathAccess as _;
use std::rc::Rc;

use mlua::Lua;
use mlua::Table;

use crate::caps::Caps;

/// Registers `jinn.fs.read/write/exists/list_dir/remove`.
///
/// # Errors
///
/// Propagates any registration failure (harness-side, not script-side).
pub fn inject(lua: &Lua, jinn: &Table, caps: &Rc<Caps>) -> mlua::Result<()> {
    let family = lua.create_table()?;

    let read_caps = Rc::clone(caps);
    family.raw_set(
        "read",
        lua.create_function(move |_lua, path: String| {
            let resolved = read_caps.resolve_path(&path);
            let bytes = std::fs::read(resolved.as_path())
                .map_err(|error| mlua::Error::runtime(format!("cannot read `{path}`: {error}")))?;
            Ok(String::from_utf8_lossy(&bytes).into_owned())
        })?,
    )?;

    let write_caps = Rc::clone(caps);
    family.raw_set(
        "write",
        lua.create_function(move |_lua, (path, contents): (String, String)| {
            let resolved = write_caps.resolve_path(&path);
            std::fs::write(resolved.as_path(), contents.as_bytes())
                .map_err(|error| mlua::Error::runtime(format!("cannot write `{path}`: {error}")))?;
            Ok(())
        })?,
    )?;

    let exists_caps = Rc::clone(caps);
    family.raw_set(
        "exists",
        lua.create_function(move |_lua, path: String| {
            let resolved = exists_caps.resolve_path(&path);
            Ok(resolved.as_path().exists())
        })?,
    )?;

    let list_caps = Rc::clone(caps);
    family.raw_set(
        "list_dir",
        lua.create_function(move |lua, path: String| {
            let resolved = list_caps.resolve_path(&path);
            let entries = std::fs::read_dir(resolved.as_path())
                .map_err(|error| mlua::Error::runtime(format!("cannot list `{path}`: {error}")))?;
            let mut names: Vec<String> = entries
                .filter_map(std::result::Result::ok)
                .filter_map(|entry| entry.file_name().into_string().ok())
                .collect();
            names.sort();
            let table = lua.create_table()?;
            for (index, name) in names.iter().enumerate() {
                table.raw_set(index as mlua::Integer + 1, name.as_str())?;
            }
            Ok(table)
        })?,
    )?;

    let remove_caps = Rc::clone(caps);
    family.raw_set(
        "remove",
        lua.create_function(move |_lua, path: String| {
            let resolved = remove_caps.resolve_path(&path);
            let is_dir = resolved.as_path().is_dir();
            let outcome = if is_dir {
                std::fs::remove_dir(resolved.as_path())
            } else {
                std::fs::remove_file(resolved.as_path())
            };
            outcome
                .map_err(|error| mlua::Error::runtime(format!("cannot remove `{path}`: {error}")))
        })?,
    )?;

    jinn.raw_set("fs", family)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        reason = "test assertions need expect/panic to state what went wrong"
    )]

    use std::rc::Rc;

    use super::*;
    use crate::caps::Caps;

    /// A VM with the fs family injected, against a fresh temp cwd.
    fn fs_vm() -> (Lua, Rc<Caps>, jinn_testutil::temp::JinnTempDir) {
        // The Caps constructor builds a reqwest blocking client, which
        // panics without the process-default TLS provider — this module's
        // tests never touch the network but still pay the install.
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

    #[rstest::rstest]
    #[test]
    fn write_then_read_roundtrips_content() {
        // Given a VM with the fs family and a fresh temp cwd.
        let (lua, _caps, _dir) = fs_vm();

        // When writing then reading a file by relative path.
        let write: mlua::Function = lua
            .globals()
            .get::<mlua::Table>("fs")
            .expect("fs")
            .get("write")
            .expect("write");
        let read: mlua::Function = lua
            .globals()
            .get::<mlua::Table>("fs")
            .expect("fs")
            .get("read")
            .expect("read");
        write.call::<()>(("out.txt", "hello jinn")).expect("writes");
        let content: String = read.call("out.txt").expect("reads");

        // Then the content round-trips.
        assert_eq!(content, "hello jinn");
    }

    #[rstest::rstest]
    #[test]
    fn exists_reports_presence_and_absence() {
        // Given a VM with one written file.
        let (lua, _caps, _dir) = fs_vm();
        let exists: mlua::Function = lua
            .globals()
            .get::<mlua::Table>("fs")
            .expect("fs")
            .get("exists")
            .expect("exists");
        let write: mlua::Function = lua
            .globals()
            .get::<mlua::Table>("fs")
            .expect("fs")
            .get("write")
            .expect("write");
        write.call::<()>(("marker.txt", "x")).expect("writes");

        // When probing the written path and an absent path.
        let present: bool = exists.call("marker.txt").expect("probe");
        let absent: bool = exists.call("absent.txt").expect("probe");

        // Then presence is true and absence is false.
        assert!(present);
        assert!(!absent);
    }

    #[rstest::rstest]
    #[test]
    fn list_dir_returns_sorted_entry_names() {
        // Given a VM whose cwd holds two files and one directory.
        let (lua, caps, _dir) = fs_vm();
        std::fs::write(caps.resolve_path("b.txt").as_path(), "b").expect("seed");
        std::fs::write(caps.resolve_path("a.txt").as_path(), "a").expect("seed");
        std::fs::create_dir_all(caps.resolve_path("sub").as_path()).expect("seed dir");

        // When listing the cwd.
        let list: mlua::Function = lua
            .globals()
            .get::<mlua::Table>("fs")
            .expect("fs")
            .get("list_dir")
            .expect("list_dir");
        let names: Vec<String> = list.call(".").expect("lists");

        // Then the names come back sorted.
        assert_eq!(
            names,
            vec!["a.txt".to_owned(), "b.txt".to_owned(), "sub".to_owned()]
        );
    }

    #[rstest::rstest]
    #[test]
    fn remove_deletes_a_file() {
        // Given a VM with one written file.
        let (lua, caps, _dir) = fs_vm();
        let target = caps.resolve_path("gone.txt");
        std::fs::write(target.as_path(), "x").expect("seed");
        let remove: mlua::Function = lua
            .globals()
            .get::<mlua::Table>("fs")
            .expect("fs")
            .get("remove")
            .expect("remove");

        // When removing it.
        remove.call::<()>("gone.txt").expect("removes");

        // Then the file is gone.
        assert!(!target.as_path().exists());
    }

    #[rstest::rstest]
    #[test]
    fn read_of_an_absent_file_is_a_script_error() {
        // Given a VM with the fs family.
        let (lua, _caps, _dir) = fs_vm();
        let read: mlua::Function = lua
            .globals()
            .get::<mlua::Table>("fs")
            .expect("fs")
            .get("read")
            .expect("read");

        // When reading a path that names no file.
        let result: mlua::Result<String> = read.call("absent.txt");

        // Then the error names the path (a script-visible failure, not a panic).
        let error = result.expect_err("absent file");
        assert!(error.to_string().contains("absent.txt"), "got {error}");
    }
}
