// The composer's state: the draft, the pending images, and the `#`
// completion the caret sits in.
//
// The draft and the chips are store state rather than the component's own,
// so a session switch — which replaces the transcript — leaves them where
// they are and the user never loses unsent text. These helpers are the one
// way the composer writes them.

import type { ClientStore } from './store/store';
import { applyPromptPick, type PromptCompletion } from './prompts';
import type { ImageUpload } from './protocol/protocol';

/** Replaces the composer's draft. */
export function setDraft(store: ClientStore, text: string): void {
  store.set((s) => {
    s.draft = text;
  });
}

/** Adds pending images to the composer, in the order given. */
export function addChips(store: ClientStore, images: ImageUpload[]): void {
  store.set((s) => {
    s.chips = [...s.chips, ...images];
  });
}

/** Removes one pending image, by its position in chip order. */
export function removeChip(store: ClientStore, index: number): void {
  store.set((s) => {
    s.chips = s.chips.filter((_, at) => at !== index);
  });
}

/** Clears the draft and every pending image — what a successful send does. */
export function clearComposer(store: ClientStore): void {
  store.set((s) => {
    s.draft = '';
    s.chips = [];
  });
}

/** Re-derives the `#` completion from the draft and the caret. A null
 * completion closes the popup. The highlight returns to the best match
 * only when the completion actually changed — a re-read over an unmoved
 * caret (a click in place, a selection event) keeps an arrow's move. */
export function setPromptCompletion(store: ClientStore, completion: PromptCompletion | null): void {
  store.set((s) => {
    const current = s.promptCompletion;
    const same =
      current !== null &&
      completion !== null &&
      current.start === completion.start &&
      current.filter === completion.filter;
    s.promptCompletion = completion;
    if (!same) s.promptHighlight = null;
  });
}

/** Moves the popup's highlight by `delta` from wherever it sits — the
 * default last row, or a row the user already moved to — clamped to `rows`. */
export function movePromptHighlight(store: ClientStore, rows: number, delta: 1 | -1): void {
  store.set((s) => {
    const from = s.promptHighlight ?? Math.max(rows - 1, 0);
    s.promptHighlight = Math.min(Math.max(from + delta, 0), Math.max(rows - 1, 0));
  });
}

/** The row the `#` popup highlights: the one the user moved to, or the
 * last row — the best match — by default. Always in range of `rows`. */
export function promptHighlightOf(store: ClientStore, rows: number): number {
  const moved = store.state.promptHighlight;
  if (moved === null) return Math.max(rows - 1, 0);
  return Math.min(moved, Math.max(rows - 1, 0));
}

/** Picks a template: the token at the caret becomes `#name`, the popup
 * closes, and the new caret position is returned for the composer to
 * place. Nothing happens when no completion is open. */
export function pickPrompt(store: ClientStore, name: string): number | null {
  const completion = store.state.promptCompletion;
  if (completion === null) return null;
  const picked = applyPromptPick(store.state.draft, completion, name);
  store.set((s) => {
    s.draft = picked.draft;
    s.promptCompletion = null;
    s.promptHighlight = null;
  });
  return picked.caret;
}
