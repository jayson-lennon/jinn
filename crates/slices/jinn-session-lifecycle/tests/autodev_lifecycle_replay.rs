//! The autodev workflow's lifecycle scripts replay correctly.
//!
//! Reads `res/workflows/autodev/lifecycle.toml` from the repo root and
//! replays each script's teardown against stored setup coordinates. This is
//! the regression test for the incident where teardown rendered recorded
//! arguments positionally: the fossil script's teardown lists its
//! placeholders in a different order than its setup, and a positional
//! replay merged the wrong branch.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test code: a missing fixture is a failed test"
)]

use std::collections::{BTreeMap, BTreeSet};

use jinn_session_lifecycle_msg::command_template::CommandTemplate;
use marked_path::{Absolute, MarkedPathBuf};

/// The workspace root, resolved at run time.
///
/// `env!("CARGO_MANIFEST_DIR")` is baked in at compile time, which for a
/// cross build (Linux compiling the Windows test exes) is a POSIX path —
/// absolute on the build host but not on Windows, where absoluteness
/// requires a drive prefix — and stale wherever the checkout has moved.
/// Resolution therefore tries, in order:
///
/// 1. the runtime `CARGO_MANIFEST_DIR` cargo sets on every test process,
/// 2. the process working directory — the cross-built exes run from the
///    shared checkout, so an ancestor of it names the repo root,
/// 3. the compile-time manifest dir — correct only for a native build.
///
/// Each seed is walked upward until a directory containing the lifecycle
/// file turns up; if none does, the test fails with instructions instead of
/// a misleading path error.
fn workspace_root() -> MarkedPathBuf<Absolute> {
    const LIFECYCLE_REL: &str = "res/workflows/autodev/lifecycle.toml";
    // Seeds are path views, not owned PathBufs: the owning raw path is
    // denied workspace-wide, and the seeds are only walked for ancestors
    // before the surviving root is marked absolute below.
    let cwd = std::env::current_dir().ok();
    let manifest_dir = std::env::var_os("CARGO_MANIFEST_DIR");
    let seeds = [
        manifest_dir.as_deref().map(std::path::Path::new),
        cwd.as_deref(),
        Some(std::path::Path::new(env!("CARGO_MANIFEST_DIR"))),
    ];

    for seed in seeds.into_iter().flatten() {
        for dir in seed.ancestors() {
            if dir.join(LIFECYCLE_REL).is_file() {
                return MarkedPathBuf::<Absolute>::new(dir).expect("a seed's ancestor is absolute");
            }
        }
    }
    panic!(
        "no ancestor of CARGO_MANIFEST_DIR or the working directory contains \
         {LIFECYCLE_REL}; run the test from the repository checkout"
    )
}

/// The path of the autodev lifecycle file, under the workspace root.
fn autodev_lifecycle_path() -> MarkedPathBuf<Absolute> {
    workspace_root()
        .join("res/workflows/autodev/lifecycle.toml")
        .expect("repo-relative constant joins keep the workspace root absolute")
}

/// Reads the autodev lifecycle file with line endings normalized to `\n`.
///
/// The textual extractor below finds field boundaries by splitting on
/// quote-then-newline sequences, and a checkout written with CRLF
/// (`core.autocrlf`, or a Windows editor) puts a `\r` between them, which
/// would silently change what each field's extraction captures.
fn read_lifecycle() -> String {
    std::fs::read_to_string(autodev_lifecycle_path())
        .expect("res/workflows/autodev/lifecycle.toml exists")
        .replace("\r\n", "\n")
}

/// Extracts every `[[session_lifecycle.script]]` entry's name, setup, and
/// teardown command from the raw TOML text.
///
/// Deliberately textual: the res/ tree is data, not a schema the lifecycle
/// crate parses as a document, and a full TOML dependency for three fields
/// would buy nothing. The scripts in this file are multi-line basic strings
/// (`"""`), and the git teardown is a single-line basic string — both forms
/// are extracted, along with their shell double quotes, which is what the
/// template parser must see anyway.
struct Script {
    name: String,
    setup: String,
    teardown: String,
}

fn parse_scripts(raw: &str) -> Vec<Script> {
    let mut scripts = Vec::new();
    for chunk in raw.split("[[session_lifecycle.script]]").skip(1) {
        let name = chunk
            .split("name = \"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .unwrap_or_default()
            .to_owned();
        let field = |key: &str| -> String {
            let multi = format!("{key} = \"\"\"");
            if let Some(rest) = chunk.split(&multi).nth(1) {
                return rest.split("\"\"\"").next().unwrap_or_default().to_owned();
            }
            // Single-line basic string: everything between the quotes.
            let marker = format!("{key} = \"");
            chunk
                .split(&marker)
                .nth(1)
                .and_then(|rest| rest.split("\"\n").next())
                .unwrap_or_default()
                .to_owned()
        };
        scripts.push(Script {
            name,
            setup: field("setup_command"),
            teardown: field("teardown_command"),
        });
    }
    scripts
}

/// The placeholder names a command template uses, as a set.
fn placeholder_names(command: &str) -> BTreeSet<String> {
    CommandTemplate::parse(command)
        .params()
        .iter()
        .filter_map(|param| match param {
            jinn_session_lifecycle_msg::command_template::Param::Named(name) => Some(name.clone()),
            _ => None,
        })
        .collect()
}

/// The template's first named placeholder in first-appearance order.
fn first_named_placeholder(command: &str) -> Option<String> {
    CommandTemplate::parse(command)
        .params()
        .iter()
        .find_map(|param| match param {
            jinn_session_lifecycle_msg::command_template::Param::Named(name) => Some(name.clone()),
            _ => None,
        })
}

#[rstest::rstest]
#[test]
fn autodev_scripts_exist_and_parse() {
    // Given the shipped autodev lifecycle file.
    let raw = read_lifecycle();

    // When its scripts are extracted.
    let scripts = parse_scripts(&raw);

    // Then both SCM scripts are present with setup and teardown commands.
    let names: Vec<&str> = scripts.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"autodev implementor (fossil)"), "{names:?}");
    assert!(names.contains(&"autodev implementor (git)"), "{names:?}");
    for script in &scripts {
        assert!(!script.setup.is_empty(), "{}: empty setup", script.name);
        assert!(
            !script.teardown.is_empty(),
            "{}: empty teardown",
            script.name
        );
    }
}

#[rstest::rstest]
#[test]
fn autodev_setup_and_teardown_name_the_same_placeholders() {
    // Given every autodev script.
    let raw = read_lifecycle();
    let scripts = parse_scripts(&raw);
    assert!(!scripts.is_empty(), "no scripts extracted");

    // When each script's setup and teardown placeholders are compared.
    let mut mismatched = Vec::new();
    for script in &scripts {
        let setup_names = placeholder_names(&script.setup);
        let teardown_names = placeholder_names(&script.teardown);
        // Teardown may use a subset — it replays only what it needs. A
        // teardown naming a placeholder setup does not record is a data
        // defect: by-name replay fails visibly for that script.
        let unrecorded: Vec<&String> = teardown_names
            .iter()
            .filter(|name| !setup_names.contains(*name))
            .collect();
        if !unrecorded.is_empty() {
            mismatched.push(format!("{}: {unrecorded:?}", script.name));
        }
    }

    // Then every script's teardown replays from its own setup. The git
    // script used to be the one exception — its teardown named a <parent>
    // its setup never recorded — but the lifecycle was repaired upstream
    // (d66a43ef5), so every script is now clean and this asserts that.
    for script in &scripts {
        assert!(
            !mismatched.iter().any(|m| m.starts_with(&script.name)),
            "{}: teardown names placeholders setup does not record",
            script.name
        );
    }
}

#[rstest::rstest]
#[test]
fn autodev_fossil_teardown_replays_the_incident_coordinates_by_name() {
    // Given the incident's recorded coordinates: setup recorded dir, repo,
    // branch, parent — in that order — while the fossil teardown lists
    // parent first.
    let raw = read_lifecycle();
    let script = parse_scripts(&raw)
        .into_iter()
        .find(|s| s.name == "autodev implementor (fossil)")
        .expect("fossil script exists");
    let stored = jinn_session_state::fields::LifecycleArgs::from_pairs([
        ("dir", "/repo/worktrees/wt-1"),
        ("repo", "jinn"),
        ("branch", "autodev-issue-001"),
        ("parent", "workforce-msg-fix"),
    ]);
    let values = stored
        .as_pairs()
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<BTreeMap<_, _>>();

    // When the teardown is rendered by name.
    let rendered = CommandTemplate::parse(&script.teardown)
        .render_named(&values)
        .expect("setup recorded every teardown placeholder");

    // Then each command carries its own value — the merge lands on the
    // parent branch, not on a positionally-shifted wrong one.
    assert!(
        rendered.contains("fossil merge workforce-msg-fix"),
        "{rendered}"
    );
    assert!(
        rendered.contains("fossil update workforce-msg-fix"),
        "{rendered}"
    );
    assert!(
        rendered.contains("fossil merge autodev-issue-001"),
        "{rendered}"
    );
    assert!(
        rendered.contains("rm -rfv /repo/worktrees/wt-1"),
        "{rendered}"
    );
    assert!(
        !rendered.contains("jinn"),
        "the repo value must not leak into any command: {rendered}"
    );
}

#[rstest::rstest]
#[test]
fn autodev_fossil_teardown_rejects_positional_replay_of_the_incident_shape() {
    // Given the incident's positional reading — what the old code did: the
    // teardown's FIRST placeholder receives setup's FIRST recorded value.
    let raw = read_lifecycle();
    let script = parse_scripts(&raw)
        .into_iter()
        .find(|s| s.name == "autodev implementor (fossil)")
        .expect("fossil script exists");

    // When the teardown's first placeholder is compared with setup's.
    let teardown_first =
        first_named_placeholder(&script.teardown).expect("teardown has placeholders");
    let setup_first = first_named_placeholder(&script.setup).expect("setup has placeholders");

    // Then the orders genuinely differ — this is the property the positional
    // renderer got wrong, and what the by-name renderer must not depend on.
    assert_ne!(teardown_first, setup_first);
}
