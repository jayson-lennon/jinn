//! The transcript projection's render tests: entry markup shapes, the
//! escaping boundary, the sources disclosure, and the footnote
//! markdown rendering.

//! Test code: assertions use expect/panic to state what went wrong.
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions need expect/panic to state what went wrong"
)]

use jinn_core_types::chat_entry::ChatEntry;
use jinn_core_types::url_citation::UrlCitation;

use crate::transcript::escape_html;
use crate::transcript::render_entry;
use crate::transcript::render_entry_streaming;

/// The fixed session id the entry renders name — attachments key the
/// image URLs off it.
fn session_id() -> jinn_core_types::SessionId {
    jinn_core_types::SessionId::try_from_string("00000000-0000-0000-0000-000000000000")
        .expect("valid uuid")
}

/// A citation with the given title and URL.
fn citation(title: &str, url: &str) -> UrlCitation {
    UrlCitation {
        url: url.to_owned(),
        title: title.to_owned(),
        content: None,
        start_index: None,
        end_index: None,
    }
}

#[rstest::rstest]
#[test]
fn an_annotation_entry_renders_as_a_collapsed_sources_disclosure() {
    // Given an annotation entry carrying two citations.
    let entry = ChatEntry::annotation(vec![
        citation("Example", "https://example.com/a"),
        citation("Docs", "https://docs.example.com/b"),
    ]);

    // When rendering it through the settled projection.
    let rendered = render_entry(&entry, &session_id());

    // Then it is a disclosure whose summary carries the source count,
    // and the summary names the sources — collapsed by default (TUI
    // parity), never force-opened.
    assert!(rendered.is_disclosure);
    assert!(!rendered.is_streaming);
    assert_eq!(
        rendered.summary.as_deref(),
        Some("Sources (2)"),
        "{rendered:?}"
    );
}

#[rstest::rstest]
#[test]
fn expanded_source_rows_link_http_urls_with_noopener() {
    // Given an annotation entry with two http(s) citations.
    let entry = ChatEntry::annotation(vec![
        citation("Example", "https://example.com/a"),
        citation("Local", "http://localhost:8080/doc"),
    ]);

    // When rendering it.
    let rendered = render_entry(&entry, &session_id());

    // Then each citation row links its URL, opening a new tab under
    // `noopener`, with the title as the link text and the URL as
    // muted text beside it — in arrival order.
    assert!(rendered
        .body_html
        .contains("<a class=\"citation-title\" href=\"https://example.com/a\" target=\"_blank\" rel=\"noopener\">Example</a>"));
    assert!(
        rendered
            .body_html
            .contains("<span class=\"citation-url\">https://example.com/a</span>")
    );
    assert!(
        rendered
            .body_html
            .contains("href=\"http://localhost:8080/doc\"")
    );
    let first = rendered
        .body_html
        .find("example.com/a")
        .expect("the first citation renders");
    let second = rendered
        .body_html
        .find("localhost:8080")
        .expect("the second citation renders");
    assert!(first < second, "rows render in arrival order");
}

#[rstest::rstest]
#[test]
fn a_non_http_citation_url_renders_as_plain_text_never_a_link() {
    // Given an annotation entry whose citation URL is a
    // `javascript:` payload (the detector never emits this; the render
    // re-checks rather than trusting it).
    let entry = ChatEntry::annotation(vec![citation(
        "Hostile",
        "javascript:alert(document.domain)",
    )]);

    // When rendering it.
    let rendered = render_entry(&entry, &session_id());

    // Then the row is unlinked — the hostile scheme renders as text.
    assert!(
        !rendered.body_html.contains("<a "),
        "{}",
        rendered.body_html
    );
    assert!(rendered.body_html.contains("javascript:alert"));
}

#[rstest::rstest]
#[test]
fn citation_fields_are_escaped_against_hostile_payloads() {
    // Given an annotation entry whose title and URL carry script
    // injection payloads.
    let entry = ChatEntry::annotation(vec![citation(
        "<script>alert('title')</script>",
        "https://example.com/\"onmouseover='alert(1)'",
    )]);

    // When rendering it.
    let rendered = render_entry(&entry, &session_id());

    // Then no raw tag or attribute-breaking quote survives into the
    // markup — every provider-supplied field is escaped, including
    // inside the href attribute value.
    assert!(
        !rendered.body_html.contains("<script>"),
        "{}",
        rendered.body_html
    );
    assert!(
        !rendered.body_html.contains("onmouseover='"),
        "{}",
        rendered.body_html
    );
    assert!(rendered.body_html.contains("&lt;script&gt;"));
    // And the href quotes are escaped entities, so the attribute
    // cannot be broken out of.
    assert!(
        rendered.body_html.contains("&quot;onmouseover"),
        "{}",
        rendered.body_html
    );
}

#[rstest::rstest]
#[test]
fn an_empty_citation_title_falls_back_to_the_url_as_the_link_text() {
    // Given an annotation entry whose citation has no title.
    let entry = ChatEntry::annotation(vec![citation("", "https://example.com/bare")]);

    // When rendering it.
    let rendered = render_entry(&entry, &session_id());

    // Then the link text is the TUI's untitled guard — the URL still
    // renders beside it as the muted text.
    assert!(
        rendered
            .body_html
            .contains("rel=\"noopener\">(untitled)</a>"),
        "{}",
        rendered.body_html
    );
}

#[rstest::rstest]
#[test]
fn footnote_markdown_renders_reference_and_definition_elements() {
    // Given an assistant entry using footnote markdown — the shape
    // model-written citations take.
    let entry = ChatEntry::assistant("Claim here[^1].\n\n[^1]: The supporting source paragraph.");

    // When rendering it.
    let rendered = render_entry(&entry, &session_id());

    // Then the reference and the definition render as elements — no
    // literal `[^1]` text survives into the markup.
    assert!(
        rendered.body_html.contains("footnote-reference"),
        "{}",
        rendered.body_html
    );
    assert!(
        rendered.body_html.contains("footnote-definition"),
        "{}",
        rendered.body_html
    );
    assert!(
        !rendered.body_html.contains("[^1]"),
        "{}",
        rendered.body_html
    );
}

#[rstest::rstest]
#[test]
fn footnote_bodies_render_supplied_markup_as_escaped_text_and_link_bare_urls() {
    // Given a footnote definition carrying a script payload and a
    // bare URL.
    let entry = ChatEntry::assistant(
        "Claim[^1].\n\n[^1]: Body <script>alert(1)</script> with https://example.com/plain.",
    );

    // When rendering it.
    let rendered = render_entry(&entry, &session_id());

    // Then the payload is demoted to text (pulldown-cmark escapes it;
    // raw HTML events are demoted by `render_markdown`).
    assert!(
        !rendered.body_html.contains("<script>"),
        "{}",
        rendered.body_html
    );

    // And the bare URL in the footnote body renders as a new-tab link
    // like any other prose address.
    assert!(
        rendered.body_html.contains(
            "<a href=\"https://example.com/plain\" target=\"_blank\" rel=\"noopener\">https://example.com/plain</a>."
        ),
        "{}",
        rendered.body_html
    );
}

#[rstest::rstest]
#[test]
fn a_thinking_entry_renders_its_markdown_syntax_literally_not_rendered() {
    // Given a thinking entry whose text carries markdown syntax.
    let entry = ChatEntry::thinking("### heading\n**bold** [link](https://x.example)");

    // When rendering it (either projection — the rule is the same).
    let settled = render_entry(&entry, &session_id());
    let streaming = render_entry_streaming(&entry, &session_id(), true);

    // Then the syntax renders literally — thinking is escaped
    // verbatim, never markdown-rendered (TUI parity).
    for rendered in [settled, streaming] {
        assert!(
            rendered.body_html.contains("### heading"),
            "{}",
            rendered.body_html
        );
        assert!(
            rendered.body_html.contains("**bold**"),
            "{}",
            rendered.body_html
        );
        assert!(
            !rendered.body_html.contains("<h3"),
            "{}",
            rendered.body_html
        );
    }
}

#[rstest::rstest]
#[test]
fn the_streaming_projection_opens_only_thinking_disclosures() {
    // Given a thinking entry and a tool-call entry.
    let thinking = ChatEntry::thinking("pondering");
    let tool_call = ChatEntry::tool_call("t1", "read_file", "/tmp/x");

    // When rendering both through the streaming projection.
    let thinking_streaming = render_entry_streaming(&thinking, &session_id(), true);
    let tool_streaming = render_entry_streaming(&tool_call, &session_id(), true);

    // Then only the thinking entry carries the open state; the tool
    // disclosure stays collapsed mid-stream (TUI parity).
    assert!(thinking_streaming.is_streaming, "{thinking_streaming:?}");
    assert!(!tool_streaming.is_streaming, "{tool_streaming:?}");
    assert!(tool_streaming.is_disclosure);
}

/// The anchor a linkable address renders as — label and attribute
/// both HTML-escaped, as the renderer emits them.
fn link(url: &str) -> String {
    let url = escape_html(url);
    format!("<a href=\"{url}\" target=\"_blank\" rel=\"noopener\">{url}</a>")
}

#[rstest::rstest]
#[case::https("see https://example.com/doc now", "https://example.com/doc")]
#[case::http("see http://example.com/doc now", "http://example.com/doc")]
#[case::trailing_period("see https://example.com/a.", "https://example.com/a")]
#[case::trailing_comma("see https://example.com/a, then", "https://example.com/a")]
#[case::trailing_bang("see https://example.com/a!", "https://example.com/a")]
#[case::unmatched_bracket("see [https://example.com/a] now", "https://example.com/a")]
#[case::wrapped_in_parens("(see https://example.com/a)", "https://example.com/a")]
#[case::balanced_parens(
    "see https://example.com/wiki/Foo(bar)",
    "https://example.com/wiki/Foo(bar)"
)]
#[case::map_coordinates(
    "see https://maps.google.com/@37.4,-122.1,15z",
    "https://maps.google.com/@37.4,-122.1,15z"
)]
fn bare_addresses_in_assistant_prose_render_as_new_tab_links(
    #[case] source: &str,
    #[case] url: &str,
) {
    // Given an assistant entry carrying the address inside prose.
    let entry = ChatEntry::assistant(source);

    // When rendering it.
    let rendered = render_entry(&entry, &session_id());

    // Then the address renders as an anchor opening in a new tab
    // under `noopener`, with the address as its own label.
    assert!(
        rendered.body_html.contains(&link(url)),
        "{}",
        rendered.body_html
    );
}

#[rstest::rstest]
#[test]
fn a_bare_address_in_a_user_entry_renders_as_a_new_tab_link() {
    // Given a user entry carrying a bare address.
    let entry = ChatEntry::user("look at https://example.com/please");

    // When rendering it.
    let rendered = render_entry(&entry, &session_id());

    // Then the address renders as a new-tab link.
    assert!(
        rendered
            .body_html
            .contains(&link("https://example.com/please")),
        "{}",
        rendered.body_html
    );
}

#[rstest::rstest]
#[test]
fn trimmed_trailing_punctuation_renders_as_text_after_the_link() {
    // Given an address ending a sentence.
    let entry = ChatEntry::assistant("see https://example.com/a.");

    // When rendering it.
    let rendered = render_entry(&entry, &session_id());

    // Then the anchor is followed by the period, so no character of
    // the transcript is lost.
    assert!(
        rendered
            .body_html
            .contains(&format!("{}</a>.", "https://example.com/a")),
        "{}",
        rendered.body_html
    );
}

#[rstest::rstest]
#[case::empty_host("see https:// now")]
#[case::bad_port("see https://example.com:99999/ now")]
fn a_malformed_address_renders_as_plain_text(#[case] source: &str) {
    // Given an assistant entry carrying an address the URL parser rejects.
    let entry = ChatEntry::assistant(source);

    // When rendering it.
    let rendered = render_entry(&entry, &session_id());

    // Then no anchor renders, and the source text survives intact.
    assert!(
        !rendered.body_html.contains("<a "),
        "{}",
        rendered.body_html
    );
    assert!(
        rendered
            .body_html
            .contains(source.trim_start_matches("see ").trim_end_matches(" now")),
        "{}",
        rendered.body_html
    );
}

#[rstest::rstest]
#[case::inline("see [the docs](https://example.com/x) here")]
#[case::autolink("see <https://example.com/x> here")]
#[case::reference("see [the docs][d] here\n\n[d]: https://example.com/x")]
fn model_written_markdown_links_open_in_a_new_tab_under_noopener(#[case] source: &str) {
    // Given an assistant entry with a markdown link to an http(s) address.
    let entry = ChatEntry::assistant(source);

    // When rendering it.
    let rendered = render_entry(&entry, &session_id());

    // Then the link's anchor carries the new-tab attributes.
    assert!(
        rendered
            .body_html
            .contains("<a href=\"https://example.com/x\" target=\"_blank\" rel=\"noopener\">"),
        "{}",
        rendered.body_html
    );
}

#[rstest::rstest]
#[test]
fn a_markdown_link_to_a_non_http_scheme_renders_its_label_without_an_anchor() {
    // Given a model-written link whose destination is a script URL.
    let entry = ChatEntry::assistant("bad [click me](javascript:alert(1)) here");

    // When rendering it.
    let rendered = render_entry(&entry, &session_id());

    // Then no anchor renders, and the label text survives.
    assert!(
        !rendered.body_html.contains("<a "),
        "{}",
        rendered.body_html
    );
    assert!(
        rendered.body_html.contains("click me"),
        "{}",
        rendered.body_html
    );
}

#[rstest::rstest]
#[test]
fn a_bare_address_used_as_a_link_label_is_not_nested_in_a_second_anchor() {
    // Given a markdown link whose label is itself a bare address.
    let entry = ChatEntry::assistant("[https://example.com/in](https://example.com/out)");

    // When rendering it.
    let rendered = render_entry(&entry, &session_id());

    // Then exactly one anchor renders — the outer link.
    assert_eq!(
        rendered.body_html.matches("<a ").count(),
        1,
        "{}",
        rendered.body_html
    );
}

#[rstest::rstest]
#[case::code_span("run `https://example.com/a` now")]
#[case::code_block("```\nhttps://example.com/a\n```")]
fn an_address_inside_code_stays_plain_text(#[case] source: &str) {
    // Given an assistant entry with an address inside code.
    let entry = ChatEntry::assistant(source);

    // When rendering it.
    let rendered = render_entry(&entry, &session_id());

    // Then no anchor renders — code is never linkified.
    assert!(
        !rendered.body_html.contains("<a "),
        "{}",
        rendered.body_html
    );
}

#[rstest::rstest]
#[case::thinking(ChatEntry::thinking("see https://example.com/a"))]
#[case::system(ChatEntry::system("see https://example.com/a"))]
#[case::error(ChatEntry::error("see https://example.com/a"))]
fn an_address_in_a_non_markdown_entry_stays_plain_text(#[case] entry: ChatEntry) {
    // When rendering an entry kind that is escaped verbatim rather
    // than markdown-rendered.
    let rendered = render_entry(&entry, &session_id());

    // Then no anchor renders.
    assert!(
        !rendered.body_html.contains("<a "),
        "{}",
        rendered.body_html
    );
}

#[rstest::rstest]
#[test]
fn an_address_in_a_tool_result_stays_plain_text() {
    // Given a tool result whose body carries an address.
    let entry = ChatEntry::tool_result(
        "call-1",
        "web_search",
        "found https://example.com/a",
        jinn_core_types::tool_result_status::ToolResultStatus::Success,
    );

    // When rendering it.
    let rendered = render_entry(&entry, &session_id());

    // Then no anchor renders.
    assert!(
        !rendered.body_html.contains("<a "),
        "{}",
        rendered.body_html
    );
}

#[rstest::rstest]
#[test]
fn an_ampersand_in_a_query_string_is_escaped_in_the_href() {
    // Given an address whose query carries two parameters.
    let entry = ChatEntry::assistant("see https://example.com/s?a=1&b=2 now");

    // When rendering it.
    let rendered = render_entry(&entry, &session_id());

    // Then the ampersand renders as an entity inside the attribute.
    assert!(
        rendered
            .body_html
            .contains("href=\"https://example.com/s?a=1&amp;b=2\""),
        "{}",
        rendered.body_html
    );
}

#[rstest::rstest]
#[test]
fn an_address_split_by_markdown_emphasis_links_only_its_leading_run() {
    // Given an address whose path carries `_(...)`, which markdown
    // reads as an emphasis delimiter before the linkifier ever sees
    // the text.
    let entry = ChatEntry::assistant("see https://en.wikipedia.org/wiki/Foo_(bar)");

    // When rendering it.
    let rendered = render_entry(&entry, &session_id());

    // Then the leading run links and the emphasis-delimited tail
    // renders as text — markdown parsing wins over address repair.
    assert!(
        rendered
            .body_html
            .contains(&link("https://en.wikipedia.org/wiki/Foo")),
        "{}",
        rendered.body_html
    );
    assert!(
        rendered.body_html.contains("_(bar)"),
        "{}",
        rendered.body_html
    );
}
