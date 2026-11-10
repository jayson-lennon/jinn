//! `<session_cwd>` token and env-expansion tests for the MCP connect path.
//!
//! Proves the observable contract at the connect boundary:
//!
//! - The `<session_cwd>` token in args and env values expands to the
//!   attached session cwd (per-session spawn); with `None` it stays
//!   literal in the command but env values pass through unexpanded.
//! - A per-session env value referencing an unresolvable `${VAR}` aborts
//!   resolution naming the variable (no value leaks).
//! - A **singleton** entry referencing the token (args or env values)
//!   refuses to connect with an error naming the conflict — the refusal
//!   fires before any spawn attempt, and a per-session entry with the
//!   same token fails at spawn instead (the legitimate shape).

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test assertions"
)]

use std::collections::BTreeMap;

use crate::connection::{connect_for_transport, expand_server_env, server_command};
use jinn_kernel::Services;
use marked_path::{Absolute, MarkedPathBuf};

fn cwd(root: &str) -> MarkedPathBuf<Absolute> {
    let path = std::path::Path::new(root);
    MarkedPathBuf::<Absolute>::new(path).expect("absolute cwd")
}

// ── server_command: token expansion ─────────────────────────────────────

#[rstest::rstest]
#[test]
fn session_cwd_token_expands_in_args() {
    // Given a config whose arg carries the token and an attached cwd.
    let session_cwd = cwd("/tmp/worktree-a");
    let config = jinn_preferences_config::schemas::mcp::McpServerConfig {
        command: Some("echo".to_owned()),
        args: vec!["--project=<session_cwd>".to_owned()],
        ..Default::default()
    };

    // When building the command.
    let cmd = server_command(&config, Some(&session_cwd));

    // Then the arg carries the absolute session cwd.
    assert_eq!(cmd.args, vec!["--project=/tmp/worktree-a"]);
}

#[rstest::rstest]
#[test]
fn jinn_only_entries_are_untouched_by_the_session_cwd_expansion() {
    // Given a <jinn>-only config and an attached cwd.
    let session_cwd = cwd("/tmp/worktree-b");
    let config = jinn_preferences_config::schemas::mcp::McpServerConfig {
        command: Some("<jinn>".to_owned()),
        args: vec!["--parent=<jinn>".to_owned()],
        ..Default::default()
    };

    // When building the command.
    let cmd = server_command(&config, Some(&session_cwd));

    // Then <jinn> expands and no cwd token was involved.
    let exe = std::env::current_exe().expect("test binary has an exe path");
    assert_eq!(cmd.program, exe.to_string_lossy());
    assert_eq!(
        cmd.args,
        vec![format!("--parent={}", exe.to_string_lossy())]
    );
}

#[rstest::rstest]
#[test]
fn none_cwd_leaves_the_token_literal_in_args() {
    // Given a config whose arg carries the token and no attached cwd.
    let config = jinn_preferences_config::schemas::mcp::McpServerConfig {
        command: Some("echo".to_owned()),
        args: vec!["--project=<session_cwd>".to_owned()],
        ..Default::default()
    };

    // When building the command.
    let cmd = server_command(&config, None);

    // Then the token stays literal (the singleton refusal fires first on
    // the real path; this asserts the builder's own behavior).
    assert_eq!(cmd.args, vec!["--project=<session_cwd>"]);
}

// ── expand_server_env: token + ${VAR} resolution ─────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn session_cwd_token_expands_in_env_values() {
    // Given an env map with one token-carrying value, resolved against a
    // fresh key store.
    let services = Services::new_fake().await;
    let session_cwd = cwd("/tmp/worktree-c");
    let server = jinn_preferences_config::schemas::mcp::McpServerConfig {
        command: Some("echo".to_owned()),
        env: BTreeMap::from([("GDA_PROJECT".to_owned(), "<session_cwd>".to_owned())]),
        ..Default::default()
    };

    // When resolving the env pairs.
    let env = expand_server_env(&services, &server, Some(&session_cwd)).expect("expand");

    // Then the value carries the absolute session cwd.
    assert_eq!(
        env,
        vec![("GDA_PROJECT".to_owned(), "/tmp/worktree-c".to_owned())]
    );
}

#[rstest::rstest]
#[tokio::test]
async fn env_token_unresolved_fails_naming_variable() {
    // Given an env value referencing a variable that was never seeded.
    let services = Services::new_fake().await;
    let server = jinn_preferences_config::schemas::mcp::McpServerConfig {
        command: Some("echo".to_owned()),
        env: BTreeMap::from([(
            "TOKEN".to_owned(),
            "Bearer ${JINN_TEST_ENV_NEVER_SET}".to_owned(),
        )]),
        ..Default::default()
    };

    // When resolving the env pairs.
    let result = expand_server_env(&services, &server, None);

    // Then resolution fails naming the variable only — no value text leaks.
    let err = result.expect_err("unresolved");
    let rendered = format!("{err:#}");
    assert!(
        rendered.contains("JINN_TEST_ENV_NEVER_SET"),
        "names var: {rendered}"
    );
    assert!(!rendered.contains("Bearer"), "no value leak: {rendered}");
}

#[rstest::rstest]
#[tokio::test]
async fn resolved_env_var_splices_into_env_value() {
    // Given an env value referencing a seeded variable.
    let services = Services::new_fake().await;
    services
        .api_keys
        .insert("JINN_TEST_ENV_SEEDED".to_owned(), "seeded-value".to_owned());
    let server = jinn_preferences_config::schemas::mcp::McpServerConfig {
        command: Some("echo".to_owned()),
        env: BTreeMap::from([(
            "TOKEN".to_owned(),
            "Bearer ${JINN_TEST_ENV_SEEDED}".to_owned(),
        )]),
        ..Default::default()
    };

    // When resolving the env pairs.
    let env = expand_server_env(&services, &server, None).expect("expand");

    // Then the seeded value is spliced in.
    assert_eq!(
        env,
        vec![("TOKEN".to_owned(), "Bearer seeded-value".to_owned())]
    );
}

// ── connect_for_transport: singleton refusal ─────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn singleton_with_session_cwd_token_refuses_to_spawn() {
    // Given a singleton stdio entry whose arg references the token, no
    // session cwd (the singleton spawn shape), and a command that could not
    // spawn anyway.
    let services = Services::new_fake().await;
    let server = jinn_preferences_config::schemas::mcp::McpServerConfig {
        singleton: true,
        command: Some("definitely-not-a-real-binary-xyzzy".to_owned()),
        args: vec!["--project=<session_cwd>".to_owned()],
        ..Default::default()
    };

    // When connecting.
    let result = connect_for_transport(&services, &server, None).await;

    // Then it fails naming the singleton-vs-token conflict — before any
    // spawn attempt (the message is the refusal, not a spawn failure).
    let Err(err) = result else {
        panic!("expected the singleton refusal");
    };
    let rendered = format!("{err:?}");
    assert!(
        rendered.contains("references <session_cwd>"),
        "conflict named: {rendered}"
    );
    assert!(
        rendered.contains("pin an absolute path instead"),
        "fix suggested: {rendered}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn singleton_with_token_in_env_value_refuses_to_spawn() {
    // Given a singleton entry whose env value references the token.
    let services = Services::new_fake().await;
    let server = jinn_preferences_config::schemas::mcp::McpServerConfig {
        singleton: true,
        command: Some("definitely-not-a-real-binary-xyzzy".to_owned()),
        env: BTreeMap::from([("GDA_PROJECT".to_owned(), "<session_cwd>".to_owned())]),
        ..Default::default()
    };

    // When connecting.
    let result = connect_for_transport(&services, &server, None).await;

    // Then the same refusal fires for the env position.
    let Err(err) = result else {
        panic!("expected the singleton refusal for the env position");
    };
    let rendered = format!("{err:?}");
    assert!(
        rendered.contains("references <session_cwd>"),
        "named: {rendered}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn per_session_entry_with_token_attempts_the_spawn() {
    // Given a per-session entry (singleton = false) carrying the token and
    // an attached cwd, whose command cannot spawn.
    let services = Services::new_fake().await;
    let session_cwd = cwd("/tmp/worktree-d");
    let server = jinn_preferences_config::schemas::mcp::McpServerConfig {
        command: Some("definitely-not-a-real-binary-xyzzy".to_owned()),
        args: vec!["--project=<session_cwd>".to_owned()],
        ..Default::default()
    };

    // When connecting.
    let result = connect_for_transport(&services, &server, Some(&session_cwd)).await;

    // Then it fails at spawn (binary missing), not at the refusal — the
    // per-session shape is legitimate.
    let Err(err) = result else {
        panic!("expected the spawn failure");
    };
    let rendered = format!("{err:?}");
    assert!(
        !rendered.contains("pin an absolute path"),
        "must not be the singleton refusal: {rendered}"
    );
}
