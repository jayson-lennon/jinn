//! Shape-based citation detection — pure functions, no I/O.
//!
//! Port of the retired `url-citations` component's detector, over
//! [`jinn_core_types::UrlCitation`] (span fields stay `None` — shape
//! detection has no assistant-text offsets).
//!
//! Three rules feed the per-session citation buffer:
//!
//! - **Call rule** ([`urls_from_call_args`]): recursively walk a tool call's
//!   JSON arguments; every http(s) URL string (anywhere: nested objects,
//!   arrays) is a candidate citation, promoted only when that call's result
//!   succeeds. Strings that themselves parse as a JSON object/array are
//!   descended into (bounded depth), so doubly-encoded payloads surface too.
//! - **Result rule** ([`citations_from_result_content`]): a successful
//!   result whose content parses as JSON contributes every object carrying
//!   both a URL (`url`, or Z.ai-style `link`; http/https) and `title` string
//!   field — the shape Parallel's `web_search`/`web_fetch` and most search
//!   MCP servers return. The snippet comes from `excerpts[0]` when present,
//!   else a bounded prefix of the object's `content` string. As in the call
//!   rule, strings wrapping further JSON are descended into — Z.ai returns
//!   its result array as a JSON *string*.
//! - **`web-search` carve-out** ([`ddg_citation`]): a tool literally named
//!   `web-search` (the removed builtin, or an MCP server reviving the name)
//!   carries no URL in its args and plain-text output, so shape detection
//!   cannot see it; rebuild the DuckDuckGo re-run URL from the `query`
//!   argument. This is the *only* tool-name-specific rule.
//!
//! Unknown shapes are ignored, never errors.

use jinn_core_types::UrlCitation;

/// Maximum depth for descending into strings that embed further JSON.
///
/// Doubly-encoded payloads (a JSON string containing JSON — Z.ai's search
/// results, for example) need one unwrap; this cap is generous headroom over
/// any legitimate wrapping and keeps pathological deeply-nested input
/// terminating with bounded work.
const MAX_EMBEDDED_JSON_DEPTH: usize = 8;

/// Citation snippet ceiling.
///
/// A wrapped fetch-result can carry a full page of markdown in its `content`
/// field (Z.ai's reader shape does); storing it verbatim would bloat
/// session history for text that is never rendered. 512 chars preserves a
/// meaningful excerpt while bounding the payload.
const MAX_SNIPPET_CHARS: usize = 512;

/// Whether a string is an http(s) URL.
fn is_http_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

/// Unwraps one layer of embedded JSON: a string whose text parses as JSON.
///
/// Any parseable value is returned — an object/array (Z.ai's wrapped result
/// array) or another string (re-quoted layers, each quote level one descent
/// step). `None` means "nothing embedded" (plain scalar text, unparseable, or
/// depth budget exhausted); the caller treats the value as opaque.
fn embedded_json(value: &serde_json::Value, depth: usize) -> Option<serde_json::Value> {
    if depth >= MAX_EMBEDDED_JSON_DEPTH {
        return None;
    }
    let text = value.as_str()?;
    serde_json::from_str::<serde_json::Value>(text).ok()
}

/// Call rule: every http(s) URL string found anywhere in the JSON value.
///
/// The argument JSON string is parsed (unparseable yields nothing — the
/// model's arguments are not our problem) and walked recursively; strings
/// inside arrays and nested objects all count (e.g. `{"urls": ["http://a"]}`
/// from Parallel's `web_fetch`, or `{"url": "..."}` from jinn's builtin
/// `web-fetch`).
pub fn urls_from_call_args(arguments: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(arguments) else {
        return Vec::new();
    };
    let mut urls = Vec::new();
    collect_urls(&value, 0, &mut urls);
    urls
}

/// Recursive walk collecting http(s) strings, descending into embedded JSON.
fn collect_urls(value: &serde_json::Value, depth: usize, urls: &mut Vec<String>) {
    match value {
        serde_json::Value::String(s) => {
            if is_http_url(s) {
                urls.push(s.clone());
            }
            // A string can hold a URL *and* wrapped JSON is not one (a JSON
            // text starts with `{`/`[`), so the two cases never collide; try
            // descending regardless of what the string held.
            if let Some(inner) = embedded_json(value, depth) {
                collect_urls(&inner, depth + 1, urls);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_urls(item, depth, urls);
            }
        }
        serde_json::Value::Object(map) => {
            for item in map.values() {
                collect_urls(item, depth, urls);
            }
        }
        _ => {}
    }
}

/// Result rule: citations for every `{url, title}` object in result JSON.
///
/// The content must parse as JSON (a failed parse is not an error — plain
/// text results like the builtin `web-search` output simply yield nothing).
/// Matching objects may sit anywhere in the tree (e.g. under a `results`
/// array key). `excerpts[0]`, when present, becomes the citation content.
pub fn citations_from_result_content(content: &str) -> Vec<UrlCitation> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(content) else {
        return Vec::new();
    };
    let mut citations = Vec::new();
    collect_url_title_objects(&value, 0, &mut citations);
    citations
}

/// Recursive walk collecting objects with both a URL (`url`, or Z.ai-style
/// `link`) and a `title` string, descending into embedded JSON.
///
/// When an object carries both keys, the canonical `url` wins.
fn collect_url_title_objects(value: &serde_json::Value, depth: usize, out: &mut Vec<UrlCitation>) {
    match value {
        serde_json::Value::Object(map) => {
            let url = map
                .get("url")
                .and_then(serde_json::Value::as_str)
                .or_else(|| map.get("link").and_then(serde_json::Value::as_str));
            let title = map.get("title").and_then(serde_json::Value::as_str);
            if let (Some(url), Some(title)) = (url, title)
                && is_http_url(url)
            {
                out.push(UrlCitation {
                    url: url.to_owned(),
                    title: title.to_owned(),
                    content: snippet_for(map),
                    start_index: None,
                    end_index: None,
                });
            }
            for item in map.values() {
                collect_url_title_objects(item, depth, out);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_url_title_objects(item, depth, out);
            }
        }
        // Strings may wrap another layer of JSON (Z.ai returns its result
        // array as a JSON *string* inside the tool output) — descend one
        // wrapped level at a time until the budget runs out.
        serde_json::Value::String(_) => {
            if let Some(inner) = embedded_json(value, depth) {
                collect_url_title_objects(&inner, depth + 1, out);
            }
        }
        _ => {}
    }
}

/// The first `excerpts` array string, when present (Parallel's shape).
fn first_excerpt(map: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    map.get("excerpts")
        .and_then(serde_json::Value::as_array)
        .and_then(|excerpts| excerpts.first())
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

/// The citation snippet: the first excerpt when present, else the object's
/// `content` string (Z.ai's shape), truncated to [`MAX_SNIPPET_CHARS`] chars.
///
/// An absent or empty snippet yields `None` (nothing worth storing).
fn snippet_for(map: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    let raw = first_excerpt(map).or_else(|| {
        map.get("content")
            .and_then(serde_json::Value::as_str)
            .filter(|content| !content.is_empty())
            .map(str::to_owned)
    })?;
    Some(truncate_chars(&raw, MAX_SNIPPET_CHARS))
}

/// A prefix of `s` of at most `max` chars, cut on a char boundary.
fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        // `cut` is a byte offset from `char_indices` — always a boundary.
        Some((cut, _)) => s.get(..cut).map_or_else(|| s.to_owned(), str::to_owned),
        None => s.to_owned(),
    }
}

/// The `web-search` carve-out: rebuild the DuckDuckGo re-run URL.
///
/// A tool named `web-search` carries `{"query": "..."}` arguments and
/// plain-text output — invisible to both generic rules. The DDG URL
/// (form-encoded query) is the "re-run this search" affordance the old core
/// collector produced; `None` means the arguments were unparseable or had
/// no query string.
pub fn ddg_citation(arguments: &str) -> Option<UrlCitation> {
    let query = serde_json::from_str::<serde_json::Value>(arguments)
        .ok()?
        .get("query")?
        .as_str()?
        .to_owned();
    Some(UrlCitation {
        url: ddg_search_url(&query),
        title: query,
        content: None,
        start_index: None,
        end_index: None,
    })
}

/// Builds `https://duckduckgo.com/?q=<form-encoded query>`.
///
/// Form-encoding ensures spaces and special characters survive terminal
/// auto-linking.
pub fn ddg_search_url(query: &str) -> String {
    let q = form_urlencoded::byte_serialize(query.as_bytes()).collect::<String>();
    format!("https://duckduckgo.com/?q={q}")
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        reason = "test code"
    )]
    use super::*;

    // ── Call rule ────────────────────────────────────────────────────────

    #[rstest::rstest]
    #[test]
    fn call_rule_finds_top_level_url() {
        // Given builtin web-fetch arguments.
        // When extracting URLs.
        // Then the URL is found.
        assert_eq!(
            urls_from_call_args(r#"{"url":"https://example.com/page"}"#),
            vec!["https://example.com/page".to_owned()]
        );
    }

    #[rstest::rstest]
    #[test]
    fn call_rule_finds_urls_in_array() {
        // Given Parallel web_fetch arguments (urls array).
        // When extracting URLs.
        // Then both URLs are found in order.
        assert_eq!(
            urls_from_call_args(
                r#"{"urls":["https://a.example","http://b.example"],"objective":"x"}"#
            ),
            vec![
                "https://a.example".to_owned(),
                "http://b.example".to_owned()
            ]
        );
    }

    #[rstest::rstest]
    #[test]
    fn call_rule_finds_nested_url() {
        // Given arguments with a URL nested in an object.
        // When extracting URLs.
        // Then the nested URL is found.
        assert_eq!(
            urls_from_call_args(r#"{"outer":{"inner":{"url":"https://deep.example"}}}"#),
            vec!["https://deep.example".to_owned()]
        );
    }

    #[rstest::rstest]
    #[test]
    fn call_rule_ignores_non_http_schemes() {
        // Given arguments with ftp and bare-host strings.
        // When extracting URLs.
        // Then nothing is found.
        assert_eq!(
            urls_from_call_args(r#"{"a":"ftp://nope","b":"example.com","c":"mailto:x@y"}"#),
            Vec::<String>::new()
        );
    }

    #[rstest::rstest]
    #[test]
    fn call_rule_ignores_unparseable_args() {
        // Given non-JSON arguments.
        // When extracting URLs.
        // Then nothing is found and nothing panics.
        assert_eq!(urls_from_call_args("not json"), Vec::<String>::new());
    }

    #[rstest::rstest]
    #[test]
    fn call_rule_descends_into_embedded_json_arguments() {
        // Given arguments whose payload field is a JSON string wrapping an
        // object that carries a URL (a doubly-encoded call argument).
        let args = r#"{"payload":"{\"urls\":[\"https://wrapped.example\"],\"note\":\"x\"}"}"#;

        // When extracting URLs.
        let urls = urls_from_call_args(args);

        // Then the URL inside the wrapped layer is found.
        assert_eq!(urls, vec!["https://wrapped.example".to_owned()]);
    }

    #[rstest::rstest]
    #[test]
    fn deep_nested_wrapping_terminates_within_depth_cap() {
        // Given arguments with a string nesting one step beyond the descent
        // budget — each layer a JSON string containing more JSON.
        //
        // CAUTION building these fixtures: every quoting layer roughly
        // DOUBLES the text length, so a few dozen layers would construct a
        // multiterabyte string (the fixture, not the detector, would hang).
        // `cap + 4` layers stays a ~100 KB input while still exercising the
        // cut-off.
        let layers = MAX_EMBEDDED_JSON_DEPTH + 4;
        let mut inner = r#""https://bottom.example""#.to_owned();
        for _ in 0..layers {
            inner = serde_json::to_string(&inner).expect("string round-trip");
        }
        let args = format!(r#"{{"payload":{inner}}}"#);

        // When extracting URLs.
        let urls = urls_from_call_args(&args);

        // Then extraction terminates promptly and the URL sitting below the
        // depth cap is invisible.
        assert_eq!(urls, Vec::<String>::new());
    }

    #[rstest::rstest]
    #[test]
    fn wrapping_within_the_depth_cap_still_surfaces_urls() {
        // Given arguments with three wrapping layers — comfortably inside
        // the descent budget.
        let mut inner = r#""https://shallow.example""#.to_owned();
        for _ in 0..3 {
            inner = serde_json::to_string(&inner).expect("string round-trip");
        }
        let args = format!(r#"{{"payload":{inner}}}"#);

        // When extracting URLs.
        let urls = urls_from_call_args(&args);

        // Then the URL inside all three layers is found.
        assert_eq!(urls, vec!["https://shallow.example".to_owned()]);
    }

    // ── Result rule ──────────────────────────────────────────────────────

    #[rstest::rstest]
    #[test]
    fn result_rule_reads_parallel_web_search_shape() {
        // Given a Parallel web_search result payload.
        let content = concat!(
            r#"{"#,
            r#""search_id": "search_e593614d82424176ae7dfce52d958cf9", "#,
            r#""results": [{"url": "https://parallel.ai/blog/series-a", "#,
            r#""title": "Parallel raises $100M Series A", "#,
            r#""publish_date": null, "#,
            r#""excerpts": ["Answer-ready excerpt text."]}]}"#
        );

        // When extracting citations.
        let citations = citations_from_result_content(content);

        // Then the result object yields one citation with the excerpt.
        assert_eq!(citations.len(), 1);
        assert_eq!(citations[0].url, "https://parallel.ai/blog/series-a");
        assert_eq!(citations[0].title, "Parallel raises $100M Series A");
        assert_eq!(
            citations[0].content.as_deref(),
            Some("Answer-ready excerpt text.")
        );
    }

    #[rstest::rstest]
    #[test]
    fn result_rule_reads_multiple_results() {
        // Given a payload with three result objects.
        let content = concat!(
            r#"{"results": ["#,
            r#"{"url":"https://a.example","title":"A"}, "#,
            r#"{"url":"https://b.example","title":"B"}, "#,
            r#"{"url":"https://c.example","title":"C"}]}"#
        );

        // When extracting citations.
        // Then all three yield, in order.
        assert_eq!(citations_from_result_content(content).len(), 3);
    }

    #[rstest::rstest]
    #[test]
    fn result_rule_ignores_plain_text() {
        // Given the builtin web-search plain-text output.
        let content = "1. Title — https://example.com/thing\n   snippet text";

        // When extracting citations.
        // Then nothing is found (not JSON).
        assert_eq!(
            citations_from_result_content(content),
            [] as [jinn_core_types::UrlCitation; 0]
        );
    }

    #[rstest::rstest]
    #[test]
    fn result_rule_ignores_json_without_url_title_pairs() {
        // Given JSON with a URL but no title pairing.
        // When extracting citations.
        // Then nothing is found.
        assert_eq!(
            citations_from_result_content(r#"{"url":"https://a.example"}"#),
            [] as [jinn_core_types::UrlCitation; 0]
        );
        assert_eq!(
            citations_from_result_content(r#"{"results":[{"url":"https://a.example"}]}"#),
            [] as [jinn_core_types::UrlCitation; 0]
        );
    }

    #[rstest::rstest]
    #[test]
    fn result_rule_ignores_non_http_urls() {
        // Given a result object with an ftp url.
        // When extracting citations.
        // Then nothing is found.
        assert_eq!(
            citations_from_result_content(
                r#"{"results":[{"url":"ftp://files.example","title":"F"}]}"#
            ),
            [] as [jinn_core_types::UrlCitation; 0]
        );
    }

    // ── link alias + embedded JSON (Z.ai shapes) ─────────────────────────

    /// Z.ai `web_search_prime` returns its results as a JSON *string* whose
    /// objects carry `{title, link, content, refer}` — i.e. the array text
    /// itself is quoted and escaped inside the tool output.
    fn zai_wrapped_results(entries: &str) -> String {
        let escaped = entries.replace('"', "\\\"");
        format!("\"[{escaped}]\"")
    }

    #[rstest::rstest]
    #[test]
    fn result_rule_decodes_double_encoded_zai_array() {
        // Given a doubly-encoded Zai-style result payload (string-wrapped
        // array of {title, link, content, refer} objects).
        let content = zai_wrapped_results(
            r#"{"title":"Mega Man Legends (series) - MMKB - Fandom","link":"https://megaman.fandom.com/wiki/Mega_Man_Legends_(series)","content":"It is centered around MegaMan Volnutt.","refer":"ref_1"},{"title":"Mega Man Legends","link":"https://en.wikipedia.org/wiki/Mega_Man_Legends","content":"The player controls Mega Man Volnutt.","refer":"ref_2"}"#,
        );

        // When extracting citations.
        let citations = citations_from_result_content(&content);

        // Then both entries surface with the link as URL and the content
        // field as the snippet.
        assert_eq!(citations.len(), 2);
        assert_eq!(
            citations[0].url,
            "https://megaman.fandom.com/wiki/Mega_Man_Legends_(series)"
        );
        assert_eq!(
            citations[0].title,
            "Mega Man Legends (series) - MMKB - Fandom"
        );
        assert_eq!(
            citations[0].content.as_deref(),
            Some("It is centered around MegaMan Volnutt.")
        );
        assert_eq!(
            citations[1].url,
            "https://en.wikipedia.org/wiki/Mega_Man_Legends"
        );
    }

    #[rstest::rstest]
    #[test]
    fn result_rule_accepts_bare_link_objects() {
        // Given an unwrapped object using `link` instead of `url`.
        let content = r#"{"results":[{"link":"https://a.example","title":"A","content":"x"}]}"#;

        // When extracting citations.
        let citations = citations_from_result_content(content);

        // Then the entry surfaces with its content snippet.
        assert_eq!(citations.len(), 1);
        assert_eq!(citations[0].url, "https://a.example");
        assert_eq!(citations[0].title, "A");
        assert_eq!(citations[0].content.as_deref(), Some("x"));
    }

    #[rstest::rstest]
    #[test]
    fn result_rule_prefers_url_over_link_when_both_present() {
        // Given an object carrying both keys.
        let content = concat!(
            r#"{"results": ["#,
            r#"{"url":"https://canonical.example","link":"https://decorated.example","title":"T"}]}"#
        );

        // When extracting citations.
        let citations = citations_from_result_content(content);

        // Then the canonical url wins.
        assert_eq!(citations[0].url, "https://canonical.example");
    }

    #[rstest::rstest]
    #[test]
    fn snippet_falls_back_to_content_when_excerpts_absent() {
        // Given a Zai-shaped object with only a `content` string.
        let content = zai_wrapped_results(
            r#"{"title":"T","link":"https://a.example","content":"snippet text here","refer":"ref_1"}"#,
        );

        // When extracting citations.
        let citations = citations_from_result_content(&content);

        // Then the citation's content is the object's content string.
        assert_eq!(citations[0].content.as_deref(), Some("snippet text here"));
    }

    #[rstest::rstest]
    #[test]
    fn snippet_truncates_long_content_on_a_char_boundary() {
        // Given a wrapped fetch-shaped object whose content is far longer
        // than the snippet cap and ends in multibyte characters.
        let long = "x".repeat(MAX_SNIPPET_CHARS + 100);
        let multibyte_tail = "日本語テキスト".repeat(200);
        for oversized in [long.clone(), format!("{long}{multibyte_tail}")] {
            let escaped = oversized.replace('\\', "\\\\").replace('"', "\\\"");
            let content = zai_wrapped_results(&format!(
                r#"{{"title":"T","link":"https://a.example","content":"{escaped}"}}"#
            ));

            // When extracting citations.
            let citations = citations_from_result_content(&content);

            // Then the stored snippet is capped at MAX_SNIPPET_CHARS chars
            // without panicking mid-character.
            let snippet = citations[0]
                .content
                .as_deref()
                .expect("oversized content yields a bounded snippet");
            assert_eq!(snippet.chars().count(), MAX_SNIPPET_CHARS);
        }
    }

    #[rstest::rstest]
    #[test]
    fn snippet_prefers_excerpts_over_content() {
        // Given an object carrying both an excerpts array and a content
        // string (a hybrid shape).
        let content = concat!(
            r#"{"results": [{"#,
            r#""url":"https://parallel.example", "#,
            r#""title":"P", "#,
            r#""excerpts":["the excerpt"], "#,
            r#""content":"the full text"}]}"#
        );

        // When extracting citations.
        let citations = citations_from_result_content(content);

        // Then the excerpt wins (Parallel parity preserved).
        assert_eq!(citations[0].content.as_deref(), Some("the excerpt"));
    }

    #[rstest::rstest]
    #[test]
    fn snippet_is_none_for_empty_or_missing_content() {
        // Given objects whose snippet fields are missing or empty strings.
        let empty_string = r#"{"results":[{"url":"https://a.example","title":"A","content":""}]}"#;
        let unwrapped_empty = zai_wrapped_results(
            r#"{"title":"B","link":"https://b.example","content":"","refer":"ref_9"}"#,
        );
        let missing =
            zai_wrapped_results(r#"{"title":"C","link":"https://c.example","refer":"ref_8"}"#);

        // When extracting citations from each.
        // Then no citation carries an empty-string snippet.
        let contents = [empty_string, unwrapped_empty.as_str(), missing.as_str()];
        for content in contents {
            let citations = citations_from_result_content(content);
            assert!(!citations.is_empty(), "{content}");
            assert!(citations[0].content.is_none(), "{content}");
        }
    }

    #[rstest::rstest]
    #[test]
    fn result_rule_ignores_non_http_link() {
        // Given a Zai-shaped entry with a non-http(s) link.
        let content = zai_wrapped_results(
            r#"{"title":"F","link":"ftp://files.example","content":"x","refer":"ref_1"}"#,
        );

        // When extracting citations.
        // Then nothing is found — the http(s) guard applies to the alias.
        assert_eq!(
            citations_from_result_content(&content),
            [] as [jinn_core_types::UrlCitation; 0]
        );
    }

    #[rstest::rstest]
    #[test]
    fn opaque_wrapper_strings_are_inert() {
        // Given payloads whose strings do NOT parse as JSON containers.
        let scalar_result = r#"[{"url":"https://a.example","title":"A","note":"just text"}]"#;
        let plain_text = "\"not json at all\"";
        let number_string = "\"42\"";

        // When extracting citations from each.
        // Then every payload is inert — unknown shapes yield nothing and
        // valid unwrapped entries inside arrays still parse normally.
        assert_eq!(citations_from_result_content(scalar_result).len(), 1);
        assert_eq!(
            citations_from_result_content(plain_text),
            [] as [jinn_core_types::UrlCitation; 0]
        );
        assert_eq!(
            citations_from_result_content(number_string),
            [] as [jinn_core_types::UrlCitation; 0]
        );
    }

    // ── web-search carve-out ─────────────────────────────────────────────

    #[rstest::rstest]
    #[test]
    fn ddg_citation_builds_from_query() {
        // Given builtin web-search arguments.
        // When building the carve-out citation.
        let citation = ddg_citation(r#"{"query":"rust async await"}"#);

        // Then the URL encodes the query with the title as the query.
        assert!(citation.is_some());
        let citation = citation.expect("checked");
        assert_eq!(citation.url, "https://duckduckgo.com/?q=rust+async+await");
        assert_eq!(citation.title, "rust async await");
    }

    #[rstest::rstest]
    #[test]
    fn ddg_citation_encodes_special_characters() {
        // Given a query with spaces and &.
        // When building the URL.
        // Then the query is form-encoded.
        assert_eq!(
            ddg_search_url("rust async & await"),
            "https://duckduckgo.com/?q=rust+async+%26+await"
        );
    }

    #[rstest::rstest]
    #[test]
    fn ddg_citation_encodes_empty_query() {
        // Given an empty query.
        // When building the URL.
        // Then it is the bare search endpoint.
        assert_eq!(ddg_search_url(""), "https://duckduckgo.com/?q=");
    }

    #[rstest::rstest]
    #[test]
    fn ddg_citation_none_without_query() {
        // Given arguments with no query field.
        // When building the carve-out.
        // Then None is returned.
        assert!(ddg_citation(r#"{"objective":"x"}"#).is_none());
        assert!(ddg_citation("not json").is_none());
    }
}
