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
  byteOffsetToIndex,
  applyEntryDelta,
  applyEntryUpsert,
  indexSnapshot,
  isOptimistic,
  newestEntryId,
  readText,
  readUserText,
} from './transcript_entries';
import type {
  AnyEntryKindMap,
  DeltaField,
  EntryEnvelope,
  EntryKind,
  EntryKindMap,
  ServerMsg,
} from '../protocol/protocol';

// Every tag a persisted map can carry: the union of each member's own key.
// `keyof` the union is `never`, since the members share no key.
type AnyTag = keyof EntryKindMap[keyof EntryKindMap];

/** The kind name each persisted tag belongs to. */
const KIND_OF: Record<AnyTag, EntryKind> = {
  User: 'user',
  System: 'system',
  Error: 'error',
  Assistant: 'assistant',
  Actor: 'actor',
  Thinking: 'thinking',
  ToolCall: 'tool_call',
  ToolResult: 'tool_result',
  Transient: 'transient',
  Compaction: 'compaction',
  Annotation: 'annotation',
  RuleInterrupt: 'rule_interrupt',
  Intersession: 'intersession',
};

/** Awaits the microtask a store write needs before it can be read back. */
async function settled(): Promise<void> {
  await Promise.resolve();
}

/** An entry with an id and one streamed kind, as the server sends it. */
function entry(id: string, kind: AnyEntryKindMap = { Assistant: '' }): EntryEnvelope {
  // The envelope's kind name follows the map's own tag, so the two can never
  // disagree: a Thinking map under an 'assistant' envelope is not a shape
  // the server sends, and the helper cannot build one.
  const tag = Object.keys(kind)[0] as AnyTag;
  return { kind: KIND_OF[tag], entry: { id, kind } };
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
function deltaMsg(
  entryId: string,
  field: DeltaField,
  chunk: string,
  offsetBytes = 0,
): Extract<ServerMsg, { type: 'entry_delta' }> {
  return { type: 'entry_delta', session_id: 's1', entry_id: entryId, field, offset_bytes: offsetBytes, chunk };
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
    applyEntryUpsert(store, upsertMsg(0, entry('a', { Assistant: 'first' })));
    await settled();

    // When a second upsert names the same id.
    applyEntryUpsert(store, upsertMsg(0, entry('a', { Assistant: 'second' })));
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
    applyEntryUpsert(store, upsertMsg(0, entry('a', { Assistant: '' })));
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
    applyEntryUpsert(store, upsertMsg(0, entry('a', { Assistant: '' })));
    await settled();
    applyEntryDelta(store, deltaMsg('a', 'text', 'Hel'));
    await settled();

    // When another chunk arrives, from where the first ended.
    applyEntryDelta(store, deltaMsg('a', 'text', 'lo', 3));
    await settled();

    // Then the text is both chunks in order.
    expect(readText(store.state.entries[0]!)).toBe('Hello');
  });

  it('a thinking chunk lands on a thinking entry', async () => {
    // Given a thinking entry holding no text.
    const store = createStore();
    applyEntryUpsert(store, upsertMsg(0, entry('t', { Thinking: '' })));
    await settled();

    // When a thinking delta arrives for it.
    applyEntryDelta(store, deltaMsg('t', 'thinking', 'hmm'));
    await settled();

    // Then the thinking text grew.
    const inner = store.state.entries[0]!.entry.kind as { Thinking: string };
    expect(inner.Thinking).toBe('hmm');
  });

  it('a thinking delta leaves the entry itself where it was', async () => {
    // Given a thinking entry holding one chunk, as the only entry.
    const store = createStore();
    applyEntryUpsert(store, upsertMsg(0, entry('t', { Thinking: 'hmm' })));
    await settled();

    // When a second thinking delta arrives for it while it is collapsed.
    applyEntryDelta(store, deltaMsg('t', 'thinking', ' more', 3));
    await settled();

    // Then only the text grew: the entry keeps its id and its place, so
    // nothing about its collapsed state has anything to react to.
    const [only] = store.state.entries;
    expect(only!.entry.id).toBe('t');
    expect(store.state.entries).toHaveLength(1);
    const inner = only!.entry.kind as { Thinking: string };
    expect(inner.Thinking).toBe('hmm more');
  });

  it('a delta repeating the creating upsert adds nothing', async () => {
    // Given an assistant entry that arrived by upsert already holding its
    // opening words — the entry that creates a reply carries its first
    // chunk, and the first delta after it carries the same words again.
    const store = createStore();
    applyEntryUpsert(store, upsertMsg(0, entry('a', { Assistant: 'This' })));
    await settled();

    // When the delta repeats exactly what the upsert delivered.
    applyEntryDelta(store, deltaMsg('a', 'text', 'This', 0));
    await settled();

    // Then the words are on the page once, not twice.
    expect(readText(store.state.entries[0]!)).toBe('This');
  });

  it('a delta overlapping the upsert adds only what is new', async () => {
    // Given an assistant entry that arrived by upsert holding `This is`.
    const store = createStore();
    applyEntryUpsert(store, upsertMsg(0, entry('a', { Assistant: 'This is' })));
    await settled();

    // When a delta arrives from offset 4, the text after `This`: an upsert
    // may have delivered past there, and this one did not, so the words land.
    applyEntryDelta(store, deltaMsg('a', 'text', ' is a sample', 4));
    await settled();

    // Then the text is what was held up to the offset, then the chunk.
    expect(readText(store.state.entries[0]!)).toBe('This is a sample');
  });

  it('a stream that repeats a character keeps both', async () => {
    // Given an assistant entry already holding `Hel`.
    const store = createStore();
    applyEntryUpsert(store, upsertMsg(0, entry('a', { Assistant: '' })));
    await settled();
    applyEntryDelta(store, deltaMsg('a', 'text', 'Hel'));
    await settled();

    // When the next chunk, from where the first ended, begins with the same
    // character the text ends with: a repeat by the stream, not a repeat of
    // text already held.
    applyEntryDelta(store, deltaMsg('a', 'text', 'lo', 3));
    await settled();

    // Then both are kept, because nothing says the second is a copy.
    expect(readText(store.state.entries[0]!)).toBe('Hello');
  });

  it('an args chunk lands on a tool call', async () => {
    // Given a tool call holding no arguments.
    const store = createStore();
    applyEntryUpsert(store, upsertMsg(0, entry('c', { ToolCall: { id: 'c', name: 'bash', arguments: '' } })));
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
    applyEntryUpsert(store, upsertMsg(0, entry('r', { ToolResult: { id: 'r', name: 'bash', content: '', status: 'Pending' } })));
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
    applyEntryUpsert(store, upsertMsg(0, entry('a', { Assistant: '' })));
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
    applyEntryUpsert(store, upsertMsg(0, entry('a', { Assistant: '' })));
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
    applyEntryUpsert(store, upsertMsg(0, entry('a', { Assistant: '' })));
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

describe('byteOffsetToIndex', () => {
  it('ASCII bytes and string indexes agree', () => {
    // Given a text of only ASCII, and an offset into it.
    // When converting the byte offset to a string index.
    const index = byteOffsetToIndex('This is', 4);

    // Then it is the same number: one byte is one code unit.
    expect(index).toBe(4);
  });

  it('a multi-byte character counts as one code unit, not its bytes', () => {
    // Given a text whose first character is three bytes in UTF-8 and one
    // code unit in UTF-16, and an offset just past it.
    // When converting the byte offset to a string index.
    const index = byteOffsetToIndex('é!', 2);

    // Then it is the character's index, not its byte count.
    expect(index).toBe(1);
  });

  it('an offset past the end of the text reads as the end', () => {
    // Given an offset beyond the text's last byte, which a frame sent
    // against a longer copy of the entry can carry.
    // When converting it to a string index.
    const index = byteOffsetToIndex('abc', 99);

    // Then it is the text's length, and not out of range.
    expect(index).toBe(3);
  });

  it('an offset that would split a character lands before it', () => {
    // Given an offset one byte into a two-byte character, which no server
    // cursor can name but a malformed frame can.
    // When converting it to a string index.
    const index = byteOffsetToIndex('aé', 2);

    // Then it is the whole character's start, so nothing is cut in half.
    expect(index).toBe(1);
  });
});

describe('readText', () => {
  it('reads the text of an entry whose kind is an object', () => {
    // Given an assistant entry holding text.
    // When reading its text.
    const text = readText(entry('a', { Assistant: 'hi' }));

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
    // Given an entry with no kind at all. No frame the server sends has
    // this shape, and the type now says so; but a frame is parsed with a
    // cast, so a malformed one can still reach the page, and it must read
    // as empty rather than throw.
    const malformed = { kind: 'assistant', entry: { id: 'a', kind: {} } } as unknown as EntryEnvelope;

    // When reading its text.
    const text = readText(malformed);

    // Then it is the empty string.
    expect(text).toBe('');
  });
});
