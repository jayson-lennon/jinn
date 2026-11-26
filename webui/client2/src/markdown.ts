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
import MarkdownIt from 'markdown-it';

const markdown = new MarkdownIt({
  html: false,
  // Model output is trusted for its text, not for the HTML it embeds.
  linkify: true,
  // Tighter, CommonMark-faithful list behaviour.
  breaks: false,
});

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
      token.attrSet('href', '');
    } else {
      token.attrSet('href', href);
    }
    // A model-written link never gets to name the opener's window.
    token.attrSet('rel', 'noopener noreferrer');
    token.attrSet('target', '_blank');
  }
  return self.renderToken(tokens, idx, options);
};

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
 */
const SANITIZE_RULES: Config = {
  // Transcript content never needs forms, embedded frames, or styles.
  FORBID_TAGS: ['style', 'form', 'input', 'button', 'select', 'textarea', 'iframe', 'object', 'embed'],
  FORBID_ATTR: ['style'],
  // Nothing in a transcript is allowed to run script.
  ALLOW_UNKNOWN_PROTOCOLS: false,
  RETURN_DOM: false,
  RETURN_TRUSTED_TYPE: false,
};

/** Renders untrusted markdown to a sanitized HTML string. */
export function renderMarkdown(text: string): string {
  return DOMPurify.sanitize(markdown.render(text), SANITIZE_RULES);
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
