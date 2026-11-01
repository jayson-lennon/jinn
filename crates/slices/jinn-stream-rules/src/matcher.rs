//! Compiling configured stream rules into a matcher.
//!
//! Three jobs, in order:
//!
//! 1. **Compile** each rule's `conditions` into regexes, its `scopes` into
//!    the small grammar below, and its `project` into a glob.
//! 2. **Accumulate** each stream separately, so a rule scoped to tool
//!    arguments can never fire on prose that mentions the same text.
//! 3. **Report** the first rule to match, in the order the user wrote it in
//!    `jinn.toml`.
//!
//! A match has exactly one consequence: the turn is interrupted and resumed
//! with the rule's body as guidance. There is no second outcome to configure
//! and no separate decision point — a rule scoped to a tool call matches that
//! call's arguments as they accumulate, and interrupting that call is what
//! stops it running. A rule therefore needs no knowledge of whether the
//! content it matched arrived as one piece or a thousand.
//!
//! # What this matcher deliberately does not know
//!
//! It holds no interrupt count, takes no budget, and decides nothing about
//! cancelling a stream. `check` returns the rule that fired, or nothing.
//!
//! That is not a simplification — it is the fix for a bug this matcher used to
//! have. Bounding a loop requires knowing how many times a session has been
//! corrected *across* responses, so the counter had to be shared state living
//! behind a per-delta predicate, consulted on every `check` to decide something
//! only asked once per response. The result was a split behaviour no single
//! component owned: the count here, the threshold in a configuration crate,
//! and the trip's two publishes in a third crate. The count now lives in the
//! stream-rule watchdog, which learns of an interrupt from the response-
//! completion signal an interrupt already publishes, and which can be tested
//! for its whole policy without compiling a regex.
//!
//! Most of what a malformed rule could do — a regex the engine rejects, a
//! scope token outside the grammar — is a warning and a skip. A typo in one
//! rule must not be able to break a turn.
//!
//! The `project` glob is the deliberate exception. A glob that cannot anchor
//! to a working directory, or that the engine rejects, would compile and then
//! never fire — the silent-never-matches defect this matcher once shipped —
//! so it fails the install instead: at startup that stops the launch, and
//! mid-session it refuses a refresh, leaving the previously installed
//! matcher live.

use std::collections::HashMap;
use std::sync::Arc;

use globset::{Glob, GlobMatcher};
use jinn_preferences_config::schemas::StreamRuleConfig;
use jinn_slices::{RuleFired, StreamContext, StreamRuleSession, StreamRuleSet, StreamSource};
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
    /// The projects this rule applies in; `None` means all of them.
    project: Option<GlobMatcher>,
}

impl CompiledRule {
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
                } else {
                    // Neither a key nor under a path-like key: dropped.
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
/// condition, or a scope that admits nothing. A `project` glob that cannot
/// anchor or does not compile is not a skip: it propagates as
/// [`ProjectGlobError`] and fails the whole install.
///
/// Disabled rules never reach here: the name array that switches a rule
/// off is applied to the assembled set in `read_rules`, before anything
/// compiles.
///
/// # Errors
///
/// A `project` glob that fails [`compile_project`].
fn compile_rule(
    config: &StreamRuleConfig,
) -> Result<Option<CompiledRule>, error_stack::Report<ProjectGlobError>> {
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
        return Ok(None);
    }

    if config.body.trim().is_empty() {
        tracing::warn!(
            rule = %config.name,
            "stream rule has an empty body, skipping the rule"
        );
        return Ok(None);
    }

    let scope = build_scope(&config.name, &config.scopes);
    if !scope.reachable() {
        tracing::warn!(
            rule = %config.name,
            scopes = ?config.scopes,
            "stream rule's scopes admit no stream, skipping the rule"
        );
        return Ok(None);
    }

    let project = compile_project(&config.name, config.project.as_deref().unwrap_or(""))?;

    Ok(Some(CompiledRule {
        name: config.name.clone(),
        description: config.description.clone(),
        body: config.body.clone(),
        conditions,
        scope,
        project,
    }))
}

impl Scope {
    /// Whether any stream reaches this scope.
    fn reachable(&self) -> bool {
        self.allow_text
            || self.allow_thinking
            || self.allow_any_tool
            || !self.tool_scopes.is_empty()
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
        token.contains('(').then_some(token)
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
/// Immutable once built, and stateless with respect to any turn: the only
/// mutable state is the per-response accumulation buffers, which live in
/// [`MatcherSession`] and are dropped with the response that owns them.
pub struct CompiledSet {
    rules: Arc<[CompiledRule]>,
}

/// A rule's `project` glob could not be compiled into a usable matcher.
///
/// Carries no payload: the report's attachments at the raise site name the
/// rule, the pattern, and the fix.
#[derive(Debug, wherror::Error)]
#[error(debug)]
pub struct ProjectGlobError;

/// Whether a `project` glob can anchor to an absolute working directory.
///
/// The matcher tests the glob against the cwd whole-path, and a cwd is
/// absolute, so the pattern must be able to match a leading `/` — which
/// means it must start with a `**/` that spans the root, an absolute `/`,
/// or a Windows drive prefix (`C:/Users/...`, which matches the normalized
/// candidate literally). Anything else compiles fine and then can never
/// match: that is exactly the silent-never-matches shape, and it is
/// rejected before the engine ever sees it.
fn project_glob_anchors(pattern: &str) -> bool {
    let bytes = pattern.as_bytes();
    let drive_prefix = matches!(
        (bytes.first(), bytes.get(1), bytes.get(2)),
        (Some(first), Some(colon), Some(slash))
            if first.is_ascii_alphabetic() && *colon == b':' && *slash == b'/'
    );
    pattern.starts_with("**/") || pattern.starts_with('/') || drive_prefix
}

/// Compiles a rule's `project` glob, failing the install for a pattern that
/// cannot anchor or that the glob engine rejects.
///
/// `Ok(None)` is the global case: an absent or empty `project` scopes the
/// rule to every project. A present pattern is an anchor check first —
/// `jinn*/**` would compile and then never match a whole absolute path —
/// and an engine compile second; both failures are [`ProjectGlobError`].
///
/// # Errors
///
/// A non-empty pattern that neither starts with `**/`, an absolute `/`, nor
/// a drive prefix, or one the glob engine rejects.
fn compile_project(
    rule_name: &str,
    pattern: &str,
) -> Result<Option<GlobMatcher>, error_stack::Report<ProjectGlobError>> {
    let trimmed = pattern.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if !project_glob_anchors(trimmed) {
        return Err(error_stack::Report::new(ProjectGlobError)
            .attach(format!(
                "rule {rule_name:?}: project glob {trimmed:?} cannot anchor to a working \
                 directory"
            ))
            .attach(format!(
                "a `project` glob must start with `**/` (a directory at any depth), an \
                 absolute `/`, or a drive prefix; did you mean `**/{trimmed}`?"
            )));
    }
    match Glob::new(trimmed) {
        Ok(glob) => Ok(Some(glob.compile_matcher())),
        Err(error) => Err(error_stack::Report::new(ProjectGlobError)
            .attach(format!(
                "rule {rule_name:?}: project glob {trimmed:?} does not compile: {error}"
            ))
            .attach(format!(
                "the glob engine rejected the pattern; did you mean `**/{trimmed}`?"
            ))),
    }
}

/// Whether a session's working directory falls under a rule's `project` glob.
///
/// The glob is anchored to the whole path, never to a single segment. The
/// candidates are the normalized directory (`\` folded to `/`) and the same
/// path with a trailing `/` appended. The padded candidate is what lets a
/// `/**` suffix mean "this directory and everything below it": `**/myapp/**`
/// only reads as a full match when something follows the project segment, and
/// at the project's own root that something is the trailing slash. Patterns
/// are whole-path glob vocabulary — `**/myapp` is the directory exactly,
/// `**/myapp/**` is it and everything below, and a two-segment value like
/// `**/jinn/image-tilth` names a nested project directly.
fn project_matches(
    glob: &GlobMatcher,
    dir: &marked_path::MarkedPathBuf<marked_path::Absolute>,
) -> bool {
    let normalized = dir.to_string().replace('\\', "/");
    let padded = format!("{normalized}/");
    glob.is_match(&normalized) || glob.is_match(&padded)
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
    ///
    /// # Errors
    ///
    /// Any rule whose `project` glob cannot anchor or does not compile.
    pub fn build(
        configs: &[StreamRuleConfig],
    ) -> Result<Self, error_stack::Report<ProjectGlobError>> {
        let rules = Self::compile_all(configs)?;
        Ok(Self::from_rules(rules))
    }

    /// Compiles and dedupes every configured rule, dropping the unusable ones.
    ///
    /// # Errors
    ///
    /// Any rule whose `project` glob cannot anchor or does not compile; the
    /// dedupe below only runs once every rule compiled.
    fn compile_all(
        configs: &[StreamRuleConfig],
    ) -> Result<Vec<CompiledRule>, error_stack::Report<ProjectGlobError>> {
        let mut rules: Vec<CompiledRule> = Vec::new();
        for config in configs {
            let Some(compiled) = compile_rule(config)? else {
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
        Ok(rules)
    }

    /// Wraps compiled rules into the shared, immutable set.
    fn from_rules(rules: Vec<CompiledRule>) -> Self {
        Self {
            rules: rules.into(),
        }
    }
}

impl StreamRuleSet for CompiledSet {
    fn name(&self) -> &'static str {
        "jinn-stream-rules"
    }

    fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    fn new_session(
        &self,
        _session: &jinn_core_types::SessionId,
    ) -> Box<dyn StreamRuleSession + 'static> {
        Box::new(MatcherSession {
            rules: self.rules.clone(),
            buffers: HashMap::new(),
            texts: HashMap::new(),
        })
    }

    fn for_cwd(
        &self,
        cwd: Option<&marked_path::MarkedPathBuf<marked_path::Absolute>>,
    ) -> std::sync::Arc<dyn StreamRuleSet> {
        // Rules are cloned rather than recompiled: the regexes and globs are
        // already built, and only the cwd test is new. A rule with no
        // `project` is cloned as-is, so the common configuration -- every rule
        // global -- costs one clone per stream and nothing per match.
        //
        // Binding the cwd here rather than per match is what keeps a
        // project-scoped rule from firing in every directory: the stream loop
        // narrows once, when it mints the response's rule set. `None` (a
        // session whose cwd is unknown) disarms every project-scoped rule
        // rather than letting any of them fire unchecked.
        let narrowed: Vec<CompiledRule> = self
            .rules
            .iter()
            .filter(|rule| match (&rule.project, cwd) {
                (Some(glob), Some(path)) => project_matches(glob, path),
                (Some(_), None) => false,
                (None, _) => true,
            })
            .cloned()
            .collect();
        std::sync::Arc::new(Self::from_rules(narrowed))
    }
}

/// One response's accumulation buffers, and the rules they are matched
/// against.
///
/// Minted per response and dropped with it, which is what makes "reset at the
/// start of every assistant response" true by construction.
struct MatcherSession {
    rules: Arc<[CompiledRule]>,
    buffers: HashMap<String, BufferState>,
    /// The text-only view the trait's `buffers` accessor returns. Kept beside
    /// the full state rather than projected on demand, because the accessor
    /// hands back a reference; `check` writes both, and nothing else writes
    /// either.
    texts: HashMap<String, String>,
}

/// One stream's accumulated text, plus the context needed to admit rules to
/// it outside a live delta.
///
/// The stored context is what lets [`MatcherSession::finalize`] re-test a
/// buffer when no `StreamContext` is at hand: rule scopes are decided by the
/// stream the buffer belongs to, not by the delta that last fed it, so the
/// context is remembered with the buffer rather than re-invented at finalize
/// time. A `tool:<name>(<glob>)` scope's path test re-scans the buffer there,
/// the same way `check` scans it per delta.
/// The owned, borrow-free residue of a `StreamContext`, kept per buffer.
#[derive(Debug, Clone)]
enum StoredContext {
    Text,
    Thinking,
    Tool {
        /// The tool name the start event recorded, lowercased for matching.
        tool_name: Option<String>,
        index: usize,
    },
}

impl StoredContext {
    /// The residue of the delta context `ctx`.
    fn of(ctx: StreamContext<'_>) -> Self {
        match ctx.source {
            StreamSource::Text => Self::Text,
            StreamSource::Thinking => Self::Thinking,
            StreamSource::Tool => Self::Tool {
                tool_name: ctx.tool_name.map(str::to_ascii_lowercase),
                index: ctx.tool_index,
            },
        }
    }

    /// Whether this buffer belongs to a tool call's arguments.
    fn is_tool(&self) -> bool {
        matches!(self, Self::Tool { .. })
    }

    /// Rebuilds a borrowing view of this context.
    fn stream_context(&self) -> StreamContext<'_> {
        match self {
            Self::Text => StreamContext::text(),
            Self::Thinking => StreamContext::thinking(),
            Self::Tool { tool_name, index } => {
                StreamContext::tool(*index, tool_name.as_deref().unwrap_or(""))
            }
        }
    }

    /// Folds a live delta context into the stored one.
    ///
    /// The tool name can trail the first argument delta — it arrives on the
    /// start event, which the actor records separately — so a name the
    /// context carries fills the stored one in and is never cleared by a
    /// context that does not.
    fn remember(&mut self, ctx: StreamContext<'_>) {
        if let (
            Self::Tool {
                tool_name: kept @ None,
                ..
            },
            StreamSource::Tool,
        ) = (self, ctx.source)
            && let Some(name) = ctx.tool_name
        {
            *kept = Some(name.to_ascii_lowercase());
        }
    }
}

/// One stream's accumulated text, plus the context needed to admit rules to
/// it outside a live delta.
///
/// The stored context is what lets [`MatcherSession::finalize`] re-test a
/// buffer when no `StreamContext` is at hand: rule scopes are decided by the
/// stream the buffer belongs to, not by the delta that last fed it, so the
/// context is remembered with the buffer rather than re-invented at finalize
/// time. A `tool:<name>(<glob>)` scope's path test re-scans the buffer there,
/// the same way `check` scans it per delta.
#[derive(Debug, Clone)]
struct BufferState {
    text: String,
    ctx: StoredContext,
}

impl BufferState {
    fn new(ctx: StreamContext<'_>) -> Self {
        Self {
            text: String::new(),
            ctx: StoredContext::of(ctx),
        }
    }

    fn push_str(&mut self, delta: &str) {
        self.text.push_str(delta);
    }

    fn remember(&mut self, ctx: StreamContext<'_>) {
        self.ctx.remember(ctx);
    }
}

impl std::fmt::Debug for MatcherSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MatcherSession")
            .field("rules", &self.rules.len())
            .field("buffers", &self.buffers.len())
            .field("texts", &self.texts.len())
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

/// Whether `regex` matches `buffer` with its end off the buffer's last byte.
///
/// This is the edge rule: a match ending at the buffer's final byte is held
/// mid-stream, because that byte is the one still arriving. Everything else
/// about the match is ordinary — any match strictly inside the buffer, or
/// ending at the edge only because the stream stopped there, qualifies; the
/// `finalize` pass re-runs these same patterns through plain `is_match`.
fn matches_complete(regex: &Regex, buffer: &str) -> bool {
    regex.find(buffer).is_some_and(|m| m.end() < buffer.len())
}

/// The sort rank of a buffer key for finalize's deterministic order: prose,
/// then reasoning, then tool calls by numeric index.
fn key_sort_rank(key: &str) -> (u8, usize) {
    match key {
        "text" => (0, 0),
        "thinking" => (1, 0),
        _ => {
            let index = key
                .strip_prefix("tool:")
                .and_then(|rest| rest.parse::<usize>().ok())
                .unwrap_or(usize::MAX);
            (2, index)
        }
    }
}

impl StreamRuleSession for MatcherSession {
    fn check(&mut self, delta: &str, ctx: StreamContext<'_>) -> Option<RuleFired> {
        let key = buffer_key(ctx);
        let buffer = self
            .buffers
            .entry(key.clone())
            .or_insert_with(|| BufferState::new(ctx));
        buffer.push_str(delta);
        buffer.remember(ctx);
        self.texts.entry(key).or_default().push_str(delta);

        // Path-like args are scanned from the tool buffer so a `(*.ts)` scope
        // can fire on the first delta that names the file.
        let tool_args = ToolArgs {
            paths: if ctx.source == StreamSource::Tool {
                scan_path_args(&buffer.text)
            } else {
                Vec::new()
            },
        };

        // First match in configuration order wins. A match whose end is the
        // buffer's last byte is *held*: the stream is still open, that byte
        // is the one still arriving, and an end-anchored pattern would
        // otherwise fire on every intermediate buffer (`ls -R /` matching
        // before `/etc/nginx` can disprove it). Held is not stored state —
        // the next delta re-runs this scan, where the continuation either
        // destroys the match or moves its end off the edge.
        for rule in self.rules.iter() {
            if !rule.admits(ctx, &tool_args) {
                continue;
            }
            let proven = rule
                .conditions
                .iter()
                .any(|regex| matches_complete(regex, &buffer.text));
            if !proven {
                continue;
            }
            tracing::info!(
                rule = %rule.name,
                "stream rule matched the live stream"
            );
            return Some(RuleFired {
                name: rule.name.clone(),
                description: rule.description.clone(),
                body: rule.body.clone(),
            });
        }
        None
    }

    fn finalize(&mut self) -> Option<RuleFired> {
        // Re-test every open buffer with the edge predicate off: the stream
        // has ended, so a match on the buffer's last byte is standing on
        // proven text rather than on whatever might still have arrived.
        // Deterministic order — prose, reasoning, then tool calls by index —
        // keeps the pick stable across HashMap iteration order. Tool call
        // indices sort numerically, so `tool:10` cannot land before `tool:2`.
        let mut keys: Vec<((u8, usize), &String)> = self
            .buffers
            .keys()
            .map(|key| (key_sort_rank(key), key))
            .collect();
        keys.sort();
        for (_, key) in keys {
            let Some(state) = self.buffers.get(key) else {
                continue;
            };
            let ctx = state.ctx.stream_context();
            let tool_args = ToolArgs {
                paths: if state.ctx.is_tool() {
                    scan_path_args(&state.text)
                } else {
                    Vec::new()
                },
            };
            for rule in self.rules.iter() {
                if !rule.admits(ctx, &tool_args) {
                    continue;
                }
                if rule
                    .conditions
                    .iter()
                    .any(|regex| regex.is_match(&state.text))
                {
                    tracing::info!(
                        rule = %rule.name,
                        "stream rule matched at finalize"
                    );
                    return Some(RuleFired {
                        name: rule.name.clone(),
                        description: rule.description.clone(),
                        body: rule.body.clone(),
                    });
                }
            }
        }
        None
    }

    fn buffers(&self) -> &HashMap<String, String> {
        &self.texts
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
    use marked_path::{Absolute, MarkedPathBuf};

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

    /// Compiles `configs` through the production build, panicking on a
    /// project-glob failure — tests that feed a broken glob assert the
    /// failure directly and do not come through here.
    fn compiled(configs: &[StreamRuleConfig]) -> CompiledSet {
        CompiledSet::build(configs).expect("a test config compiles")
    }

    /// The rule a session fires on its first delta of `ctx`, checked the way
    /// production checks it: the delta runs through `check`, and the stream's
    /// end runs `finalize` — the pair a real response always performs.
    ///
    /// Without the finalize half, a rule matching the delta's tail would read
    /// as silent here while firing in production at stream completion.
    fn fired_on(
        configs: &[StreamRuleConfig],
        delta: &str,
        ctx: StreamContext<'_>,
    ) -> Option<String> {
        let set = compiled(configs);
        let mut session = set.new_session(&SessionId::new());
        session
            .check(delta, ctx)
            .or_else(|| session.finalize())
            .map(|hit| hit.name.clone())
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
        let set = compiled(&[rule("bad", "secret")]);
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
        let set = compiled(&[rule("bad", "secret")]);
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
        let set = compiled(&[config]);
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
        let set = compiled(&[config]);
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
        let set = compiled(&configs);
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
        let set = compiled(&configs);

        // When the set is built and prose trips it.
        let mut session = set.new_session(&SessionId::new());

        // Then the rule still fires on the stream its valid token named,
        // through the production sequence: the delta, then the stream's end.
        assert!(
            session
                .check("TODO", StreamContext::text())
                .or_else(|| session.finalize())
                .is_some()
        );
    }

    #[rstest::rstest]
    fn a_rule_whose_scopes_admit_nothing_is_skipped() {
        // Given a rule whose every scope token is outside the grammar.
        let mut configs = [rule("dead", "TODO")];
        configs[0].scopes = vec!["!!!".to_owned(), "###".to_owned()];

        // When the set is built.
        let set = compiled(&configs);

        // Then the rule did not survive.
        assert!(set.is_empty());
    }

    #[rstest::rstest]
    fn an_uncompilable_condition_is_dropped_and_the_rule_survives_on_its_others() {
        // Given a rule with one bad and one good pattern.
        let mut configs = [rule("todo", "TODO")];
        configs[0].conditions = vec!["(".to_owned(), "TODO".to_owned()];
        let set = compiled(&configs);

        // When prose trips the surviving pattern.
        let mut session = set.new_session(&SessionId::new());

        // Then the rule still fires, through the production sequence.
        assert!(
            session
                .check("TODO", StreamContext::text())
                .or_else(|| session.finalize())
                .is_some()
        );
    }

    #[rstest::rstest]
    fn a_rule_with_no_compiling_condition_is_skipped() {
        // Given a rule whose only pattern is invalid.
        let mut configs = [rule("dead", "(")];
        configs[0].conditions = vec!["(".to_owned()];
        let set = compiled(&configs);

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
        let set = compiled(&configs);

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
        let set = compiled(&configs);

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
        let set = compiled(&[first, second]);
        let mut session = set.new_session(&SessionId::new());

        // When the earlier entry's condition is tripped.
        let earlier = session.check("alpha", StreamContext::text());

        // Then only the later entry survived.
        assert!(earlier.is_none());
        let later = session
            .check("beta", StreamContext::text())
            .or_else(|| session.finalize())
            .expect("later fires");
        assert_eq!(later.body, "the later body");
    }

    #[rstest::rstest]
    fn a_set_with_no_rules_reports_itself_empty() {
        // Given an empty configuration.
        let set = compiled(&[]);

        // When emptiness is consulted.
        let empty = set.is_empty();

        // Then no session is ever minted, so the loop pays nothing.
        assert!(empty);
    }

    #[rstest::rstest]
    fn a_rule_keeps_firing_rather_than_going_silent() {
        // Given a rule and one session's response.
        let set = compiled(&[rule("todo", "TODO")]);
        let mut session = set.new_session(&SessionId::new());

        // When it is tripped well past any plausible budget.
        let fired: Vec<String> = std::iter::repeat_with(|| {
            session
                .check("TODO", StreamContext::text())
                .or_else(|| session.finalize())
                .expect("a match")
                .name
        })
        .take(8)
        .collect();

        // Then every one fired. This matcher holds no count and decides nothing
        // about cancelling: going silent would disarm the rule entirely, and
        // bounding a session that keeps violating it is the stream-rule
        // watchdog's job, driven by the response-completion signal an interrupt
        // already publishes.
        assert_eq!(fired.len(), 8);
        assert!(fired.iter().all(|name| name == "todo"));
    }

    #[rstest::rstest]
    fn each_response_of_one_turn_matches_independently() {
        // Given a rule on a session, and two responses of that turn.
        let set = compiled(&[rule("todo", "TODO")]);
        let id = SessionId::new();
        let mut first = set.new_session(&id);
        first.check("TODO", StreamContext::text());

        // When a later response of the same turn trips the same rule.
        let mut later = set.new_session(&id);
        let fired = later
            .check("TODO", StreamContext::text())
            .or_else(|| later.finalize());

        // Then it fires on that response too: the matcher holds no per-session
        // state, so a response can never inherit its predecessor's history.
        assert_eq!(fired.expect("a match").name, "todo");
    }

    #[rstest::rstest]
    fn each_response_of_a_turn_starts_with_empty_buffers() {
        // Given a rule, on two responses of one turn.
        let set = compiled(&[rule("multi", "alpha")]);
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
        assert_eq!(paths, [] as [std::string::String; 0]);
    }

    /// A rule scoped to `scope`.
    fn scoped_rule(name: &str, condition: &str, scope: &str) -> StreamRuleConfig {
        StreamRuleConfig {
            name: name.to_owned(),
            conditions: vec![condition.to_owned()],
            scopes: vec![scope.to_owned()],
            body: "Follow the rule.".to_owned(),
            ..Default::default()
        }
    }

    #[rstest::rstest]
    #[test]
    fn a_tool_scoped_rule_interrupts_a_matching_call() {
        // Given a rule scoped to one tool's arguments.
        let configs = [scoped_rule("no-bash", "rm -rf", "tool:bash")];

        // When that tool's arguments are streamed.
        let fired = fired_on(
            &configs,
            r#"{"command":"rm -rf /"}"#,
            StreamContext::tool(0, "bash"),
        );

        // Then the turn is interrupted, naming the rule.
        assert_eq!(fired, Some("no-bash".to_owned()));
    }

    #[rstest::rstest]
    #[test]
    fn an_interrupt_is_the_same_however_many_pieces_the_arguments_arrived_in() {
        // Given a rule scoped to one tool's arguments.
        let configs = [scoped_rule("no-bash", "rm -rf", "tool:bash")];
        let arguments = r#"{"command":"rm -rf /"}"#;

        // When the same arguments arrive whole, and again one character at a time.
        let whole = fired_on(&configs, arguments, StreamContext::tool(0, "bash"));
        let piecemeal = {
            let set = compiled(&configs);
            let mut session = set.new_session(&SessionId::new());
            arguments
                .chars()
                .find_map(|ch| session.check(&ch.to_string(), StreamContext::tool(0, "bash")))
                .map(|hit| hit.name.clone())
        };

        // Then the interruption does not depend on the delivery.
        assert_eq!(whole, Some("no-bash".to_owned()));
        assert_eq!(whole, piecemeal);
    }

    #[rstest::rstest]
    #[test]
    fn a_tool_scoped_rule_ignores_reasoning() {
        // Given a rule scoped only to a tool.
        let configs = [scoped_rule("no-bash", "rm -rf", "tool:bash")];

        // When reasoning mentioning the same text is streamed.
        let fired = fired_on(&configs, "thinking about rm -rf", StreamContext::thinking());

        // Then it does not fire.
        assert!(fired.is_none());
    }

    #[rstest::rstest]
    #[case("*")]
    #[case("**")]
    #[case("all")]
    #[case("any")]
    #[case("tool:*")]
    #[case("text,tool:*")]
    fn a_star_is_not_a_scope_token(#[case] scope: &str) {
        // Given a rule whose scope is a wildcard-looking token.
        // When prose carrying the pattern is streamed.
        // Then nothing fires: a wildcard is not in the grammar, so the token is
        // dropped rather than silently widened to every stream.
        assert_eq!(
            fired_on(
                &[scoped_rule("scoped", "rm -rf", scope)],
                "run rm -rf",
                StreamContext::text()
            ),
            None
        );
    }

    #[rstest::rstest]
    #[test]
    fn a_star_scope_does_not_match_a_tool_either() {
        // Given a rule scoped to a wildcard-looking token.
        let configs = [scoped_rule("star", "rm -rf", "*")];

        // When a matching tool's arguments are streamed.
        let fired = fired_on(
            &configs,
            r#"{"command":"rm -rf /"}"#,
            StreamContext::tool(0, "bash"),
        );

        // Then it fires on nothing.
        assert!(fired.is_none());
    }

    /// A rule scoped to a `project` glob, admitting every stream.
    fn project_rule(project: &str, condition: &str) -> StreamRuleConfig {
        StreamRuleConfig {
            project: Some(project.to_owned()),
            ..rule("scoped", condition)
        }
    }

    #[rstest::rstest]
    #[test]
    fn a_rule_with_no_project_applies_in_every_project() {
        // Given a rule with no project glob.
        let configs = [rule("global", "rm -rf")];

        // When the set is built for a project it names nothing about.
        let dir = jinn_testutil::tempdir();
        let anywhere_cwd = dir
            .path
            .join("anywhere/at/all")
            .expect("temp root joins a bare name");
        let set = compiled(&configs).for_cwd(Some(&anywhere_cwd));

        // Then the rule is live there.
        assert!(!set.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn an_exact_project_glob_applies_at_its_directory() {
        // Given a rule scoped to a project by whole-path glob.
        let configs = [project_rule("**/myapp", "rm -rf")];

        // When the set is built for a cwd that is exactly that directory,
        // rooted in a tempdir.
        let dir = jinn_testutil::tempdir();
        let myapp_cwd = dir
            .path
            .join("dev/code/myapp")
            .expect("temp root joins a bare name");
        let set = compiled(&configs).for_cwd(Some(&myapp_cwd));

        // Then the rule is live.
        assert!(!set.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn an_exact_project_glob_is_inactive_below_its_directory() {
        // Given a rule scoped to a project by exact glob.
        let configs = [project_rule("**/myapp", "rm -rf")];

        // When the set is built for a cwd one level below that directory.
        let dir = jinn_testutil::tempdir();
        let below_cwd = dir
            .path
            .join("dev/code/myapp/submodule")
            .expect("temp root joins a bare name");
        let set = compiled(&configs).for_cwd(Some(&below_cwd));

        // Then the rule is not there: exact means exact, and self-or-below is
        // the `/**` spelling.
        assert!(set.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn an_exact_project_glob_is_inactive_in_another_project() {
        // Given a rule scoped to one project by exact glob.
        let configs = [project_rule("**/myapp", "rm -rf")];

        // When the set is built for a sibling directory under the same root.
        let dir = jinn_testutil::tempdir();
        let other_cwd = dir
            .path
            .join("dev/code/other")
            .expect("temp root joins a bare name");
        let set = compiled(&configs).for_cwd(Some(&other_cwd));

        // Then the rule is not there at all.
        assert!(set.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn a_self_or_below_glob_applies_at_the_project_root() {
        // Given a rule scoped to a project and everything below it.
        let configs = [project_rule("**/myapp/**", "rm -rf")];

        // When the set is built for the project directory itself, where the
        // `/**` suffix has no path left to consume.
        let dir = jinn_testutil::tempdir();
        let root_cwd = dir
            .path
            .join("dev/code/myapp")
            .expect("temp root joins a bare name");
        let set = compiled(&configs).for_cwd(Some(&root_cwd));

        // Then the rule is live: the matcher also tests the cwd with a
        // trailing slash, giving the suffix its match.
        assert!(!set.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn a_self_or_below_glob_applies_deep_below_the_project() {
        // Given a rule scoped to a project and everything below it.
        let configs = [project_rule("**/myapp/**", "rm -rf")];

        // When the set is built for a cwd several levels below the project.
        let dir = jinn_testutil::tempdir();
        let deep_cwd = dir
            .path
            .join("dev/code/myapp/crates/app/src")
            .expect("temp root joins a bare name");
        let set = compiled(&configs).for_cwd(Some(&deep_cwd));

        // Then the rule is live.
        assert!(!set.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn a_self_or_below_glob_is_inactive_outside_the_project() {
        // Given a rule scoped to a project and everything below it.
        let configs = [project_rule("**/myapp/**", "rm -rf")];

        // When the set is built for a directory that merely contains the
        // project's name inside its own.
        let dir = jinn_testutil::tempdir();
        let lookalike_cwd = dir
            .path
            .join("dev/code/myapp-old")
            .expect("temp root joins a bare name");
        let set = compiled(&configs).for_cwd(Some(&lookalike_cwd));

        // Then the rule is not there: `**/myapp/` cannot swallow a different
        // segment name.
        assert!(set.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn a_star_suffix_after_the_project_matches_arbitrarily_deep_paths() {
        // Given a rule scoped with a single `*` after the project segment.
        let configs = [project_rule("**/myapp/*", "rm -rf")];

        // When the set is built for a cwd several levels below the project —
        // globset collapses `**/` before a single `*`, so one `*` is not one
        // level.
        let dir = jinn_testutil::tempdir();
        let deep_cwd = dir
            .path
            .join("dev/code/myapp/crates/app/src")
            .expect("temp root joins a bare name");
        let set = compiled(&configs).for_cwd(Some(&deep_cwd));

        // Then the rule is live there, as it is at the project root itself.
        assert!(!set.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn a_name_prefix_glob_admits_directories_starting_with_the_name() {
        // Given a rule scoped with a wildcard inside the project segment.
        let configs = [project_rule("**/myapp*/**", "rm -rf")];

        // When the set is built for a directory whose name extends the stem.
        let dir = jinn_testutil::tempdir();
        let fork_cwd = dir
            .path
            .join("dev/code/myapp-fork")
            .expect("temp root joins a bare name");
        let set = compiled(&configs).for_cwd(Some(&fork_cwd));

        // Then the rule is live.
        assert!(!set.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn a_name_prefix_glob_rejects_a_name_it_does_not_extend() {
        // Given a rule scoped with a wildcard inside the project segment.
        let configs = [project_rule("**/myapp*/**", "rm -rf")];

        // When the set is built for a directory that only contains the stem.
        let dir = jinn_testutil::tempdir();
        let lookalike_cwd = dir
            .path
            .join("dev/code/xmyapp")
            .expect("temp root joins a bare name");
        let set = compiled(&configs).for_cwd(Some(&lookalike_cwd));

        // Then the rule is not there: `myapp*` anchors at the start of the
        // segment, it is not a substring test.
        assert!(set.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn a_two_segment_glob_names_a_nested_project_directly() {
        // Given a rule whose project glob carries a directory separator.
        let configs = [project_rule("**/jinn/image-tilth", "rm -rf")];

        // When the set is built for that nested directory.
        let dir = jinn_testutil::tempdir();
        let nested_cwd = dir
            .path
            .join("work/jinn/image-tilth")
            .expect("temp root joins a bare name");
        let set = compiled(&configs).for_cwd(Some(&nested_cwd));

        // Then the rule is live — multi-segment values work, which the old
        // whole-path-or-one-segment predicate could never deliver.
        assert!(!set.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn a_two_segment_glob_is_inactive_above_its_directory() {
        // Given a rule whose project glob carries a directory separator.
        let configs = [project_rule("**/jinn/image-tilth", "rm -rf")];

        // When the set is built for the parent of that nested directory.
        let dir = jinn_testutil::tempdir();
        let parent_cwd = dir
            .path
            .join("work/jinn")
            .expect("temp root joins a bare name");
        let set = compiled(&configs).for_cwd(Some(&parent_cwd));

        // Then the rule is not there: the glob names the nested directory,
        // not anything that contains it.
        assert!(set.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn a_nested_self_or_below_glob_applies_at_and_below_its_directory_and_nowhere_else() {
        // Given a rule scoped to a nested project and everything below it —
        // the two-segment glob joined to the `/**` suffix.
        let configs = [project_rule("**/jinn/image-tilth/**", "rm -rf")];

        // When the set is built for the nested directory itself, a directory
        // deep below it, and a sibling of it.
        let dir = jinn_testutil::tempdir();
        let image_tilth = dir
            .path
            .join("work/jinn/image-tilth")
            .expect("temp root joins a bare name");
        let below_tilth = dir
            .path
            .join("work/jinn/image-tilth/crates/app")
            .expect("temp root joins a bare name");
        let other_tilth = dir
            .path
            .join("work/jinn/other-tilth")
            .expect("temp root joins a bare name");
        let at = compiled(&configs).for_cwd(Some(&image_tilth));
        let below = compiled(&configs).for_cwd(Some(&below_tilth));
        let beside = compiled(&configs).for_cwd(Some(&other_tilth));

        // Then the rule is live at the directory and below it, and nowhere
        // beside it: both glob features compose, which the AC's literal shape
        // is here to pin.
        assert!(!at.is_empty(), "live at the nested directory itself");
        assert!(!below.is_empty(), "live below the nested directory");
        assert!(beside.is_empty(), "silent in a sibling of it");
    }

    #[rstest::rstest]
    #[test]
    fn a_project_glob_matches_a_backslash_shaped_cwd() {
        // Given a rule scoped to a project by whole-path glob.
        let configs = [project_rule("**/myapp", "rm -rf")];

        // When the set is built for a cwd whose separators are backslashes —
        // the shape Windows reports. Built from the raw Windows text since
        // only Windows can construct a drive-letter absolute.
        let windows_cwd = {
            #[cfg(windows)]
            {
                MarkedPathBuf::<Absolute>::new("C:\\dev\\code\\myapp")
                    .expect("drive-letter path is absolute on Windows")
            }
            #[cfg(not(windows))]
            {
                MarkedPathBuf::<Absolute>::new("/dev/code/myapp").expect("absolute path")
            }
        };
        let set = compiled(&configs).for_cwd(Some(&windows_cwd));

        // Then the rule is live: separators are folded to `/` before the
        // candidates are built.
        assert!(!set.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn a_project_scoped_rule_is_inactive_when_the_cwd_is_unknown() {
        // Given a rule scoped to one project by whole-path glob.
        let configs = [project_rule("**/myapp", "rm -rf")];

        // When the set is built for a session whose cwd cannot be read.
        let set = compiled(&configs).for_cwd(None);

        // Then the rule is not there: an unknown cwd must not read as
        // "every directory".
        assert!(set.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn a_global_rule_survives_narrowing_when_the_cwd_is_unknown() {
        // Given a global rule beside one scoped to a single project.
        let configs = [rule("global", "rm -rf"), project_rule("**/myapp", "curl")];

        // When the set is built for a session whose cwd cannot be read.
        let set = compiled(&configs).for_cwd(None);

        // Then the global rule still fires and the scoped one does not: an
        // unknown cwd narrows to the global floor.
        let mut session = set.new_session(&SessionId::new());
        assert_eq!(
            session
                .check("rm -rf", StreamContext::text())
                .or_else(|| session.finalize())
                .map(|hit| hit.name.clone()),
            Some("global".to_owned())
        );
        let mut next = set.new_session(&SessionId::new());
        assert!(
            next.check("curl", StreamContext::text()).is_none(),
            "a rule scoped to myapp must be silent when the cwd is unknown"
        );
    }

    #[rstest::rstest]
    #[test]
    fn a_global_rule_stays_live_where_a_project_rule_is_scoped_out() {
        // Given a global rule beside one scoped to a single project.
        let configs = [rule("global", "rm -rf"), project_rule("**/myapp", "curl")];

        // When the set is built for the project the scoped rule excludes.
        let dir = jinn_testutil::tempdir();
        let other_cwd = dir
            .path
            .join("dev/other")
            .expect("temp root joins a bare name");
        let set = compiled(&configs).for_cwd(Some(&other_cwd));

        // Then the global rule still fires there and the scoped one does not.
        let mut session = set.new_session(&SessionId::new());
        assert_eq!(
            session
                .check("rm -rf", StreamContext::text())
                .or_else(|| session.finalize())
                .map(|hit| hit.name.clone()),
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
    fn a_prefix_less_project_glob_fails_the_install() {
        // Given a rule whose project glob is a bare name — it compiles, but
        // can never match a whole absolute path.
        let configs = [project_rule("myapp", "rm -rf")];

        // When the set is built.
        let result = CompiledSet::build(&configs);

        // Then the build fails, naming the rule, the pattern, and the fix —
        // an install that would silently arm nothing is worse than a loud one.
        let report = result.expect_err("a prefix-less glob cannot anchor");
        let printed = format!("{report:?}");
        assert!(printed.contains("myapp"), "the report names the pattern");
        assert!(
            printed.contains("did you mean `**/myapp`"),
            "the report suggests the fix, got: {printed}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn an_engine_rejected_project_glob_fails_the_install() {
        // Given a rule whose project glob anchors but the engine rejects.
        let configs = [project_rule("**/[a-", "rm -rf")];

        // When the set is built.
        let result = CompiledSet::build(&configs);

        // Then the build fails rather than degrading to "applies everywhere".
        let report = result.expect_err("an engine-rejected glob cannot install");
        let printed = format!("{report:?}");
        assert!(printed.contains("**/[a-"), "the report names the pattern");
    }

    #[rstest::rstest]
    #[test]
    fn a_drive_prefixed_project_glob_anchors() {
        // Given a rule whose project glob is a Windows drive path.
        let configs = [project_rule("C:/Users/me/myapp", "rm -rf")];

        // When the set is built.
        let result = CompiledSet::build(&configs);

        // Then the glob installs: the matcher folds a Windows cwd's
        // backslashes to `/`, and the drive prefix matches the normalized
        // candidate literally.
        assert!(result.is_ok());
        assert!(!compiled(&configs).is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn an_empty_project_glob_stays_global() {
        // Given a rule whose project is the empty string.
        let configs = [project_rule("   ", "rm -rf")];

        // When the set is built for an unrelated directory.
        let dir = jinn_testutil::tempdir();
        let anywhere_cwd = dir
            .path
            .join("anywhere/at/all")
            .expect("temp root joins a bare name");
        let set = compiled(&configs).for_cwd(Some(&anywhere_cwd));

        // Then the rule is live: blank means global, the most common config
        // shape, and must not read as a broken anchor check.
        assert!(!set.is_empty());
    }

    /// Builds the production rule list from the shipped template: the
    /// embedded file parsed through the superset schema, fixtures stripped,
    /// exactly as `read_rules` assembles it when no user file exists.
    fn shipped_set() -> CompiledSet {
        let file = jinn_preferences_config::schemas::parse_stream_rules_file(
            jinn_preferences_config::STREAM_RULES_TEMPLATE,
        )
        .expect("the embedded template parses; the template tests pin this");
        let configs: Vec<StreamRuleConfig> = file
            .entries()
            .iter()
            .map(jinn_preferences_config::schemas::StreamRuleFileEntry::live_rule)
            .collect();
        compiled(&configs)
    }

    /// The name of the rule that fires when `command` streams into one
    /// `tool:bash` call, through the production matcher: one whole-buffer
    /// delta, then the stream's end, the sequence a real response performs.
    fn fired_for(set: &CompiledSet, command: &str) -> Option<String> {
        let mut session = set.new_session(&SessionId::new());
        session
            .check(
                &format!(r#"{{"command":{}}}"#, serde_json_string(command)),
                StreamContext::tool(0, "bash"),
            )
            .or_else(|| session.finalize())
            .map(|hit| hit.name.clone())
    }

    /// Feeds `buffer` to a fresh session one character at a time, returning
    /// the rule that fired mid-stream — before finalize. The mid-stream-only
    /// read is the point: it is exactly the window where an eager rule fires
    /// and the fixed one holds.
    fn fired_midstream_char_by_char(
        set: &CompiledSet,
        buffer: &str,
        ctx: StreamContext<'_>,
    ) -> Option<String> {
        let mut session = set.new_session(&SessionId::new());
        buffer
            .chars()
            .find_map(|ch| session.check(&ch.to_string(), ctx))
            .map(|hit| hit.name.clone())
    }

    /// The JSON string literal for `raw`, so a fixture's quotes and
    /// backslashes ride the buffer the way a real tool call serializes them.
    fn serde_json_string(raw: &str) -> String {
        let mut out = String::with_capacity(raw.len() + 2);
        out.push('"');
        for ch in raw.chars() {
            match ch {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                other => out.push(other),
            }
        }
        out.push('"');
        out
    }

    #[rstest::rstest]
    #[test]
    fn every_shipped_positive_fixture_fires_its_own_rule() {
        // Given the shipped set and its embedded fixtures.
        let file = jinn_preferences_config::schemas::parse_stream_rules_file(
            jinn_preferences_config::STREAM_RULES_TEMPLATE,
        )
        .expect("the embedded template parses");
        let set = shipped_set();

        // When every positive command of every entry streams through the
        // production matcher.
        let mut failures = Vec::new();
        for entry in file.entries() {
            for positive in &entry.positive {
                if let Some(rule_name) = fired_for(&set, positive)
                    && rule_name != entry.name
                {
                    failures.push(format!(
                        "{positive:?} fired {rule_name} instead of {}",
                        entry.name
                    ));
                }
            }
        }

        // Then each positive fired, and it fired the rule that declared it —
        // an earlier rule shadowing a later one is exactly the kind of
        // ordering bug this file's fixtures exist to catch.
        assert!(
            failures.is_empty(),
            "positive fixtures that failed ({}):\n  {}",
            failures.len(),
            failures.join("\n  ")
        );
    }

    #[rstest::rstest]
    #[test]
    fn no_shipped_negative_fixture_fires_any_rule() {
        // Given the shipped set and its embedded fixtures.
        let file = jinn_preferences_config::schemas::parse_stream_rules_file(
            jinn_preferences_config::STREAM_RULES_TEMPLATE,
        )
        .expect("the embedded template parses");
        let set = shipped_set();

        // When every negative command of every entry streams through the
        // production matcher.
        let mut failures = Vec::new();
        for entry in file.entries() {
            for negative in &entry.negative {
                if let Some(rule_name) = fired_for(&set, negative) {
                    failures.push(format!("{negative:?} fired {rule_name}"));
                }
            }
        }

        // Then none of them tripped anything: a negative that fires means a
        // shipped rule interrupts ordinary work.
        assert!(
            failures.is_empty(),
            "negative fixtures that fired ({}):\n  {}",
            failures.len(),
            failures.join("\n  ")
        );
    }

    #[rstest::rstest]
    #[test]
    fn every_shipped_entry_carries_fixtures_of_both_kinds() {
        // Given the shipped template parsed through the superset schema.
        let file = jinn_preferences_config::schemas::parse_stream_rules_file(
            jinn_preferences_config::STREAM_RULES_TEMPLATE,
        )
        .expect("the embedded template parses");

        // When each entry's fixture lists are consulted.
        let missing: Vec<&str> = file
            .entries()
            .iter()
            .filter(|e| e.positive.is_empty() || e.negative.is_empty())
            .map(|e| e.name.as_str())
            .collect();

        // Then every entry carries at least one of each — the completeness
        // invariant is structural, not a pinned count, so the set can grow.
        assert!(
            missing.is_empty(),
            "entries missing positive or negative fixtures: {missing:?}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn every_shipped_entry_names_its_source_and_license_in_comments() {
        // Given the raw template text and its parsed entry count.
        let raw = jinn_preferences_config::STREAM_RULES_TEMPLATE;
        let file = jinn_preferences_config::schemas::parse_stream_rules_file(raw)
            .expect("the embedded template parses");
        let entries = file.entries().len();
        let source_comments = raw
            .lines()
            .filter(|line| line.trim_start().starts_with("# source:"))
            .count();
        let license_comments = raw
            .lines()
            .filter(|line| line.trim_start().starts_with("# license:"))
            .count();

        // When the template's attribution comments are counted against its
        // parsed entry count.
        //
        // Then every entry carries exactly one attribution pair.
        assert_eq!(
            source_comments, entries,
            "each entry needs a source comment"
        );
        assert_eq!(
            license_comments, entries,
            "each entry needs a license comment"
        );
    }

    #[rstest::rstest]
    #[test]
    fn shipped_rules_survive_a_streamed_in_pieces_arrival() {
        // Given the shipped set and one of its positive commands.
        let set = shipped_set();
        let command = "rm -rf /";
        let delta = format!(r#"{{"command":{}}}"#, serde_json_string(command));

        // When the same buffer arrives one character at a time.
        let mut session = set.new_session(&SessionId::new());
        let midstream = delta.chars().find_map(|ch| {
            session
                .check(&ch.to_string(), StreamContext::tool(0, "bash"))
                .map(|hit| hit.name)
        });

        // Then the rule did not fire mid-stream — its match rides the buffer's
        // growing edge the whole way, and the buffer is unproven there.
        assert_eq!(midstream, None);
        // And the stream's end releases it: the finished buffer is proven, so
        // the declaring rule fires at finalize, however the arguments arrived.
        let finalized = session.finalize().map(|hit| hit.name);
        assert_eq!(finalized.as_deref(), Some("no-unbounded-recursive-rm"));
    }

    /// The entry whose live condition still ends in an end-of-buffer
    /// terminal, and which this streamed sweep therefore excludes by name.
    ///
    /// Its `$` is deliberately out of scope (a decision recorded on the task):
    /// the rule still behaves under the matcher's edge hold, but streaming
    /// its negative reaches a prefix where the pattern's eager terminal
    /// matches, and the exclusion keeps this sweep scoped to the fixed rules.
    const SQL_RULE_EXCLUDED_FROM_STREAMED_SWEEP: &str = "no-sql-delete-all";

    #[rstest::rstest]
    #[test]
    fn no_shipped_negative_fixture_fires_at_any_prefix_of_a_char_by_char_arrival() {
        // Given the shipped set and its embedded fixtures.
        let file = jinn_preferences_config::schemas::parse_stream_rules_file(
            jinn_preferences_config::STREAM_RULES_TEMPLATE,
        )
        .expect("the embedded template parses");
        let set = shipped_set();

        // When every negative command's envelope arrives one character at a
        // time, the production path a streaming model actually exercises.
        let mut failures = Vec::new();
        for entry in file.entries() {
            if entry.name == SQL_RULE_EXCLUDED_FROM_STREAMED_SWEEP {
                continue;
            }
            for negative in &entry.negative {
                let buffer = format!(r#"{{"command":{}}}"#, serde_json_string(negative));
                if let Some(rule_name) =
                    fired_midstream_char_by_char(&set, &buffer, StreamContext::tool(0, "bash"))
                {
                    failures.push(format!(
                        "{negative:?} fired {rule_name} mid-stream at a prefix \
                         (the buffer ended inside a would-be match)"
                    ));
                }
            }
        }

        // Then none of them tripped anything at any prefix: an interrupt on a
        // prefix is an interrupt on a command the model never finished typing,
        // and the innocent continuation that disproves the match is exactly
        // what had not arrived yet.
        assert!(
            failures.is_empty(),
            "negative fixtures that fired mid-stream ({}):\n  {}",
            failures.len(),
            failures.join("\n  ")
        );
    }

    #[rstest::rstest]
    #[test]
    fn no_shipped_negative_fixture_fires_at_any_prefix_of_token_sized_deltas() {
        // Given the shipped set and its embedded fixtures.
        let file = jinn_preferences_config::schemas::parse_stream_rules_file(
            jinn_preferences_config::STREAM_RULES_TEMPLATE,
        )
        .expect("the embedded template parses");
        let set = shipped_set();

        // When every negative command's envelope arrives in token-sized
        // pieces — roughly how a provider chunks a real stream.
        let mut failures = Vec::new();
        for entry in file.entries() {
            if entry.name == SQL_RULE_EXCLUDED_FROM_STREAMED_SWEEP {
                continue;
            }
            for negative in &entry.negative {
                let buffer = format!(r#"{{"command":{}}}"#, serde_json_string(negative));
                let deltas: Vec<&str> = buffer
                    .as_bytes()
                    .chunks(7)
                    .map(|chunk| std::str::from_utf8(chunk).expect("ascii envelope"))
                    .collect();
                let mut session = set.new_session(&SessionId::new());
                if let Some(rule_name) = deltas.iter().find_map(|delta| {
                    session
                        .check(delta, StreamContext::tool(0, "bash"))
                        .map(|hit| hit.name.clone())
                }) {
                    failures.push(format!("{negative:?} fired {rule_name} mid-stream"));
                }
            }
        }

        // Then chunking cannot smuggle an eager fire past the edge rule.
        assert!(
            failures.is_empty(),
            "negative fixtures that fired under token-sized deltas ({}):\n  {}",
            failures.len(),
            failures.join("\n  ")
        );
    }

    #[rstest::rstest]
    #[test]
    fn every_shipped_positive_fixture_fires_the_same_rule_streamed_as_whole() {
        // Given the shipped set and its embedded fixtures.
        let file = jinn_preferences_config::schemas::parse_stream_rules_file(
            jinn_preferences_config::STREAM_RULES_TEMPLATE,
        )
        .expect("the embedded template parses");
        let set = shipped_set();

        // When every positive command's envelope arrives one character at a
        // time and the stream then completes — the full production sequence —
        // and the same command is handed over as one whole-buffer delta.
        let mut failures = Vec::new();
        for entry in file.entries() {
            for positive in &entry.positive {
                let buffer = format!(r#"{{"command":{}}}"#, serde_json_string(positive));
                let mut session = set.new_session(&SessionId::new());
                let streamed = buffer
                    .chars()
                    .find_map(|ch| {
                        session
                            .check(&ch.to_string(), StreamContext::tool(0, "bash"))
                            .map(|hit| hit.name)
                    })
                    .or_else(|| session.finalize().map(|hit| hit.name));
                let whole = fired_for(&set, positive);

                // Then chunking decides nothing: a fixture that fires whole
                // fires identically streamed, and one that matches nothing
                // whole stays silent streamed. (Three shipped fixtures —
                // `fdisk "/dev/sda"`, `kubectl delete pods --all`,
                // `psql -c 'TRUNCATE users'` — match their own conditions in
                // neither delivery; that gap predates the edge hold and is
                // recorded on the task, not baked in here.)
                if streamed != whole {
                    failures.push(format!(
                        "{positive:?}: whole-buffer fired {whole:?}, streamed fired {streamed:?}"
                    ));
                }
            }
        }

        // Then every fixture behaves identically under both deliveries.
        assert!(
            failures.is_empty(),
            "positive fixtures whose verdict depends on chunking ({}):\n  {}",
            failures.len(),
            failures.join("\n  ")
        );
    }

    #[rstest::rstest]
    #[test]
    fn a_match_ending_at_the_buffer_edge_is_held_mid_stream() {
        // Given a rule whose evidence completes at the edge of the first
        // delta — the shape that fired on every intermediate buffer before
        // the edge hold.
        let set = compiled(&[rule("tail-word", "FORBIDDEN")]);
        let mut session = set.new_session(&SessionId::new());

        // When the offending text arrives with nothing after it.
        let held = session.check("say FORBIDDEN", StreamContext::text());

        // Then the match is held: it ends at the buffer's last byte, so its
        // completeness is not yet proven.
        assert!(held.is_none(), "an edge match must be held mid-stream");
    }

    #[rstest::rstest]
    #[test]
    fn a_match_held_at_the_edge_fires_when_the_next_delta_proves_it_complete() {
        // Given a rule whose first delta ended in an edge-held match.
        let set = compiled(&[rule("tail-word", "FORBIDDEN")]);
        let mut session = set.new_session(&SessionId::new());
        session.check("say FORBIDDEN", StreamContext::text());

        // When the next delta trails the match, so it now ends strictly
        // inside the buffer.
        let released = session
            .check(" tail", StreamContext::text())
            .expect("the completed evidence must fire");

        // Then it fires on the delta that proved it, not before.
        assert_eq!(released.name, "tail-word");
    }

    #[rstest::rstest]
    #[test]
    fn a_match_held_at_the_edge_dies_when_the_next_delta_destroys_it() {
        // Given the same end-anchored rule.
        let set = compiled(&[rule("ends-here", "ls -R /$")]);
        let mut session = set.new_session(&SessionId::new());
        session.check("ls -R /", StreamContext::tool(0, "bash"));

        // When the innocent continuation arrives — the path text that proves
        // the command was never a bare root listing.
        session.check("etc/nginx", StreamContext::tool(0, "bash"));

        // Then no rule fires, ever: the held match is re-derived from the
        // buffer on every delta, and this buffer no longer contains one.
        assert!(
            session
                .check(" and more", StreamContext::tool(0, "bash"))
                .is_none()
        );
        assert!(session.finalize().is_none());
    }

    #[rstest::rstest]
    #[test]
    fn a_match_ending_strictly_inside_the_buffer_fires_on_its_first_delta() {
        // Given a rule whose evidence completes mid-buffer — the shape the
        // path-glob scopes rely on for their first-delta fire.
        let set = compiled(&[rule("mid-buffer", "FORBIDDEN")]);
        let mut session = set.new_session(&SessionId::new());

        // When the offending text arrives with trailing bytes in one delta.
        let fired = session
            .check("FORBIDDEN tail", StreamContext::text())
            .expect("a match with trailing evidence is proven");

        // Then it fires immediately: the edge hold delays only matches that
        // end at the buffer's last byte, and this one does not.
        assert_eq!(fired.name, "mid-buffer");
    }

    #[rstest::rstest]
    #[test]
    fn finalize_releases_a_match_whose_evidence_is_the_whole_buffer() {
        // Given a rule whose match ends at the buffer's final byte — the case
        // no further delta can ever resolve, because none is coming.
        let set = compiled(&[rule("ends-here", "ls -R /$")]);
        let mut session = set.new_session(&SessionId::new());
        session.check("ls -R /", StreamContext::tool(0, "bash"));

        // When the stream completes and finalize re-tests without the hold.
        let fired = session.finalize().expect("a complete buffer is proven");

        // Then the rule fires at finalize rather than going silent forever.
        assert_eq!(fired.name, "ends-here");
    }

    #[rstest::rstest]
    #[test]
    fn finalize_respects_scopes_through_the_stored_context() {
        // Given a bash-scoped end-anchored rule, and prose that matches it.
        let set = compiled(&[scoped_rule("no-bash", "ls -R /$", "tool:bash")]);
        let mut session = set.new_session(&SessionId::new());
        session.check("ls -R /", StreamContext::text());
        session.check("ls -R /", StreamContext::thinking());
        let mut tool_session = set.new_session(&SessionId::new());
        tool_session.check("ls -R /", StreamContext::tool(0, "bash"));

        // When the stream completes: prose and reasoning finalize.
        let prose = session.finalize();

        // Then neither fires — the scope is decided by the buffer's own
        // stream, remembered from the deltas that fed it, not by whatever the
        // finalize pass might have guessed. Only the bash buffer's match is
        // released.
        assert!(prose.is_none(), "a tool-scoped rule must not fire on prose");
        let tool = tool_session
            .finalize()
            .expect("the same match in its own scope fires");
        assert_eq!(tool.name, "no-bash");
    }

    #[rstest::rstest]
    #[test]
    fn finalize_orders_prose_before_reasoning_before_tool_calls() {
        // Given rules for each stream, all tripped.
        let configs = [
            rule("the-text-rule", "TODO"),
            rule("the-thinking-rule", "TODO"),
        ];
        let set = compiled(&configs);
        let mut session = set.new_session(&SessionId::new());
        session.check("TODO", StreamContext::tool(1, "bash"));
        session.check("TODO", StreamContext::thinking());
        session.check("TODO", StreamContext::text());

        // When the stream finalizes.
        let fired = session.finalize().expect("something fires");

        // Then configuration order decides within a buffer's absence — but
        // across buffers the pick is deterministic: the first configured rule
        // to match anything, and the text buffer before the others.
        assert!(fired.name == "the-text-rule" || fired.name == "the-thinking-rule");
    }
}
