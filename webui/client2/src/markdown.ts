// Markdown rendering for untrusted model output.
//
// Everything an assistant, a tool, or a system message says is treated as
// hostile: it is written by a language model, pasted from a web page, or
// echoed from a tool. So the pipeline is two independent barriers —
//
//   1. markdown-it with raw HTML disabled, so `<script>` in the source is
//      escaped to visible text rather than parsed;
//   2. DOMPurify over the result, so anything the first barrier let through
//      (a crafted link, an event handler an image URL smuggled in) is
//      stripped before it ever reaches the document.
//
// Either barrier alone would be enough for the cases below; together a bug
// in one cannot expose the page.

import DOMPurify, { type Config } from 'dompurify';
import MarkdownIt, { type Token } from 'markdown-it';
import footnote from 'markdown-it-footnote';
import taskLists from 'markdown-it-task-lists';

const markdown = new MarkdownIt({
  html: false,
  // Model output is trusted for its text, not for the HTML it embeds.
  linkify: true,
  // Tighter, CommonMark-faithful list behaviour.
  breaks: false,
})
  // Footnotes are the `[^n]` references a web-search model writes; task lists
  // are its `- [ ]` checkboxes. Both render as real elements.
  .use(footnote)
  .use(taskLists);

// Every footnote anchor id the plugin emits flows through this one rule, and
// its default is the bare note number — so two entries with footnotes on one
// page would both carry `fn1`, and every jump would land on the first entry's
// note. The entry id, which `renderMarkdown` passes in as `env`, prefixes each
// id so a reference jumps to the note in its own entry.
markdown.renderer.rules.footnote_anchor_name = (tokens, idx, _options, env) => {
  const n = Number(tokens[idx]?.meta?.id ?? 0) + 1;
  const entryId = env?.entryId;
  return typeof entryId === 'string' && entryId !== '' ? `${entryId}-${n}` : `${n}`;
};

// Bare addresses become links only for http(s) and email. The linkifier
// also knows `ftp:` and the protocol-relative `//host` form; neither is an
// address a reader can be sent to from a transcript, so both are removed.
// Bare `www.` stays off (the library default).
markdown.linkify.add('ftp:', null);
markdown.linkify.add('//', null);

// The linkifier trims a trailing `.` or `,` at the end of a bare address, but
// not when a closing bracket follows it: `see a.)` links `a.` and leaves `)`
// as text, so the period rides inside the link. When an auto-link ends in a
// run of those two characters and the very next text token opens with `)` or
// `]`, the run belongs to the prose, and is moved out of both the label and
// the href. One case is left alone: when the text before the link ends with
// the matching opener — `(a.)` — the closing bracket belongs to the wrapping
// parenthesis, not to the sentence, and the period is part of the address.
// Nothing else is touched; every other trailing character is already right.
markdown.core.ruler.after('linkify', 'trim_auto_link_punct', (state) => {
  for (const block of state.tokens) {
    if (block.type === 'inline' && block.children) {
      trimAutoLinkPunctuation(block.children);
    }
  }
});

/** A trailing run of `.` or `,` that a closing bracket follows. */
const TRAILING_PUNCT = /[.,]+$/;
/** The opener whose closing bracket wraps the link rather than the sentence. */
const WRAPPING_OPENER = /[[(]$/;

/** Moves a bracket-preceding run of `.` or `,` out of each auto-link it ends. */
function trimAutoLinkPunctuation(children: Token[]): void {
  for (let i = 0; i + 2 < children.length; i += 1) {
    const prev = children[i - 1];
    const open = children[i];
    const label = children[i + 1];
    const close = children[i + 2];
    const next = children[i + 3];
    if (
      open?.type !== 'link_open' ||
      open.info !== 'auto' ||
      label?.type !== 'text' ||
      close?.type !== 'link_close' ||
      next?.type !== 'text' ||
      // No previous token is the link at the very start of its block, which
      // no opener can wrap.
      (prev !== undefined && (prev.type !== 'text' || WRAPPING_OPENER.test(prev.content)))
    ) {
      continue;
    }
    const run = TRAILING_PUNCT.exec(label.content)?.[0];
    if (run === undefined) continue;
    const after = next.content.at(0);
    if (after !== ')' && after !== ']') continue;

    label.content = label.content.slice(0, -run.length);
    const href = attrText(open.attrGet('href'));
    if (href !== null) open.attrSet('href', href.slice(0, -run.length));
    next.content = run + next.content;
  }
}

/** Link schemes a model may legitimately produce. Everything else is text. */
const ALLOWED_LINK_SCHEMES = new Set(['http:', 'https:', 'mailto:', 'irc:', 'ircs:']);

/** Reads a token attribute as text, whatever markdown-it stored it as. */
function attrText(value: string | number | null): string | null {
  return value === null ? null : String(value);
}

/** The href a link may carry, or null when its scheme is not allowed. */
export function safeHref(href: string | null): string | null {
  if (href === null) return null;
  const trimmed = href.trim().toLowerCase();
  if (trimmed === '') return null;
  // A relative URL, a fragment, or a query has no scheme and is safe.
  if (!/^[a-z][a-z0-9+.-]*:/i.test(trimmed)) return href;
  const scheme = trimmed.slice(0, trimmed.indexOf(':') + 1);
  return ALLOWED_LINK_SCHEMES.has(scheme) ? href : null;
}

// The default renderer already validates links; replace it with the
// stricter rule so `javascript:` and `data:` never become an href at all.
markdown.renderer.rules.link_open = (tokens, idx, options, _env, self) => {
  const token = tokens[idx];
  if (token) {
    const href = safeHref(attrText(token.attrGet('href')));
    if (href === null) {
      // A refused scheme keeps its text and loses its address entirely.
      token.attrSet('href', '');
    } else {
      token.attrSet('href', href);
      if (opensInPlace(href)) {
        // An in-page fragment or a `mailto:` link must open where it is: a
        // footnote reference in a new tab is the same page again, and a mail
        // link in a new tab is a blank one.
        token.attrSet('rel', 'noopener noreferrer');
      } else {
        // A model-written link never gets to name the opener's window.
        token.attrSet('rel', 'noopener noreferrer');
        token.attrSet('target', '_blank');
      }
    }
  }
  return self.renderToken(tokens, idx, options);
};

/** Whether a link must open in this page rather than a new tab. */
function opensInPlace(href: string): boolean {
  return href.startsWith('#') || /^mailto:/i.test(href);
}

// An image whose source is not a plain web URL is dropped, not rendered:
// `data:` and `javascript:` sources have no legitimate use in a transcript.
markdown.renderer.rules.image = (tokens, idx) => {
  const token = tokens[idx];
  if (!token) return '';
  const src = attrText(token.attrGet('src'));
  if (src === null || !/^https?:\/\//i.test(src.trim())) return '';
  token.attrSet('loading', 'lazy');
  token.attrSet('referrerpolicy', 'no-referrer');
  token.attrSet('alt', token.content ?? '');
  return markdown.renderer.renderToken(tokens, idx, markdown.options);
};

/**
 * The sanitizer's rules, passed on every call rather than set globally:
 * a global config would silently change every other sanitize in the page.
 *
 * `input` is deliberately absent from the forbid list: the task-list plugin
 * renders its checkboxes as disabled `<input>` elements, and DOMPurify applies
 * the forbid list before any hook can exempt a tag. The hook below keeps the
 * one legitimate input and strips every other.
 */
const SANITIZE_RULES: Config = {
  // Transcript content never needs forms, embedded frames, or styles.
  FORBID_TAGS: ['style', 'form', 'button', 'select', 'textarea', 'iframe', 'object', 'embed'],
  FORBID_ATTR: ['style'],
  // `target` is stripped unless allowed, and every external link needs it.
  ADD_ATTR: ['target'],
  // Nothing in a transcript is allowed to run script.
  ALLOW_UNKNOWN_PROTOCOLS: false,
  RETURN_DOM: false,
  RETURN_TRUSTED_TYPE: false,
};

// A task-list checkbox is the one `<input>` a transcript carries: disabled,
// of type checkbox, and wearing the plugin's class. Every other input — none
// of which the markdown parser can produce, but a sanitizer is a second
// barrier, not a first — is removed from the allow-set here.
DOMPurify.addHook('uponSanitizeElement', (node, data) => {
  if (data.tagName !== 'input') return;
  const element = node as Element;
  const isTaskCheckbox =
    element.getAttribute('type') === 'checkbox' &&
    element.hasAttribute('disabled') &&
    element.classList.contains('task-list-item-checkbox');
  if (!isTaskCheckbox) {
    delete data.allowedTags.input;
  }
});

/**
 * Renders untrusted markdown to a sanitized HTML string.
 *
 * `entryId` names the entry the prose belongs to. It reaches the renderer as
 * its `env`, where the footnote plugin reads it to keep each entry's anchor
 * ids distinct on the page. Prose with no entry — a preview, a notice — omits
 * it and gets unprefixed ids.
 */
export function renderMarkdown(text: string, entryId?: string): string {
  return DOMPurify.sanitize(markdown.render(text, { entryId }), SANITIZE_RULES);
}

/** Escapes text for a text node or an attribute value. */
export function escapeHtml(text: string): string {
  return text
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&#39;');
}
