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

use crate::transcript::render_entry;
use crate::transcript::render_entry_streaming;

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
    let rendered = render_entry(&entry);

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
    let rendered = render_entry(&entry);

    // Then each citation row links its URL, opening a new tab under
    // `noopener`, with the title as the link text and the URL as
    // muted text beside it — in arrival order.
    assert!(rendered
        .body_html
        .contains("<a class=\"citation-title\" href=\"https://example.com/a\" target=\"_blank\" rel=\"noopener\">Example</a>"));
    assert!(rendered
        .body_html
        .contains("<span class=\"citation-url\">https://example.com/a</span>"));
    assert!(rendered
        .body_html
        .contains("href=\"http://localhost:8080/doc\""));
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
    let rendered = render_entry(&entry);

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
    let rendered = render_entry(&entry);

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
    let rendered = render_entry(&entry);

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
    let entry = ChatEntry::assistant(
        "Claim here[^1].\n\n[^1]: The supporting source paragraph.",
    );

    // When rendering it.
    let rendered = render_entry(&entry);

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
fn footnote_bodies_render_supplied_markup_as_escaped_text_and_urls_stay_plain() {
    // Given a footnote definition carrying a script payload and a
    // bare URL.
    let entry = ChatEntry::assistant(
        "Claim[^1].\n\n[^1]: Body <script>alert(1)</script> with https://example.com/plain.",
    );

    // When rendering it.
    let rendered = render_entry(&entry);

    // Then the payload is demoted to text (pulldown-cmark escapes it;
    // raw HTML events are demoted by `render_markdown`) and the bare
    // URL stays plain text — the sources entry is the clickable
    // surface, footnotes never autolink.
    assert!(
        !rendered.body_html.contains("<script>"),
        "{}",
        rendered.body_html
    );
    assert!(rendered.body_html.contains("https://example.com/plain"));
    let link_count = rendered.body_html.matches("<a ").count();
    let ref_count = rendered.body_html.matches("footnote-reference").count();
    assert_eq!(
        link_count, ref_count,
        "the only anchors are footnote refs (no autolinked URL): {}",
        rendered.body_html
    );
}

#[rstest::rstest]
#[test]
fn a_thinking_entry_renders_its_markdown_syntax_literally_not_rendered() {
    // Given a thinking entry whose text carries markdown syntax.
    let entry = ChatEntry::thinking("### heading\n**bold** [link](https://x.example)");

    // When rendering it (either projection — the rule is the same).
    let settled = render_entry(&entry);
    let streaming = render_entry_streaming(&entry, true);

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
    let thinking_streaming = render_entry_streaming(&thinking, true);
    let tool_streaming = render_entry_streaming(&tool_call, true);

    // Then only the thinking entry carries the open state; the tool
    // disclosure stays collapsed mid-stream (TUI parity).
    assert!(thinking_streaming.is_streaming, "{thinking_streaming:?}");
    assert!(!tool_streaming.is_streaming, "{tool_streaming:?}");
    assert!(tool_streaming.is_disclosure);
}
