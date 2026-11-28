// The thinking disclosure's state: which entries are open, and how a
// click flips one without touching the rest.

import { describe, it, expect } from 'vitest';
import { NO_THINKING_OPEN, isThinkingOpen, toggleThinking } from './thinking_state';

describe('isThinkingOpen', () => {
  it('an entry that was never clicked is collapsed', () => {
    // Given no entry has been opened.
    // When asking about a thinking entry.
    const open = isThinkingOpen(NO_THINKING_OPEN, 'e1');

    // Then it reads as collapsed.
    expect(open).toBe(false);
  });
});

describe('toggleThinking', () => {
  it('opens a collapsed entry', () => {
    // Given no entry open.
    // When the header of a thinking entry is clicked.
    const next = toggleThinking(NO_THINKING_OPEN, 'e1');

    // Then that entry is open.
    expect(isThinkingOpen(next, 'e1')).toBe(true);
  });

  it('closes an entry that was open', () => {
    // Given one thinking entry open.
    const opened = toggleThinking(NO_THINKING_OPEN, 'e1');

    // When its header is clicked again.
    const next = toggleThinking(opened, 'e1');

    // Then it is collapsed again.
    expect(isThinkingOpen(next, 'e1')).toBe(false);
  });

  it('leaves every other entry as it was', () => {
    // Given two thinking entries open.
    const opened = toggleThinking(toggleThinking(NO_THINKING_OPEN, 'e1'), 'e2');

    // When a third is clicked.
    const next = toggleThinking(opened, 'e3');

    // Then the two stay open beside it.
    expect(isThinkingOpen(next, 'e1')).toBe(true);
    expect(isThinkingOpen(next, 'e2')).toBe(true);
    expect(isThinkingOpen(next, 'e3')).toBe(true);
  });

  it('does not mutate the set it was given', () => {
    // Given one thinking entry open.
    const opened = toggleThinking(NO_THINKING_OPEN, 'e1');

    // When another is clicked.
    toggleThinking(opened, 'e2');

    // Then the first set is unchanged: a store read of it sees no change.
    expect(opened.has('e2')).toBe(false);
  });
});
