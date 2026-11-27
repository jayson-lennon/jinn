// The composer's `#` completion: pure logic over a draft and a caret.
//
// Three questions, three functions, nothing else: where is the `#` token
// the caret sits in, which templates match what was typed after it, and
// what does the draft become when one is picked. Nothing here touches the
// store, the DOM, or the server — the list is shipped whole and filtered
// here, so the server is asked nothing per keystroke.
//
// The rules mirror the terminal popup's own (chat-input `token.rs`): the
// `#` must open a word, and the caret must lie within the token. Indices
// are UTF-16 code units, the textarea's own — `#`, whitespace, and template
// names are ASCII in practice.

/** Where a `#` completion sits in the draft: the index of the `#`, and the
 * filter — the text between it and the caret. */
export interface PromptCompletion {
  /** The index of the `#` in the draft. */
  readonly start: number;
  /** The text between the `#` and the caret. */
  readonly filter: string;
}

/** The most rows the popup lists — the terminal popup's own cap. */
export const PROMPT_MAX_ROWS = 20;

const isSpace = (ch: string | undefined): boolean =>
  ch === ' ' || ch === '\n' || ch === '\t' || ch === '\r';

/**
 * The `#` token the caret sits in, or null when it sits in none.
 *
 * Walks left from the caret to the nearest `#`. The `#` must open a word —
 * the start of the draft, or right after a space or a newline — so `a#b`
 * never completes. The token runs to the next whitespace or a further `#`,
 * and the caret must lie within it, so a caret moved past the token is
 * outside the completion. Whitespace behind the caret with no `#` before it
 * on the line means there is no token.
 */
export function findPromptCompletion(
  draft: string,
  caret: number,
): PromptCompletion | null {
  if (caret < 0 || caret > draft.length) return null;
  let i = caret;
  for (;;) {
    if (draft[i] === '#') {
      const atBoundary = i === 0 || draft[i - 1] === ' ' || draft[i - 1] === '\n';
      if (!atBoundary) return null;
      let end = i + 1;
      while (end < draft.length && !isSpace(draft[end]) && draft[end] !== '#') end += 1;
      if (caret >= i && caret <= end) return { start: i, filter: draft.slice(i + 1, caret) };
      return null;
    }
    if (i !== caret && isSpace(draft[i])) return null;
    if (i === 0) return null;
    i -= 1;
  }
}

/**
 * The templates matching a filter: a case-insensitive subsequence match on
 * the name, weakest first and best last, capped at `PROMPT_MAX_ROWS` — the
 * terminal popup's order, so the highlighted row is the last one on both
 * surfaces. An empty filter is the list as shipped, in the store's order.
 */
export function filterPrompts<T extends { name: string }>(
  templates: readonly T[],
  filter: string,
): T[] {
  if (filter === '') return templates.slice(0, PROMPT_MAX_ROWS);
  const needle = filter.toLowerCase();
  const scored: Array<{ item: T; score: number }> = [];
  for (const item of templates) {
    const score = subsequenceScore(item.name.toLowerCase(), needle);
    if (score !== null) scored.push({ item, score });
  }
  scored.sort(
    (a, b) =>
      a.score - b.score ||
      (a.item.name < b.item.name ? -1 : a.item.name > b.item.name ? 1 : 0),
  );
  return scored.slice(-PROMPT_MAX_ROWS).map((s) => s.item);
}

/**
 * Whether `needle`'s characters appear in order in `name`, and how tight
 * the match is: lower is better, so a prefix beats a scattered match. Null
 * when it is not a subsequence.
 */
function subsequenceScore(name: string, needle: string): number | null {
  let at = 0;
  let first = -1;
  for (const ch of needle) {
    const found = name.indexOf(ch, at);
    if (found === -1) return null;
    if (first === -1) first = found;
    at = found + 1;
  }
  // The span the match covers, plus how far in the first character sat.
  return at - first + first;
}

/**
 * The draft after picking a template: the token from its `#` through the
 * caret is replaced by `#name`, and the caret lands after it. Whatever
 * follows the caret is kept. The bare name is what is inserted — the body
 * arrives through the expansion at send time, exactly as a hand-typed token.
 */
export function applyPromptPick(
  draft: string,
  completion: PromptCompletion,
  name: string,
): { draft: string; caret: number } {
  const token = `#${name}`;
  const next =
    draft.slice(0, completion.start) +
    token +
    draft.slice(completion.start + 1 + completion.filter.length);
  return { draft: next, caret: completion.start + token.length };
}
