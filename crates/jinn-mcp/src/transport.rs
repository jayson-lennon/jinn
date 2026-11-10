//! Transport helpers for connecting to MCP servers.
//!
//! Currently this module covers HTTP-mode concerns: allocating a free local
//! port for a managed child-process server (so jinn can hand it an explicit
//! port rather than trusting the server to report where it bound).

use marked_path::{Absolute, MarkedPathBuf};
use std::net::{SocketAddr, TcpListener};

/// Allocate a free TCP port on `bind_addr` via bind-and-release.
///
/// Binds to `(bind_addr, 0)` (the OS picks a free port), reads the assigned
/// port, then drops the listener. The caller is expected to spawn the MCP
/// server immediately with this port as a CLI argument.
///
/// The release→rebind window is instant on localhost (no `TIME_WAIT` delay),
/// so the child re-binding the same port succeeds. The only failure mode is an
/// unrelated process grabbing that exact port in the microsecond between
/// release and the child's bind — astronomically rare on localhost, and a clean
/// retry if it happens.
///
/// # Errors
///
/// Returns an error if binding to `(bind_addr, 0)` fails (e.g. `bind_addr` is
/// not a valid local address).
pub fn pick_free_port(bind_addr: &str) -> Result<u16, std::io::Error> {
    let ip: std::net::IpAddr = bind_addr
        .parse()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let listener = TcpListener::bind(SocketAddr::from((ip, 0)))?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

/// Expand `<ip>` and `<port>` replacement tokens in command arguments.
///
/// Mirrors the lifecycle-script token convention. Each `<ip>` substring is
/// replaced with `ip`, and each `<port>` substring with the decimal `port`.
/// Arguments with no tokens pass through unchanged. Pure and allocation-only.
pub fn expand_tokens(args: &[String], ip: &str, port: u16) -> Vec<String> {
    args.iter()
        .map(|a| a.replace("<ip>", ip).replace("<port>", &port.to_string()))
        .collect()
}

/// Expand the `<jinn>` replacement token in one command component (the
/// program name or a single argument): each `<jinn>` substring becomes the
/// path of the running jinn executable.
///
/// Lets a shipped `[mcp.<name>]` entry name jinn itself without assuming
/// jinn is on `PATH` — the config works identically for `cargo run`, a
/// release tarball, or a binstall layout. Mirrors `<ip>`/`<port>`.
///
/// A failed `current_exe` (near-impossible off exotic sandboxes) leaves the
/// token unexpanded; the spawn then fails naming the literal `<jinn>`, which
/// is legible enough for a condition that basically cannot happen. Pure and
/// allocation-only.
#[must_use]
pub fn expand_jinn_token(value: &str, jinn_exe: &MarkedPathBuf<Absolute>) -> String {
    value.replace("<jinn>", &jinn_exe.to_string())
}

/// Expand the `<session_cwd>` replacement token in one command component (an
/// argument or an env value): each `<session_cwd>` substring becomes the
/// enabling session's working directory.
///
/// Lets one static `[mcp.<name>]` entry serve every session regardless of
/// which directory it was started in — each per-session spawn expands the
/// token to that session's cwd, so a cwd-sensitive server (one locating a
/// project via an env var, say) pins to the right tree with zero per-project
/// config edits. Mirrors `<jinn>`.
///
/// The caller decides what an unresolvable cwd means: jinn carries the
/// token only on per-session spawns, and a singleton server referencing the
/// token is refused at connect instead of silently binding to whichever
/// session enabled it first. Pure and allocation-only.
#[must_use]
pub fn expand_session_cwd_token(value: &str, session_cwd: &MarkedPathBuf<Absolute>) -> String {
    value.replace("<session_cwd>", &session_cwd.to_string())
}

/// Extracts the host portion from a URL template like `http://127.0.0.1:<port>/mcp`.
///
/// Strips the scheme prefix and takes everything up to the first `:` (port
/// separator) or `/` (path separator), whichever comes first. Returns the raw
/// host string without validation — callers pass it to [`pick_free_port`] which
/// parses it as an IP address.
#[must_use]
pub fn parse_host(url_template: &str) -> String {
    let after_scheme = url_template
        .split_once("://")
        .map_or(url_template, |(_, rest)| rest);
    let host = after_scheme
        .split_once(':')
        .map_or(after_scheme, |(host, _)| host);
    host.split_once('/')
        .map_or(host, |(host, _)| host)
        .to_owned()
}
#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test assertions")]
    use super::*;

    #[rstest::rstest]
    #[test]
    fn pick_free_port_returns_a_rebindable_port() {
        // Given loopback as the bind address.
        // When allocating a port.
        let port = pick_free_port("127.0.0.1").expect("allocate port");

        // Then the same port can be re-bound immediately (bind-and-release works).
        let rebind = TcpListener::bind(("127.0.0.1", port));
        assert!(
            rebind.is_ok(),
            "port {port} should be immediately rebindable"
        );
    }

    #[rstest::rstest]
    #[test]
    fn pick_free_port_returns_distinct_ports_across_calls() {
        // Given loopback as the bind address.
        // When allocating two ports in quick succession.
        let a = pick_free_port("127.0.0.1").expect("allocate port a");
        let b = pick_free_port("127.0.0.1").expect("allocate port b");

        // Then they are distinct (OS rotates assigned ports).
        assert_ne!(a, b);
    }

    #[rstest::rstest]
    #[test]
    fn pick_free_port_rejects_invalid_bind_addr() {
        // Given an invalid address.
        // When allocating a port.
        let result = pick_free_port("not-an-ip");

        // Then it errors.
        assert!(result.is_err());
    }

    #[rstest::rstest]
    #[test]
    fn expand_tokens_replaces_ip_and_port() {
        // Given args with both tokens.
        let args = vec![
            "server.js".to_owned(),
            "--port".to_owned(),
            "<port>".to_owned(),
            "--host".to_owned(),
            "<ip>".to_owned(),
        ];

        // When expanding.
        let out = expand_tokens(&args, "127.0.0.1", 42365);

        // Then tokens are replaced with the bind addr and port.
        assert_eq!(
            out,
            vec!["server.js", "--port", "42365", "--host", "127.0.0.1"]
        );
    }

    #[rstest::rstest]
    #[test]
    fn expand_tokens_passes_through_args_without_tokens() {
        // Given args with no tokens.
        let args = vec!["--stdio".to_owned(), "--verbose".to_owned()];

        // When expanding.
        let out = expand_tokens(&args, "127.0.0.1", 42365);

        // Then args pass through unchanged.
        assert_eq!(out, vec!["--stdio", "--verbose"]);
    }

    #[rstest::rstest]
    #[test]
    fn parse_host_extracts_from_url_with_port_token() {
        // Given a url template with <port> token.
        // When parsing the host.
        let host = parse_host("http://127.0.0.1:<port>/mcp");

        // Then the host is extracted before the port.
        assert_eq!(host, "127.0.0.1");
    }

    #[rstest::rstest]
    #[test]
    fn parse_host_extracts_from_url_without_path() {
        // Given a url template with no path.
        // When parsing the host.
        let host = parse_host("http://0.0.0.0:<port>");

        // Then the host is extracted.
        assert_eq!(host, "0.0.0.0");
    }

    #[rstest::rstest]
    #[test]
    fn expand_tokens_handles_multiple_tokens_per_arg() {
        // Given an arg containing both tokens (e.g. a combined URL-ish arg).
        let args = vec!["http://<ip>:<port>/mcp".to_owned()];

        // When expanding.
        let out = expand_tokens(&args, "0.0.0.0", 8080);

        // Then both tokens in the same arg are replaced.
        assert_eq!(out, vec!["http://0.0.0.0:8080/mcp"]);
    }

    #[rstest::rstest]
    #[test]
    fn expand_tokens_replaces_port_in_url_template() {
        // Given a url template with a <port> token (as used by local_http).
        let url_template = "http://127.0.0.1:<port>/mcp".to_owned();

        // When expanding as a single-element args list.
        let expanded = expand_tokens(&[url_template], "127.0.0.1", 42365);

        // Then the port token is replaced with the allocated port.
        assert_eq!(expanded, vec!["http://127.0.0.1:42365/mcp"]);
    }

    #[rstest::rstest]
    #[test]
    fn expand_jinn_token_replaces_program_and_args() {
        // Given an absolute executable path and a program name and an arg
        // both carrying the <jinn> token.
        let root = jinn_testutil::paths::TestRoot::new();
        let exe = root.join_str("bin/jinn").expect("executable path");

        // When expanding.
        let program = expand_jinn_token("<jinn>", &exe);
        let arg = expand_jinn_token("mcp overgraph --parent <jinn>", &exe);

        // Then every occurrence becomes the executable path.
        assert_eq!(program, exe.to_string());
        assert_eq!(arg, format!("mcp overgraph --parent {exe}"));
    }

    #[rstest::rstest]
    #[test]
    fn expand_jinn_token_passes_through_without_token() {
        // Given a value with no token.
        // When expanding.
        let root = jinn_testutil::paths::TestRoot::new();
        let exe = root.join_str("jinn").expect("executable path");
        let out = expand_jinn_token("npx", &exe);

        // Then the value passes through unchanged.
        assert_eq!(out, "npx");
    }

    #[rstest::rstest]
    #[test]
    fn expand_jinn_token_handles_multiple_occurrences_in_one_value() {
        // Given one value naming jinn twice.
        let root = jinn_testutil::paths::TestRoot::new();
        let exe = root.join_str("jinn").expect("executable path");
        let exe_str = exe.to_string();

        // When expanding.
        let out = expand_jinn_token("<jinn> --jinn <jinn>", &exe);

        // Then both occurrences are replaced.
        assert_eq!(out, format!("{exe_str} --jinn {exe_str}"));
    }

    #[rstest::rstest]
    #[test]
    fn expand_session_cwd_token_replaces_args_and_env_values() {
        // Given an absolute session cwd and a value carrying the token.
        let root = jinn_testutil::paths::TestRoot::new();
        let cwd = root.join_str("worktrees/game-a").expect("session cwd");

        // When expanding an arg and an env value.
        let arg = expand_session_cwd_token("--project=<session_cwd>", &cwd);
        let env_value = expand_session_cwd_token("<session_cwd>/addons", &cwd);

        // Then every occurrence becomes the session cwd.
        let cwd_str = cwd.to_string();
        assert_eq!(arg, format!("--project={cwd_str}"));
        assert_eq!(env_value, format!("{cwd_str}/addons"));
    }

    #[rstest::rstest]
    #[test]
    fn expand_session_cwd_token_passes_through_without_token() {
        // Given a value with no token.
        // When expanding.
        let root = jinn_testutil::paths::TestRoot::new();
        let cwd = root.join_str("somewhere").expect("session cwd");
        let out = expand_session_cwd_token("/absolute/path", &cwd);

        // Then the value passes through unchanged.
        assert_eq!(out, "/absolute/path");
    }

    #[rstest::rstest]
    #[test]
    fn expand_session_cwd_token_is_independent_of_the_jinn_token() {
        // Given one value carrying both tokens.
        let root = jinn_testutil::paths::TestRoot::new();
        let exe = root.join_str("bin/jinn").expect("executable path");
        let cwd = root.join_str("worktrees/game-b").expect("session cwd");

        // When expanding the jinn token and then the cwd token.
        let out = expand_session_cwd_token(&expand_jinn_token("<jinn>:<session_cwd>", &exe), &cwd);

        // Then both expand — the two token texts are disjoint, so order is
        // unobservable.
        assert_eq!(out, format!("{exe}:{cwd}"));
    }
}
