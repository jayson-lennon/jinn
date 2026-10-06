use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;

use tree_sitter_highlight::Highlighter;

use super::config::{highlight_to_style, HIGHLIGHT_NAMES};
use super::{CodeHighlighter, StyleSegment};

struct LangEntry {
    language: tree_sitter::Language,
    highlights_query: &'static str,
}

macro_rules! lang_entry {
    ($lang_crate:ident) => {{
        LangEntry {
            language: $lang_crate::LANGUAGE.into(),
            highlights_query: $lang_crate::HIGHLIGHTS_QUERY,
        }
    }};
}

#[cfg(any(
    feature = "highlight-lang-slate",
    feature = "highlight-lang-javascript",
    feature = "highlight-lang-c",
    feature = "highlight-lang-cpp",
    feature = "highlight-lang-bash",
    feature = "highlight-lang-solidity",
))]
macro_rules! lang_entry_sq {
    ($lang_crate:ident) => {{
        LangEntry {
            language: $lang_crate::LANGUAGE.into(),
            highlights_query: $lang_crate::HIGHLIGHT_QUERY,
        }
    }};
}

fn get_lang(lang: &str) -> Option<LangEntry> {
    match lang {
        #[cfg(feature = "highlight-lang-rust")]
        "rust" => Some(lang_entry!(tree_sitter_rust)),

        #[cfg(feature = "highlight-lang-slate")]
        "slate" => Some(lang_entry!(tree_sitter_slate)),

        #[cfg(feature = "highlight-lang-python")]
        "python" | "py" => Some(lang_entry!(tree_sitter_python)),

        #[cfg(feature = "highlight-lang-go")]
        "go" | "golang" => Some(lang_entry!(tree_sitter_go)),

        #[cfg(feature = "highlight-lang-java")]
        "java" => Some(lang_entry!(tree_sitter_java)),

        #[cfg(feature = "highlight-lang-javascript")]
        "javascript" | "js" => Some(lang_entry_sq!(tree_sitter_javascript)),

        #[cfg(feature = "highlight-lang-typescript")]
        "typescript" | "ts" => Some(LangEntry {
            language: tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            highlights_query: tree_sitter_typescript::HIGHLIGHTS_QUERY,
        }),

        #[cfg(feature = "highlight-lang-typescript")]
        "tsx" => Some(LangEntry {
            language: tree_sitter_typescript::LANGUAGE_TSX.into(),
            highlights_query: tree_sitter_typescript::HIGHLIGHTS_QUERY,
        }),

        #[cfg(feature = "highlight-lang-c")]
        "c" => Some(lang_entry_sq!(tree_sitter_c)),

        #[cfg(feature = "highlight-lang-cpp")]
        "cpp" | "c++" | "cxx" => Some(lang_entry_sq!(tree_sitter_cpp)),

        #[cfg(feature = "highlight-lang-c-sharp")]
        "csharp" | "c#" | "cs" => Some(lang_entry!(tree_sitter_c_sharp)),

        #[cfg(feature = "highlight-lang-bash")]
        "bash" | "sh" | "shell" | "zsh" => Some(lang_entry_sq!(tree_sitter_bash)),

        #[cfg(feature = "highlight-lang-ruby")]
        "ruby" | "rb" => Some(lang_entry!(tree_sitter_ruby)),

        #[cfg(feature = "highlight-lang-swift")]
        "swift" => Some(lang_entry!(tree_sitter_swift)),

        #[cfg(feature = "highlight-lang-php")]
        "php" => Some(LangEntry {
            language: tree_sitter_php::LANGUAGE_PHP.into(),
            highlights_query: tree_sitter_php::HIGHLIGHTS_QUERY,
        }),

        #[cfg(feature = "highlight-lang-scala")]
        "scala" => Some(lang_entry!(tree_sitter_scala)),

        #[cfg(feature = "highlight-lang-kotlin")]
        "kotlin" | "kt" => Some(LangEntry {
            language: tree_sitter_kotlin_ng::LANGUAGE.into(),
            highlights_query: KOTLIN_HIGHLIGHTS,
        }),

        #[cfg(feature = "highlight-lang-lua")]
        "lua" => Some(lang_entry!(tree_sitter_lua)),

        #[cfg(feature = "highlight-lang-haskell")]
        "haskell" | "hs" => Some(lang_entry!(tree_sitter_haskell)),

        #[cfg(feature = "highlight-lang-elixir")]
        "elixir" | "ex" => Some(lang_entry!(tree_sitter_elixir)),

        #[cfg(feature = "highlight-lang-yaml")]
        "yaml" | "yml" => Some(lang_entry!(tree_sitter_yaml)),

        #[cfg(feature = "highlight-lang-dart")]
        "dart" => Some(lang_entry!(tree_sitter_dart)),

        #[cfg(feature = "highlight-lang-zig")]
        "zig" => Some(lang_entry!(tree_sitter_zig)),

        #[cfg(feature = "highlight-lang-r")]
        "r" => Some(lang_entry!(tree_sitter_r)),

        #[cfg(feature = "highlight-lang-ocaml")]
        "ocaml" => Some(LangEntry {
            language: tree_sitter_ocaml::LANGUAGE_OCAML.into(),
            highlights_query: tree_sitter_ocaml::HIGHLIGHTS_QUERY,
        }),

        #[cfg(feature = "highlight-lang-nix")]
        "nix" => Some(lang_entry!(tree_sitter_nix)),

        #[cfg(feature = "highlight-lang-html")]
        "html" | "htm" => Some(lang_entry!(tree_sitter_html)),

        #[cfg(feature = "highlight-lang-css")]
        "css" | "scss" | "less" => Some(lang_entry!(tree_sitter_css)),

        #[cfg(feature = "highlight-lang-xml")]
        "xml" | "svg" | "xsd" => Some(LangEntry {
            language: tree_sitter_xml::LANGUAGE_XML.into(),
            highlights_query: tree_sitter_xml::XML_HIGHLIGHT_QUERY,
        }),

        #[cfg(feature = "highlight-lang-json")]
        "json" => Some(lang_entry!(tree_sitter_json)),

        #[cfg(feature = "highlight-lang-toml")]
        "toml" => Some(lang_entry!(tree_sitter_toml_ng)),

        #[cfg(feature = "highlight-lang-sql")]
        "sql" => Some(lang_entry!(tree_sitter_sequel)),

        #[cfg(feature = "highlight-lang-solidity")]
        "solidity" | "sol" => Some(lang_entry_sq!(tree_sitter_solidity)),

        #[cfg(feature = "highlight-lang-diff")]
        "diff" | "patch" => Some(lang_entry!(tree_sitter_diff)),

        #[cfg(feature = "highlight-lang-regex")]
        "regex" | "regexp" => Some(lang_entry!(tree_sitter_regex)),

        #[cfg(feature = "highlight-lang-powershell")]
        "powershell" | "ps1" | "pwsh" => Some(lang_entry!(tree_sitter_powershell)),

        #[cfg(feature = "highlight-lang-objc")]
        "objc" | "objective-c" | "objectivec" => Some(lang_entry!(tree_sitter_objc)),

        #[cfg(feature = "highlight-lang-cmake")]
        "cmake" => Some(LangEntry {
            language: tree_sitter_cmake::LANGUAGE.into(),
            highlights_query: CMAKE_HIGHLIGHTS,
        }),

        #[cfg(feature = "highlight-lang-proto")]
        "proto" | "protobuf" => Some(LangEntry {
            language: tree_sitter_proto::LANGUAGE.into(),
            highlights_query: PROTO_HIGHLIGHTS,
        }),

        _ => None,
    }
}

#[cfg(feature = "highlight-lang-kotlin")]
const KOTLIN_HIGHLIGHTS: &str = r#"
(line_comment) @comment
(block_comment) @comment

(identifier) @variable
((identifier) @variable.builtin (#eq? @variable.builtin "it"))
((identifier) @variable.builtin (#eq? @variable.builtin "field"))
(this_expression) @variable.builtin
(super_expression) @variable.builtin

(class_parameter (identifier) @property)
(class_body (property_declaration (variable_declaration (identifier) @property)))

(enum_entry (identifier) @constant)

; The -ng grammar has no `import_header` wrapper; imports are bare `import`
; nodes. `package_header` contains identifier/qualified_identifier.
(package_header (qualified_identifier (identifier) @namespace))
(import (identifier) @include)
(import (qualified_identifier) @include)

(label) @label

(function_declaration . (identifier) @function)
(getter ("get") @function.builtin)
(setter ("set") @function.builtin)
(primary_constructor) @constructor
(secondary_constructor ("constructor") @constructor)
(constructor_invocation (user_type) @constructor)

; The -ng grammar no longer models `parameter_with_optional_type` nor
; bare `type_identifier`; types are matched via `user_type`.
(parameter (identifier) @variable.parameter)
(lambda_literal (lambda_parameters (variable_declaration (identifier) @variable.parameter)))

(call_expression . (identifier) @function)
(call_expression (navigation_expression (identifier) @function) .)

; Literals were collapsed into `number_literal` in the -ng grammar;
; `null`/`boolean` have no dedicated node and are matched as keywords below.
(number_literal) @number
(float_literal) @number
(character_literal) @string
(string_literal) @string
(multiline_string_literal) @string
(escape_sequence) @string.escape

(type_alias ("typealias") @keyword)
[
  (class_modifier) (member_modifier) (function_modifier)
  (property_modifier) (platform_modifier) (variance_modifier)
  (parameter_modifier) (visibility_modifier) (reification_modifier)
  (inheritance_modifier)
] @keyword
["val" "var" "enum" "class" "object" "interface"] @keyword
("fun") @keyword.function
["if" "else" "when"] @conditional
["for" "do" "while"] @repeat
["try" "catch" "throw" "finally"] @exception
; `break` and `continue` are not anonymous tokens in the -ng grammar;
; they parse as identifiers and are matched by the (identifier) @variable rule.
["return"] @keyword.return

(annotation "@" @attribute (use_site_target)? @attribute)
(annotation (user_type) @attribute)
(annotation (constructor_invocation (user_type) @attribute))
(file_annotation "@" @attribute "file" @attribute ":" @attribute)

["!" "!=" "!==" "=" "==" "===" ">" ">=" "<" "<=" "||" "&&"
 "+" "++" "+=" "-" "--" "-=" "*" "*=" "/" "/=" "%" "%="
 "." "?:" "!!" "is" "in" "as" "as?" ".." "..<" "->"] @operator

[("(") (")") ("[") ("]") ("{") ("}")] @punctuation.bracket
["." "," ";" ":" "::"] @punctuation.delimiter
"#;

#[cfg(feature = "highlight-lang-cmake")]
const CMAKE_HIGHLIGHTS: &str = r#"
[
  (line_comment)
  (bracket_comment)
] @comment

(quoted_argument) @string
(bracket_argument) @string
(variable) @variable
(variable_ref) @variable

(normal_command (identifier) @function)

;
; NOTE: tree-sitter-cmake has no reserved keywords. Control-flow commands like
; if/else/foreach/while/return/macro are ordinary identifiers handled by the
; (normal_command (identifier) @function) rule above. Do not add a @keyword block here.

[
  "ENV" "CACHE"
] @namespace

["$" "{" "}"] @punctuation.special
["(" ")"] @punctuation.bracket
"#;

#[cfg(feature = "highlight-lang-proto")]
const PROTO_HIGHLIGHTS: &str = r#"
[
  "syntax" "package" "option" "import" "service" "rpc"
  "returns" "message" "enum" "oneof" "repeated"
  "reserved" "to" "stream" "map" "extend" "extensions"
  "optional" "required"
] @keyword

[(key_type) (type) (message_name) (enum_name) (service_name) (rpc_name)] @type
(string) @string
[(int_lit) (float_lit)] @number
[(true) (false)] @constant.builtin
(comment) @comment
["(" ")" "[" "]" "{" "}"] @punctuation.bracket
"#;

fn build_config(entry: &LangEntry) -> Option<tree_sitter_highlight::HighlightConfiguration> {
    let mut config = tree_sitter_highlight::HighlightConfiguration::new(
        entry.language.clone(),
        "",
        entry.highlights_query,
        "",
        "",
    )
    .ok()?;
    config.configure(HIGHLIGHT_NAMES);
    Some(config)
}

/// Compiled highlight configurations, keyed by grammar and query.
///
/// `HighlightConfiguration::new` parses *and compiles* the highlight query at
/// runtime — a fixed cost of several milliseconds for a large grammar, paid in
/// full on every `highlight()` call. It is the dominant cost of highlighting and
/// it does not depend on the code being highlighted, so a configuration is built
/// once per grammar and reused thereafter.
///
/// Bounded by the set of grammars compiled into the binary (fixed by cargo
/// features), so the map cannot grow without bound. A query that fails to build
/// is deliberately not cached, leaving that path to degrade on every call
/// exactly as it did before.
fn config_cache() -> &'static Mutex<
    HashMap<
        (tree_sitter::Language, &'static str),
        Arc<tree_sitter_highlight::HighlightConfiguration>,
    >,
> {
    static CACHE: OnceLock<
        Mutex<
            HashMap<
                (tree_sitter::Language, &'static str),
                Arc<tree_sitter_highlight::HighlightConfiguration>,
            >,
        >,
    > = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The compiled configuration for `entry`, built once per grammar.
fn cached_config(entry: &LangEntry) -> Option<Arc<tree_sitter_highlight::HighlightConfiguration>> {
    let mut cache = config_cache().lock();
    let key = (entry.language.clone(), entry.highlights_query);
    if let Some(config) = cache.get(&key) {
        return Some(Arc::clone(config));
    }
    let config = Arc::new(build_config(entry)?);
    cache.insert(key, Arc::clone(&config));
    Some(config)
}

// Compile-time proof that a cached configuration can be shared across threads:
// `HighlightConfiguration` is immutable and its fields are all `Send + Sync`, so
// the `Arc` it lives behind is safe to hand to any thread. A `Highlighter` is
// not, which is why the cache stores configurations rather than highlighters.
#[cfg(test)]
const _: fn() = || {
    fn _assert_shared_config(c: &Arc<tree_sitter_highlight::HighlightConfiguration>) {
        fn require_send_sync<T: Send + Sync>() {}
        require_send_sync::<Arc<tree_sitter_highlight::HighlightConfiguration>>();
        let _ = c;
    }
};

pub struct TreeSitterHighlighter {
    highlighter: Mutex<Highlighter>,
}

// Compile-time proof that the highlighter uses a non-poisoning mutex.
// `parking_lot::Mutex` cannot poison (no PoisonError in its API), so the
// `.lock().unwrap()` panic surface that `std::sync::Mutex` would create is
// eliminated at the type level. If a future edit swaps the field back to
// `std::sync::Mutex`, this assertion fails to compile.
#[cfg(test)]
const _: fn() = || {
    fn _assert_parking_lot_mutex(h: &TreeSitterHighlighter) {
        let _: &parking_lot::Mutex<Highlighter> = &h.highlighter;
    }
};

impl TreeSitterHighlighter {
    pub fn new() -> Self {
        Self {
            highlighter: Mutex::new(Highlighter::new()),
        }
    }
}

impl Default for TreeSitterHighlighter {
    fn default() -> Self {
        Self::new()
    }
}

impl CodeHighlighter for TreeSitterHighlighter {
    fn highlight(&self, lang: &str, code: &str) -> Vec<StyleSegment> {
        let entry = match get_lang(lang) {
            Some(e) => e,
            None => return Vec::new(),
        };
        let config = match cached_config(&entry) {
            Some(c) => c,
            None => return Vec::new(),
        };
        let mut hl = self.highlighter.lock();

        let events = match hl.highlight(&config, code.as_bytes(), None, |_| None) {
            Ok(e) => e,
            Err(_) => return Vec::new(),
        };

        let mut segments = Vec::new();
        let mut style_stack: Vec<usize> = Vec::new();

        for event in events {
            match event {
                Ok(tree_sitter_highlight::HighlightEvent::Source { start, end }) => {
                    let style = style_stack
                        .last()
                        .map(|&idx| highlight_to_style(idx))
                        .unwrap_or_default();
                    if start != end {
                        segments.push(StyleSegment { start, end, style });
                    }
                }
                Ok(tree_sitter_highlight::HighlightEvent::HighlightStart(
                    tree_sitter_highlight::Highlight(idx),
                )) => {
                    style_stack.push(idx);
                }
                Ok(tree_sitter_highlight::HighlightEvent::HighlightEnd) => {
                    style_stack.pop();
                }
                Err(_) => break,
            }
        }

        segments
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use ratatui::style::{Color, Modifier, Style};

    #[cfg(feature = "highlight-lang-kotlin")]
    fn comment_style() -> Style {
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC)
    }

    // Test 1: Kotlin renders (regression) — the original crash.
    #[cfg(feature = "highlight-lang-kotlin")]
    #[test]
    fn kotlin_renders_without_panic() {
        let hl = TreeSitterHighlighter::new();
        let segs = hl.highlight("kotlin", "fun main() {}");
        assert!(
            !segs.is_empty(),
            "Kotlin should produce highlighted segments"
        );
    }

    // Test 2: Kotlin block comment recognized via corrected node name.
    #[cfg(feature = "highlight-lang-kotlin")]
    #[test]
    fn kotlin_block_comment_styled() {
        let hl = TreeSitterHighlighter::new();
        let segs = hl.highlight("kotlin", "/* x */");
        let has_comment = segs.iter().any(|s| s.style == comment_style());
        assert!(
            has_comment,
            "At least one segment should carry the comment style"
        );
    }

    // Test 3: A malformed query degrades gracefully (returns None, no panic).
    #[test]
    fn build_config_rejects_invalid_query() {
        let entry = LangEntry {
            language: tree_sitter_rust::LANGUAGE.into(),
            highlights_query: "(does_not_exist) @comment",
        };
        let config = build_config(&entry);
        assert!(config.is_none(), "Invalid query must yield None, not panic");
    }

    // Test 4: Unknown language degrades to empty segments.
    #[test]
    fn unknown_language_returns_empty() {
        let hl = TreeSitterHighlighter::new();
        let segs = hl.highlight("brainfuck", "+[->]+");
        assert!(
            segs.is_empty(),
            "Unknown language must yield empty segments"
        );
    }

    // Test 5: Valid language with empty code returns empty segments.
    #[test]
    fn empty_code_returns_empty() {
        let hl = TreeSitterHighlighter::new();
        let segs = hl.highlight("rust", "");
        assert!(segs.is_empty(), "Empty code must yield empty segments");
    }

    // Test 6: CMake query still valid (audit guard).
    #[cfg(feature = "highlight-lang-cmake")]
    #[test]
    fn cmake_build_config_is_some() {
        let entry = get_lang("cmake").expect("cmake entry must exist");
        assert!(build_config(&entry).is_some(), "CMake query must build");
    }

    // Test 7: Proto query still valid (audit guard).
    #[cfg(feature = "highlight-lang-proto")]
    #[test]
    fn proto_build_config_is_some() {
        let entry = get_lang("proto").expect("proto entry must exist");
        assert!(build_config(&entry).is_some(), "Proto query must build");
    }

    // Test 8: Slate query still valid, and real Slate source produces
    // styled spans (audit guard).
    #[cfg(feature = "highlight-lang-slate")]
    #[test]
    fn slate_query_builds_and_highlights() {
        let entry = get_lang("slate").expect("slate entry must exist");
        assert!(build_config(&entry).is_some(), "Slate query must build");

        let hl = TreeSitterHighlighter::new();
        let segs = hl.highlight(
            "slate",
            "pub fn area(w: i64, h: i64) -> i64 {\n    w * h\n}\n",
        );
        assert!(!segs.is_empty(), "Slate should produce styled segments");
        assert!(
            segs.iter().any(|s| s.style != ratatui::style::Style::default()),
            "Slate output must contain at least one non-default style"
        );
    }

    // Test 9: Slate's keywords, literals and names each land on a distinct
    // style. Catches a query that compiles but matches nothing -- the failure
    // mode a plain `!segs.is_empty()` check would miss.
    //
    // `StyleSegment` carries byte RANGES, not text, so the source is sliced
    // back out to assert on which token a style actually covers.
    #[cfg(feature = "highlight-lang-slate")]
    #[test]
    fn slate_distinguishes_constructs() {
        const SRC: &str = "pub fn area(w: i64) -> i64 {\n    let n = 42;\n    n + w\n}\n";

        let hl = TreeSitterHighlighter::new();
        let segs = hl.highlight("slate", SRC);

        let tokens: Vec<(String, ratatui::style::Style)> = segs
            .iter()
            .filter(|s| s.style != ratatui::style::Style::default())
            .map(|s| (SRC[s.start..s.end].to_string(), s.style))
            .collect();

        for want in ["pub", "fn", "area", "let", "42"] {
            assert!(
                tokens.iter().any(|(t, _)| t == want),
                "expected `{want}` to be styled; styled tokens were {:?}",
                tokens.iter().map(|(t, _)| t).collect::<Vec<_>>()
            );
        }

        let distinct: std::collections::HashSet<_> = tokens.iter().map(|(_, s)| *s).collect();
        assert!(
            distinct.len() >= 3,
            "expected varied Slate styles, got {} distinct: {tokens:?}",
            distinct.len()
        );
    }

    // Test 9: Sequential highlight() calls never panic — the parking_lot
    // mutex cannot poison, so repeated use stays alive.
    #[test]
    fn sequential_highlight_calls_do_not_panic() {
        let hl = TreeSitterHighlighter::new();
        let _ = hl.highlight("rust", "fn a() {}");
        let _ = hl.highlight("rust", "fn b() {}");
        // If we got here, the second lock() did not panic.
    }

    /// Ceiling on a repeat highlight in an unoptimized test build. The cached
    /// path costs well under a tenth of a millisecond here; recompiling the
    /// query costs tens of milliseconds. Anything near this bound means the
    /// configuration cache stopped working.
    const WARM_HIGHLIGHT_BUDGET: Duration = Duration::from_millis(1);

    #[test]
    fn repeat_highlight_reuses_the_compiled_configuration() {
        // Given a highlighter that has already compiled the rust configuration.
        let hl = TreeSitterHighlighter::new();
        let code = "fn main() { println!(\"hi\"); }";
        assert!(!hl.highlight("rust", code).is_empty());

        // When highlighting the same language again.
        let started = Instant::now();
        let segments = hl.highlight("rust", code);
        let elapsed = started.elapsed();

        // Then the segments are still produced, and no query compilation was paid.
        assert!(!segments.is_empty(), "rust should still highlight");
        assert!(
            elapsed < WARM_HIGHLIGHT_BUDGET,
            "a cached configuration must make a repeat highlight cheap, took {elapsed:?}"
        );
    }

    #[test]
    fn a_repeated_highlight_produces_identical_segments() {
        // Given a highlighter warmed on a snippet.
        let hl = TreeSitterHighlighter::new();
        let code = "fn main() { let x: u32 = 1; }";
        let first = hl.highlight("rust", code);

        // When highlighting the same snippet again.
        let second = hl.highlight("rust", code);

        // Then the cached configuration renders exactly what the fresh one did:
        // same segment boundaries, same styles.
        let shape = |segments: &[StyleSegment]| -> Vec<(usize, usize, Style)> {
            segments.iter().map(|s| (s.start, s.end, s.style)).collect()
        };
        assert_eq!(shape(&first), shape(&second));
    }

    #[test]
    fn a_query_that_fails_to_build_is_not_cached() {
        // Given an entry whose query does not compile.
        let entry = LangEntry {
            language: tree_sitter_rust::LANGUAGE.into(),
            highlights_query: "(does_not_exist) @comment",
        };
        let first = cached_config(&entry);
        let second = cached_config(&entry);

        // Then both attempts fail, so the failure path degrades on every call
        // rather than being memoised into a permanent miss.
        assert!(first.is_none());
        assert!(second.is_none());
        // And nothing was stored for it.
        assert_eq!(
            config_cache()
                .lock()
                .keys()
                .filter(|(lang, _)| *lang == entry.language)
                .count(),
            1,
            "only the valid rust query should be cached for this grammar"
        );
    }

    #[test]
    fn a_cached_configuration_is_shared_between_highlighters() {
        // Given two independent highlighters.
        let first = TreeSitterHighlighter::new();
        let second = TreeSitterHighlighter::new();
        first.highlight("rust", "fn a() {}");

        // When the second one highlights the same language.
        let started = Instant::now();
        let segments = second.highlight("rust", "fn b() {}");
        let elapsed = started.elapsed();

        // Then the cache is process-wide, not per highlighter, so it is cheap too.
        assert!(!segments.is_empty(), "rust should still highlight");
        assert!(
            elapsed < WARM_HIGHLIGHT_BUDGET,
            "the configuration cache must be shared across highlighters, took {elapsed:?}"
        );
    }
}
