// @vitest-environment jsdom
//
// The markdown renderer, proven against the `!markdown` showcase document
// the sample provider streams — the same bytes the e2e tier renders, so a
// rule here and the page a user sees are checked against one input.
//
// jsdom is required, not a convenience: the sanitizer needs a real `window`,
// and without one DOMPurify is inert and every assertion below would pass
// against unsanitized markup.

import { describe, it, expect } from 'vitest';
import { renderMarkdown, safeHref } from './markdown';
import showcase from '../../../../crates/jinn-provider/src/resources/markdown_showcase.md?raw';

/** The showcase rendered into a detached document. */
function render(text: string = showcase, entryId?: string): Document {
  return new DOMParser().parseFromString(renderMarkdown(text, entryId), 'text/html');
}

/** The `href` of every link in a rendered document. */
function hrefs(doc: Document): (string | null)[] {
  return Array.from(doc.querySelectorAll('a')).map((a) => a.getAttribute('href'));
}

describe('the showcase document', () => {
  it('renders its heading and its table as real elements', () => {
    // Given the showcase.
    // When rendering.
    const doc = render();

    // Then the heading and the table are elements, with the table's cells.
    expect(doc.querySelectorAll('h2').length).toBe(1);
    expect(doc.querySelectorAll('table').length).toBe(1);
    expect(doc.querySelectorAll('table th').length).toBe(2);
    expect(doc.querySelectorAll('table td').length).toBe(2);
  });

  it('links a bare address, opening in a new tab', () => {
    // Given the showcase, carrying a bare address.
    // When rendering.
    const doc = render();

    // Then it is a link that opens in a new tab, with the safe rel.
    const a = doc.querySelector('a[href="https://example.com/plain"]');
    expect(a).not.toBeNull();
    expect(a?.getAttribute('target')).toBe('_blank');
    expect(a?.getAttribute('rel')).toContain('noopener');
  });

  it('a trailing period is not part of the link', () => {
    // Given an address that ends a sentence.
    // When rendering.
    // Then the link stops before the period.
    expect(hrefs(render())).toContain('https://example.com/dot');
  });

  it('a trailing comma is not part of the link', () => {
    // Given an address followed by a comma.
    // When rendering.
    // Then the link stops before the comma.
    expect(hrefs(render())).toContain('https://example.com/comma');
  });

  it('a closing bracket after an address is not part of the link', () => {
    // Given an address followed by a bracket.
    // When rendering.
    // Then the link stops before the bracket.
    expect(hrefs(render())).toContain('https://example.com/bracket');
  });

  it('an address wrapped in brackets is linked', () => {
    // Given an address in square brackets, which CommonMark reads as a link.
    // When rendering.
    // Then it is a link to the address.
    expect(hrefs(render())).toContain('https://example.com/wrapped');
  });

  it('an address with underscores and parentheses keeps them in the link', () => {
    // Given a Wikipedia-style address with underscores and a parenthesised part.
    // When rendering.
    // Then the whole address, underscores and parentheses included, is the link.
    expect(hrefs(render())).toContain('https://en.wikipedia.org/wiki/Foo_(bar)');
  });

  it('an address inside a code span is not a link', () => {
    // Given an address inside backticks.
    // When rendering.
    // Then no link points at it.
    expect(hrefs(render())).not.toContain('https://example.com/in-code');
    expect(render().body.textContent).toContain('https://example.com/in-code');
  });

  it('an address inside a code block is not a link', () => {
    // Given an address inside a fenced block.
    // When rendering.
    // Then no link points at it, and the block holds it as text.
    expect(hrefs(render())).not.toContain('https://example.com/in-block');
    expect(render().querySelector('pre')?.textContent).toContain('https://example.com/in-block');
  });

  it.each([
    ['a bare www', 'www.example.com'],
    ['an ftp address', 'ftp://files.example.com/x'],
    ['a protocol-relative address', '//cdn.example.com/x.js'],
    ['a mistyped scheme', 'xhttps://example.com'],
  ])('%s stays text, not a link', (_label, address) => {
    // Given the showcase, carrying the address as plain text.
    // When rendering.
    // Then no link points at it, and the text is still there to read.
    expect(hrefs(render())).not.toContain(address);
    expect(render().body.textContent).toContain(address);
  });

  it('an email address becomes a mailto link in place', () => {
    // Given a sentence carrying an email address.
    // When rendering.
    const doc = render();

    // Then the address is a mailto link, and the sentence around it is intact.
    expect(hrefs(doc)).toContain('mailto:dev@example.com');
    expect(doc.body.textContent).toContain('Mail the author at dev@example.com about it.');
  });

  it('a footnote reference and its note are real elements, linked', () => {
    // Given the showcase, carrying a footnote pair.
    // When rendering.
    const doc = render();

    // Then the reference jumps to the note in the same document.
    const ref = doc.querySelector('a[href^="#fn"]');
    expect(ref).not.toBeNull();
    expect(doc.querySelector('a[href^="#fnref"]')).not.toBeNull();
  });

  it('two entries with footnotes get distinct anchor ids', () => {
    // Given the same footnote rendered for two different entries.
    // When rendering each under its own entry id.
    const one = render('[^n]: note', 'entry-one');
    const two = render('[^n]: note', 'entry-two');

    // Then no anchor id is shared between them.
    const ids = (doc: Document): string[] =>
      Array.from(doc.querySelectorAll('[id]')).map((el) => el.id);
    const shared = ids(one).filter((id) => ids(two).includes(id));
    expect(shared).toEqual([]);
  });

  it('a task list renders real checkboxes', () => {
    // Given the showcase, carrying a task list.
    // When rendering.
    const doc = render();

    // Then each item has a checkbox, checked as the source says.
    const boxes = Array.from(
      doc.querySelectorAll<HTMLInputElement>('input[type="checkbox"]'),
    );
    expect(boxes.length).toBe(2);
    expect(boxes[0]?.checked).toBe(true);
    expect(boxes[1]?.checked).toBe(false);
  });
});

describe('model-written markup', () => {
  it('raw HTML in the source is escaped to visible text', () => {
    // Given a model writing an HTML tag in its prose.
    // When rendering.
    const doc = render('a <b>bold</b> word');

    // Then no element is created; the tags read as text.
    expect(doc.querySelector('b')).toBeNull();
    expect(doc.body.textContent).toContain('<b>bold</b>');
  });

  it('a script tag is escaped, never executed', () => {
    // Given a model writing a script tag.
    // When rendering.
    const doc = render('<script>window.pwned = true</script>');

    // Then there is no script element, and the text is visible.
    expect(doc.querySelector('script')).toBeNull();
    expect(doc.body.textContent).toContain('<script>');
  });

  it('a javascript: link is stripped to plain text', () => {
    // Given a model writing a link with a dangerous scheme.
    // When rendering.
    const doc = render('[click](javascript:alert(1))');

    // Then the anchor is removed whole, and its text stays to read.
    expect(doc.querySelector('a')).toBeNull();
    expect(doc.body.textContent).toContain('click');
  });
});

describe('safeHref', () => {
  it.each([
    ['javascript:', 'javascript:alert(1)'],
    ['data:', 'data:text/html,<b>x</b>'],
    ['empty', ''],
    ['blank', '   '],
    ['null', null],
  ])('%s yields no href', (_label, href) => {
    // Given a dangerous or empty href.
    // When making it safe.
    const safe = safeHref(href);

    // Then there is no href to use.
    expect(safe).toBeNull();
  });

  it.each([
    ['https', 'https://example.com/a'],
    ['http', 'http://example.com/a'],
    ['relative', '/local/path'],
    ['fragment', '#section'],
    ['mailto', 'mailto:dev@example.com'],
  ])('%s is kept', (_label, href) => {
    // Given a safe href.
    // When making it safe.
    const safe = safeHref(href);

    // Then it is kept as given.
    expect(safe).toBe(href);
  });
});
