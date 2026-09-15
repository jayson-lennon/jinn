//! jinn is an MCP *client*: it connects to externally-defined MCP servers
//! over stdio or HTTP. Servers are declared in `jinn.toml` under
//! `[[mcp_server]]` and enabled per-session from the persisted `SessionCore`.
//!
//! See [`McpServerConfig`] and [`TransportKind`].

pub mod picker_entry;

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

use error_stack::Report;
use wherror::Error;

/// How jinn connects to an MCP server.
///
/// Serialized as a flat string field (`transport = "stdio"`, `transport = "local_http"`,
/// etc.), not a sub-table. The default is [`Stdio`](TransportKind::Stdio), which is what an
/// absent `transport` field in `jinn.toml` deserializes to, so existing stdio-only
/// configurations keep working unchanged.
///
/// - [`Stdio`](TransportKind::Stdio) — jinn spawns the server as a child
///   process and speaks JSON-RPC over its stdin/stdout. The default and the
///   universal fallback.
/// - [`LocalHttp`](TransportKind::LocalHttp) — jinn spawns the server as a child
///   process in HTTP mode, allocates a free local port, and connects to it
///   over `StreamableHTTP` at the expanded `url`. Both stdout and stderr are
///   captured for the inspector logs pane (unlike stdio, stdout is free for
///   the server's own logs in this mode). The `url` field carries a template
///   with a `<port>` token; the host portion is the bind address and `<port>`
///   is expanded with the jinn-allocated port.
/// - [`RemoteHttp`](TransportKind::RemoteHttp) — jinn connects to an
///   already-running HTTP server at the `url`; it owns no process.
///
/// See [`McpServerConfig`] for the `url` and `command` fields.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    /// JSON-RPC over the child process's stdin/stdout.
    #[default]
    Stdio,
    /// `StreamableHTTP` over a managed local child process.
    LocalHttp,
    /// `StreamableHTTP` to an externally-managed server at `url`.
    RemoteHttp,
}

/// One configured MCP server.
///
/// Declared in `jinn.toml` under `[mcp_server.<name>]` — the table name IS
/// the server's identity (the per-session enablement identifier stored in
/// `SessionCore::enabled_mcp_servers` and the tool-namespace segment
/// `mcp__<name>__<tool>`); there is no `name` field to drift out of sync
/// with the key.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Executable command to launch the server (e.g. `"npx"`).
    ///
    /// Optional: `None` is valid for [`RemoteHttp`](TransportKind::RemoteHttp)
    /// servers, which jinn never spawns.
    #[serde(default)]
    pub command: Option<String>,
    /// Arguments passed to the command.
    ///
    /// For [`LocalHttp`](TransportKind::LocalHttp) servers, `<ip>` and `<port>`
    /// replacement tokens are expanded into the configured bind address (parsed
    /// from `url`) and the allocated port respectively (e.g.
    /// `["server.js", "--port", "<port>", "--host", "<ip>"]`).
    #[serde(default)]
    pub args: Vec<String>,
    /// How jinn connects to this server. Defaults to
    /// [`Stdio`](TransportKind::Stdio) when absent, so existing stdio-only
    /// configurations keep working unchanged.
    #[serde(default)]
    pub transport: TransportKind,
    /// The HTTP URL template (`LocalHttp`) or full URL (`RemoteHttp`).
    ///
    /// For [`LocalHttp`](TransportKind::LocalHttp), the host portion of this
    /// template is the bind address, and `<port>` is expanded with the
    /// jinn-allocated port (e.g. `"http://127.0.0.1:<port>/mcp"`).
    ///
    /// For [`RemoteHttp`](TransportKind::RemoteHttp), this is the full URL of
    /// an externally-managed server (e.g. `"http://localhost:3001/mcp"`).
    ///
    /// Unused for [`Stdio`](TransportKind::Stdio).
    #[serde(default)]
    pub url: Option<String>,
    /// Start this server already enabled in newly created sessions.
    ///
    /// Off by default (jinn's historical behavior): servers spawn only after
    /// the user enables them in the MCP picker. `true` seeds the server's
    /// name into each new session's enabled set at creation, so its
    /// connection comes up without a picker visit. Toggle state is owned by
    /// the session afterwards and persists per-session; this flag never
    /// re-enables itself on existing sessions.
    #[serde(default)]
    pub auto_enable: bool,
    /// HTTP headers sent on `StreamableHTTP` connections
    /// ([`LocalHttp`](TransportKind::LocalHttp) and
    /// [`RemoteHttp`](TransportKind::RemoteHttp)).
    ///
    /// Values may embed `${ENV_VAR}` tokens (e.g. `"Bearer ${ZAI_API_KEY}"`)
    /// which are expanded from the environment once at startup; header names
    /// are taken literally. Unused for [`Stdio`](TransportKind::Stdio).
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

impl McpServerConfig {
    /// One-line description for the MCP server picker.
    ///
    /// Shows the launch command and args so the user can tell servers apart
    /// and spot misconfiguration at a glance.
    #[must_use]
    pub fn description_for_picker(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(cmd) = &self.command {
            parts.push(cmd.clone());
        }
        parts.extend(self.args.iter().cloned());
        parts.join(" ")
    }
}

/// Errors from expanding `${VAR}` tokens in configured MCP header values.
#[derive(Debug, Error)]
#[error(debug)]
pub enum HeaderExpandError {
    /// A `${VAR}` token referenced a variable missing (or empty) from the
    /// key store. The variable's *name* is carried; values never are.
    UnresolvedVariable { variable: String },
}

/// The injected resolver the expansion functions use to look up variable
/// values. A trait object (not `impl Fn`) so closure arguments coerce
/// reliably at every call site instead of tripping HRTB inference.
type ResolveFn<'a> = &'a dyn Fn(&str) -> Option<String>;

/// True for the first character of a legal environment-variable name.
fn is_env_name_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

/// True for subsequent characters of a legal environment-variable name.
fn is_env_name_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Extracts every `${NAME}` variable referenced anywhere in `values`, for
/// startup resolution into the key store.
///
/// Only well-formed tokens (`[A-Za-z_][A-Za-z0-9_]*` between `${` and the
/// next `}`) are collected; malformed constructs yield nothing. Malformed
/// values can never expand at connect time either, so ignoring them here is
/// consistent rather than lossy. Names are deduplicated; an empty `values`
/// yields an empty set.
#[must_use]
pub fn referenced_header_variables(values: &[&str]) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for value in values {
        let mut rest: &str = value;
        while let Some(dollar) = rest.find("${") {
            // All offsets come from `find`, which only returns char-boundary
            // positions, so these `get`s always succeed.
            let Some(scan) = rest.get(dollar + 2..) else {
                break;
            };
            match scan.find('}') {
                Some(close) => {
                    let candidate = scan.get(..close).unwrap_or_default();
                    let mut chars = candidate.chars();
                    let head_ok = chars.next().is_some_and(is_env_name_start);
                    let tail_ok = chars.all(is_env_name_continue);
                    if head_ok && tail_ok {
                        names.insert(candidate.to_owned());
                    }
                    // Resume after the closing brace.
                    rest = scan.get(close + 1..).unwrap_or_default();
                }
                None => break, // unterminated: nothing more to find
            }
        }
    }
    names
}

/// Scans `raw` left to right, splicing in resolved variables at each
/// well-formed `${NAME}` token.
///
/// Malformed constructs (`${}`, unterminated `${`, bad name characters)
/// pass through literally: everything from the `$` through the scanned
/// region is copied verbatim and scanning resumes after the `${`. Literals
/// without tokens are returned unchanged. Resolution is delegated entirely
/// to `resolve`; a `None` (or empty-string) resolution aborts with
/// [`HeaderExpandError::UnresolvedVariable`], carrying the variable *name*
/// only — never any secret value.
///
/// # Errors
///
/// Returns [`HeaderExpandError::UnresolvedVariable`] when any token names a
/// variable `resolve` cannot supply.
pub fn expand_header_value(
    raw: &str,
    resolve: ResolveFn<'_>,
) -> Result<String, Report<HeaderExpandError>> {
    if !raw.contains("${") {
        return Ok(raw.to_owned());
    }

    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(dollar) = rest.find("${") {
        if let Some(prefix) = rest.get(..dollar) {
            out.push_str(prefix);
        }
        // All offsets come from `find`, which only returns char-boundary
        // positions, so these `get`s always succeed. Scoping keeps `rest`
        // always advancing — this is what guarantees termination even on
        // malformed tokens like "${no-close".
        let after_token_start = {
            let scan = rest.get(dollar + 2..).unwrap_or_default();
            match scan.find('}') {
                // Well-formed: validate every char between ${ and }.
                Some(close) => {
                    let region = scan.get(..close).unwrap_or_default();
                    let mut chars = region.chars();
                    let head_ok = chars.next().is_some_and(is_env_name_start);
                    let tail_ok = chars.all(is_env_name_continue);
                    if head_ok && tail_ok {
                        let name = region;
                        let value = resolve(name).unwrap_or_default();
                        if value.is_empty() {
                            return Err(Report::new(HeaderExpandError::UnresolvedVariable {
                                variable: name.to_owned(),
                            }));
                        }
                        out.push_str(&value);
                        2 + close + 1 // consumed "${NAME}"
                    } else {
                        // Bad name characters: copy the whole "${...}" literally.
                        out.push_str("${");
                        out.push_str(region);
                        out.push('}');
                        2 + close + 1
                    }
                }
                // Unterminated "${": emit "${" and resume after it, so the
                // same `$` is never re-scanned.
                None => {
                    out.push_str("${");
                    2
                }
            }
        };
        rest = rest.get(dollar + after_token_start..).unwrap_or_default();
    }
    out.push_str(rest);
    Ok(out)
}

/// Expands `${VAR}` tokens across all of a server's configured headers,
/// preserving the deterministic [`BTreeMap`] iteration order as ordered
/// `(name, value)` pairs.
///
/// Fails fast on the first unresolvable variable; nothing partial escapes.
///
/// # Errors
///
/// Returns [`HeaderExpandError::UnresolvedVariable`] when any header value
/// references a variable missing from the key store.
pub fn expand_mcp_headers(
    headers: &BTreeMap<String, String>,
    resolve: ResolveFn<'_>,
) -> Result<Vec<(String, String)>, Report<HeaderExpandError>> {
    let pairs = headers
        .iter()
        .map(|(name, value)| {
            let expanded = expand_header_value(value, resolve)?;
            Ok((name.clone(), expanded))
        })
        .collect::<Result<Vec<_>, Report<HeaderExpandError>>>()?;
    Ok(pairs)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::items_after_statements,
        reason = "test assertions"
    )]
    use super::*;

    #[rstest::rstest]
    #[test]
    fn absent_transport_defaults_to_stdio() {
        // Given a config TOML with no transport field.
        let toml = r#"
            command = "npx"
            args = ["@excalimate/mcp-server", "--stdio"]
        "#;

        // When deserializing.
        let config: McpServerConfig = toml::from_str(toml).expect("parse");

        // Then transport defaults to Stdio (backward-compat regression guard).
        assert_eq!(config.transport, TransportKind::Stdio);
    }

    #[rstest::rstest]
    #[test]
    fn command_is_optional_deserializes_when_absent() {
        // Given a config TOML with no command field.
        let toml = r#"
            transport = "remote_http"
            url = "http://localhost:3001/mcp"
        "#;

        // When deserializing.
        let config: McpServerConfig = toml::from_str(toml).expect("parse");

        // Then command is None.
        assert!(config.command.is_none());
    }

    #[rstest::rstest]
    #[test]
    fn remote_http_config_needs_no_command() {
        // Given a RemoteHttp config with command set to None.
        let config = McpServerConfig {
            command: None,
            args: vec![],
            transport: TransportKind::RemoteHttp,
            url: Some("http://localhost:3001/mcp".to_owned()),
            headers: BTreeMap::new(),
            auto_enable: false,
        };

        // When serializing and deserializing.
        let toml = toml::to_string(&config).expect("serialize");
        let back: McpServerConfig = toml::from_str(&toml).expect("deserialize");

        // Then command stays None and the config round-trips.
        assert!(back.command.is_none());
        assert_eq!(back.transport, TransportKind::RemoteHttp);
    }

    #[rstest::rstest]
    #[test]
    fn absent_headers_default_to_empty() {
        // Given a config TOML with no headers field.
        let toml = r#"
            transport = "remote_http"
            url = "http://localhost:3001/mcp"
        "#;

        // When deserializing.
        let config: McpServerConfig = toml::from_str(toml).expect("parse");

        // Then headers is empty (backward-compat regression guard).
        assert!(config.headers.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn headers_deserialize_as_map_under_server_entry() {
        // Given a RemoteHttp config declaring auth headers as an inline table.
        let toml = r#"
            transport = "remote_http"
            url = "http://localhost:3001/mcp"

            [mcp_server.headers-table-probe.headers]
            Authorization = "Bearer ${ZAI_API_KEY}"
            X-Api-Key = "${OTHER_KEY}"
        "#;

        // When deserializing the enclosing preferences document.
        #[derive(Deserialize)]
        struct Probe {
            mcp_server: BTreeMap<String, McpServerConfig>,
        }
        let probe: Probe = toml::from_str(toml).expect("parse");

        // When reading the server's headers.
        let server = &probe.mcp_server["headers-table-probe"];

        // Then both headers survive as configured.
        assert_eq!(
            server.headers.get("Authorization").map(String::as_str),
            Some("Bearer ${ZAI_API_KEY}")
        );
        assert_eq!(
            server.headers.get("X-Api-Key").map(String::as_str),
            Some("${OTHER_KEY}")
        );
    }

    #[rstest::rstest]
    #[test]
    fn bearer_prefix_survives_expansion() {
        // Given a header value embedding one token inside a scheme prefix,
        // backed by a resolver supplying the secret.
        let resolve = |name: &str| (name == "T_KEY").then(|| "secret-value".to_owned());

        // When expanding the value.
        let expanded = expand_header_value("Bearer ${T_KEY}", &resolve).expect("expand");

        // Then the prefix and secret are spliced together.
        assert_eq!(expanded, "Bearer secret-value");
    }

    #[rstest::rstest]
    #[test]
    fn multiple_tokens_expand_in_one_value() {
        // Given a value referencing two distinct variables.
        let resolve = |name: &str| match name {
            "A" => Some("va".to_owned()),
            "B" => Some("vb".to_owned()),
            _ => None,
        };

        // When expanding.
        let expanded = expand_header_value("${A}-${B}", &resolve).expect("expand");

        // Then both are spliced in order.
        assert_eq!(expanded, "va-vb");
    }

    #[rstest::rstest]
    #[test]
    fn value_without_tokens_is_literal() {
        // Given a plain literal value.
        // When expanding.
        let expanded = expand_header_value("plaintext", &|_| None).expect("expand");

        // Then it passes through unchanged even though nothing resolves.
        assert_eq!(expanded, "plaintext");
    }

    #[rstest::rstest]
    #[test]
    fn unset_variable_fails_naming_variable() {
        // Given a resolver with no entry for the referenced variable.
        let resolve = |_: &str| -> Option<String> { None };

        // When expanding a value referencing it.
        let result = expand_header_value("Bearer ${MISSING_VAR}", &resolve);

        // Then expansion fails naming the variable.
        let err = result.expect_err("unresolved");
        assert!(matches!(
            err.current_context(),
            HeaderExpandError::UnresolvedVariable { variable: v } if v == "MISSING_VAR"
        ));
        // And the failure names the variable without leaking any value text.
        let rendered = format!("{err}");
        assert!(rendered.contains("MISSING_VAR"));
        assert!(!rendered.contains("secret"));
    }

    #[rstest::rstest]
    #[test]
    fn empty_resolved_value_treated_as_unset() {
        // Given a resolver that returns an empty string for the variable.
        let resolve = |name: &str| (name == "EMPTY_VAR").then(String::new);

        // When expanding a value referencing it.
        let result = expand_header_value("${EMPTY_VAR}", &resolve);

        // Then it is treated exactly like an unset variable.
        assert!(matches!(
            result.expect_err("unresolved").current_context(),
            HeaderExpandError::UnresolvedVariable { variable: v } if v == "EMPTY_VAR"
        ));
    }

    #[rstest::rstest]
    #[case::unterminated("${no-close")]
    #[case::empty_name("${}")]
    #[case::plain_dollar("$VAR")]
    #[case::bad_name_char("${BAD CHAR}")]
    fn malformed_tokens_pass_through_literally(#[case] raw: &str) {
        // Given raw values with malformed token syntax.
        // When expanding with no variables available at all.
        let expanded = expand_header_value(raw, &|_| None).expect("expand");

        // Then every input passes through byte-for-byte unchanged.
        assert_eq!(expanded, raw);
    }

    #[rstest::rstest]
    #[test]
    fn malformed_then_valid_token_still_expands_valid() {
        // Given a value mixing a malformed construct with a real variable.
        let resolve = |name: &str| (name == "GOOD").then(|| "yes".to_owned());

        // When expanding.
        let expanded = expand_header_value("${no-close}${GOOD}", &resolve).expect("expand");

        // Then the malformed part stays literal and the valid token expands.
        assert_eq!(expanded, "${no-close}yes");
    }

    #[rstest::rstest]
    #[test]
    fn trailing_text_after_last_token_is_kept() {
        // Given a value ending in literal text after a token.
        let resolve = |name: &str| (name == "K").then(|| "v".to_owned());

        // When expanding.
        let expanded = expand_header_value("${K}-suffix", &resolve).expect("expand");

        // Then the suffix survives.
        assert_eq!(expanded, "v-suffix");
    }

    #[rstest::rstest]
    #[test]
    fn double_dollar_prefix_expands_inner_token() {
        // Given "$${VAR}" — dollar immediately before an open delimiter.
        let resolve = |name: &str| (name == "VAR").then(|| "val".to_owned());

        // When expanding.
        let expanded = expand_header_value("$${VAR}", &resolve).expect("expand");

        // Then the inner token expands ($$ accepts the documented quirk).
        assert_eq!(expanded, "$val");
    }

    #[rstest::rstest]
    #[test]
    fn expand_mcp_headers_preserves_order_and_pairs() {
        // Given multiple configured headers referencing different variables.
        let mut headers = BTreeMap::new();
        headers.insert("Authorization".to_owned(), "Bearer ${TOKEN}".to_owned());
        headers.insert("X-Api-Key".to_owned(), "${KEY}".to_owned());
        let resolve = |name: &str| match name {
            "TOKEN" => Some("t".to_owned()),
            "KEY" => Some("k".to_owned()),
            _ => None,
        };

        // When expanding the whole map.
        let pairs = expand_mcp_headers(&headers, &resolve).expect("expand");

        // Then deterministic ordered pairs come back, values expanded.
        assert_eq!(
            pairs,
            vec![
                ("Authorization".to_owned(), "Bearer t".to_owned()),
                ("X-Api-Key".to_owned(), "k".to_owned()),
            ]
        );
    }

    #[rstest::rstest]
    #[test]
    fn expand_mcp_headers_fails_on_first_unresolvable() {
        // Given one resolvable and one unresolvable header.
        let mut headers = BTreeMap::new();
        headers.insert("X-Good".to_owned(), "${OK}".to_owned());
        headers.insert("X-Bad".to_owned(), "${NOT_THERE}".to_owned());
        let resolve = |name: &str| (name == "OK").then(|| "fine".to_owned());

        // When expanding the whole map.
        let result = expand_mcp_headers(&headers, &resolve);

        // Then nothing partial escapes: the error names the missing variable.
        let err = result.expect_err("unresolved");
        assert!(matches!(
            err.current_context(),
            HeaderExpandError::UnresolvedVariable { variable: v } if v == "NOT_THERE"
        ));
    }

    #[rstest::rstest]
    #[test]
    fn absent_auto_enable_defaults_to_false() {
        // Given a config TOML with no auto_enable field.
        let toml = r#"
            command = "npx"
            args = ["@excalimate/mcp-server", "--stdio"]
        "#;

        // When deserializing.
        let config: McpServerConfig = toml::from_str(toml).expect("parse");

        // Then auto_enable defaults to false (historical behavior:
        // servers start disabled in new sessions until the user opts in).
        assert!(!config.auto_enable);
    }

    #[rstest::rstest]
    #[test]
    fn explicit_auto_enable_false_deserializes() {
        // Given a config TOML with auto_enable explicitly false.
        let toml = r#"
            command = "npx"
            auto_enable = false
        "#;

        // When deserializing.
        let config: McpServerConfig = toml::from_str(toml).expect("parse");

        // Then the flag reads back false.
        assert!(!config.auto_enable);
    }

    #[rstest::rstest]
    #[test]
    fn explicit_auto_enable_true_deserializes_and_round_trips() {
        // Given a config TOML with auto_enable enabled.
        let toml = r#"
            command = "npx"
            args = ["-y", "@modelcontextprotocol/server-everything"]
            auto_enable = true
        "#;

        // When deserializing and re-serializing.
        let config: McpServerConfig = toml::from_str(toml).expect("parse");
        let serialized = toml::to_string(&config).expect("serialize");
        let back: McpServerConfig = toml::from_str(&serialized).expect("deserialize");

        // Then the flag is preserved as true through the round-trip.
        assert!(config.auto_enable);
        // And it survives re-serialization.
        assert!(back.auto_enable);
    }
}
