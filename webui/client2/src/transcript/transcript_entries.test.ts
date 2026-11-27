// The transcript's entry state: where an upsert lands, what a delta
// appends to, and how a page of older entries joins the tail.
//
// Every helper writes the store, so every test awaits one microtask before
// it reads the result — a same-tick read returns the value before the write.

import { describe, it, expect } from 'vitest';
import { createStore } from '../store/store';
import {
  addOptimistic,
  applyEntriesPage,
  applyEntryDelta,
  applyEntryUpsert,
  indexSnapshot,
  isOptimistic,
  newestEntryId,
  readText,
  readUserText,
} from './transcript_entries';
import type { DeltaField, EntryEnvelope, ServerMsg } from '../protocol/protocol';

/** Awaits the microtask a store write needs before it can be read back. */
async function settled(): Promise<void> {
  await Promise.resolve();
}

/** An entry with an id and one streamed kind, as the server sends it. */
function entry(id: string, kind: Record<string, unknown> = { Assistant: { text: '' } }): EntryEnvelope {
  return { kind: 'assistant', entry: { id, kind } };
}

/** A user entry, as the server sends it after a message is accepted. */
function userEntry(id: string, text: string): EntryEnvelope {
  return { kind: 'user', entry: { id, kind: { User: { display: text, expanded: text } } } };
}

/** An `entry_upsert` frame. */
function upsertMsg(index: number, e: EntryEnvelope, sessionId: string | null = null): Extract<ServerMsg, { type: 'entry_upsert' }> {
  return { type: 'entry_upsert', session_id: sessionId ?? 's1', index, entry: e };
}

/** An `entry_delta` frame. */
function deltaMsg(entryId: string, field: DeltaField, chunk: string): Extract<ServerMsg, { type: 'entry_delta' }> {
  return { type: 'entry_delta', session_id: 's1', entry_id: entryId, field, chunk };
}

/** The ids the transcript holds, in order. */
function ids(store: ReturnType<typeof createStore>): string[] {
  return store.state.entries.map((e) => e.entry.id);
}

describe('isOptimistic', () => {
  it('an id with the optimistic prefix is optimistic', () => {
    // Given an id the page minted for a just-sent message.
    // When asking whether it is optimistic.
    const yes = isOptimistic('optimistic-0');

    // Then it is.
    expect(yes).toBe(true);
  });

  it('a server id is not optimistic', () => {
    // Given an id the server minted.
    // When asking whether it is optimistic.
    const yes = isOptimistic('abc123');

    // Then it is not.
    expect(yes).toBe(false);
  });
});

describe('newestEntryId', () => {
  it('the newest real entry id, ignoring an optimistic placeholder after it', () => {
    // Given a real entry followed by a placeholder.
    const entries = [entry('real-1'), entry('optimistic-0')];

    // When reading the newest id.
    const newest = newestEntryId(entries);

    // Then it is the real one.
    expect(newest).toBe('real-1');
  });

  it('an empty transcript has no newest entry', () => {
    // Given no entries.
    // When reading the newest id.
    const newest = newestEntryId([]);

    // Then there is none.
    expect(newest).toBeNull();
  });
});

describe('applyEntryUpsert', () => {
  it('lands the entry at the index the server names', async () => {
    // Given an empty transcript with nothing before it.
    const store = createStore();

    // When an upsert names index 0.
    applyEntryUpsert(store, upsertMsg(0, entry('a')));
    await settled();

    // Then the entry is the transcript's only one.
    expect(ids(store)).toEqual(['a']);
  });

  it('shifts the index by what precedes the tail', async () => {
    // Given a transcript holding a tail that starts 40 entries in.
    const store = createStore();
    store.set((s) => {
      s.sessionId = 's1';
      s.entriesBefore = 40;
      s.entries = [entry('old')];
    });
    indexSnapshot(store.state.entries);
    await settled();

    // When an upsert names index 41, the slot after the tail's last.
    applyEntryUpsert(store, upsertMsg(41, entry('new')));
    await settled();

    // Then it lands at the end of the tail, not at index 41.
    expect(ids(store)).toEqual(['old', 'new']);
  });

  it('a same-id upsert replaces the earlier copy', async () => {
    // Given a transcript holding entry `a` at index 0.
    const store = createStore();
    applyEntryUpsert(store, upsertMsg(0, entry('a', { Assistant: { text: 'first' } })));
    await settled();

    // When a second upsert names the same id.
    applyEntryUpsert(store, upsertMsg(0, entry('a', { Assistant: { text: 'second' } })));
    await settled();

    // Then there is one copy, the newer one.
    expect(ids(store)).toEqual(['a']);
    expect(readText(store.state.entries[0]!)).toBe('second');
  });

  it('a user upsert drops every optimistic placeholder', async () => {
    // Given a transcript with a placeholder and a real entry.
    const store = createStore();
    applyEntryUpsert(store, upsertMsg(0, entry('real')));
    await settled();
    addOptimistic(store, 'unsent');
    await settled();

    // When the server's user entry arrives.
    applyEntryUpsert(store, upsertMsg(1, userEntry('u1', 'sent')));
    await settled();

    // Then the placeholder is gone and the real entries remain.
    expect(ids(store)).toEqual(['real', 'u1']);
  });

  it('an assistant upsert keeps the optimistic placeholder', async () => {
    // Given a transcript with a placeholder.
    const store = createStore();
    addOptimistic(store, 'unsent');
    await settled();

    // When an assistant entry arrives.
    applyEntryUpsert(store, upsertMsg(0, entry('a1')));
    await settled();

    // Then the placeholder is still there.
    expect(ids(store).some((id) => isOptimistic(id))).toBe(true);
  });

  it('a frame for another session changes nothing', async () => {
    // Given a page viewing session s1.
    const store = createStore();
    store.set((s) => {
      s.sessionId = 's1';
    });
    await settled();

    // When an upsert for session s2 arrives.
    applyEntryUpsert(store, upsertMsg(0, entry('x'), 's2'));
    await settled();

    // Then the transcript is unchanged.
    expect(ids(store)).toEqual([]);
  });
});

describe('applyEntryDelta', () => {
  it('appends a text chunk to an assistant entry', async () => {
    // Given an assistant entry holding no text.
    const store = createStore();
    applyEntryUpsert(store, upsertMsg(0, entry('a', { Assistant: { text: '' } })));
    await settled();

    // When a text delta arrives for it.
    applyEntryDelta(store, deltaMsg('a', 'text', 'Hel'));
    await settled();

    // Then the text grew by the chunk.
    expect(readText(store.state.entries[0]!)).toBe('Hel');
  });

  it('a second chunk follows the first', async () => {
    // Given an assistant entry already holding `Hel`.
    const store = createStore();
    applyEntryUpsert(store, upsertMsg(0, entry('a', { Assistant: { text: '' } })));
    await settled();
    applyEntryDelta(store, deltaMsg('a', 'text', 'Hel'));
    await settled();

    // When another chunk arrives.
    applyEntryDelta(store, deltaMsg('a', 'text', 'lo'));
    await settled();

    // Then the text is both chunks in order.
    expect(readText(store.state.entries[0]!)).toBe('Hello');
  });

  it('a thinking chunk lands on a thinking entry', async () => {
    // Given a thinking entry holding no text.
    const store = createStore();
    applyEntryUpsert(store, upsertMsg(0, entry('t', { Thinking: { text: '' } })));
    await settled();

    // When a thinking delta arrives for it.
    applyEntryDelta(store, deltaMsg('t', 'thinking', 'hmm'));
    await settled();

    // Then the thinking text grew.
    const inner = store.state.entries[0]!.entry.kind as { Thinking: { text: string } };
    expect(inner.Thinking.text).toBe('hmm');
  });

  it('an args chunk lands on a tool call', async () => {
    // Given a tool call holding no arguments.
    const store = createStore();
    applyEntryUpsert(store, upsertMsg(0, entry('c', { ToolCall: { arguments: '' } })));
    await settled();

    // When an args delta arrives for it.
    applyEntryDelta(store, deltaMsg('c', 'args', '{"a"'));
    await settled();

    // Then the arguments grew.
    const inner = store.state.entries[0]!.entry.kind as { ToolCall: { arguments: string } };
    expect(inner.ToolCall.arguments).toBe('{"a"');
  });

  it('an output chunk lands on a tool result', async () => {
    // Given a tool result holding no content.
    const store = createStore();
    applyEntryUpsert(store, upsertMsg(0, entry('r', { ToolResult: { content: '' } })));
    await settled();

    // When an output delta arrives for it.
    applyEntryDelta(store, deltaMsg('r', 'output', '42'));
    await settled();

    // Then the content grew.
    const inner = store.state.entries[0]!.entry.kind as { ToolResult: { content: string } };
    expect(inner.ToolResult.content).toBe('42');
  });

  it('a delta whose field does not fit the entry kind is ignored', async () => {
    // Given an assistant entry, which has no arguments field.
    const store = createStore();
    applyEntryUpsert(store, upsertMsg(0, entry('a', { Assistant: { text: '' } })));
    await settled();

    // When an args delta arrives for it.
    applyEntryDelta(store, deltaMsg('a', 'args', '{"a"'));
    await settled();

    // Then the entry is unchanged.
    expect(readText(store.state.entries[0]!)).toBe('');
  });

  it('a delta for an unknown entry id is ignored', async () => {
    // Given a transcript holding entry `a`.
    const store = createStore();
    applyEntryUpsert(store, upsertMsg(0, entry('a', { Assistant: { text: '' } })));
    await settled();

    // When a delta names an id the transcript does not hold.
    applyEntryDelta(store, deltaMsg('nope', 'text', 'x'));
    await settled();

    // Then the transcript is unchanged.
    expect(readText(store.state.entries[0]!)).toBe('');
  });

  it('a frame for another session changes nothing', async () => {
    // Given a page viewing session s1 with one entry.
    const store = createStore();
    store.set((s) => {
      s.sessionId = 's1';
    });
    applyEntryUpsert(store, upsertMsg(0, entry('a', { Assistant: { text: '' } })));
    await settled();

    // When a delta for session s2 arrives naming that entry.
    const frame = { ...deltaMsg('a', 'text', 'x'), session_id: 's2' };
    applyEntryDelta(store, frame);
    await settled();

    // Then the entry is unchanged.
    expect(readText(store.state.entries[0]!)).toBe('');
  });
});

describe('applyEntriesPage', () => {
  it('prepends the page and records what precedes the tail', async () => {
    // Given a transcript holding one entry and nothing before it.
    const store = createStore();
    store.set((s) => {
      s.sessionId = 's1';
    });
    applyEntryUpsert(store, upsertMsg(0, entry('tail')));
    await settled();

    // When a page of two older entries arrives, with 3 before them.
    applyEntriesPage(store, {
      type: 'entries_page',
      session_id: 's1',
      entries: [entry('older-1'), entry('older-2')],
      entries_before: 3,
    });
    await settled();

    // Then they sit before the tail and the offset is recorded.
    expect(ids(store)).toEqual(['older-1', 'older-2', 'tail']);
    expect(store.state.entriesBefore).toBe(3);
  });

  it('a page for another session changes nothing', async () => {
    // Given a page viewing session s1.
    const store = createStore();
    store.set((s) => {
      s.sessionId = 's1';
    });
    await settled();

    // When a page for session s2 arrives.
    applyEntriesPage(store, {
      type: 'entries_page',
      session_id: 's2',
      entries: [entry('x')],
      entries_before: 0,
    });
    await settled();

    // Then the transcript is unchanged.
    expect(ids(store)).toEqual([]);
  });
});

describe('addOptimistic', () => {
  it('appends a user placeholder carrying the text', async () => {
    // Given an empty transcript.
    const store = createStore();

    // When a message is sent.
    addOptimistic(store, 'hello');
    await settled();

    // Then one entry holds that text, under an optimistic id.
    const [only] = store.state.entries;
    expect(readUserText(only!)).toBe('hello');
    expect(isOptimistic(only!.entry.id)).toBe(true);
  });
});

describe('readText', () => {
  it('reads the text of an entry whose kind is an object', () => {
    // Given an assistant entry holding text.
    // When reading its text.
    const text = readText(entry('a', { Assistant: { text: 'hi' } }));

    // Then it is the held text.
    expect(text).toBe('hi');
  });

  it('reads a kind that is a bare string', () => {
    // Given an entry whose kind is the text itself.
    // When reading its text.
    const text = readText(entry('a', { System: 'note' }));

    // Then it is that string.
    expect(text).toBe('note');
  });

  it('an empty kind reads as no text', () => {
    // Given an entry with no kind at all.
    // When reading its text.
    const text = readText(entry('a', {}));

    // Then it is the empty string.
    expect(text).toBe('');
  });
});
