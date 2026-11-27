// The composer's `#` completion: where the token sits, which templates
// match, and what the draft becomes when one is picked.
//
// Pure logic over a draft and a caret. A `|` in a draft marks the caret, so
// a Given reads as the text the user sees.

import { describe, it, expect } from 'vitest';
import {
  applyPromptPick,
  filterPrompts,
  findPromptCompletion,
  PROMPT_MAX_ROWS,
} from './prompts';

/** Splits a draft whose `|` marks the caret into (text, caret). */
function at(marked: string): [string, number] {
  const caret = marked.indexOf('|');
  return [marked.slice(0, caret) + marked.slice(caret + 1), caret];
}

/** A template list of `count` rows named t00, t01, … */
function templates(count: number): { name: string }[] {
  return Array.from({ length: count }, (_, i) => ({ name: `t${String(i).padStart(2, '0')}` }));
}

describe('findPromptCompletion', () => {
  it('a hash at the start of the draft is a completion', () => {
    // Given a lone `#` with the caret after it.
    // When finding the completion.
    const found = findPromptCompletion(...at('#|'));

    // Then the token is the `#`, with nothing typed after it.
    expect(found).toEqual({ start: 0, filter: '' });
  });

  it.each([
    ['after a space', 'hello #|'],
    ['after a newline', 'hello\n#|'],
  ])('a hash %s is a completion', (_label, marked) => {
    // Given a `#` that opens a word, with the caret after it.
    // When finding the completion.
    const found = findPromptCompletion(...at(marked));

    // Then the token is that `#`.
    expect(found).toEqual({ start: marked.indexOf('#'), filter: '' });
  });

  it('a hash inside a word is no completion', () => {
    // Given `issue#42`, whose `#` does not open a word.
    // When finding the completion at the end of the draft.
    const found = findPromptCompletion(...at('issue#42|'));

    // Then there is none.
    expect(found).toBeNull();
  });

  it('a caret on the hash itself is a completion', () => {
    // Given a caret sitting on the `#`, as Home leaves it.
    // When finding the completion.
    const found = findPromptCompletion(...at('|#'));

    // Then the caret is still within the token.
    expect(found).toEqual({ start: 0, filter: '' });
  });

  it('a caret before the token is no completion', () => {
    // Given text before a token, so there is somewhere before it to go.
    // When finding the completion at the start of the line.
    const found = findPromptCompletion(...at('|some text #plan'));

    // Then there is none: the caret sits in no token.
    expect(found).toBeNull();
  });

  it('a caret one past the token is no completion', () => {
    // Given a token followed by a space.
    // When finding the completion on that space.
    const found = findPromptCompletion(...at('#plan |'));

    // Then there is none: the caret left the token.
    expect(found).toBeNull();
  });

  it('the filter is the text between the hash and the caret', () => {
    // Given a token with the caret after part of a name.
    // When finding the completion.
    const found = findPromptCompletion(...at('some text #pl|'));

    // Then the filter is what was typed since the `#`.
    expect(found).toEqual({ start: 10, filter: 'pl' });
  });

  it('a second hash ends the token', () => {
    // Given two tokens, the caret in the second.
    // When finding the completion.
    const found = findPromptCompletion(...at('#one #tw|'));

    // Then the completion is the second `#`.
    expect(found).toEqual({ start: 5, filter: 'tw' });
  });

  it.each([
    ['negative', -1],
    ['past the end', 99],
  ])('a caret %s is no completion', (_label, caret) => {
    // Given a caret position outside the draft.
    // When finding the completion.
    const found = findPromptCompletion('#plan', caret);

    // Then there is none.
    expect(found).toBeNull();
  });
});

describe('filterPrompts', () => {
  it('an empty filter lists the templates in the order given', () => {
    // Given three templates.
    const list = [{ name: 'c' }, { name: 'a' }, { name: 'b' }];

    // When filtering with nothing typed.
    const rows = filterPrompts(list, '');

    // Then the list is as shipped, not reordered.
    expect(rows.map((t) => t.name)).toEqual(['c', 'a', 'b']);
  });

  it('an empty filter is capped at the most rows the popup lists', () => {
    // Given more templates than the popup lists.
    const list = templates(PROMPT_MAX_ROWS + 5);

    // When filtering with nothing typed.
    const rows = filterPrompts(list, '');

    // Then only the first page is listed.
    expect(rows).toHaveLength(PROMPT_MAX_ROWS);
    expect(rows[0]?.name).toBe('t00');
  });

  it('a filter orders the rows by how far the match spans, widest last', () => {
    // Given templates the subsequence filter matches with different spans.
    const list = [
      { name: 'plan-review' },
      { name: 'deploy' },
      { name: 'plan' },
      { name: 'explain' },
    ];

    // When filtering on `plan`.
    const rows = filterPrompts(list, 'plan');

    // Then a score is the index after the last matched letter, so the
    // narrow match sits first and the widest last — the row the popup
    // highlights by default.
    expect(rows.map((t) => t.name)).toEqual(['plan', 'plan-review', 'explain']);
  });

  it('a filter matches case-insensitively', () => {
    // Given a template with a capital letter.
    const list = [{ name: 'Plan' }];

    // When filtering in lowercase.
    const rows = filterPrompts(list, 'pl');

    // Then it matches.
    expect(rows).toHaveLength(1);
  });

  it('a filter nothing matches yields no rows', () => {
    // Given templates none of which contain the filter's letters in order.
    const list = [{ name: 'alpha' }, { name: 'beta' }];

    // When filtering on a non-subsequence.
    const rows = filterPrompts(list, 'zzz');

    // Then nothing is listed.
    expect(rows).toEqual([]);
  });
});

describe('applyPromptPick', () => {
  it('a pick replaces the token with the hash-name and keeps the rest', () => {
    // Given a completion on `#pl` with text after the caret.
    const [draft, caret] = at('see #pl| rest');

    // When picking `plan`.
    const picked = applyPromptPick(draft, { start: 4, filter: 'pl' }, 'plan');

    // Then the token became `#plan`, the rest is kept, and the caret follows it.
    expect(picked).toEqual({ draft: 'see #plan rest', caret: 9 });
  });

  it('a pick with nothing after the caret ends the draft with the name', () => {
    // Given a completion at the end of the draft.
    // When picking `plan`.
    const picked = applyPromptPick('#', { start: 0, filter: '' }, 'plan');

    // Then the draft is the hash-name and the caret is at its end.
    expect(picked).toEqual({ draft: '#plan', caret: 5 });
  });

  it('a pick replaces the whole typed filter, not just the matched part', () => {
    // Given a filter typed further than the name's own letters.
    // When picking `plan`.
    const picked = applyPromptPick('#planx', { start: 0, filter: 'planx' }, 'plan');

    // Then every typed character after the `#` is replaced.
    expect(picked.draft).toBe('#plan');
  });
});
