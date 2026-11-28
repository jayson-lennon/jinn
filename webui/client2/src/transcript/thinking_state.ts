// Which thinking entries the reader has opened. Page-owned view state: it
// lives in the transcript and never reaches the store or the socket.

/**
 * The ids of the thinking entries currently expanded. Absent means
 * collapsed, which is the default for every thinking entry.
 */
export type ThinkingOpen = ReadonlySet<string>;

/** No entry open: the state a transcript starts with. */
export const NO_THINKING_OPEN: ThinkingOpen = new Set();

/**
 * Whether the named thinking entry is expanded. An id that is not in the
 * set is collapsed, so a thinking entry that has never been clicked — and
 * one still streaming its first tokens — reads as closed.
 */
export function isThinkingOpen(open: ThinkingOpen, id: string): boolean {
  return open.has(id);
}

/**
 * Flips the named entry: open becomes closed and closed becomes open. The
 * set is replaced, never mutated, so a store read of it sees the change.
 */
export function toggleThinking(open: ThinkingOpen, id: string): ThinkingOpen {
  const next = new Set(open);
  if (next.has(id)) {
    next.delete(id);
  } else {
    next.add(id);
  }
  return next;
}
