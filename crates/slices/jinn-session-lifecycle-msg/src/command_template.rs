//! Command template parser - extracts positional and named parameters from shell commands.
//!
//! A [`CommandTemplate`] parses a command string like `script.sh $1 $2 $1` or
//! `./foo.sh <branch> <target>` and extracts the unique parameters in order of
//! first appearance. It can then:
//!
//! - **Render** the command with concrete arguments
//! - **Display** the command with human-readable tokens
//!
//! # Parameter syntax
//!
//! - `<name>` - named parameter (positional, filled by arg in same position)
//! - `$1` through `$9` - numeric positional parameters (backward compatibility)
//! - `$@` and `$*` - "all args" sentinel (accepts variable number of args)
//!
//! Parameters are deduplicated by identity. `$1 <foo> $1` produces `[Positional(1), Named("foo")]`
//! and substitutes both `$1` occurrences with the same arg.

use regex::Regex;
use std::fmt;
use unicode_segmentation::UnicodeSegmentation;

/// A segment of a displayed command line, tagged with its parameter index.
///
/// Used by [`CommandTemplate::display_line_segments`] to produce structured
/// output suitable for styled rendering. Static text has `param_index = None`;
/// parameter placeholders and their substituted values have `Some(idx)` where
/// `idx` is the index into the template's params list.
///
/// This design enables future per-argument color schemes (gradient, rainbow)
/// by only changing the color-assignment logic in the renderer - the data
/// structure already knows which arg each segment belongs to.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplaySegment {
    /// The text to display (placeholder like `<branch>` or substituted value).
    pub text: String,
    /// Index into the template's `params` list, or `None` for static text.
    pub param_index: Option<usize>,
}

impl DisplaySegment {
    /// Creates a static (non-parameter) segment.
    fn static_text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            param_index: None,
        }
    }

    /// Creates a parameter segment with the given param index.
    fn param(text: impl Into<String>, index: usize) -> Self {
        Self {
            text: text.into(),
            param_index: Some(index),
        }
    }
}

/// A classified span from tokenizing a display-form command line.
#[derive(Debug, Clone, PartialEq)]
enum Span {
    /// Static text between (or surrounding) placeholders.
    Static(String),
    /// A `<...>` placeholder. Contains the inner text (e.g., `"branch"` from `<branch>`).
    Placeholder(String),
}

/// A single parameter extracted from a command template.
///
/// Parameters are deduplicated - each unique token appears at most once.
/// During rendering, *all* occurrences (including duplicates) in the source
/// string are replaced with the corresponding argument value.
#[derive(Debug, Clone, PartialEq)]
pub enum Param {
    /// A named parameter like `<foo>` - filled by positional args.
    Named(String),
    /// A numeric positional parameter like `$1`.
    Positional(usize),
    /// The "all args" splat (`$@`, `$*`).
    Splat,
}

impl fmt::Display for Param {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Named(name) => write!(f, "<{name}>"),
            Self::Positional(n) => write!(f, "${n}"),
            Self::Splat => write!(f, "$@"),
        }
    }
}

/// A parsed shell command template with extracted parameters.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandTemplate {
    /// The original command string.
    source: String,
    /// Unique parameters in order of first appearance.
    /// E.g., `script.sh $1 <branch> $@` → `[Positional(1), Named("branch"), Splat]`.
    params: Vec<Param>,
}

/// Try to parse a `$N`, `$@`, or `$*` token at position `i` in `graphemes`.
///
/// Returns `Some((Param, graphemes_consumed))` on success, `None` if position `i`
/// is not a recognized dollar token.
fn try_parse_dollar(graphemes: &[&str], i: usize) -> Option<(Param, usize)> {
    let current = *graphemes.get(i)?;
    if current != "$" {
        return None;
    }
    let next = *graphemes.get(i + 1)?;
    let first_byte = *next.as_bytes().first()?;
    if next.len() == 1 && first_byte.is_ascii_digit() && first_byte != b'0' {
        let n = (first_byte - b'0') as usize;
        Some((Param::Positional(n), 2))
    } else if next == "@" || next == "*" {
        Some((Param::Splat, 2))
    } else {
        None
    }
}

/// Try to parse a `<name>` token at position `i` in `graphemes`.
///
/// Returns `Some((Param::Named(name), graphemes_consumed))` if a well-formed
/// `<name>` token is found (non-empty name, closing `>` present).
/// Returns `None` otherwise.
fn try_parse_named(graphemes: &[&str], i: usize) -> Option<(Param, usize)> {
    if graphemes.get(i)? != &"<" {
        return None;
    }
    let start = i + 1;
    let mut end = start;
    while let Some(g) = graphemes.get(end) {
        if *g == ">" {
            break;
        }
        end += 1;
    }
    if end > start && graphemes.get(end) == Some(&">") {
        let name: String = graphemes.get(start..end)?.join("");
        Some((Param::Named(name), end - i + 1))
    } else {
        None
    }
}

impl CommandTemplate {
    /// Parse a command string and extract parameters.
    ///
    /// Recognizes three token types:
    /// - `<name>` - a named parameter
    /// - `$1`–`$9` - a numeric positional parameter
    /// - `$@` / `$*` - the "all args" splat
    ///
    /// Parameters are deduplicated: if the same token appears multiple times,
    /// only the first occurrence is recorded. The order of first appearance
    /// defines the parameter order for arg assignment.
    #[must_use]
    pub fn parse(command: &str) -> Self {
        let mut params: Vec<Param> = Vec::new();
        let graphemes: Vec<&str> = command.graphemes(true).collect();
        let mut i = 0;

        while i < graphemes.len() {
            if let Some((param, consumed)) =
                try_parse_dollar(&graphemes, i).or_else(|| try_parse_named(&graphemes, i))
            {
                if !params.contains(&param) {
                    params.push(param);
                }
                i += consumed;
                continue;
            }
            i += 1;
        }

        Self {
            source: command.to_owned(),
            params,
        }
    }

    /// Whether this template requires any arguments.
    pub fn has_params(&self) -> bool {
        !self.params.is_empty()
    }

    /// The number of non-splat parameters.
    ///
    /// For `$1 $2 $@` this returns 2 - the number of positional-or-named slots
    /// that consume one argument each. Splat consumes all remaining args.
    #[must_use]
    pub fn param_count(&self) -> usize {
        self.params
            .iter()
            .filter(|p| !matches!(p, Param::Splat))
            .count()
    }

    /// Whether `$@` or `$*` was found in the command.
    #[must_use]
    pub fn has_splat(&self) -> bool {
        self.params.iter().any(|p| matches!(p, Param::Splat))
    }

    /// The unique parameters in order of first appearance.
    #[must_use]
    pub fn params(&self) -> &[Param] {
        &self.params
    }

    /// Render the command with concrete arguments substituted.
    ///
    /// Args are assigned positionally: `params[0]` → `args[0]`, `params[1]` → `args[1]`, etc.
    /// Splat (`$@` / `$*`) is replaced with all args joined by spaces.
    /// Named params (`<name>`) receive the positional arg at their index.
    ///
    /// # Panics
    ///
    /// Panics if there aren't enough args for the non-splat params.
    pub fn render(&self, args: &[String]) -> String {
        let mut result = self.source.clone();

        // Build a map: for each non-splat param, which arg index it uses.
        // Splat is handled separately.

        // Replace non-splat params in order, assigning args sequentially.
        // Each non-splat param gets the next available arg (args[0], args[1], ...).
        // If an arg is missing, substitute empty string instead of panicking.
        let mut arg_idx = 0usize;
        for param in &self.params {
            match param {
                Param::Named(name) => {
                    let search = format!("<{name}>");
                    let replacement = if let Some(arg) = args.get(arg_idx) {
                        shell_quote(arg)
                    } else {
                        String::new()
                    };
                    result = result.replace(&search, &replacement);
                    arg_idx += 1;
                }
                Param::Positional(_n) => {
                    let search = format!("{param}");
                    let replacement = if let Some(arg) = args.get(arg_idx) {
                        shell_quote(arg)
                    } else {
                        String::new()
                    };
                    result = result.replace(&search, &replacement);
                    arg_idx += 1;
                }
                Param::Splat => {
                    // Handled below after non-slot args are consumed.
                }
            }
        }

        // Replace $@ and $* with remaining args (those not consumed by non-splat params).
        if self.has_splat() {
            let joined = args
                .get(arg_idx..)
                .map(|remaining| {
                    remaining
                        .iter()
                        .map(|a| shell_quote(a))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            result = result.replace("$@", &joined);
            result = result.replace("$*", &joined);
        }

        result
    }

    /// Render the command with display tokens.
    ///
    /// - `<name>` stays as `<name>` (already display-ready)
    /// - `$1` → `<1>`, `$2` → `<2>` etc.
    /// - `$@` / `$*` → `<args>`
    ///
    /// Used in the arg-input popup UI.
    #[must_use]
    pub fn display(&self) -> String {
        let mut result = self.source.clone();

        // Replace in reverse order by source occurrence to avoid offset issues.
        // Since we're doing string.replace, order doesn't matter for correctness,
        // but we do non-splat first, then splat.
        for param in self.params.iter().rev() {
            match param {
                Param::Named(_name) => {
                    // <name> is already display-ready, no conversion needed.
                }
                Param::Positional(n) => {
                    let search = format!("${n}");
                    result = result.replace(&search, &format!("<{n}>"));
                }
                Param::Splat => {
                    result = result.replace("$@", "<args>");
                    result = result.replace("$*", "<args>");
                }
            }
        }

        result
    }

    /// Produce structured display lines with parameter substitution for the arg-input popup.
    ///
    /// Splits the display form of the command on ` && `, producing one
    /// [`Vec<DisplaySegment>`] per line. Each segment is tagged with its parameter
    /// index (or `None` for static text). When a user-provided arg is available
    /// for a parameter, the placeholder is replaced with the arg value.
    ///
    /// The first line is bare; subsequent lines are prefixed with `&& ` in a static
    /// segment. Non-last lines are suffixed with ` \` in a static segment.
    ///
    /// This is render-time only - it does not affect command execution.
    #[must_use]
    pub fn display_line_segments(&self, args: &[String]) -> Vec<Vec<DisplaySegment>> {
        // Build the display form of the command (same logic as display()).
        let display_str = self.display();

        // Split on " && " to get the raw segments.
        let raw_parts: Vec<&str> = display_str.split(" && ").collect();

        // For each raw part, tokenize into DisplaySegments.
        let mut lines = Vec::with_capacity(raw_parts.len());
        for (line_idx, raw) in raw_parts.iter().enumerate() {
            let mut segments = Vec::new();

            // Prefix for non-first lines.
            if line_idx > 0 {
                segments.push(DisplaySegment::static_text("  && "));
            }

            // Parse the raw text for <...> placeholders.
            segments.extend(self.tokenize_display_line(raw, args));

            // Suffix for non-last lines.
            if line_idx < raw_parts.len() - 1 {
                segments.push(DisplaySegment::static_text(" \\"));
            }

            lines.push(segments);
        }

        lines
    }

    /// Tokenize a display-form line into `DisplaySegment`s.
    ///
    /// Decomposes the line into [`Span`]s, then substitutes any args that are
    /// available for the corresponding parameters.
    fn tokenize_display_line(&self, line: &str, args: &[String]) -> Vec<DisplaySegment> {
        let spans = tokenize_spans(line);
        substitute_spans(spans, &self.params, args)
    }
}

/// Tokenize a display-form line into [`Span`]s by finding all `<...>` patterns.
///
/// Pure function - no `&self`, no args, no param lookup.
/// Unclosed `<` without a matching `>` is treated as static text (not a placeholder).
#[expect(clippy::expect_used, reason = "infallible")]
fn tokenize_spans(line: &str) -> Vec<Span> {
    static RE: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new("<([^>]+)>").expect("invalid regex"));

    let mut spans = Vec::new();
    let mut last_end = 0;

    for caps in RE.captures_iter(line) {
        let m = caps.get(0).expect("capture group 0 always exists");
        // Emit preceding static text if any.
        if m.start() > last_end
            && let Some(text) = line.get(last_end..m.start())
        {
            spans.push(Span::Static(text.to_owned()));
        }
        let inner = caps
            .get(1)
            .expect("capture group 1 exists")
            .as_str()
            .to_owned();
        spans.push(Span::Placeholder(inner));
        last_end = m.end();
    }

    // Emit trailing static text if any.
    if last_end < line.len()
        && let Some(text) = line.get(last_end..)
    {
        spans.push(Span::Static(text.to_owned()));
    }

    spans
}

/// Substitute args into classified [`Span`]s, producing display-ready [`DisplaySegment`]s.
///
/// For [`Span::Placeholder`], looks up the corresponding param. If an arg is
/// available, substitutes the arg value; otherwise keeps the placeholder text.
/// [`Span::Static`] passes through as a static [`DisplaySegment`].
fn substitute_spans(spans: Vec<Span>, params: &[Param], args: &[String]) -> Vec<DisplaySegment> {
    // Build lookup: (display_token, param_index, arg_offset).
    let mut arg_offset = 0usize;
    let mut token_map: Vec<(String, usize, usize)> = Vec::new();
    for (pidx, param) in params.iter().enumerate() {
        match param {
            Param::Named(name) => {
                token_map.push((format!("<{name}>"), pidx, arg_offset));
                arg_offset += 1;
            }
            Param::Positional(n) => {
                token_map.push((format!("<{n}>"), pidx, arg_offset));
                arg_offset += 1;
            }
            Param::Splat => {
                token_map.push(("<args>".to_owned(), pidx, arg_offset));
            }
        }
    }

    spans
        .into_iter()
        .map(|span| match span {
            Span::Static(text) => DisplaySegment::static_text(text),
            Span::Placeholder(inner) => {
                let full_token = format!("<{inner}>");
                match token_map.iter().find(|(tok, _, _)| *tok == full_token) {
                    Some((_, param_idx, arg_off)) => {
                        let display_text = if *arg_off < args.len() {
                            match params.get(*param_idx) {
                                Some(Param::Splat) => args
                                    .get(*arg_off..)
                                    .map(|s| s.join(" "))
                                    .unwrap_or_default(),
                                _ => args.get(*arg_off).cloned().unwrap_or_default(),
                            }
                        } else {
                            full_token.clone()
                        };
                        DisplaySegment::param(display_text, *param_idx)
                    }
                    None => DisplaySegment::static_text(full_token),
                }
            }
        })
        .collect()
}

/// Shell-quote a value for safe interpolation into a `$SHELL -c` command.
///
/// Uses single-quote wrapping with `\'\'\'` escape for embedded single quotes.
/// Only applies quoting when the value contains spaces or shell-special characters.
/// Safe values pass through unchanged.
#[must_use]
pub fn shell_quote(s: &str) -> String {
    /// Characters that require shell quoting.
    const SHELL_SPECIAL: &[char] = &[
        ' ', '\t', '\n', '\r', '|', '&', ';', '<', '>', '$', '`', '\\', '"', '\'', '(', ')', '*',
        '?', '[', ']', '~', '#', '!', '{', '}', '=', ':',
    ];

    if s.is_empty() {
        return "''".to_owned();
    }

    if !s.contains(SHELL_SPECIAL) {
        return s.to_owned();
    }

    // Wrap in single quotes, escaping embedded single quotes as '\'\''.
    let escaped = s.replace('\'', "'\\''");
    format!("'{escaped}'")
}

/// Parse a user input string into arguments, preserving quote characters.
///
/// Same splitting logic as [`parse_quoted_args`] (splits on unquoted whitespace,
/// respects backslash escapes) but keeps the double-quote characters in the tokens.
/// Used for display purposes so users see their literal input including quotes.
#[must_use]
pub fn split_preserving_quotes(input: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let graphemes: Vec<&str> = input.graphemes(true).collect();
    let mut i = 0;

    while i < graphemes.len() {
        let Some(g) = graphemes.get(i).copied() else {
            break;
        };
        if in_quotes {
            if g == "\\" {
                // Backslash escape inside quotes: next grapheme is literal (skip backslash).
                current.push('\\');
                i += 1;
                if let Some(next) = graphemes.get(i).copied() {
                    current.push_str(next);
                }
            } else if g == "\"" {
                // End of quoted section - keep the quote char.
                current.push('"');
                in_quotes = false;
            } else {
                current.push_str(g);
            }
        } else if g == "\\" {
            // Backslash escape outside quotes.
            current.push('\\');
            i += 1;
            if let Some(next) = graphemes.get(i).copied() {
                current.push_str(next);
            }
        } else if g == "\"" {
            // Start of quoted section - keep the quote char.
            current.push('"');
            in_quotes = true;
        } else if g.chars().next().is_some_and(char::is_whitespace) {
            if !current.is_empty() {
                args.push(current.clone());
                current.clear();
            }
        } else {
            current.push_str(g);
        }
        i += 1;
    }

    if !current.is_empty() {
        args.push(current);
    }

    args
}

/// Parse a user input string into arguments, respecting double quotes and backslash escapes.
///
/// Rules:
/// - Outside quotes, whitespace separates tokens.
/// - Inside `"..."`, everything (including spaces) is one token; the quotes are stripped.
/// - Backslash escapes: `\"` → `"`, `\\` → `\`, `\x` → `x` for any other char.
/// - An unterminated quote treats the remaining input as the content of the quote.
///
/// # Examples
///
/// ```text
/// foo bar        → ["foo", "bar"]
/// "foo bar"      → ["foo bar"]
/// a "b c" d      → ["a", "b c", "d"]
/// foo\"bar       → ["foo\"bar"]
/// ```
#[must_use]
pub fn parse_quoted_args(input: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let graphemes: Vec<&str> = input.graphemes(true).collect();
    let mut i = 0;

    while i < graphemes.len() {
        let Some(g) = graphemes.get(i).copied() else {
            break;
        };
        if in_quotes {
            if g == "\\" {
                // Backslash escape inside quotes: next grapheme is literal.
                i += 1;
                if let Some(next) = graphemes.get(i).copied() {
                    current.push_str(next);
                } else {
                    // Trailing backslash - treat as literal.
                    current.push('\\');
                }
            } else if g == "\"" {
                // End of quoted section.
                in_quotes = false;
            } else {
                current.push_str(g);
            }
        } else if g == "\\" {
            // Backslash escape outside quotes.
            i += 1;
            if let Some(next) = graphemes.get(i).copied() {
                current.push_str(next);
            } else {
                current.push('\\');
            }
        } else if g == "\"" {
            in_quotes = true;
        } else if g.chars().next().is_some_and(char::is_whitespace) {
            if !current.is_empty() {
                args.push(current.clone());
                current.clear();
            }
        } else {
            current.push_str(g);
        }
        i += 1;
    }

    if !current.is_empty() {
        args.push(current);
    }

    args
}

impl fmt::Display for CommandTemplate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.display())
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::*;

    #[rstest::rstest]
    fn parse_no_params() {
        // Given a setup command with no parameter placeholders.
        let command = "echo hello";

        // When parsing it as a command template.
        let tmpl = CommandTemplate::parse(command);

        // Then it reports having no parameters.
        assert!(!tmpl.has_params());
        // And its parameter list is empty.
        assert_eq!(tmpl.params(), []);
        // And its parameter count is zero.
        assert_eq!(tmpl.param_count(), 0);
    }

    #[rstest::rstest]
    fn parse_one_param() {
        // Given a setup command with a single positional placeholder.
        let command = "script.sh $1";

        // When parsing it as a command template.
        let tmpl = CommandTemplate::parse(command);

        // Then it reports having parameters.
        assert!(tmpl.has_params());
        // And the parameter list holds that one positional parameter.
        assert_eq!(tmpl.params(), &[Param::Positional(1)]);
        // And the parameter count is one.
        assert_eq!(tmpl.param_count(), 1);
    }

    #[rstest::rstest]
    fn parse_multiple_params() {
        // Given a setup command with two positional placeholders.
        let command = "script.sh $1 $2";

        // When parsing it as a command template.
        let tmpl = CommandTemplate::parse(command);

        // Then both positional parameters are extracted in order.
        assert_eq!(tmpl.params(), &[Param::Positional(1), Param::Positional(2)]);
        // And the parameter count is two.
        assert_eq!(tmpl.param_count(), 2);
    }

    #[rstest::rstest]
    fn parse_deduplicates_repeated_params() {
        // Given a setup command that repeats a positional placeholder.
        let command = "script.sh $1 $2 $1";

        // When parsing it as a command template.
        let tmpl = CommandTemplate::parse(command);

        // Then the repeated parameter appears only once.
        assert_eq!(tmpl.params(), &[Param::Positional(1), Param::Positional(2)]);
        // And the parameter count stays at two.
        assert_eq!(tmpl.param_count(), 2);
    }

    #[rstest::rstest]
    fn parse_splat_at() {
        // Given a setup command using the `@` splat placeholder.
        let command = "script.sh $@";

        // When parsing it as a command template.
        let tmpl = CommandTemplate::parse(command);

        // Then the splat placeholder is recognized.
        assert!(tmpl.has_splat());
        // And the template is treated as parameterized.
        assert!(tmpl.has_params());
    }

    #[rstest::rstest]
    fn parse_splat_star() {
        // Given a setup command using the `*` splat placeholder.
        let command = "script.sh $*";

        // When parsing it as a command template.
        let tmpl = CommandTemplate::parse(command);

        // Then the splat placeholder is recognized.
        assert!(tmpl.has_splat());
    }

    #[rstest::rstest]
    fn parse_mixed_numbered_and_splat() {
        // Given a setup command mixing a positional placeholder with a splat.
        let command = "script.sh $1 $@";

        // When parsing it as a command template.
        let tmpl = CommandTemplate::parse(command);

        // Then the positional parameter and the splat are both extracted.
        assert_eq!(tmpl.params(), &[Param::Positional(1), Param::Splat]);
        // And the splat placeholder is recognized.
        assert!(tmpl.has_splat());
        // And only the positional parameter is counted.
        assert_eq!(tmpl.param_count(), 1);
    }

    #[rstest::rstest]
    fn parse_skips_dollar_zero() {
        // Given a setup command using the shell name placeholder `$0`.
        let command = "echo $0";

        // When parsing it as a command template.
        let tmpl = CommandTemplate::parse(command);

        // Then it reports having no parameters.
        assert!(!tmpl.has_params());
    }

    #[rstest::rstest]
    fn parse_non_consecutive_params() {
        // Given a setup command whose positional placeholders are not adjacent.
        let command = "script.sh $1 $3";

        // When parsing it as a command template.
        let tmpl = CommandTemplate::parse(command);

        // Then both placeholders are extracted at their original positions.
        assert_eq!(tmpl.params(), &[Param::Positional(1), Param::Positional(3)]);
        // And the parameter count is two.
        assert_eq!(tmpl.param_count(), 2);
    }

    #[rstest::rstest]
    fn parse_named_param() {
        // Given a setup command with a single named placeholder.
        let command = "script.sh <branch>";

        // When parsing it as a command template.
        let tmpl = CommandTemplate::parse(command);

        // Then it reports having parameters.
        assert!(tmpl.has_params());
        // And the parameter list holds that one named parameter.
        assert_eq!(tmpl.params(), &[Param::Named("branch".to_owned())]);
        // And the parameter count is one.
        assert_eq!(tmpl.param_count(), 1);
    }

    #[rstest::rstest]
    fn parse_multiple_named_params() {
        // Given a setup command with two named placeholders.
        let command = "script.sh <branch> <target>";

        // When parsing it as a command template.
        let tmpl = CommandTemplate::parse(command);

        // Then both named parameters are extracted in order.
        assert_eq!(
            tmpl.params(),
            &[
                Param::Named("branch".to_owned()),
                Param::Named("target".to_owned()),
            ]
        );
        // And the parameter count is two.
        assert_eq!(tmpl.param_count(), 2);
    }

    #[rstest::rstest]
    fn parse_deduplicates_named_params() {
        // Given a setup command that repeats a named placeholder.
        let command = "script.sh <branch> <target> <branch>";

        // When parsing it as a command template.
        let tmpl = CommandTemplate::parse(command);

        // Then the repeated name appears only once.
        assert_eq!(
            tmpl.params(),
            &[
                Param::Named("branch".to_owned()),
                Param::Named("target".to_owned()),
            ]
        );
        // And the parameter count stays at two.
        assert_eq!(tmpl.param_count(), 2);
    }

    #[rstest::rstest]
    fn parse_mixed_named_and_positional() {
        // Given a setup command mixing a named and a positional placeholder.
        let command = "script.sh <branch> $1";

        // When parsing it as a command template.
        let tmpl = CommandTemplate::parse(command);

        // Then both parameter kinds are extracted in order.
        assert_eq!(
            tmpl.params(),
            &[Param::Named("branch".to_owned()), Param::Positional(1)]
        );
        // And the parameter count is two.
        assert_eq!(tmpl.param_count(), 2);
    }

    #[rstest::rstest]
    fn parse_named_with_splat() {
        // Given a setup command mixing a named placeholder with a splat.
        let command = "script.sh <branch> $@";

        // When parsing it as a command template.
        let tmpl = CommandTemplate::parse(command);

        // Then the named parameter and the splat are both extracted.
        assert_eq!(
            tmpl.params(),
            &[Param::Named("branch".to_owned()), Param::Splat]
        );
        // And the splat placeholder is recognized.
        assert!(tmpl.has_splat());
        // And only the named parameter is counted.
        assert_eq!(tmpl.param_count(), 1);
    }

    #[rstest::rstest]
    fn parse_multiple_named_same_value() {
        // Given a setup command whose first named placeholder repeats.
        let command = "script.sh <foo> <bar> <foo>";

        // When parsing it as a command template.
        let tmpl = CommandTemplate::parse(command);

        // Then the repeated name appears only once.
        assert_eq!(
            tmpl.params(),
            &[
                Param::Named("foo".to_owned()),
                Param::Named("bar".to_owned())
            ]
        );
        // And the parameter count stays at two.
        assert_eq!(tmpl.param_count(), 2);
    }

    #[rstest::rstest]
    fn render_no_params() {
        // Given a template parsed from a command with no placeholders.
        let tmpl = CommandTemplate::parse("echo hello");

        // When rendering it with no arguments.
        let rendered = tmpl.render(&[]);

        // Then the command text is unchanged.
        assert_eq!(rendered, "echo hello");
    }

    #[rstest::rstest]
    fn render_one_param() {
        // Given a template with a single positional placeholder and one argument.
        let tmpl = CommandTemplate::parse("script.sh $1");
        let args = ["my-branch".to_owned()];

        // When rendering it.
        let rendered = tmpl.render(&args);

        // Then the placeholder is replaced by the argument.
        assert_eq!(rendered, "script.sh my-branch");
    }

    #[rstest::rstest]
    fn render_multiple_params() {
        // Given a template with two positional placeholders and two arguments.
        let tmpl = CommandTemplate::parse("script.sh $1 $2");
        let args = ["foo".to_owned(), "bar".to_owned()];

        // When rendering it.
        let rendered = tmpl.render(&args);

        // Then each placeholder takes its positional argument.
        assert_eq!(rendered, "script.sh foo bar");
    }

    #[rstest::rstest]
    fn render_repeated_param() {
        // Given a template that repeats a positional placeholder.
        let tmpl = CommandTemplate::parse("script.sh $1 $2 $1");
        let args = ["branch".to_owned(), "dir".to_owned()];

        // When rendering it.
        let rendered = tmpl.render(&args);

        // Then the repeated placeholder is filled from the first argument both times.
        assert_eq!(rendered, "script.sh branch dir branch");
    }

    #[rstest::rstest]
    fn render_splat() {
        // Given a template with a splat placeholder and three arguments.
        let tmpl = CommandTemplate::parse("script.sh $@");
        let args = ["a".to_owned(), "b".to_owned(), "c".to_owned()];

        // When rendering it.
        let rendered = tmpl.render(&args);

        // Then the splat expands to every argument in order.
        assert_eq!(rendered, "script.sh a b c");
    }

    #[rstest::rstest]
    fn render_one_named_param() {
        // Given a template with a single named placeholder and one argument.
        let tmpl = CommandTemplate::parse("script.sh <branch>");
        let args = ["my-feature".to_owned()];

        // When rendering it.
        let rendered = tmpl.render(&args);

        // Then the named placeholder is replaced by the argument.
        assert_eq!(rendered, "script.sh my-feature");
    }

    #[rstest::rstest]
    fn render_multiple_named_params() {
        // Given a template with two named placeholders and two arguments.
        let tmpl = CommandTemplate::parse("script.sh <branch> <target>");
        let args = ["my-feature".to_owned(), "/tmp/workdir".to_owned()];

        // When rendering it.
        let rendered = tmpl.render(&args);

        // Then each named placeholder takes the argument in its own position.
        assert_eq!(rendered, "script.sh my-feature /tmp/workdir");
    }

    #[rstest::rstest]
    fn render_named_with_splat() {
        // Given a template mixing a named placeholder, a splat, and three arguments.
        let tmpl = CommandTemplate::parse("script.sh <branch> $@");
        let args = ["my-feature".to_owned(), "a".to_owned(), "b".to_owned()];

        // When rendering it.
        let rendered = tmpl.render(&args);

        // Then the named placeholder takes the first argument and the splat the rest.
        assert_eq!(rendered, "script.sh my-feature a b");
    }

    #[rstest::rstest]
    fn render_repeated_named_param() {
        // Given a template that repeats a named placeholder alongside a positional one.
        let tmpl = CommandTemplate::parse("script.sh <branch> $2 <branch>");
        let args = ["my-feature".to_owned(), "other".to_owned()];

        // When rendering it.
        let rendered = tmpl.render(&args);

        // Then the repeated named placeholder is filled from the first argument both times.
        assert_eq!(rendered, "script.sh my-feature other my-feature");
    }

    #[rstest::rstest]
    fn render_mixed_named_and_positional() {
        // Given a template mixing a named and a positional placeholder.
        let tmpl = CommandTemplate::parse("script.sh <branch> $1");
        let args = ["my-feature".to_owned(), "dup".to_owned()];

        // When rendering it.
        let rendered = tmpl.render(&args);

        // Then <branch> takes args[0] and $1 takes args[1].
        assert_eq!(rendered, "script.sh my-feature dup");
    }

    #[rstest::rstest]
    fn display_no_params() {
        // Given a template parsed from a command with no placeholders.
        let tmpl = CommandTemplate::parse("echo hello");

        // When building its display form.
        let display = tmpl.display();

        // Then the display form is the command text itself.
        assert_eq!(display, "echo hello");
    }

    #[rstest::rstest]
    fn display_with_params() {
        // Given a template with two positional placeholders.
        let tmpl = CommandTemplate::parse("script.sh $1 $2");

        // When building its display form.
        let display = tmpl.display();

        // Then each placeholder is shown in angle-bracket form.
        assert_eq!(display, "script.sh <1> <2>");
    }

    #[rstest::rstest]
    fn display_with_splat() {
        // Given a template with a splat placeholder.
        let tmpl = CommandTemplate::parse("script.sh $@");

        // When building its display form.
        let display = tmpl.display();

        // Then the splat is shown as the `<args>` placeholder.
        assert_eq!(display, "script.sh <args>");
    }

    #[rstest::rstest]
    fn display_repeated_params() {
        // Given a template that repeats a positional placeholder.
        let tmpl = CommandTemplate::parse("script.sh $1 $2 $1");

        // When building its display form.
        let display = tmpl.display();

        // Then the repetition is preserved in the display form.
        assert_eq!(display, "script.sh <1> <2> <1>");
    }

    #[rstest::rstest]
    fn display_named_params() {
        // Given a template with two named placeholders.
        let tmpl = CommandTemplate::parse("script.sh <branch> <target>");

        // When building its display form.
        let display = tmpl.display();

        // Then the named placeholders are shown unchanged.
        assert_eq!(display, "script.sh <branch> <target>");
    }

    #[rstest::rstest]
    fn display_named_with_positional() {
        // Given a template mixing a named and a positional placeholder.
        let tmpl = CommandTemplate::parse("script.sh <branch> $1");

        // When building its display form.
        let display = tmpl.display();

        // Then only the positional placeholder is converted.
        assert_eq!(display, "script.sh <branch> <1>");
    }

    #[rstest::rstest]
    fn display_does_not_confuse_redirection_with_params() {
        // Given a template that mixes a placeholder with a shell redirection.
        let tmpl = CommandTemplate::parse("echo $1 > output.txt");

        // When building its display form.
        let display = tmpl.display();

        // Then the redirection is left alone and only the placeholder is converted.
        assert_eq!(display, "echo <1> > output.txt");
    }

    #[rstest::rstest]
    fn render_preserves_redirection() {
        // Given a template that mixes a placeholder with a shell redirection.
        let tmpl = CommandTemplate::parse("echo $1 > output.txt");
        let args = ["hello".to_owned()];

        // When rendering it.
        let rendered = tmpl.render(&args);

        // Then the redirection survives rendering.
        assert_eq!(rendered, "echo hello > output.txt");
    }

    #[rstest::rstest]
    fn parse_unclosed_angle_bracket_is_not_a_param() {
        // Given a setup command with an unclosed named placeholder.
        let command = "script.sh <unclosed";

        // When parsing it as a command template.
        let tmpl = CommandTemplate::parse(command);

        // Then it reports having no parameters.
        assert!(!tmpl.has_params());
    }

    #[rstest::rstest]
    fn parse_empty_angle_bracket_is_not_a_param() {
        // Given a setup command with an empty angle-bracket placeholder.
        let command = "script.sh <>";

        // When parsing it as a command template.
        let tmpl = CommandTemplate::parse(command);

        // Then it reports having no parameters.
        assert!(!tmpl.has_params());
    }

    #[rstest::rstest]
    fn display_trait_delegates_to_display_method() {
        // Given a template with two positional placeholders.
        let tmpl = CommandTemplate::parse("script.sh $1 $2");

        // When formatting it with the Display trait.
        let formatted = format!("{tmpl}");

        // Then the output matches the explicit display form.
        assert_eq!(formatted, tmpl.display());
    }

    #[rstest::rstest]
    fn display_line_segments_no_params() {
        // Given a simple command with no params and no &&.
        let tmpl = CommandTemplate::parse("echo hello");

        // When getting display line segments with no args.
        let lines = tmpl.display_line_segments(&[]);

        // Then it produces a single line with one static segment.
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], vec![DisplaySegment::static_text("echo hello")]);
    }

    #[rstest::rstest]
    fn display_line_segments_no_params_single_arg_ignored() {
        // Given a command with no params.
        let tmpl = CommandTemplate::parse("echo hello");

        // When passing args (should be ignored since no placeholders).
        let lines = tmpl.display_line_segments(&["ignored".to_owned()]);

        // Then same as no args.
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], vec![DisplaySegment::static_text("echo hello")]);
    }

    #[rstest::rstest]
    fn display_line_segments_splits_on_and_and() {
        // Given a command with && and no params.
        let tmpl = CommandTemplate::parse("echo hello && echo world");

        // When getting display line segments.
        let lines = tmpl.display_line_segments(&[]);

        // Then it produces two lines with continuation markers.
        assert_eq!(lines.len(), 2);
        // Line 1: "echo hello \"
        assert_eq!(
            lines[0],
            vec![
                DisplaySegment::static_text("echo hello"),
                DisplaySegment::static_text(" \\"),
            ]
        );
        // Line 2: "&& echo world"
        assert_eq!(
            lines[1],
            vec![
                DisplaySegment::static_text("  && "),
                DisplaySegment::static_text("echo world"),
            ]
        );
    }

    #[rstest::rstest]
    fn display_line_segments_five_parts() {
        // Given a long command split into 5 parts by &&.
        let tmpl = CommandTemplate::parse(
            "mkdir <branch> && cd <branch> && fossil open ../jinn.fossil && fossil commit -m 'Open <branch>' --branch <branch> --allow-empty && echo ./<branch>",
        );

        // When getting display line segments with no args.
        let lines = tmpl.display_line_segments(&[]);

        // Then it produces 5 lines.
        assert_eq!(lines.len(), 5);

        // First line: "mkdir <branch> \"
        assert!(lines[0].last().unwrap().text.ends_with('\\'));
        // Last line: "&& echo ./<branch>" (no trailing \).
        assert!(!lines[4].last().unwrap().text.ends_with('\\'));
        // Lines 2-4 start with "  && ".
        assert_eq!(lines[1][0].text, "  && ");
        assert_eq!(lines[2][0].text, "  && ");
        assert_eq!(lines[3][0].text, "  && ");
        assert_eq!(lines[4][0].text, "  && ");
    }

    #[rstest::rstest]
    fn display_line_segments_last_line_no_trailing_backslash() {
        // Given a two-part command.
        let tmpl = CommandTemplate::parse("echo hello && echo world");

        // When getting display line segments.
        let lines = tmpl.display_line_segments(&[]);

        // Then the first line has trailing \ but the last doesn't.
        let first_line_text: String = lines[0].iter().map(|s| s.text.as_str()).collect();
        let last_line_text: String = lines[1].iter().map(|s| s.text.as_str()).collect();
        assert!(first_line_text.ends_with('\\'));
        assert!(!last_line_text.ends_with('\\'));
    }

    #[rstest::rstest]
    fn display_line_segments_substitutes_named_params() {
        // Given a command with named params and &&.
        let tmpl = CommandTemplate::parse("mkdir <branch> && cd <branch>");

        // When getting display line segments with an arg.
        let lines = tmpl.display_line_segments(&["my-feature".to_owned()]);

        // Then <branch> is replaced with "my-feature" and tagged with param index.
        // Line 1: static("mkdir "), param("my-feature", 0), static(" \\").
        assert_eq!(lines[0].len(), 3);
        assert_eq!(lines[0][0], DisplaySegment::static_text("mkdir "));
        assert_eq!(lines[0][1], DisplaySegment::param("my-feature", 0));
        assert_eq!(lines[0][2], DisplaySegment::static_text(" \\"));

        // Line 2: static("  && "), static("cd "), param("my-feature", 0).
        assert_eq!(lines[1][0], DisplaySegment::static_text("  && "));
        assert_eq!(lines[1][1], DisplaySegment::static_text("cd "));
        assert_eq!(lines[1][2], DisplaySegment::param("my-feature", 0));
    }

    #[rstest::rstest]
    fn display_line_segments_substitutes_positional_params() {
        // Given a command with $1 $2 and &&.
        let tmpl = CommandTemplate::parse("script.sh $1 && other.sh $2");

        // When getting display line segments with args.
        let lines = tmpl.display_line_segments(&["foo".to_owned(), "bar".to_owned()]);

        // Then params are replaced in display form (<1> and <2> substituted).
        // Line 1: static("script.sh "), param("foo", 0), static(" \\").
        assert_eq!(lines[0].len(), 3);
        assert_eq!(lines[0][0], DisplaySegment::static_text("script.sh "));
        assert_eq!(lines[0][1], DisplaySegment::param("foo", 0));

        // Line 2: static("  && "), static("other.sh "), param("bar", 1).
        assert_eq!(lines[1][1], DisplaySegment::static_text("other.sh "));
        assert_eq!(lines[1][2], DisplaySegment::param("bar", 1));
    }

    #[rstest::rstest]
    fn display_line_segments_unfilled_params_keep_placeholder() {
        // Given a command with two named params but only one arg provided.
        let tmpl = CommandTemplate::parse("mkdir <branch> && cd <target>");

        // When getting display line segments with only one arg.
        let lines = tmpl.display_line_segments(&["my-feature".to_owned()]);

        // Then the first param is substituted and the second keeps its placeholder.
        // Line 1: static("mkdir "), param("my-feature", 0).
        assert_eq!(lines[0][1], DisplaySegment::param("my-feature", 0));

        // Line 2: static("  && "), static("cd "), param("<target>", 1).
        assert_eq!(lines[1][1], DisplaySegment::static_text("cd "));
        assert_eq!(lines[1][2], DisplaySegment::param("<target>", 1));
    }

    #[rstest::rstest]
    fn display_line_segments_no_args_shows_all_placeholders() {
        // Given a command with named params.
        let tmpl = CommandTemplate::parse("mkdir <branch> && cd <branch>");

        // When getting display line segments with no args.
        let lines = tmpl.display_line_segments(&[]);

        // Then all <branch> placeholders are preserved and tagged with param index.
        assert_eq!(lines[0][1], DisplaySegment::param("<branch>", 0));
        assert_eq!(lines[1][2], DisplaySegment::param("<branch>", 0));
    }

    #[rstest::rstest]
    fn display_line_segments_mixed_named_and_positional() {
        // Given a command with mixed param types.
        let tmpl = CommandTemplate::parse("script.sh <branch> $1 && echo done");

        // When getting display line segments with both args.
        let lines = tmpl.display_line_segments(&["my-branch".to_owned(), "extra".to_owned()]);

        // Then both params are substituted.
        // Line 1: static("script.sh "), param("my-branch", 0), static(" "), param("extra", 1), ...
        assert_eq!(lines[0][1], DisplaySegment::param("my-branch", 0));
        assert_eq!(lines[0][3], DisplaySegment::param("extra", 1));
    }

    #[rstest::rstest]
    fn render_missing_named_param_substitutes_empty() {
        // Given a template with two named params but only one arg.
        let tmpl = CommandTemplate::parse("script.sh <branch> <target>");

        // When rendering with missing args.
        let result = tmpl.render(&["my-branch".to_owned()]);

        // Then the missing param is replaced with empty string (no panic).
        assert_eq!(result, "script.sh my-branch ");
    }

    #[rstest::rstest]
    fn render_missing_positional_param_substitutes_empty() {
        // Given a template with $1 $2 but only one arg.
        let tmpl = CommandTemplate::parse("script.sh $1 $2");

        // When rendering with missing args.
        let result = tmpl.render(&["first".to_owned()]);

        // Then the missing param is replaced with empty string (no panic).
        assert_eq!(result, "script.sh first ");
    }

    #[rstest::rstest]
    fn render_empty_args_list_does_not_panic() {
        // Given a template with params but no args.
        let tmpl = CommandTemplate::parse("script.sh $1 $2");

        // When rendering with empty args list.
        let result = tmpl.render(&[]);

        // Then no panic - missing params replaced with empty.
        assert_eq!(result, "script.sh  ");
    }

    #[rstest::rstest]
    fn parse_quoted_args_empty_input() {
        // Given an empty argument string.
        let input = "";

        // When parsing it into arguments.
        let args = parse_quoted_args(input);

        // Then no arguments are produced.
        assert_eq!(args, Vec::<String>::new());
    }

    #[rstest::rstest]
    fn parse_quoted_args_whitespace_only() {
        // Given an argument string containing only spaces.
        let input = "   ";

        // When parsing it into arguments.
        let args = parse_quoted_args(input);

        // Then no arguments are produced.
        assert_eq!(args, Vec::<String>::new());
    }

    #[rstest::rstest]
    fn parse_quoted_args_unquoted_single() {
        // Given a single unquoted token.
        let input = "foo";

        // When parsing it into arguments.
        let args = parse_quoted_args(input);

        // Then the token is the only argument.
        assert_eq!(args, vec!["foo".to_owned()]);
    }

    #[rstest::rstest]
    fn parse_quoted_args_unquoted_multiple() {
        // Given three whitespace-separated unquoted tokens.
        let input = "foo bar baz";

        // When parsing it into arguments.
        let args = parse_quoted_args(input);

        // Then each token becomes its own argument.
        assert_eq!(
            args,
            vec!["foo".to_owned(), "bar".to_owned(), "baz".to_owned()]
        );
    }

    #[rstest::rstest]
    fn parse_quoted_args_quoted_single() {
        // Given a single quoted token containing a space.
        let input = "\"foo bar\"";

        // When parsing it into arguments.
        let args = parse_quoted_args(input);

        // Then the quotes are stripped and the space is kept.
        assert_eq!(args, vec!["foo bar".to_owned()]);
    }

    #[rstest::rstest]
    fn parse_quoted_args_quoted_preserves_internal_spaces() {
        // Given a quoted token containing several consecutive spaces.
        let input = "\"hello   world\"";

        // When parsing it into arguments.
        let args = parse_quoted_args(input);

        // Then the internal spacing is preserved verbatim.
        assert_eq!(args, vec!["hello   world".to_owned()]);
    }

    #[rstest::rstest]
    fn parse_quoted_args_mixed_quoted_and_unquoted() {
        // Given a quoted token surrounded by unquoted tokens.
        let input = "a \"b c\" d";

        // When parsing it into arguments.
        let args = parse_quoted_args(input);

        // Then each of the three tokens becomes its own argument.
        assert_eq!(args, vec!["a".to_owned(), "b c".to_owned(), "d".to_owned()]);
    }

    #[rstest::rstest]
    fn parse_quoted_args_quoted_at_start() {
        // Given a quoted token followed by an unquoted token.
        let input = "\"foo bar\" baz";

        // When parsing it into arguments.
        let args = parse_quoted_args(input);

        // Then both tokens become arguments with the quotes stripped.
        assert_eq!(args, vec!["foo bar".to_owned(), "baz".to_owned()]);
    }

    #[rstest::rstest]
    fn parse_quoted_args_quoted_at_end() {
        // Given an unquoted token followed by a quoted token.
        let input = "foo \"bar baz\"";

        // When parsing it into arguments.
        let args = parse_quoted_args(input);

        // Then both tokens become arguments with the quotes stripped.
        assert_eq!(args, vec!["foo".to_owned(), "bar baz".to_owned()]);
    }

    #[rstest::rstest]
    fn parse_quoted_args_adjacent_quoted_tokens() {
        // Given two quoted tokens with nothing between them.
        let input = "\"foo\"\"bar\"";

        // When parsing it into arguments.
        let args = parse_quoted_args(input);

        // Then they are concatenated into a single argument.
        assert_eq!(args, vec!["foobar".to_owned()]);
    }

    #[rstest::rstest]
    fn parse_quoted_args_empty_quotes() {
        // Given a quoted token with nothing inside it.
        let input = "\"\"";

        // When parsing it into arguments.
        let args = parse_quoted_args(input);

        // Then no arguments are produced.
        assert_eq!(args, Vec::<String>::new());
    }

    #[rstest::rstest]
    fn parse_quoted_args_empty_quotes_between_tokens() {
        // Given an empty quoted token between two unquoted tokens.
        let input = "foo \"\" bar";

        // When parsing it into arguments.
        let args = parse_quoted_args(input);

        // Then the empty token is dropped and the others survive.
        assert_eq!(args, vec!["foo".to_owned(), "bar".to_owned()]);
    }

    #[rstest::rstest]
    fn parse_quoted_args_unterminated_quote() {
        // Given a quoted token whose closing quote is missing.
        let input = "\"foo bar";

        // When parsing it into arguments.
        let args = parse_quoted_args(input);

        // Then the rest of the input becomes the token content.
        assert_eq!(args, vec!["foo bar".to_owned()]);
    }

    #[rstest::rstest]
    fn parse_quoted_args_unterminated_quote_with_spaces() {
        // Given an unterminated quote followed by further words.
        let input = "\"foo bar baz";

        // When parsing it into arguments.
        let args = parse_quoted_args(input);

        // Then everything after the opening quote is one token.
        assert_eq!(args, vec!["foo bar baz".to_owned()]);
    }

    #[rstest::rstest]
    fn parse_quoted_args_escaped_quote_outside_quotes() {
        // Given an unquoted token with a backslash-escaped quote.
        let input = "foo\\\"bar";

        // When parsing it into arguments.
        let args = parse_quoted_args(input);

        // Then the backslash is dropped and the quote is literal.
        assert_eq!(args, vec!["foo\"bar".to_owned()]);
    }

    #[rstest::rstest]
    fn parse_quoted_args_escaped_quote_inside_quotes() {
        // Given a quoted token with a backslash-escaped quote.
        let input = "\"foo\\\"bar\"";

        // When parsing it into arguments.
        let args = parse_quoted_args(input);

        // Then the backslash is dropped and the quote is literal.
        assert_eq!(args, vec!["foo\"bar".to_owned()]);
    }

    #[rstest::rstest]
    fn parse_quoted_args_escaped_backslash_outside_quotes() {
        // Given an unquoted token with an escaped backslash.
        let input = "foo\\\\bar";

        // When parsing it into arguments.
        let args = parse_quoted_args(input);

        // Then the pair of backslashes collapses to one.
        assert_eq!(args, vec!["foo\\bar".to_owned()]);
    }

    #[rstest::rstest]
    fn parse_quoted_args_escaped_backslash_inside_quotes() {
        // Given a quoted token with an escaped backslash.
        let input = "\"foo\\\\bar\"";

        // When parsing it into arguments.
        let args = parse_quoted_args(input);

        // Then the pair of backslashes collapses to one.
        assert_eq!(args, vec!["foo\\bar".to_owned()]);
    }

    #[rstest::rstest]
    fn parse_quoted_args_escaped_other_char() {
        // Given an unquoted token with a backslash before a plain character.
        let input = "foo\\nbar";

        // When parsing it into arguments.
        let args = parse_quoted_args(input);

        // Then the backslash is dropped without interpreting the escape.
        assert_eq!(args, vec!["foonbar".to_owned()]);
    }

    #[rstest::rstest]
    fn parse_quoted_args_trailing_backslash_outside_quotes() {
        // Given an unquoted token ending in a backslash.
        let input = "foo\\";

        // When parsing it into arguments.
        let args = parse_quoted_args(input);

        // Then the backslash is kept as a literal character.
        assert_eq!(args, vec!["foo\\".to_owned()]);
    }

    #[rstest::rstest]
    fn parse_quoted_args_trailing_backslash_inside_quotes() {
        // Given a quoted token ending in a backslash.
        let input = "\"foo\\";

        // When parsing it into arguments.
        let args = parse_quoted_args(input);

        // Then the backslash is kept as a literal character.
        assert_eq!(args, vec!["foo\\".to_owned()]);
    }

    #[rstest::rstest]
    fn parse_quoted_args_escaped_space_outside_quotes() {
        // Given an unquoted token with a backslash-escaped space.
        let input = "foo\\ bar";

        // When parsing it into arguments.
        let args = parse_quoted_args(input);

        // Then the escape does not split the token and the space survives.
        assert_eq!(args, vec!["foo bar".to_owned()]);
    }

    #[rstest::rstest]
    fn parse_quoted_args_complex_mixed() {
        // Given a string mixing quoted sections, escapes, and unquoted tokens.
        let input = "branch \"my feature\" target\\ dir";

        // When parsing it into arguments.
        let args = parse_quoted_args(input);

        // Then each of the three sources produces its own argument.
        assert_eq!(
            args,
            vec![
                "branch".to_owned(),
                "my feature".to_owned(),
                "target dir".to_owned(),
            ]
        );
    }

    #[rstest::rstest]
    fn parse_quoted_args_multiple_spaces_between_tokens() {
        // Given two tokens separated by several spaces.
        let input = "foo    bar";

        // When parsing it into arguments.
        let args = parse_quoted_args(input);

        // Then the run of spaces acts as a single separator.
        assert_eq!(args, vec!["foo".to_owned(), "bar".to_owned()]);
    }

    #[rstest::rstest]
    fn parse_quoted_args_leading_and_trailing_whitespace() {
        // Given two tokens padded with leading and trailing spaces.
        let input = "  foo bar  ";

        // When parsing it into arguments.
        let args = parse_quoted_args(input);

        // Then the padding is ignored and only the two tokens remain.
        assert_eq!(args, vec!["foo".to_owned(), "bar".to_owned()]);
    }

    #[rstest::rstest]
    fn shell_quote_safe_value_passes_through() {
        // Given a value with no shell-special characters.
        let value = "hello";

        // When shell-quoting it.
        let quoted = shell_quote(value);

        // Then it is returned unchanged.
        assert_eq!(quoted, "hello");
    }

    #[rstest::rstest]
    fn shell_quote_value_with_spaces_is_wrapped() {
        // Given a value containing a space.
        let value = "my branch";

        // When shell-quoting it.
        let quoted = shell_quote(value);

        // Then it is wrapped in single quotes.
        assert_eq!(quoted, "'my branch'");
    }

    #[rstest::rstest]
    fn shell_quote_empty_string_is_empty_quotes() {
        // Given the empty value.
        let value = "";

        // When shell-quoting it.
        let quoted = shell_quote(value);

        // Then it becomes an empty quoted word.
        assert_eq!(quoted, "''");
    }

    #[rstest::rstest]
    fn shell_quote_embedded_single_quote_is_escaped() {
        // Given a value containing a single quote.
        let value = "it's here";

        // When shell-quoting it.
        let quoted = shell_quote(value);

        // Then the embedded quote is closed, escaped, and reopened.
        assert_eq!(quoted, "'it'\\''s here'");
    }

    #[rstest::rstest]
    fn shell_quote_dollar_sign_is_quoted() {
        // Given a value containing a dollar sign.
        let value = "$HOME";

        // When shell-quoting it.
        let quoted = shell_quote(value);

        // Then it is wrapped so the shell does not expand it.
        assert_eq!(quoted, "'$HOME'");
    }

    #[rstest::rstest]
    fn shell_quote_semicolon_is_quoted() {
        // Given a value containing a command separator.
        let value = "foo;bar";

        // When shell-quoting it.
        let quoted = shell_quote(value);

        // Then it is wrapped so the semicolon stays literal.
        assert_eq!(quoted, "'foo;bar'");
    }

    #[rstest::rstest]
    fn shell_quote_pipe_is_quoted() {
        // Given a value containing a pipe.
        let value = "foo|bar";

        // When shell-quoting it.
        let quoted = shell_quote(value);

        // Then it is wrapped so the pipe does not start a new command.
        assert_eq!(quoted, "'foo|bar'");
    }

    #[rstest::rstest]
    fn shell_quote_path_with_slash_is_safe() {
        // Given a filesystem path.
        let value = "/tmp/workdir";

        // When shell-quoting it.
        let quoted = shell_quote(value);

        // Then forward slashes are not shell-special, so it passes through.
        assert_eq!(quoted, "/tmp/workdir");
    }

    #[rstest::rstest]
    fn shell_quote_hyphenated_value_is_safe() {
        // Given a hyphenated value.
        let value = "my-branch";

        // When shell-quoting it.
        let quoted = shell_quote(value);

        // Then hyphens are not shell-special, so it passes through.
        assert_eq!(quoted, "my-branch");
    }

    #[rstest::rstest]
    fn split_preserving_quotes_keeps_quotes() {
        // Given a quoted token followed by an unquoted token.
        let input = "\"my branch\" target";

        // When splitting it into tokens.
        let tokens = split_preserving_quotes(input);

        // Then the surrounding quotes are retained on the first token.
        assert_eq!(
            tokens,
            vec!["\"my branch\"".to_owned(), "target".to_owned()]
        );
    }

    #[rstest::rstest]
    fn split_preserving_quotes_no_quotes() {
        // Given two unquoted tokens.
        let input = "foo bar";

        // When splitting it into tokens.
        let tokens = split_preserving_quotes(input);

        // Then each whitespace-separated word becomes a token.
        assert_eq!(tokens, vec!["foo".to_owned(), "bar".to_owned()]);
    }

    #[rstest::rstest]
    fn split_preserving_quotes_empty_input() {
        // Given an empty string.
        let input = "";

        // When splitting it into tokens.
        let tokens = split_preserving_quotes(input);

        // Then no tokens are produced.
        assert_eq!(tokens, Vec::<String>::new());
    }

    #[rstest::rstest]
    fn split_preserving_quotes_single_quoted_arg() {
        // Given a single quoted token containing a space.
        let input = "\"hello world\"";

        // When splitting it into tokens.
        let tokens = split_preserving_quotes(input);

        // Then the quotes are kept and the token is not split on the space.
        assert_eq!(tokens, vec!["\"hello world\"".to_owned()]);
    }

    #[rstest::rstest]
    fn split_preserving_quotes_unterminated_quote() {
        // Given a quoted token whose closing quote is missing.
        let input = "\"foo bar";

        // When splitting it into tokens.
        let tokens = split_preserving_quotes(input);

        // Then the remainder is one token with its quote preserved.
        assert_eq!(tokens, vec!["\"foo bar".to_owned()]);
    }

    #[rstest::rstest]
    fn tokenize_spans_no_placeholders_returns_single_static() {
        // Given plain text with no angle brackets.
        // When tokenizing.
        let spans = tokenize_spans("hello world");
        // Then a single Static span is returned.
        assert_eq!(spans, vec![Span::Static("hello world".to_owned())]);
    }

    #[rstest::rstest]
    fn tokenize_spans_single_placeholder_with_surrounding_text() {
        // Given text with one placeholder.
        // When tokenizing.
        let spans = tokenize_spans("hello <branch> world");
        // Then three spans: static, placeholder, static.
        assert_eq!(
            spans,
            vec![
                Span::Static("hello ".to_owned()),
                Span::Placeholder("branch".to_owned()),
                Span::Static(" world".to_owned()),
            ]
        );
    }

    #[rstest::rstest]
    fn tokenize_spans_adjacent_placeholders() {
        // Given two placeholders with no gap.
        // When tokenizing.
        let spans = tokenize_spans("<a><b>");
        // Then two Placeholder spans with no Static between them.
        assert_eq!(
            spans,
            vec![
                Span::Placeholder("a".to_owned()),
                Span::Placeholder("b".to_owned()),
            ]
        );
    }

    #[rstest::rstest]
    fn tokenize_spans_unclosed_bracket_is_static() {
        // Given text with an unclosed angle bracket.
        // When tokenizing.
        let spans = tokenize_spans("a <unclosed b");
        // Then the whole string is static (no match).
        assert_eq!(spans, vec![Span::Static("a <unclosed b".to_owned())]);
    }

    #[rstest::rstest]
    fn tokenize_spans_empty_angles_are_static() {
        // Given text with empty angle brackets.
        // When tokenizing.
        let spans = tokenize_spans("a <> b");
        // Then empty <> is not matched (regex requires at least one char).
        assert_eq!(spans, vec![Span::Static("a <> b".to_owned())]);
    }

    #[rstest::rstest]
    fn tokenize_spans_empty_string_returns_empty() {
        // Given an empty string.
        // When tokenizing.
        let spans = tokenize_spans("");
        // Then no spans are produced.
        assert_eq!(spans, [] as [Span; 0]);
    }

    #[rstest::rstest]
    fn tokenize_spans_only_placeholder() {
        // Given text that is only a placeholder.
        // When tokenizing.
        let spans = tokenize_spans("<branch>");
        // Then a single Placeholder span.
        assert_eq!(spans, vec![Span::Placeholder("branch".to_owned())]);
    }

    #[rstest::rstest]
    fn tokenize_spans_placeholder_at_start_and_end() {
        // Given text starting and ending with placeholders.
        // When tokenizing.
        let spans = tokenize_spans("<start> middle <end>");
        // Then two placeholders with static in between.
        assert_eq!(
            spans,
            vec![
                Span::Placeholder("start".to_owned()),
                Span::Static(" middle ".to_owned()),
                Span::Placeholder("end".to_owned()),
            ]
        );
    }

    #[rstest::rstest]
    fn substitute_spans_all_args_provided() {
        // Given spans with two placeholders and both args available.
        let spans = vec![
            Span::Static("mkdir ".to_owned()),
            Span::Placeholder("branch".to_owned()),
        ];
        let params = vec![Param::Named("branch".to_owned())];
        let args = vec!["my-feature".to_owned()];

        // When substituting.
        let segments = substitute_spans(spans, &params, &args);

        // Then the placeholder is replaced with the arg value.
        assert_eq!(segments[0], DisplaySegment::static_text("mkdir "));
        assert_eq!(segments[1], DisplaySegment::param("my-feature", 0));
    }

    #[rstest::rstest]
    fn substitute_spans_missing_args_keep_placeholder() {
        // Given spans with a placeholder but no args.
        let spans = vec![
            Span::Static("mkdir ".to_owned()),
            Span::Placeholder("branch".to_owned()),
        ];
        let params = vec![Param::Named("branch".to_owned())];

        // When substituting with no args.
        let segments = substitute_spans(spans, &params, &[]);

        // Then the placeholder text is preserved.
        assert_eq!(segments[1], DisplaySegment::param("<branch>", 0));
    }

    #[rstest::rstest]
    fn substitute_spans_splat_joins_remaining_args() {
        // Given a Splat param and spans.
        let spans = vec![Span::Placeholder("args".to_owned())];
        let params = vec![Param::Splat];
        let args = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];

        // When substituting.
        let segments = substitute_spans(spans, &params, &args);

        // Then all args are joined with spaces.
        assert_eq!(segments[0], DisplaySegment::param("a b c", 0));
    }

    #[rstest::rstest]
    fn substitute_spans_unknown_placeholder_is_static() {
        // Given a placeholder that doesn't match any param.
        let spans = vec![Span::Placeholder("unknown".to_owned())];
        let params: Vec<Param> = vec![];

        // When substituting.
        let segments = substitute_spans(spans, &params, &[]);

        // Then it's treated as static text.
        assert_eq!(segments[0], DisplaySegment::static_text("<unknown>"));
    }

    // try_parse_dollar edge cases
    #[rstest::rstest]
    fn try_parse_dollar_at_end_of_string_returns_none() {
        // Given "$" as the last grapheme.
        let graphemes: Vec<&str> = vec!["$"];

        // When trying to parse at position 0.
        let result = try_parse_dollar(&graphemes, 0);

        // Then None is returned (no next grapheme to read).
        assert!(result.is_none());
    }

    #[rstest::rstest]
    fn try_parse_dollar_zero_returns_none() {
        // Given "$0" graphemes.
        let graphemes: Vec<&str> = vec!["$", "0"];

        // When trying to parse.
        let result = try_parse_dollar(&graphemes, 0);

        // Then None is returned ($0 is excluded).
        assert!(result.is_none());
    }

    #[rstest::rstest]
    fn try_parse_dollar_digit_returns_positional() {
        // Given "$5" graphemes.
        let graphemes: Vec<&str> = vec!["$", "5"];

        // When trying to parse.
        let result = try_parse_dollar(&graphemes, 0);

        // Then Positional(5) is returned, consuming 2 graphemes.
        let (param, consumed) = result.expect("should parse");
        assert_eq!(param, Param::Positional(5));
        assert_eq!(consumed, 2);
    }

    #[rstest::rstest]
    fn try_parse_dollar_at_returns_splat() {
        // Given "$@" graphemes.
        let graphemes: Vec<&str> = vec!["$", "@"];

        // When trying to parse.
        let result = try_parse_dollar(&graphemes, 0);

        // Then Splat is returned.
        let (param, consumed) = result.expect("should parse");
        assert_eq!(param, Param::Splat);
        assert_eq!(consumed, 2);
    }

    #[rstest::rstest]
    fn try_parse_dollar_star_returns_splat() {
        // Given "$*" graphemes.
        let graphemes: Vec<&str> = vec!["$", "*"];

        // When trying to parse.
        let result = try_parse_dollar(&graphemes, 0);

        // Then Splat is returned.
        let (param, consumed) = result.expect("should parse");
        assert_eq!(param, Param::Splat);
        assert_eq!(consumed, 2);
    }

    #[rstest::rstest]
    fn try_parse_dollar_non_digit_letter_returns_none() {
        // Given "$a" graphemes.
        let graphemes: Vec<&str> = vec!["$", "a"];

        // When trying to parse.
        let result = try_parse_dollar(&graphemes, 0);

        // Then None is returned.
        assert!(result.is_none());
    }

    #[rstest::rstest]
    fn try_parse_dollar_at_non_dollar_position_returns_none() {
        // Given graphemes where position 1 is not "$".
        let graphemes: Vec<&str> = vec!["a", "$"];

        // When trying to parse at position 0.
        let result = try_parse_dollar(&graphemes, 0);

        // Then None is returned.
        assert!(result.is_none());
    }

    // try_parse_named edge cases
    #[rstest::rstest]
    fn try_parse_named_unclosed_angle_returns_none() {
        // Given "<unclosed" (no closing ">".
        let graphemes: Vec<&str> = "<unclosed".graphemes(true).collect();

        // When trying to parse at position 0.
        let result = try_parse_named(&graphemes, 0);

        // Then None is returned.
        assert!(result.is_none());
    }

    #[rstest::rstest]
    fn try_parse_named_empty_angles_returns_none() {
        // Given "<>" graphemes.
        let graphemes: Vec<&str> = "<>".graphemes(true).collect();

        // When trying to parse at position 0.
        let result = try_parse_named(&graphemes, 0);

        // Then None is returned (empty name).
        assert!(result.is_none());
    }

    #[rstest::rstest]
    fn try_parse_named_valid_returns_named_param() {
        // Given "<branch>" graphemes.
        let graphemes: Vec<&str> = "<branch>".graphemes(true).collect();

        // When trying to parse at position 0.
        let result = try_parse_named(&graphemes, 0);

        // Then Named("branch") is returned, consuming all 8 graphemes.
        let (param, consumed) = result.expect("should parse");
        assert_eq!(param, Param::Named("branch".to_owned()));
        assert_eq!(consumed, 8);
    }

    #[rstest::rstest]
    fn try_parse_named_at_non_angle_position_returns_none() {
        // Given "a<b>" graphemes.
        let graphemes: Vec<&str> = "a<b>".graphemes(true).collect();

        // When trying to parse at position 0.
        let result = try_parse_named(&graphemes, 0);

        // Then None is returned.
        assert!(result.is_none());
    }

    #[rstest::rstest]
    fn try_parse_named_adjacent_names() {
        // Given "<a><b>" graphemes.
        let graphemes: Vec<&str> = "<a><b>".graphemes(true).collect();

        // When parsing both.
        let first = try_parse_named(&graphemes, 0).expect("first");
        let second = try_parse_named(&graphemes, first.1).expect("second");

        // Then both are named params with correct consumed counts.
        assert_eq!(first.0, Param::Named("a".to_owned()));
        assert_eq!(second.0, Param::Named("b".to_owned()));
    }

    // split_preserving_quotes boundary cases
    #[rstest::rstest]
    fn split_preserving_quotes_backslash_at_end_inside_quotes() {
        // Given a quoted string ending in backslash.
        let input = "\"foo\\";

        // When splitting it into tokens.
        let tokens = split_preserving_quotes(input);

        // Then the backslash is preserved in the output.
        assert_eq!(tokens, vec!["\"foo\\".to_owned()]);
    }

    #[rstest::rstest]
    fn split_preserving_quotes_backslash_escape_outside_quotes() {
        // Given a backslash-escaped space outside quotes.
        let input = "foo\\ bar";

        // When splitting it into tokens.
        let tokens = split_preserving_quotes(input);

        // Then it produces a single token with the backslash.
        assert_eq!(tokens, vec!["foo\\ bar".to_owned()]);
    }

    #[rstest::rstest]
    fn split_preserving_quotes_adjacent_quotes() {
        // Given two adjacent quoted sections.
        let input = "\"a\"\"b\"";

        // When splitting it into tokens.
        let tokens = split_preserving_quotes(input);

        // Then they are merged into a single token with quotes preserved.
        assert_eq!(tokens, vec!["\"a\"\"b\"".to_owned()]);
    }

    #[rstest::rstest]
    fn split_preserving_quotes_whitespace_separates_tokens() {
        // Given multiple whitespace-separated tokens.
        let input = "a b c";

        // When splitting it into tokens.
        let tokens = split_preserving_quotes(input);

        // Then three separate tokens are returned.
        assert_eq!(tokens, vec!["a".to_owned(), "b".to_owned(), "c".to_owned()]);
    }

    #[rstest::rstest]
    fn parse_command_template_adjacent_dollar_params() {
        // Given a setup command with two adjacent positional placeholders.
        let command = "$1$2";

        // When parsing it as a command template.
        let tmpl = CommandTemplate::parse(command);

        // Then both placeholders are extracted.
        assert_eq!(tmpl.params(), &[Param::Positional(1), Param::Positional(2)]);
    }
}
