// The composer's state: the draft, the pending images, and the `#`
// completion's highlight.
//
// Every helper writes the store, so every test awaits one microtask before
// it reads the result — a same-tick read returns the value before the write.

import { describe, it, expect } from 'vitest';
import { createStore } from './store';
import {
  addChips,
  clearComposer,
  movePromptHighlight,
  pickPrompt,
  promptHighlightOf,
  removeChip,
  setDraft,
  setPromptCompletion,
} from './composer_state';
import type { ImageUpload } from './protocol';

/** A pending image with a name that tells it apart in an assertion. */
function image(name: string): ImageUpload {
  return { name, data_base64: 'AA==' };
}

/** Awaits the microtask a store write needs before it can be read back. */
async function settled(): Promise<void> {
  await Promise.resolve();
}

describe('setDraft', () => {
  it('replaces the draft with the text given', async () => {
    // Given a store with an empty draft.
    const store = createStore();

    // When setting the draft.
    setDraft(store, 'hello');
    await settled();

    // Then the draft is that text.
    expect(store.state.draft).toBe('hello');
  });

  it('replaces an earlier draft rather than appending to it', async () => {
    // Given a store holding a draft.
    const store = createStore();
    setDraft(store, 'first');
    await settled();

    // When setting the draft again.
    setDraft(store, 'second');
    await settled();

    // Then only the second text remains.
    expect(store.state.draft).toBe('second');
  });
});

describe('addChips', () => {
  it('appends the images in the order given', async () => {
    // Given a store with one pending image.
    const store = createStore();
    addChips(store, [image('a')]);
    await settled();

    // When adding two more.
    addChips(store, [image('b'), image('c')]);
    await settled();

    // Then they follow the first, in order.
    expect(store.state.chips.map((c) => c.name)).toEqual(['a', 'b', 'c']);
  });
});

describe('removeChip', () => {
  it('removes only the image at the index given', async () => {
    // Given three pending images.
    const store = createStore();
    addChips(store, [image('a'), image('b'), image('c')]);
    await settled();

    // When removing the middle one.
    removeChip(store, 1);
    await settled();

    // Then the other two remain, in order.
    expect(store.state.chips.map((c) => c.name)).toEqual(['a', 'c']);
  });
});

describe('clearComposer', () => {
  it('clears the draft and every pending image', async () => {
    // Given a draft and two images.
    const store = createStore();
    setDraft(store, 'unsent text');
    addChips(store, [image('a'), image('b')]);
    await settled();

    // When clearing the composer.
    clearComposer(store);
    await settled();

    // Then both are empty.
    expect(store.state.draft).toBe('');
    expect(store.state.chips).toEqual([]);
  });
});

describe('setPromptCompletion', () => {
  it('opens the popup on the completion given and resets the highlight', async () => {
    // Given a highlight the user moved, and no popup.
    const store = createStore();
    store.set((s) => {
      s.promptHighlight = 1;
    });
    await settled();

    // When setting a completion.
    setPromptCompletion(store, { start: 0, filter: 'pl' });
    await settled();

    // Then the completion is the one given and the highlight is back to default.
    expect(store.state.promptCompletion).toEqual({ start: 0, filter: 'pl' });
    expect(store.state.promptHighlight).toBeNull();
  });

  it('a null completion closes the popup', async () => {
    // Given an open completion.
    const store = createStore();
    setPromptCompletion(store, { start: 0, filter: 'pl' });
    await settled();

    // When setting the completion to null.
    setPromptCompletion(store, null);
    await settled();

    // Then the popup is closed.
    expect(store.state.promptCompletion).toBeNull();
  });

  it('an unchanged completion keeps a highlight the user moved', async () => {
    // Given an open completion and the highlight moved off the default.
    const store = createStore();
    setPromptCompletion(store, { start: 0, filter: 'pl' });
    await settled();
    movePromptHighlight(store, 3, -1);
    await settled();

    // When the same completion is set again.
    setPromptCompletion(store, { start: 0, filter: 'pl' });
    await settled();

    // Then the moved highlight survives.
    expect(store.state.promptHighlight).toBe(1);
  });
});

describe('movePromptHighlight', () => {
  it('moves up from the default last row', async () => {
    // Given a popup with three rows and the highlight untouched.
    const store = createStore();

    // When moving up.
    movePromptHighlight(store, 3, -1);
    await settled();

    // Then the highlight lands on the row above the last.
    expect(store.state.promptHighlight).toBe(1);
  });

  it('clamps at the first row', async () => {
    // Given the highlight already on the first row.
    const store = createStore();
    movePromptHighlight(store, 3, -1);
    movePromptHighlight(store, 3, -1);
    await settled();

    // When moving up again.
    movePromptHighlight(store, 3, -1);
    await settled();

    // Then it stays on the first row.
    expect(store.state.promptHighlight).toBe(0);
  });

  it('clamps at the last row', async () => {
    // Given the highlight untouched, so already on the last of three rows.
    const store = createStore();

    // When moving down.
    movePromptHighlight(store, 3, 1);
    await settled();

    // Then it stays on the last row.
    expect(store.state.promptHighlight).toBe(2);
  });
});

describe('promptHighlightOf', () => {
  it('the default is the last row', () => {
    // Given a popup with three rows and no moved highlight.
    const store = createStore();

    // When reading the highlight.
    const at = promptHighlightOf(store, 3);

    // Then it is the last row.
    expect(at).toBe(2);
  });

  it('a moved highlight is the row it was moved to', async () => {
    // Given the highlight moved up from the last of three rows.
    const store = createStore();
    movePromptHighlight(store, 3, -1);
    await settled();

    // When reading the highlight.
    const at = promptHighlightOf(store, 3);

    // Then it is the moved-to row.
    expect(at).toBe(1);
  });

  it('a highlight past the last row reads as the last row', async () => {
    // Given the highlight on row 2, and only one row to show.
    const store = createStore();
    store.set((s) => {
      s.promptHighlight = 2;
    });
    await settled();

    // When reading the highlight against one row.
    const at = promptHighlightOf(store, 1);

    // Then it clamps to the only row there is.
    expect(at).toBe(0);
  });

  it('with no rows the highlight reads as row zero', () => {
    // Given no rows.
    const store = createStore();

    // When reading the highlight.
    const at = promptHighlightOf(store, 0);

    // Then it is zero, never negative.
    expect(at).toBe(0);
  });
});

describe('pickPrompt', () => {
  it('rewrites the token, closes the popup, and returns the new caret', async () => {
    // Given a completion on `#pl` in a draft.
    const store = createStore();
    setDraft(store, 'see #pl rest');
    setPromptCompletion(store, { start: 4, filter: 'pl' });
    await settled();

    // When picking `plan`.
    const caret = pickPrompt(store, 'plan');
    await settled();

    // Then the token became `#plan`, the popup closed, and the caret follows it.
    expect(store.state.draft).toBe('see #plan rest');
    expect(store.state.promptCompletion).toBeNull();
    expect(caret).toBe(9);
  });

  it('does nothing when no completion is open', async () => {
    // Given a draft and no completion.
    const store = createStore();
    setDraft(store, 'untouched');
    await settled();

    // When picking a template.
    const caret = pickPrompt(store, 'plan');

    // Then the draft is untouched and no caret is returned.
    expect(caret).toBeNull();
    expect(store.state.draft).toBe('untouched');
  });
});
