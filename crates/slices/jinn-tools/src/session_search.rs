//! `session_search` built-in tool — full-text search across persisted sessions.
//!
//! Searches the FTS index over all persisted session entries (see
//! [`jinn_domain::feat::session_search`]). Queries are passed to FTS5 `MATCH`
//! unmodified; SQLite syntax errors are surfaced verbatim so the caller can
//! self-correct. Results are a flat, bm25-ranked top-N with per-session
//! rollup counts — no pagination; refine the query instead of paging.

use crate::tool_types::ToolContext;
use jinn_core_types::tool_types::{ToolCall, ToolDefinition, ToolResult};
use jinn_domain::feat::session::session_summary::SessionSummary;
use jinn_domain::feat::session_search::{SearchParams, SearchableRole};

use std::fmt::Write as _;

use super::BoxedToolFuture;

/// Upper bound on hits returned per search.
const MAX_LIMIT: u64 = 50;
/// Default number of hits returned per search.
const DEFAULT_LIMIT: u64 = 20;

/// Returns the tool definition for the `session_search` built-in tool.
#[must_use]
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "session_search".to_owned(),
        description: "Search across persisted sessions by content. Runs a full-text query \
 over every saved conversation (user, assistant, tool_call, tool_result, system, error, and \
 compaction text — never actor/thinking text).\n\nThe query uses SQLite FTS5 MATCH syntax, \
 passed through unmodified:\n  plain words: auth flow\n  phrases: \"retry with backoff\"\n  \
 prefix: migrat*\n  column filter: tool_result NEAR('timeout' 'config')\n  \
 boolean: session_fetch AND NOT pagination\n\n\
 Results are the N best matches ranked by relevance, each on one line with its session id, \
 session title, date, role, entry id, and a snippet where matches are wrapped in << >>. \
 A per-session match-count rollup shows which sessions are worth drilling into. There is no \
 pagination — if the top results miss, refine the query instead of paging.\n\n\
 Scope:\n  omitted or \"current\": only the current session\n  \"all\": every persisted session\n  \
 \"project:<name>\": sessions whose project directory ends with <name> (case-insensitive; \
 name is matched against the final path component)\n  \
 a specific session: pass session_id directly\n\n\
 Drill into a hit with session_fetch using its entry id.\n\nExamples:\n  \
 session_search({\"query\": \"rowid mapping\"})\n  \
 session_search({\"query\": \"fts5 snippet\", \"scope\": \"all\", \"fields\": [\"assistant\", \"tool_result\"]})\n  \
 session_search({\"query\": \"compactor\", \"scope\": \"project:session-search\"})"
            .to_owned(),
        prompt_snippet: None,
        prompt_guidelines: vec![
            "Prefer session_search over re-reading old sessions entry by entry; it finds \
 content in sessions that were never loaded."
                .to_owned(),
            "Queries are FTS5 MATCH expressions — if the syntax is wrong the SQLite error is \
 returned verbatim; fix and retry."
                .to_owned(),
        ],
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "FTS5 MATCH expression, e.g. 'auth flow', '\"retry with backoff\"', 'migrat*', 'tool_result NEAR(timeout config)'."
                },
                "scope": {
                    "type": "string",
                    "description": "Where to search: omit for the current session, 'all' for every persisted session, or 'project:<name>' for sessions in that project (name matched against the last path component, case-insensitive)."
                },
                "session_id": {
                    "type": "string",
                    "description": "Search exactly one session by id (overrides scope)."
                },
                "fields": {
                    "type": "array",
                    "items": { "type": "string", "enum": ["user", "assistant", "tool_call", "tool_result", "system", "error", "compaction"] },
                    "description": "Entry roles to search. Defaults to ['assistant', 'user']. 'actor' and 'thinking' are never indexed."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of hits to return (1-50). Defaults to 20."
                },
                "since": {
                    "type": "string",
                    "description": "Only entries on or after this ISO date/datetime (e.g. '2026-09-01' or '2026-09-01T12:00:00Z')."
                },
                "until": {
                    "type": "string",
                    "description": "Only entries on or before this ISO date/datetime."
                }
            },
            "required": ["query"]
        }),
        server_tool_type: None,
    }
}

/// Parsed and validated tool arguments.
#[derive(Debug)]
struct SearchArgs {
    query: String,
    /// Explicit session id (overrides scope).
    session_id: Option<String>,
    scope: Option<String>,
    fields: Vec<SearchableRole>,
    limit: u64,
    since: Option<jiff::Timestamp>,
    until: Option<jiff::Timestamp>,
}

/// A parsed scope value.
#[derive(Debug)]
enum Scope {
    /// The calling session.
    Current,
    /// Every persisted session.
    All,
    /// Sessions whose project path ends with this component (case-insensitive).
    Project(String),
}

/// Parses and validates raw JSON arguments.
fn parse_args(raw: &str) -> Result<SearchArgs, String> {
    let args: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| format!("invalid JSON arguments: {e}"))?;

    let query = args
        .get("query")
        .and_then(serde_json::Value::as_str)
        .ok_or("query is required")?
        .trim()
        .to_owned();
    if query.is_empty() {
        return Err("query is required".to_owned());
    }

    let scope = match args.get("scope").and_then(serde_json::Value::as_str) {
        None | Some("") => None,
        Some(s) => Some(s.to_owned()),
    };

    let session_id = args
        .get("session_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);

    let fields = match args.get("fields") {
        None | Some(serde_json::Value::Null) => {
            vec![SearchableRole::Assistant, SearchableRole::User]
        }
        Some(serde_json::Value::Array(items)) => {
            let mut roles = Vec::with_capacity(items.len());
            for item in items {
                let name = item.as_str().ok_or("fields must be an array of strings")?;
                let role = SearchableRole::parse(name).ok_or_else(|| {
                    format!("unknown field '{name}': actor and thinking are never indexed")
                })?;
                roles.push(role);
            }
            roles
        }
        Some(_) => return Err("fields must be an array of strings".to_owned()),
    };

    let limit = match args.get("limit") {
        None | Some(serde_json::Value::Null) => DEFAULT_LIMIT,
        Some(v) => {
            let n = v.as_u64().ok_or("limit must be a positive integer")?;
            n.clamp(1, MAX_LIMIT)
        }
    };

    let since = parse_date_arg(&args, "since")?;
    let until = parse_date_arg(&args, "until")?;

    Ok(SearchArgs {
        query,
        session_id,
        scope,
        fields,
        limit,
        since,
        until,
    })
}

/// Parses an optional ISO date/datetime string argument into a UTC instant.
fn parse_date_arg(args: &serde_json::Value, key: &str) -> Result<Option<jiff::Timestamp>, String> {
    let Some(raw) = args.get(key).and_then(serde_json::Value::as_str) else {
        return Ok(None);
    };
    if raw.trim().is_empty() {
        return Ok(None);
    }
    // Date-only strings are midnight UTC; datetimes parse directly.
    let ts: jiff::Timestamp = match raw.parse::<jiff::Timestamp>() {
        Ok(ts) => ts,
        Err(_) => {
            let date = raw
                .parse::<jiff::civil::Date>()
                .map_err(|e| format!("invalid {key} '{raw}': {e}"))?;
            let zoned = date
                .to_zoned(jiff::tz::TimeZone::UTC)
                .map_err(|e| format!("invalid {key} '{raw}': {e}"))?;
            jiff::Timestamp::from(zoned)
        }
    };
    Ok(Some(ts))
}

/// Parses a scope string into a [`Scope`].
fn parse_scope(raw: &str) -> Result<Scope, String> {
    match raw {
        "current" | "" => Ok(Scope::Current),
        "all" => Ok(Scope::All),
        rest => rest
            .strip_prefix("project:")
            .filter(|name| !name.is_empty())
            .map(|name| Scope::Project(name.to_owned()))
            .ok_or_else(|| {
                format!(
                    "invalid scope '{raw}': use \"current\", \"all\", or \"project:<name>\" \
 (known projects are listed in the error when a name is unknown)"
                )
            }),
    }
}

/// The resolved session-id set for a search, with titles for output.
#[derive(Debug, Default)]
struct ResolvedSessions {
    ids: Vec<String>,
    titles: std::collections::HashMap<String, String>,
}

/// Resolves the scope to a concrete session-id set.
///
/// `all` and `project:` enumerate `load_summaries()` (archived sessions
/// included — the summaries table is the DB, not memory). For `project:`,
/// the name is matched case-insensitively against the final path component
/// of each distinct session project path.
async fn resolve_sessions(
    scope: Scope,
    explicit: Option<String>,
    current: Option<&jinn_domain::protocol::SessionId>,
    store: &jinn_domain::feat::session::session_store::SessionStoreService,
) -> Result<ResolvedSessions, String> {
    if let Some(id) = explicit {
        return {
            let mut titles = std::collections::HashMap::new();
            titles.insert(id.clone(), String::new());
            Ok(ResolvedSessions {
                ids: vec![id],
                titles,
            })
        };
    }

    let summaries: Vec<SessionSummary> = store.load_summaries().await.map_err(|e| e.to_string())?;
    let mut titles = std::collections::HashMap::new();
    for summary in &summaries {
        titles.insert(summary.session_id.to_string(), summary.title.clone());
    }

    match scope {
        Scope::Current => {
            let Some(current) = current else {
                return Err("no current session; pass scope:\"all\" or a session_id".to_owned());
            };
            Ok(ResolvedSessions {
                ids: vec![current.to_string()],
                titles,
            })
        }
        Scope::All => Ok(ResolvedSessions {
            ids: summaries.iter().map(|s| s.session_id.to_string()).collect(),
            titles,
        }),
        Scope::Project(want) => {
            let known: Vec<String> = {
                let mut names: Vec<String> = summaries
                    .iter()
                    .filter_map(|s| s.project.as_ref())
                    .filter_map(|p| p.file_name())
                    .map(|n| n.to_string_lossy().to_string())
                    .collect();
                names.sort_by_key(|n| n.to_lowercase());
                names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
                names
            };
            let matched: Vec<String> = known
                .iter()
                .filter(|name| name.eq_ignore_ascii_case(&want))
                .cloned()
                .collect();
            if matched.len() != 1 {
                return Err(if known.is_empty() {
                    "no sessions belong to any project; scope \"project:<name>\" cannot resolve"
                        .to_owned()
                } else {
                    format!(
                        "unknown project '{want}'; known projects: {}",
                        known.join(", ")
                    )
                });
            }
            let name = matched
                .first()
                .ok_or("project match vanished between filter and select")?;
            Ok(ResolvedSessions {
                ids: summaries
                    .iter()
                    .filter(|s| {
                        s.project
                            .as_ref()
                            .and_then(|p| p.file_name())
                            .is_some_and(|n| n.eq_ignore_ascii_case(name))
                    })
                    .map(|s| s.session_id.to_string())
                    .collect(),
                titles,
            })
        }
    }
}

/// Builds the successful [`ToolResult`] from a search outcome.
fn outcome_to_result(
    outcome: &jinn_domain::feat::session_search::SearchOutcome,
    sessions: &ResolvedSessions,
    query: &str,
) -> String {
    let mut out = String::new();

    if outcome.total_matches == 0 {
        let _ = writeln!(out, "search: '{query}' — no matches");
        return out;
    }

    let _ = writeln!(
        out,
        "search: '{query}' — {} matches in {} sessions (showing {} best)",
        outcome.total_matches,
        outcome.per_session.len(),
        outcome.hits.len()
    );
    let _ = writeln!(
        out,
        "legend: # | session `id` \"title\" | date | role | `entry id` [flags] — snippet follows; <<term>> = match"
    );

    let rollup: Vec<String> = outcome
        .per_session
        .iter()
        .map(|(id, count)| {
            let title = sessions.titles.get(id).map_or("", String::as_str);
            format!("`{id}` \"{title}\" ×{count}")
        })
        .collect();
    let _ = writeln!(out, "sessions: {}", rollup.join(" · "));

    for (i, hit) in outcome.hits.iter().enumerate() {
        let title = sessions
            .titles
            .get(&hit.session_id)
            .map_or("", String::as_str);
        let date = hit.entry_ts.split('T').next().unwrap_or(&hit.entry_ts);
        let snippet = collapse_whitespace(&hit.snippet);
        let flag = if hit.excluded {
            " [excluded from context]"
        } else {
            ""
        };
        let _ = writeln!(
            out,
            "{} | `{}` \"{title}\" | {date} | {} | `{}`{flag} — {snippet}",
            i + 1,
            hit.session_id,
            hit.role.as_str(),
            hit.entry_id,
        );
    }
    out
}

/// Collapses all whitespace runs to single spaces so a hit is one line.
fn collapse_whitespace(snippet: &str) -> String {
    snippet.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Executes the `session_search` built-in tool.
///
/// # Errors
///
/// Never returns `Err`; failures are reported as `ToolResult` with
/// `success = false` carrying the reason.
pub fn execute(call: ToolCall, ctx: ToolContext) -> BoxedToolFuture {
    let args_str = call.arguments.clone();
    let tool_call_id = call.id;
    let tool_name = call.name;

    Box::pin(async move {
        let fail = |msg: String| ToolResult {
            tool_call_id: tool_call_id.clone(),
            name: tool_name.clone(),
            content: format!("Error: {msg}"),
            success: false,
            full_content: None,
            truncation: None,
            pin_position: None,
        };

        let args = match parse_args(&args_str) {
            Ok(v) => v,
            Err(e) => return fail(e),
        };

        let Some(store) = ctx.session_store else {
            return fail("session store unavailable".to_owned());
        };

        let scope = args
            .scope
            .as_deref()
            .map_or(Ok(Scope::Current), parse_scope);
        let scope = match scope {
            Ok(s) => s,
            Err(e) => return fail(e),
        };

        let sessions = match resolve_sessions(
            scope,
            args.session_id.clone(),
            ctx.session_id.as_ref(),
            &store,
        )
        .await
        {
            Ok(v) => v,
            Err(e) => return fail(e),
        };
        if sessions.ids.is_empty() {
            return fail("scope resolved to no sessions".to_owned());
        }

        let params = SearchParams {
            query: args.query.clone(),
            session_ids: sessions.ids.clone(),
            roles: args.fields.clone(),
            since: args.since,
            until: args.until,
            limit: usize::try_from(args.limit).unwrap_or(usize::MAX),
        };
        let outcome = match store.search(params).await {
            Ok(v) => v,
            Err(e) => return fail(format!("{e:?}")),
        };

        ToolResult {
            tool_call_id,
            name: tool_name,
            content: outcome_to_result(&outcome, &sessions, &args.query),
            success: true,
            full_content: None,
            truncation: None,
            pin_position: None,
        }
    })
}

#[cfg(test)]
mod tests;
