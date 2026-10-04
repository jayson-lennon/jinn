//! Compiling configured stream rules into a matcher.
//!
//! Four jobs, in order:
//!
//! 1. **Compile** each rule's `conditions` into regexes, its `scopes` into
//!    the small grammar below, and its `project` into a glob.
//! 2. **Accumulate** each stream separately, so a rule scoped to tool
//!    arguments can never fire on prose that mentions the same text.
//! 3. **Report** the first rule to match, in the order the user wrote it in
//!    `jinn.toml`.
//! 4. **Deny** a completed tool call whose arguments trip a `fail_tool`
//!    rule, independently of whether anything was streamed.
//!
//! Matching runs against the accumulated buffer, never against a single
//! chunk, so a rule's effect does not depend on how a provider chose to
//! break its output up. A rule that must act on a finished call — the
//! `fail_tool` case — is consulted over that same completed content at the
//! executor, which is why a rule is not limited to the moment something is
//! streaming past.
//!
//! Everything a malformed rule could do — a regex the engine rejects, a
//! scope token outside the grammar, an `on_trigger` that names nothing, a
//! glob that will not compile — is a warning and a skip. A typo in one rule
//! must not be able to break a turn.

use std::collections::HashMap;
use std::sync::Arc;

use globset::{Glob, GlobMatcher};
use jinn_preferences_config::schemas::{FAIL_TOOL_TRIGGER, StreamRuleConfig};
use jinn_slices::{
    RuleFired, StreamContext, StreamRuleSession, StreamRuleSet, StreamSource, TurnFires,
};
use regex::Regex;

/// Which streams a rule admits.
#[derive(Debug, Clone)]
struct Scope {
    /// The rule may fire on assistant prose.
    allow_text: bool,
    /// The rule may fire on reasoning output.
    allow_thinking: bool,
    /// The rule may fire on any tool's arguments, regardless of name or path.
    allow_any_tool: bool,
    /// The rule may fire on a named tool's arguments, optionally narrowed by
    /// a path glob.
    tool_scopes: Vec<ToolScope>,
}

/// One `tool:<name>(<glob>)` scope token.
#[derive(Debug, Clone)]
struct ToolScope {
    /// The tool this scope names, or `None` for a path-only scope.
    tool_name: Option<String>,
    /// The path glob, tested against the tool call's path-like arguments.
    path: Option<GlobMatcher>,
}

/// What a matching rule causes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuleTrigger {
    /// Interrupt the turn and resume it with the rule's guidance. The
    /// default, and what an absent `on_trigger` means.
    Interrupt,
    /// Deny the matched tool call before it runs.
    FailTool,
}

impl RuleTrigger {
    /// Resolves a configured `on_trigger`, warning and returning `None` for a
    /// value that names nothing.
    ///
    /// Rejecting the rule outright rather than defaulting it matters: an
    /// unrecognized value is a typo, and guessing would either interrupt a
    /// turn the user asked to have blocked or silently block one they asked
    /// to have interrupted.
    fn parse(rule_name: &str, value: &str) -> Option<Self> {
        match value.trim() {
            "" => Some(Self::Interrupt),
            FAIL_TOOL_TRIGGER => Some(Self::FailTool),
            other => {
                tracing::warn!(
                    rule = %rule_name,
                    value = %other,
                    known = FAIL_TOOL_TRIGGER,
                    "stream rule names an unknown on_trigger, leaving the rule inert"
                );
                None
            }
        }
    }
}

/// One compiled rule: its name, the regexes, where it may fire, and what it does.
///
/// `Clone` shares the compiled `Regex` and `GlobMatcher` values rather than
/// rebuilding them, which is what makes narrowing a set to a project cheap.
#[derive(Debug, Clone)]
struct CompiledRule {
    /// The rule's `name`, used as the fire key and reported in the log.
    name: String,
    /// The rule's `description`, carried for the message that names it.
    description: String,
    /// The rule's `body`, injected when the rule fires.
    body: String,
    /// The compiled `conditions`. A rule survives on any pattern that
    /// compiles; the others are dropped with a warning.
    conditions: Vec<Regex>,
    /// Where the rule may fire.
    scope: Scope,
    /// What a match causes.
    trigger: RuleTrigger,
    /// The projects this rule applies in; `None` means all of them.
    project: Option<GlobMatcher>,
}

impl CompiledRule {
    /// Whether this rule denies a finished tool call rather than
    /// interrupting the turn.
    ///
    /// Split out from the trigger itself because a `fail_tool` rule can
    /// still interrupt: it names what happens at the executor, and a rule
    /// scoped to a tool is perfectly well matched mid-stream too. Both
    /// outcomes are wanted, so the trigger decides what a match is worth
    /// at each site rather than gating one behaviour behind the other.
    fn denies_tools(&self) -> bool {
        matches!(self.trigger, RuleTrigger::FailTool)
    }

    /// Whether the rule may fire on `ctx`'s stream, given the tool call's
    /// path-like argument.
    ///
    /// A `tool:<name>(<glob>)` scope matches when the tool name is equal
    /// *and* any path-like argument of that call matches the glob, tested
    /// against both the full path and the bare basename so a rule written
    /// `(*.ts)` matches an absolute path. Admitting a call hands the whole
    /// argument buffer to the condition, content included — the glob decides
    /// which files the rule applies to, never what counts as a violation.
    fn admits(&self, ctx: StreamContext<'_>, args: &ToolArgs) -> bool {
        match ctx.source {
            StreamSource::Text => self.scope.allow_text,
            StreamSource::Thinking => self.scope.allow_thinking,
            StreamSource::Tool => {
                if self.scope.allow_any_tool {
                    return true;
                }
                let tool_name = ctx.tool_name.map(str::to_ascii_lowercase);
                self.scope.tool_scopes.iter().any(|scope| {
                    if let Some(expected) = &scope.tool_name
                        && tool_name.as_deref() != Some(expected.as_str())
                    {
                        return false;
                    }
                    match &scope.path {
                        Some(glob) => args.any_path_matches(glob),
                        None => true,
                    }
                })
            }
        }
    }
}

/// The path-like arguments of a tool call, extracted from its serialized JSON.
///
/// A tool call streams its arguments as partial JSON — `{"file_path": "/src/`
/// is a valid prefix — so the buffer is frequently not parseable. A tolerant
/// scanner pulls out any string value under a path-ish key without ever
/// failing, which is what keeps a `tool:edit(*.ts)` scope working from the
/// first delta rather than only once the JSON closes.
#[derive(Debug, Default)]
struct ToolArgs {
    paths: Vec<String>,
}

impl ToolArgs {
    /// Whether any extracted path matches `glob`.
    fn any_path_matches(&self, glob: &GlobMatcher) -> bool {
        self.paths.iter().any(|path| {
            let normalized = path.replace('\\', "/");
            glob.is_match(&normalized)
                || normalized
                    .rsplit('/')
                    .next()
                    .is_some_and(|base| base != normalized && glob.is_match(base))
        })
    }
}

/// Pulls out string values under path-like keys from a possibly-incomplete
/// JSON argument buffer.
///
/// Walks the buffer as characters, tracking the current key. A string value
/// whose key ends in `path`, `file`, `dir`, or `name` is retained. Unparseable
/// input is not an error: a partial buffer simply yields fewer paths.
fn scan_path_args(json: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut key = String::new();
    let mut chars = json.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => {
                let mut value = String::new();
                let mut closed = false;
                while let Some(inner) = chars.next() {
                    match inner {
                        '\\' => {
                            if let Some(escaped) = chars.next() {
                                value.push(escaped);
                            }
                        }
                        '"' => {
                            closed = true;
                            break;
                        }
                        other => value.push(other),
                    }
                }
                if !closed {
                    break;
                }
                // A string is a key when it is followed by a colon, else a
                // value. Deciding here keeps the scanner single-pass.
                while chars.peek().is_some_and(|c| c.is_whitespace()) {
                    chars.next();
                }
                if chars.peek() == Some(&':') {
                    key = value;
                } else if is_path_key(&key) {
                    paths.push(value);
                }
            }
            _ => {}
        }
    }
    paths
}

/// Whether an argument key names a path the glob scope should test.
fn is_path_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    lower.ends_with("path")
        || lower.ends_with("file")
        || lower.ends_with("dir")
        || lower.ends_with("pattern")
}

/// Compiles one configured rule, or `None` when it is unusable.
///
/// Returns `None` — after warning — for an empty body, no compiling
/// condition, or a scope that admits nothing.
fn compile_rule(config: &StreamRuleConfig) -> Option<CompiledRule> {
    let conditions: Vec<Regex> = config
        .conditions
        .iter()
        .filter_map(|pattern| match Regex::new(pattern) {
            Ok(regex) => Some(regex),
            Err(error) => {
                tracing::warn!(
                    rule = %config.name,
                    pattern = %pattern,
                    error = %error,
                    "stream rule condition does not compile, dropping the pattern"
                );
                None
            }
        })
        .collect();

    if conditions.is_empty() {
        tracing::warn!(
            rule = %config.name,
            "stream rule has no usable condition, skipping the rule"
        );
        return None;
    }

    if config.body.trim().is_empty() {
        tracing::warn!(
            rule = %config.name,
            "stream rule has an empty body, skipping the rule"
        );
        return None;
    }

    let scope = build_scope(&config.name, &config.scopes);
    if !scope.reachable() {
        tracing::warn!(
            rule = %config.name,
            scopes = ?config.scopes,
            "stream rule's scopes admit no stream, skipping the rule"
        );
        return None;
    }

    let trigger = RuleTrigger::parse(&config.name, config.on_trigger.as_deref().unwrap_or(""))?;

    // A denial needs something to deny. Without a tool scope a `fail_tool`
    // rule would match prose, find no call to refuse, and do nothing at all
    // while appearing configured -- so it is rejected here, where the warning
    // can name the rule, rather than silently inert at the executor.
    if matches!(trigger, RuleTrigger::FailTool) && !scope.reaches_tools() {
        tracing::warn!(
            rule = %config.name,
            scopes = ?config.scopes,
            trigger = FAIL_TOOL_TRIGGER,
            "stream rule denies tool calls but scopes to no tool, skipping the rule"
        );
        return None;
    }

    let project = compile_project(&config.name, config.project.as_deref().unwrap_or(""));

    Some(CompiledRule {
        name: config.name.clone(),
        description: config.description.clone(),
        body: config.body.clone(),
        conditions,
        scope,
        trigger,
        project,
    })
}

impl Scope {
    /// Whether any stream reaches this scope.
    fn reachable(&self) -> bool {
        self.allow_text
            || self.allow_thinking
            || self.allow_any_tool
            || !self.tool_scopes.is_empty()
    }

    /// Whether any tool call reaches this scope.
    fn reaches_tools(&self) -> bool {
        self.allow_any_tool || !self.tool_scopes.is_empty()
    }
}

/// Resolves a rule's scope tokens into a [`Scope`].
///
/// An absent or empty token list admits every stream. A token outside the
/// grammar is dropped with a warning naming the rule, never fatally.
fn build_scope(rule_name: &str, tokens: &[String]) -> Scope {
    if tokens.is_empty() {
        return Scope {
            allow_text: true,
            allow_thinking: true,
            allow_any_tool: true,
            tool_scopes: Vec::new(),
        };
    }

    let mut scope = Scope {
        allow_text: false,
        allow_thinking: false,
        allow_any_tool: false,
        tool_scopes: Vec::new(),
    };

    for raw in tokens {
        let token = raw.trim();
        let lower = token.to_ascii_lowercase();
        match lower.as_str() {
            "text" => {
                scope.allow_text = true;
                continue;
            }
            "thinking" => {
                scope.allow_thinking = true;
                continue;
            }
            "tool" | "toolcall" => {
                scope.allow_any_tool = true;
                continue;
            }
            _ => {}
        }

        match parse_tool_scope(token) {
            Some(tool_scope) => {
                if tool_scope.tool_name.is_none() && tool_scope.path.is_none() {
                    scope.allow_any_tool = true;
                } else {
                    scope.tool_scopes.push(tool_scope);
                }
            }
            None => {
                tracing::warn!(
                    rule = %rule_name,
                    token = %raw,
                    "stream rule scope token is outside the grammar, dropping the token"
                );
            }
        }
    }

    scope
}

/// Parses a `tool:<name>(<glob>)` token into a [`ToolScope`].
///
/// Returns `None` for anything outside the grammar. A `(<glob>)` with a
/// pattern the glob engine rejects is still a scope with a `None` path, so a
/// bad glob degrades to "any call to this tool" rather than nothing.
fn parse_tool_scope(token: &str) -> Option<ToolScope> {
    let inner = token.strip_prefix("tool:").or_else(|| {
        // A bare `<name>(<glob>)` with no `tool:` prefix is also accepted,
        // matching the bare-tool-name case in the grammar.
        if token.contains('(') {
            Some(token)
        } else {
            None
        }
    })?;
    let inner = inner.trim();

    let (name_part, path_part) = match inner.split_once('(') {
        Some((name, rest)) => {
            let path = rest.strip_suffix(')')?;
            (name.trim().to_ascii_lowercase(), Some(path.trim()))
        }
        None => (inner.to_ascii_lowercase(), None),
    };

    // A bare word with no parens and no prefix is not a tool scope unless it
    // carries the `tool:` prefix or a glob — otherwise the caller already
    // handled `tool`/`text`/`thinking` above.
    if name_part.is_empty() && path_part.is_none() {
        return None;
    }

    let path = path_part.and_then(|pattern| {
        if pattern.is_empty() {
            return None;
        }
        match Glob::new(pattern) {
            Ok(glob) => Some(glob.compile_matcher()),
            Err(error) => {
                tracing::warn!(
                    token = %token,
                    error = %error,
                    "stream rule tool scope glob does not compile, matching any call to the tool"
                );
                None
            }
        }
    });

    let tool_name = if name_part.is_empty() {
        None
    } else {
        Some(name_part)
    };

    Some(ToolScope { tool_name, path })
}

/// The compiled, shared rule set for the slice.
///
/// Immutable once built. The per-response buffers live in
/// [`MatcherSession`]; the per-turn fire record lives in the `Arc<TurnFires>`
/// shared by every response of a turn.
pub struct CompiledSet {
    rules: Arc<[CompiledRule]>,
    turns: std::sync::Mutex<HashMap<jinn_core_types::SessionId, std::sync::Arc<TurnFires>>>,
}

/// Compiles a `project` glob, warning and returning `None` for a pattern the
/// glob engine rejects.
///
/// `None` is the global case, and a rejected pattern degrades to it: the rule
/// applies everywhere rather than nowhere, because a rule the user meant to
/// restrict is far more useful over-broad than silently disarmed.
fn compile_project(rule_name: &str, pattern: &str) -> Option<GlobMatcher> {
    let trimmed = pattern.trim();
    if trimmed.is_empty() {
        return None;
    }
    match Glob::new(trimmed) {
        Ok(glob) => Some(glob.compile_matcher()),
        Err(error) => {
            tracing::warn!(
                rule = %rule_name,
                pattern = %trimmed,
                error = %error,
                "stream rule project glob does not compile, applying the rule to every project"
            );
            None
        }
    }
}

/// Whether `dir` matches `glob`.
///
/// Tested against the full path and its tail components, so a rule written
/// `**/myapp` matches a session rooted at `/home/someone/code/myapp` without
/// the user having to spell out their absolute home directory.
fn project_matches(glob: &GlobMatcher, dir: &std::path::Path) -> bool {
    let normalized = dir.to_string_lossy().replace('\\', "/");
    glob.is_match(&normalized)
        || normalized
            .split('/')
            .filter(|segment| !segment.is_empty())
            .any(|segment| glob.is_match(segment))
}

impl std::fmt::Debug for CompiledSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledSet")
            .field("rules", &self.rules.len())
            .finish()
    }
}

impl CompiledSet {
    /// Compiles every configured rule, dropping the unusable ones with a
    /// warning. A later duplicate name replaces an earlier one, warning, so
    /// the file's last word on a name is the one that fires.
    pub fn build(configs: &[StreamRuleConfig]) -> Self {
        Self::from_rules(Self::compile_all(configs))
    }

    /// Compiles and dedupes every configured rule, dropping the unusable ones.
    fn compile_all(configs: &[StreamRuleConfig]) -> Vec<CompiledRule> {
        let mut rules: Vec<CompiledRule> = Vec::new();
        for config in configs {
            let Some(compiled) = compile_rule(config) else {
                continue;
            };
            if let Some(existing) = rules.iter_mut().find(|r| r.name == compiled.name) {
                tracing::warn!(
                    rule = %compiled.name,
                    "duplicate stream rule name, the later entry replaces the earlier"
                );
                *existing = compiled;
            } else {
                rules.push(compiled);
            }
        }
        rules
    }

    /// Wraps compiled rules into the shared, immutable set.
    fn from_rules(rules: Vec<CompiledRule>) -> Self {
        Self {
            rules: rules.into(),
            turns: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// The first `fail_tool` rule whose conditions match a completed call's
    /// arguments, in configuration order.
    ///
    /// Deliberately stateless: a denial is not an interruption, so it neither
    /// consumes the turn's fire budget nor buffers anything. The call has
    /// already finished streaming by the time this is asked, so there is
    /// nothing to accumulate and nothing to reset.
    fn deny(&self, tool_name: &str, arguments: &str) -> Option<RuleFired> {
        let ctx = StreamContext::tool(0, tool_name);
        let tool_args = ToolArgs {
            paths: scan_path_args(arguments),
        };
        self.rules
            .iter()
            .filter(|rule| rule.denies_tools() && rule.admits(ctx, &tool_args))
            .find(|rule| {
                rule.conditions
                    .iter()
                    .any(|regex| regex.is_match(arguments))
            })
            .map(|rule| RuleFired {
                name: rule.name.clone(),
                description: rule.description.clone(),
                body: rule.body.clone(),
            })
    }
}

impl StreamRuleSet for CompiledSet {
    fn name(&self) -> &'static str {
        "jinn-stream-rules"
    }

    fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    fn new_session(&self, session: &jinn_core_types::SessionId) -> Box<dyn StreamRuleSession> {
        let mut turns = self
            .turns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let fires = std::sync::Arc::clone(
            turns
                .entry(session.clone())
                .or_insert_with(|| std::sync::Arc::new(TurnFires::default())),
        );
        Box::new(MatcherSession {
            rules: self.rules.clone(),
            fires,
            buffers: HashMap::new(),
        })
    }

    fn end_turn(&self, session: &jinn_core_types::SessionId) {
        let mut turns = self
            .turns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        turns.remove(session);
    }

    fn deny_tool_call(&self, tool_name: &str, arguments: &str) -> Option<RuleFired> {
        self.deny(tool_name, arguments)
    }

    fn for_project(&self, project: &std::path::Path) -> std::sync::Arc<dyn StreamRuleSet> {
        // Rules are cloned rather than recompiled: the regexes and globs are
        // already built, and only the project test is new. A rule with no
        // `project` is cloned as-is, so the common configuration -- every rule
        // global -- costs one clone per session and nothing per match.
        let narrowed: Vec<CompiledRule> = self
            .rules
            .iter()
            .filter(|rule| match &rule.project {
                Some(glob) => project_matches(glob, project),
                None => true,
            })
            .cloned()
            .collect();
        std::sync::Arc::new(Self::from_rules(narrowed))
    }
}

/// One response's accumulation buffers, and the rules they are matched
/// against.
struct MatcherSession {
    rules: Arc<[CompiledRule]>,
    fires: std::sync::Arc<TurnFires>,
    buffers: HashMap<String, String>,
}

impl std::fmt::Debug for MatcherSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MatcherSession")
            .field("rules", &self.rules.len())
            .field("buffers", &self.buffers.len())
            .finish()
    }
}

/// The buffer key for a delta: prose, reasoning, or one tool call.
fn buffer_key(ctx: StreamContext<'_>) -> String {
    match ctx.source {
        StreamSource::Text => "text".to_owned(),
        StreamSource::Thinking => "thinking".to_owned(),
        // Per-call, so two tool calls in one response cannot bleed into each
        // other's arguments.
        StreamSource::Tool => format!("tool:{}", ctx.tool_index),
    }
}

impl StreamRuleSession for MatcherSession {
    fn check(&mut self, delta: &str, ctx: StreamContext<'_>) -> Option<RuleFired> {
        let key = buffer_key(ctx);
        let buffer = self.buffers.entry(key).or_default();
        buffer.push_str(delta);

        // Path-like args are scanned from the tool buffer so a `(*.ts)` scope
        // can fire on the first delta that names the file.
        let tool_args = ToolArgs {
            paths: if ctx.source == StreamSource::Tool {
                scan_path_args(buffer)
            } else {
                Vec::new()
            },
        };

        // First match in configuration order wins.
        for rule in self.rules.iter() {
            if !rule.admits(ctx, &tool_args) {
                continue;
            }
            if rule.conditions.iter().any(|regex| regex.is_match(buffer))
                && self.fires.record(&rule.name)
            {
                return Some(RuleFired {
                    name: rule.name.clone(),
                    description: rule.description.clone(),
                    body: rule.body.clone(),
                });
            }
        }
        None
    }

    fn buffers(&self) -> &HashMap<String, String> {
        &self.buffers
    }
}
#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        reason = "test code"
    )]

    use super::{CompiledSet, scan_path_args};
    use jinn_core_types::SessionId;
    use jinn_preferences_config::schemas::StreamRuleConfig;
    use jinn_slices::{StreamContext, StreamRuleSet};

    /// A rule with the given name, one condition, and no scopes — so it
    /// admits every stream.
    fn rule(name: &str, condition: &str) -> StreamRuleConfig {
        StreamRuleConfig {
            name: name.to_owned(),
            conditions: vec![condition.to_owned()],
            body: "Follow the rule.".to_owned(),
            ..Default::default()
        }
    }

    /// The rule a session fires on its first delta of `ctx`.
    fn fired_on(
        configs: &[StreamRuleConfig],
        delta: &str,
        ctx: StreamContext<'_>,
    ) -> Option<String> {
        let set = CompiledSet::build(configs);
        let mut session = set.new_session(&SessionId::new());
        session.check(delta, ctx).map(|hit| hit.name)
    }

    #[rstest::rstest]
    fn a_rule_with_no_scopes_fires_on_prose() {
        // Given a rule with no scopes.
        let configs = [rule("todo", "TODO")];

        // When a prose delta trips it.
        let fired = fired_on(&configs, "leave a TODO here", StreamContext::text());

        // Then it fires.
        assert_eq!(fired.as_deref(), Some("todo"));
    }

    #[rstest::rstest]
    fn a_rule_with_no_scopes_fires_on_reasoning() {
        // Given a rule with no scopes.
        let configs = [rule("todo", "TODO")];

        // When a reasoning delta trips it.
        let fired = fired_on(&configs, "TODO", StreamContext::thinking());

        // Then it fires, since an absent scope admits every stream.
        assert_eq!(fired.as_deref(), Some("todo"));
    }

    #[rstest::rstest]
    fn a_rule_with_no_scopes_fires_on_tool_arguments() {
        // Given a rule with no scopes.
        let configs = [rule("todo", "TODO")];

        // When a tool-argument delta trips it.
        let fired = fired_on(
            &configs,
            r#"{"note":"TODO"}"#,
            StreamContext::tool(0, "bash"),
        );

        // Then it fires.
        assert_eq!(fired.as_deref(), Some("todo"));
    }

    #[rstest::rstest]
    fn a_text_scoped_rule_does_not_fire_on_reasoning() {
        // Given a rule scoped to prose only.
        let mut configs = [rule("todo", "TODO")];
        configs[0].scopes = vec!["text".to_owned()];

        // When a reasoning delta trips it.
        let fired = fired_on(&configs, "TODO", StreamContext::thinking());

        // Then it does not fire.
        assert!(fired.is_none());
    }

    #[rstest::rstest]
    fn a_text_scoped_rule_fires_on_prose() {
        // Given a rule scoped to prose only.
        let mut configs = [rule("todo", "TODO")];
        configs[0].scopes = vec!["text".to_owned()];

        // When a prose delta trips it.
        let fired = fired_on(&configs, "TODO", StreamContext::text());

        // Then it fires.
        assert_eq!(fired.as_deref(), Some("todo"));
    }

    #[rstest::rstest]
    fn a_prose_match_does_not_see_a_tool_stream_buffer() {
        // Given a rule scoped to every stream, on a session.
        let set = CompiledSet::build(&[rule("bad", "secret")]);
        let mut session = set.new_session(&SessionId::new());

        // When tool arguments carry the offending text and prose does not.
        session.check(r#"{"content":"secret"}"#, StreamContext::tool(0, "write"));

        // Then a prose delta does not fire, because the buffers are separate.
        assert!(
            session
                .check("harmless prose", StreamContext::text())
                .is_none()
        );
    }

    #[rstest::rstest]
    fn two_tool_calls_buffer_separately() {
        // Given a rule on every stream, on a session.
        let set = CompiledSet::build(&[rule("bad", "secret")]);
        let mut session = set.new_session(&SessionId::new());

        // When one tool call streams an argument and a second streams prose.
        session.check(r#"{"a":"secret"}"#, StreamContext::tool(0, "write"));

        // Then the second call's own buffer does not inherit the first's.
        assert!(
            session
                .check(r#"{"b":"harmless"}"#, StreamContext::tool(1, "write"))
                .is_none()
        );
    }

    #[rstest::rstest]
    fn a_tool_scoped_rule_ignores_a_different_tool() {
        // Given a rule scoped to `edit` on TypeScript.
        let mut configs = [rule("ts", ": any")];
        configs[0].scopes = vec!["tool:edit(*.ts)".to_owned()];

        // When a `write` call trips it.
        let fired = fired_on(
            &configs,
            r#"{"file_path":"src/a.ts","new_string":"let x: any"}"#,
            StreamContext::tool(0, "write"),
        );

        // Then it does not fire.
        assert!(fired.is_none());
    }

    #[rstest::rstest]
    fn a_tool_scoped_rule_fires_on_the_named_tool_and_matching_path() {
        // Given a rule scoped to `edit` on TypeScript.
        let mut configs = [rule("ts", ": any")];
        configs[0].scopes = vec!["tool:edit(*.ts)".to_owned()];

        // When an `edit` call on a TypeScript file trips it.
        let fired = fired_on(
            &configs,
            r#"{"file_path":"/repo/src/a.ts","new_string":"let x: any"}"#,
            StreamContext::tool(0, "edit"),
        );

        // Then it fires, matching the glob against the bare basename.
        assert_eq!(fired.as_deref(), Some("ts"));
    }

    #[rstest::rstest]
    fn a_glob_scope_hands_the_whole_argument_buffer_to_the_condition() {
        // Given a rule whose condition matches the *content* of an edit, and
        // a scope that selects the file by name.
        let mut config = rule("ts-no-any", ": any");
        config.scopes = vec!["tool:edit(*.ts)".to_owned()];
        let set = CompiledSet::build(&[config]);
        let mut session = set.new_session(&SessionId::new());

        // When the edit writes the forbidden text into a .ts file.
        let fired = session.check(
            r#"{"file_path":"src/app.ts","new_string":"let x: any = 5;"#,
            StreamContext::tool(0, "edit"),
        );

        // Then it fires: the glob picks the file, the regex reads the content.
        assert!(
            fired.is_some(),
            "a glob scope filters which calls apply; it must not hide the \
             argument content the condition matches against"
        );
    }

    #[rstest::rstest]
    fn a_glob_scope_still_excludes_a_file_the_glob_does_not_name() {
        // Given the same content-targeted rule.
        let mut config = rule("ts-no-any", ": any");
        config.scopes = vec!["tool:edit(*.ts)".to_owned()];
        let set = CompiledSet::build(&[config]);
        let mut session = set.new_session(&SessionId::new());

        // When the same text is written into a file outside the glob.
        let fired = session.check(
            r#"{"file_path":"src/app.py","new_string":"x: any = 5"}"#,
            StreamContext::tool(0, "edit"),
        );

        // Then it does not fire: the glob is what excludes it.
        assert!(fired.is_none());
    }

    #[rstest::rstest]
    fn a_tool_scoped_rule_ignores_a_non_matching_extension() {
        // Given a rule scoped to `edit` on TypeScript.
        let mut configs = [rule("ts", ": any")];
        configs[0].scopes = vec!["tool:edit(*.ts)".to_owned()];

        // When an `edit` call on a Rust file trips it.
        let fired = fired_on(
            &configs,
            r#"{"file_path":"src/a.rs","new_string":"let x: any"}"#,
            StreamContext::tool(0, "edit"),
        );

        // Then it does not fire.
        assert!(fired.is_none());
    }

    #[rstest::rstest]
    fn a_tool_scoped_rule_fires_from_a_partial_argument_buffer() {
        // Given a rule scoped to `edit` on TypeScript.
        let mut configs = [rule("ts", ": any")];
        configs[0].scopes = vec!["tool:edit(*.ts)".to_owned()];
        let set = CompiledSet::build(&configs);
        let mut session = set.new_session(&SessionId::new());

        // When the path arrives before the JSON value closes.
        session.check(
            r#"{"file_path":"/repo/src/a.ts","new_string":"x: any"#,
            StreamContext::tool(0, "edit"),
        );

        // Then it fires, because the path scan tolerates an incomplete buffer.
        assert!(
            session
                .check(r#"y"}"#, StreamContext::tool(0, "edit"))
                .is_some()
        );
    }

    #[rstest::rstest]
    fn a_bare_tool_scope_token_admits_every_tool() {
        // Given a rule scoped to tools generally.
        let mut configs = [rule("todo", "TODO")];
        configs[0].scopes = vec!["tool".to_owned()];

        // When a tool call trips it.
        let fired = fired_on(&configs, r#"{"c":"TODO"}"#, StreamContext::tool(0, "bash"));

        // Then it fires.
        assert_eq!(fired.as_deref(), Some("todo"));
    }

    #[rstest::rstest]
    fn an_out_of_grammar_scope_token_is_dropped_without_disarming_the_rule() {
        // Given a rule with one valid and one invalid scope token.
        let mut configs = [rule("todo", "TODO")];
        configs[0].scopes = vec!["text".to_owned(), "not-a-scope!!".to_owned()];
        let set = CompiledSet::build(&configs);

        // When the set is built and prose trips it.
        let mut session = set.new_session(&SessionId::new());

        // Then the rule still fires on the stream its valid token named.
        assert!(session.check("TODO", StreamContext::text()).is_some());
    }

    #[rstest::rstest]
    fn a_rule_whose_scopes_admit_nothing_is_skipped() {
        // Given a rule whose every scope token is outside the grammar.
        let mut configs = [rule("dead", "TODO")];
        configs[0].scopes = vec!["!!!".to_owned(), "###".to_owned()];

        // When the set is built.
        let set = CompiledSet::build(&configs);

        // Then the rule did not survive.
        assert!(set.is_empty());
    }

    #[rstest::rstest]
    fn an_uncompilable_condition_is_dropped_and_the_rule_survives_on_its_others() {
        // Given a rule with one bad and one good pattern.
        let mut configs = [rule("todo", "TODO")];
        configs[0].conditions = vec!["(".to_owned(), "TODO".to_owned()];
        let set = CompiledSet::build(&configs);

        // When prose trips the surviving pattern.
        let mut session = set.new_session(&SessionId::new());

        // Then the rule still fires.
        assert!(session.check("TODO", StreamContext::text()).is_some());
    }

    #[rstest::rstest]
    fn a_rule_with_no_compiling_condition_is_skipped() {
        // Given a rule whose only pattern is invalid.
        let mut configs = [rule("dead", "(")];
        configs[0].conditions = vec!["(".to_owned()];
        let set = CompiledSet::build(&configs);

        // When the set is built.
        let empty = set.is_empty();

        // Then the rule did not survive.
        assert!(empty);
    }

    #[rstest::rstest]
    fn a_rule_with_an_empty_body_is_skipped() {
        // Given a rule with nothing to inject.
        let mut configs = [rule("silent", "TODO")];
        configs[0].body = "   ".to_owned();
        let set = CompiledSet::build(&configs);

        // When the set is built.
        let empty = set.is_empty();

        // Then the rule did not survive, since firing it would interrupt the
        // turn with nothing to say.
        assert!(empty);
    }

    #[rstest::rstest]
    fn a_rule_with_no_conditions_is_skipped() {
        // Given a rule with no patterns at all.
        let mut configs = [rule("vacuous", "TODO")];
        configs[0].conditions = Vec::new();
        let set = CompiledSet::build(&configs);

        // When the set is built.
        let empty = set.is_empty();

        // Then the rule did not survive.
        assert!(empty);
    }

    #[rstest::rstest]
    fn the_first_matching_rule_in_configuration_order_wins() {
        // Given two rules that both match the same delta, second written first.
        let configs = [
            rule("first-written", "TODO"),
            rule("second-written", "TODO"),
        ];

        // When the delta trips both.
        let fired = fired_on(&configs, "TODO", StreamContext::text());

        // Then the one written first in the list wins.
        assert_eq!(fired.as_deref(), Some("first-written"));
    }

    #[rstest::rstest]
    fn a_duplicate_name_replaces_the_earlier_entry() {
        // Given two entries sharing a name, with different bodies.
        let first = rule("dup", "alpha");
        let mut second = rule("dup", "beta");
        second.body = "the later body".to_owned();
        let set = CompiledSet::build(&[first, second]);
        let mut session = set.new_session(&SessionId::new());

        // When the earlier entry's condition is tripped.
        let earlier = session.check("alpha", StreamContext::text());

        // Then only the later entry survived.
        assert!(earlier.is_none());
        let later = session
            .check("beta", StreamContext::text())
            .expect("later fires");
        assert_eq!(later.body, "the later body");
    }

    #[rstest::rstest]
    fn a_set_with_no_rules_reports_itself_empty() {
        // Given an empty configuration.
        let set = CompiledSet::build(&[]);

        // When emptiness is consulted.
        let empty = set.is_empty();

        // Then no session is ever minted, so the loop pays nothing.
        assert!(empty);
    }

    #[rstest::rstest]
    fn a_fired_rule_does_not_fire_again_in_the_same_turn_past_the_cap() {
        // Given a rule and one turn's session.
        let set = CompiledSet::build(&[rule("todo", "TODO")]);
        let mut session = set.new_session(&SessionId::new());

        // When it is tripped repeatedly.
        let fired: Vec<bool> = (0..4)
            .map(|_| session.check("TODO", StreamContext::text()).is_some())
            .collect();

        // Then it fires three times and is ignored after that.
        assert_eq!(fired, vec![true, true, true, false]);
    }

    #[rstest::rstest]
    fn ending_a_turn_lets_the_rule_fire_again() {
        // Given a rule whose turn has exhausted its cap.
        let set = CompiledSet::build(&[rule("todo", "TODO")]);
        let id = SessionId::new();
        {
            let mut session = set.new_session(&id);
            for _ in 0..3 {
                session.check("TODO", StreamContext::text());
            }
            assert!(session.check("TODO", StreamContext::text()).is_none());
        }

        // When the turn ends and a new one begins.
        set.end_turn(&id);
        let mut next = set.new_session(&id);

        // Then the rule can fire again.
        assert!(next.check("TODO", StreamContext::text()).is_some());
    }

    #[rstest::rstest]
    fn each_response_of_a_turn_starts_with_empty_buffers() {
        // Given a rule, on two responses of one turn.
        let set = CompiledSet::build(&[rule("multi", "alpha")]);
        let id = SessionId::new();
        let mut first = set.new_session(&id);
        first.check("alpha", StreamContext::text());

        // When a second response begins.
        let second = set.new_session(&id);

        // Then its buffer is empty, so an aborted attempt's text cannot leak
        // into the retry's match.
        assert!(second.buffers().values().all(String::is_empty));
    }

    #[rstest::rstest]
    fn a_path_argument_is_scanned_out_of_a_partial_buffer() {
        // Given a partial argument buffer naming a file.
        let partial = r#"{"file_path":"/repo/src/a.ts","new_str"#;

        // When it is scanned.
        let paths = scan_path_args(partial);

        // Then the path is found even though the JSON never closes.
        assert_eq!(paths, vec!["/repo/src/a.ts".to_owned()]);
    }

    #[rstest::rstest]
    fn a_non_path_argument_is_not_scanned() {
        // Given a partial buffer carrying only a non-path argument.
        let partial = r#"{"old_string":"x","new_str"#;

        // When it is scanned.
        let paths = scan_path_args(partial);

        // Then nothing is retained.
        assert!(paths.is_empty());
    }

    /// A rule scoped to `scope` and carrying `on_trigger`.
    fn scoped_rule(name: &str, condition: &str, scope: &str, on_trigger: &str) -> StreamRuleConfig {
        StreamRuleConfig {
            name: name.to_owned(),
            conditions: vec![condition.to_owned()],
            scopes: vec![scope.to_owned()],
            body: "Follow the rule.".to_owned(),
            on_trigger: Some(on_trigger.to_owned()),
            ..Default::default()
        }
    }

    #[rstest::rstest]
    #[test]
    fn a_rule_denying_a_tool_call_does_not_deny_a_different_tool() {
        // Given a rule scoped to one tool's arguments.
        let configs = [scoped_rule("no-bash", "rm -rf", "tool:bash", "fail_tool")];

        // When the executor consults the set about a different tool.
        let denied = CompiledSet::build(&configs).deny_tool_call("read", r#"{"path":"rm -rf"}"#);

        // Then the call is allowed, because the rule does not scope to it.
        assert!(denied.is_none());
    }

    #[rstest::rstest]
    #[test]
    fn a_rule_denying_a_tool_call_does_deny_a_matching_call() {
        // Given a rule scoped to one tool's arguments.
        let configs = [scoped_rule("no-bash", "rm -rf", "tool:bash", "fail_tool")];

        // When the executor consults the set about that tool.
        let denied = CompiledSet::build(&configs)
            .deny_tool_call("bash", r#"{"command":"rm -rf /"}"#)
            .map(|hit| hit.name);

        // Then the call is denied, naming the rule.
        assert_eq!(denied, Some("no-bash".to_owned()));
    }

    #[rstest::rstest]
    #[test]
    fn a_denial_is_the_same_however_many_pieces_the_arguments_arrived_in() {
        // Given a rule scoped to one tool's arguments.
        let configs = [scoped_rule("no-bash", "rm -rf", "tool:bash", "fail_tool")];
        let arguments = r#"{"command":"rm -rf /"}"#;
        let set = CompiledSet::build(&configs);

        // When the executor consults the set once per streamed fragment.
        let whole = set.deny_tool_call("bash", arguments).map(|hit| hit.name);
        let piecemeal = set.deny_tool_call("bash", arguments).map(|hit| hit.name);

        // Then the denial does not depend on the delivery.
        assert_eq!(whole, Some("no-bash".to_owned()));
        assert_eq!(whole, piecemeal);
    }

    #[rstest::rstest]
    #[test]
    fn an_unknown_on_trigger_leaves_the_rule_inert() {
        // Given a rule naming an on_trigger that means nothing.
        let configs = [scoped_rule("typo", "rm -rf", "tool:bash", "fail_tol")];

        // When the executor consults the set about a matching call.
        let denied =
            CompiledSet::build(&configs).deny_tool_call("bash", r#"{"command":"rm -rf /"}"#);

        // Then the rule does nothing at all.
        assert!(denied.is_none());
    }

    #[rstest::rstest]
    #[test]
    fn an_unknown_on_trigger_does_not_stop_a_sibling_rule_from_firing() {
        // Given a rule with a typo'd trigger beside a rule that is fine.
        let configs = [
            scoped_rule("typo", "rm -rf", "tool:bash", "fail_tol"),
            scoped_rule("good", "rm -rf", "tool:bash", "fail_tool"),
        ];

        // When the executor consults the set about a matching call.
        let denied = CompiledSet::build(&configs)
            .deny_tool_call("bash", r#"{"command":"rm -rf /"}"#)
            .map(|hit| hit.name);

        // Then the working rule still denies it.
        assert_eq!(denied, Some("good".to_owned()));
    }

    #[rstest::rstest]
    #[test]
    fn a_denial_rule_scoped_to_no_tool_is_skipped() {
        // Given a rule that denies tool calls but scopes only to prose.
        let configs = [scoped_rule("mismatched", "rm -rf", "text", "fail_tool")];

        // When the executor consults the set about a matching call.
        let denied =
            CompiledSet::build(&configs).deny_tool_call("bash", r#"{"command":"rm -rf /"}"#);

        // Then the rule was dropped rather than left armed with nothing to do.
        assert!(denied.is_none());
    }

    #[rstest::rstest]
    #[test]
    fn a_rule_with_no_project_applies_in_every_project() {
        // Given a rule with no project glob.
        let configs = [rule("global", "rm -rf")];

        // When the set is built for a project it names nothing about.
        let set =
            CompiledSet::build(&configs).for_project(std::path::Path::new("/anywhere/at/all"));

        // Then the rule is live there.
        assert!(!set.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn a_project_scoped_rule_applies_in_a_matching_project() {
        // Given a rule scoped to one project by name.
        let configs = [StreamRuleConfig {
            project: Some("myapp".to_owned()),
            ..rule("scoped", "rm -rf")
        }];

        // When the set is built for that project.
        let set =
            CompiledSet::build(&configs).for_project(std::path::Path::new("/home/dev/code/myapp"));

        // Then the rule is live.
        assert!(!set.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn a_project_scoped_rule_is_inactive_in_another_project() {
        // Given a rule scoped to one project by name.
        let configs = [StreamRuleConfig {
            project: Some("myapp".to_owned()),
            ..rule("scoped", "rm -rf")
        }];

        // When the set is built for a different project.
        let set =
            CompiledSet::build(&configs).for_project(std::path::Path::new("/home/dev/code/other"));

        // Then the rule is not there at all.
        assert!(set.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn a_global_rule_stays_live_where_a_project_rule_is_scoped_out() {
        // Given a global rule beside one scoped to a single project.
        let configs = [
            rule("global", "rm -rf"),
            StreamRuleConfig {
                project: Some("myapp".to_owned()),
                ..rule("scoped", "curl")
            },
        ];

        // When the set is built for the project the scoped rule excludes.
        let set = CompiledSet::build(&configs).for_project(std::path::Path::new("/home/dev/other"));

        // Then the global rule still fires there and the scoped one does not.
        let mut session = set.new_session(&SessionId::new());
        assert_eq!(
            session
                .check("rm -rf", StreamContext::text())
                .map(|hit| hit.name),
            Some("global".to_owned())
        );
        let mut next = set.new_session(&SessionId::new());
        assert!(
            next.check("curl", StreamContext::text()).is_none(),
            "the rule scoped to myapp must be inactive outside it"
        );
    }

    #[rstest::rstest]
    #[test]
    fn a_project_glob_that_does_not_compile_applies_everywhere() {
        // Given a rule whose project glob the engine rejects.
        let configs = [StreamRuleConfig {
            project: Some("***[".to_owned()),
            ..rule("bad-glob", "rm -rf")
        }];

        // When the set is built for an unrelated project.
        let set = CompiledSet::build(&configs).for_project(std::path::Path::new("/home/dev/other"));

        // Then the rule applies over-broad rather than being disarmed.
        assert!(!set.is_empty());
    }
}
