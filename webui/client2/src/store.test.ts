// The store's dispatch: every `ServerMsg` frame, applied to a real store,
// and the state it leaves behind.
//
// `apply()` is the one function every frame from the server goes through.
// Every test builds a store, feeds it one frame, awaits one microtask for
// the write to land, and reads the result back the way a component does.

import { describe, it, expect } from 'vitest';
import { apply, belongsToViewedSession, createStore, isBusy, type ClientStore } from './store';
import type { EntryEnvelope, Phase, PromptTemplate, ServerMsg, SessionRow } from './protocol';

/** Awaits the microtask a store write needs before it can be read back. */
async function settled(): Promise<void> {
  await Promise.resolve();
}

/** A session row with every field defaulted, overridable by name. */
function sessionRow(overrides: Partial<SessionRow> = {}): SessionRow {
  return {
    session_id: 's1',
    title: 'Session',
    is_active: false,
    is_busy: false,
    is_tearing_down: false,
    in_library: false,
    last_settled_at: null,
    parent_session: null,
    origin: 'root',
    depth: 0,
    ...overrides,
  } as SessionRow;
}

/** A `snapshot` frame with every field defaulted, overridable by name. */
function snapshot(overrides: Partial<Extract<ServerMsg, { type: 'snapshot' }>> = {}): Extract<ServerMsg, { type: 'snapshot' }> {
  return {
    type: 'snapshot',
    protocol: 1,
    session_id: 's1',
    phase: { session_id: 's1', phase: 'Idle', queue_len: 0 },
    model_label: 'model',
    title: 'Title',
    persona: 'persona',
    revision: 1,
    entries: [],
    entries_before: 0,
    sessions: [],
    library: [],
    hydrating: false,
    theme: 'dark',
    prompt_templates: [],
    ...overrides,
  };
}

/** An entry with an id, as the snapshot's transcript carries it. */
function entry(id: string): EntryEnvelope {
  return { kind: 'assistant', entry: { id, kind: { Assistant: { text: id } } } };
}

describe('snapshot', () => {
  it('sets every session fact the frame carries', async () => {
    // Given a store in its initial state.
    const store = createStore();

    // When a snapshot lands with a non-default fact in every slot.
    apply(store, snapshot({
      session_id: 's9',
      phase: { session_id: 's9', phase: 'Working', queue_len: 2 },
      model_label: 'sonnet',
      title: 'A title',
      persona: 'reviewer',
      revision: 7,
      entries: [entry('e1')],
      entries_before: 40,
      theme: 'light',
      prompt_templates: [{ name: 'plan' } as PromptTemplate],
    }));
    await settled();

    // Then every one of them is the store's.
    const s = store.state;
    expect(s.sessionId).toBe('s9');
    expect(s.phase).toBe('Working');
    expect(s.queueLen).toBe(2);
    expect(s.modelLabel).toBe('sonnet');
    expect(s.title).toBe('A title');
    expect(s.persona).toBe('reviewer');
    expect(s.revision).toBe(7);
    expect(s.entries.map((e) => e.entry.id)).toEqual(['e1']);
    expect(s.entriesBefore).toBe(40);
    expect(s.theme).toBe('light');
    expect(s.promptTemplates.map((t) => t.name)).toEqual(['plan']);
  });

  it('marks the page synced and ends the "loading more" state', async () => {
    // Given a store with a page load under way.
    const store = createStore();
    store.set((s) => {
      s.loadingMore = true;
    });
    await settled();

    // When a snapshot lands.
    apply(store, snapshot());
    await settled();

    // Then the page is synced and no longer loading more.
    expect(store.state.synced).toBe(true);
    expect(store.state.loadingMore).toBe(false);
  });

  it('leaves the draft and the pending images as they were', async () => {
    // Given a draft and an image the user has not sent.
    const store = createStore();
    store.set((s) => {
      s.draft = 'unsent';
      s.chips = [{ name: 'a.png', data_base64: 'AA==' }];
    });
    await settled();

    // When a snapshot lands for another session.
    apply(store, snapshot({ session_id: 's2' }));
    await settled();

    // Then the draft and the image survive the switch.
    expect(store.state.draft).toBe('unsent');
    expect(store.state.chips).toHaveLength(1);
  });

  it('clears the context figure, which the snapshot does not carry', async () => {
    // Given a context figure from the previous session's tick.
    const store = createStore();
    store.set((s) => {
      s.contextLabel = '41%';
    });
    await settled();

    // When a snapshot lands.
    apply(store, snapshot());
    await settled();

    // Then the figure is cleared, so the old one never lingers.
    expect(store.state.contextLabel).toBe('');
  });

  it('closes an open `#` completion, whose list belonged to the session left', async () => {
    // Given an open completion on the old session's templates.
    const store = createStore();
    store.set((s) => {
      s.promptCompletion = { start: 0, filter: 'pl' };
      s.promptHighlight = 1;
    });
    await settled();

    // When a snapshot lands.
    apply(store, snapshot());
    await settled();

    // Then the popup is closed and its highlight reset.
    expect(store.state.promptCompletion).toBeNull();
    expect(store.state.promptHighlight).toBeNull();
  });

  it('replaces the session-scoped picker rows with the frame', async () => {
    // Given skill rows left over from another session.
    const store = createStore();
    store.set((s) => {
      s.skillRows = [{ name: 'old' } as never];
      s.skillFilter = 'o';
    });
    await settled();

    // When a snapshot lands.
    apply(store, snapshot());
    await settled();

    // Then they are cleared: they were not this session's.
    expect(store.state.skillRows).toEqual([]);
    expect(store.state.skillFilter).toBe('');
  });

  it('ends the loading overlay when the snapshot names the awaited session', async () => {
    // Given a page waiting on s1.
    const store = createStore();
    store.set((s) => {
      s.loadingSessionId = 's1';
    });
    await settled();

    // When the snapshot for s1 lands.
    apply(store, snapshot({ session_id: 's1' }));
    await settled();

    // Then the overlay is gone.
    expect(store.state.loadingSessionId).toBeNull();
  });

  it('leaves the overlay up when the snapshot names another session', async () => {
    // Given a page waiting on s1.
    const store = createStore();
    store.set((s) => {
      s.loadingSessionId = 's1';
    });
    await settled();

    // When a snapshot for s2 lands first — the old session's, late.
    apply(store, snapshot({ session_id: 's2' }));
    await settled();

    // Then the overlay stays until the real one lands.
    expect(store.state.loadingSessionId).toBe('s1');
  });
});

/** A page already holding session s1, as a frame for s2 must find it. */
async function viewingS1(): Promise<ClientStore> {
  const store = createStore();
  apply(store, snapshot({ session_id: 's1' }));
  await settled();
  return store;
}

describe('phase', () => {
  it('sets the phase and queue length for the viewed session', async () => {
    // Given a page holding s1.
    const store = await viewingS1();

    // When a phase frame arrives for s1.
    apply(store, { type: 'phase', session_id: 's1', phase: 'Streaming', queue_len: 3 });
    await settled();

    // Then both facts are the store's.
    expect(store.state.phase).toBe('Streaming');
    expect(store.state.queueLen).toBe(3);
  });
});

describe('model', () => {
  it('sets the model label for the viewed session', async () => {
    // Given a page holding s1.
    const store = await viewingS1();

    // When a model frame arrives for s1.
    apply(store, { type: 'model', session_id: 's1', label: 'opus' });
    await settled();

    // Then the label is the store's.
    expect(store.state.modelLabel).toBe('opus');
  });
});

describe('context_usage', () => {
  it('sets the persona and the context figure for the viewed session', async () => {
    // Given a page holding s1.
    const store = await viewingS1();

    // When a context frame arrives for s1.
    apply(store, { type: 'context_usage', session_id: 's1', persona: 'planner', label: '41%' });
    await settled();

    // Then both are the store's.
    expect(store.state.persona).toBe('planner');
    expect(store.state.contextLabel).toBe('41%');
  });
});

describe('a frame for a session the page is not viewing', () => {
  it.each([
    ['phase', { type: 'phase', session_id: 's2', phase: 'Streaming', queue_len: 9 } as ServerMsg],
    ['model', { type: 'model', session_id: 's2', label: 'other' } as ServerMsg],
    ['context_usage', { type: 'context_usage', session_id: 's2', persona: 'other', label: '9%' } as ServerMsg],
    ['title', { type: 'title', session_id: 's2', title: 'Other' } as ServerMsg],
  ])('%s changes nothing', async (_label, frame) => {
    // Given a page holding s1, with a fact in the slot the frame would write.
    const store = await viewingS1();

    // When the frame for s2 arrives.
    apply(store, frame);
    await settled();

    // Then the store is exactly as the snapshot left it.
    expect(store.state.phase).toBe('Idle');
    expect(store.state.modelLabel).toBe('model');
    expect(store.state.persona).toBe('persona');
    expect(store.state.contextLabel).toBe('');
    expect(store.state.title).toBe('Title');
  });
});

describe('title', () => {
  it('updates the title bar and the sidebar row together', async () => {
    // Given a page holding s1, with s1's row in the sidebar.
    const store = await viewingS1();
    apply(store, { type: 'sessions', rows: [sessionRow()], library: [], hydrating: false });
    await settled();

    // When a title frame arrives for s1.
    apply(store, { type: 'title', session_id: 's1', title: 'Renamed' });
    await settled();

    // Then both places show the new title.
    expect(store.state.title).toBe('Renamed');
    expect(store.state.sessions[0]?.title).toBe('Renamed');
  });

  it('leaves the other rows alone', async () => {
    // Given a page whose sidebar holds two rows.
    const store = await viewingS1();
    apply(store, {
      type: 'sessions',
      rows: [sessionRow({ session_id: 's1', title: 'One' }), sessionRow({ session_id: 's2', title: 'Two' })],
      library: [],
      hydrating: false,
    });
    await settled();

    // When a title frame renames s1.
    apply(store, { type: 'title', session_id: 's1', title: 'Renamed' });
    await settled();

    // Then only s1's row changed.
    expect(store.state.sessions.map((r) => r.title)).toEqual(['Renamed', 'Two']);
  });
});

describe('sessions', () => {
  it('replaces both lists and the hydrating flag', async () => {
    // Given a page with stale lists.
    const store = await viewingS1();

    // When a sessions frame arrives with new lists.
    apply(store, {
      type: 'sessions',
      rows: [sessionRow({ session_id: 'a' })],
      library: [sessionRow({ session_id: 'b' })],
      hydrating: true,
    });
    await settled();

    // Then the lists are the frame's.
    expect(store.state.sessions.map((r) => r.session_id)).toEqual(['a']);
    expect(store.state.library.map((r) => r.session_id)).toEqual(['b']);
    expect(store.state.hydrating).toBe(true);
  });

  it('marks the viewed session seen at the settle time the frame reports', async () => {
    // Given a page holding s1, never seen.
    const store = await viewingS1();

    // When a sessions frame reports s1 settled at noon.
    apply(store, {
      type: 'sessions',
      rows: [sessionRow({ session_id: 's1', last_settled_at: '2026-01-01T12:00:00Z' })],
      library: [],
      hydrating: false,
    });
    await settled();

    // Then the seen time is that settle, the server's own.
    expect(store.state.lastSeen.s1).toBe('2026-01-01T12:00:00Z');
  });
});

describe('theme', () => {
  it('sets the theme', async () => {
    // Given a page in dark mode.
    const store = await viewingS1();

    // When a theme frame arrives.
    apply(store, { type: 'theme', theme: 'light' });
    await settled();

    // Then the store's theme is the frame's.
    expect(store.state.theme).toBe('light');
  });
});

describe('new_session_options', () => {
  it('sets the projects and lifecycles', async () => {
    // Given a page with no options.
    const store = createStore();

    // When an options frame arrives.
    apply(store, { type: 'new_session_options', projects: ['/p'], lifecycles: [] });
    await settled();

    // Then they are the store's.
    expect(store.state.newSession?.projects).toEqual(['/p']);
  });
});

describe('prompt_templates', () => {
  it('sets the templates and resets the highlight for the viewed session', async () => {
    // Given a page holding s1 with the highlight moved.
    const store = await viewingS1();
    store.set((s) => {
      s.promptHighlight = 2;
    });
    await settled();

    // When a prompt_templates frame arrives for s1.
    apply(store, { type: 'prompt_templates', session_id: 's1', templates: [{ name: 'plan' } as PromptTemplate] });
    await settled();

    // Then the templates are the frame's and the highlight is back to default.
    expect(store.state.promptTemplates.map((t) => t.name)).toEqual(['plan']);
    expect(store.state.promptHighlight).toBeNull();
  });

  it('a frame for another session changes nothing', async () => {
    // Given a page holding s1.
    const store = await viewingS1();

    // When a prompt_templates frame arrives for s2.
    apply(store, { type: 'prompt_templates', session_id: 's2', templates: [{ name: 'x' } as PromptTemplate] });
    await settled();

    // Then the templates are unchanged.
    expect(store.state.promptTemplates).toEqual([]);
  });
});

describe('error', () => {
  it('pushes one toast carrying the message', async () => {
    // Given a page with no toasts.
    const store = createStore();

    // When an error frame arrives.
    apply(store, { type: 'error', message: 'refused' });
    await settled();

    // Then one notice carries it.
    expect(store.state.toasts.map((t) => t.message)).toEqual(['refused']);
  });
});

describe('current and ping', () => {
  it.each([
    ['current', { type: 'current' } as ServerMsg],
    ['ping', { type: 'ping' } as ServerMsg],
  ])('%s changes nothing', async (_label, frame) => {
    // Given a page in a known state.
    const store = await viewingS1();
    const before = JSON.stringify({
      phase: store.state.phase,
      title: store.state.title,
      entries: store.state.entries,
      toasts: store.state.toasts,
    });

    // When the frame arrives.
    apply(store, frame);
    await settled();

    // Then the state is identical.
    const after = JSON.stringify({
      phase: store.state.phase,
      title: store.state.title,
      entries: store.state.entries,
      toasts: store.state.toasts,
    });
    expect(after).toBe(before);
  });
});

describe('belongsToViewedSession', () => {
  it('a page with no session accepts any session', () => {
    // Given a page holding no session.
    const store = createStore();

    // When asking whether a frame for s1 belongs.
    const yes = belongsToViewedSession(store, 's1');

    // Then it does.
    expect(yes).toBe(true);
  });

  it('a page holding s1 accepts s1 and no other', async () => {
    // Given a page holding s1.
    const store = await viewingS1();

    // When asking about s1 and about s2.
    const own = belongsToViewedSession(store, 's1');
    const other = belongsToViewedSession(store, 's2');

    // Then only s1 belongs.
    expect(own).toBe(true);
    expect(other).toBe(false);
  });
});

describe('isBusy', () => {
  it.each([
    ['Idle', false],
    ['Streaming', true],
    ['Working', true],
    ['Cancelling', true],
  ] as [Phase, boolean][])('%s is busy: %s', (phase, busy) => {
    // Given a phase.
    // When asking whether it is busy.
    const yes = isBusy(phase);

    // Then only Idle is not.
    expect(yes).toBe(busy);
  });
});

// --- the dash ---

function sessionStats(
  overrides: Partial<Extract<ServerMsg, { type: 'session_stats' }>> = {},
): Extract<ServerMsg, { type: 'session_stats' }> {
  return {
    type: 'session_stats',
    session_id: 's1',
    cwd: '/proj',
    cache_hit_percent: 97,
    tokens_sent: '1.2k',
    tokens_received: '3.4k',
    context_label: '12.3k / 200k (6%)',
    cost: '$0.00123',
    turn_count: 4,
    working: '00:01:02',
    model: '(openai)/gpt',
    tree: null,
    ...overrides,
  };
}

describe('the dash', () => {
  it('lands the session statistics in the dash', async () => {
    // Given a page viewing s1 with no statistics.
    const store = createStore();
    apply(store, snapshot({ session_id: 's1' }));
    await settled();

    // When the statistics arrive.
    apply(store, sessionStats());
    await settled();

    // Then the dash holds the figures the frame carried.
    expect(store.state.dashStats).toMatchObject({ turn_count: 4, working: '00:01:02', tree: null });
  });

  it('lands the pin rows in the dash', async () => {
    // Given a page viewing s1.
    const store = createStore();
    apply(store, snapshot({ session_id: 's1' }));
    await settled();

    // When the pin rows arrive.
    apply(store, { type: 'dash_pin_rows', session_id: 's1', rows: [{ entry_id: 'e7', text: '[TOP] > hi' }] });
    await settled();

    // Then the dash holds the one pin.
    expect(store.state.dashPins).toEqual([{ entry_id: 'e7', text: '[TOP] > hi' }]);
  });

  it('lands the task list in the dash', async () => {
    // Given a page viewing s1.
    const store = createStore();
    apply(store, snapshot({ session_id: 's1' }));
    await settled();

    // When the task list arrives.
    apply(store, {
      type: 'dash_tasks',
      session_id: 's1',
      phases: [{ description: 'Build', is_active: true, has_pending_work: true, tasks: [{ description: 'write', status: 'Pending' }] }],
      done: 0,
      total: 1,
    });
    await settled();

    // Then the dash holds the one phase and the counts.
    expect(store.state.dashTasks).toMatchObject({ done: 0, total: 1, phases: [{ description: 'Build', is_active: true }] });
  });

  it('lands the enabled MCP rows in the dash', async () => {
    // Given a page viewing s1.
    const store = createStore();
    apply(store, snapshot({ session_id: 's1' }));
    await settled();

    // When the MCP rows arrive.
    apply(store, { type: 'dash_mcp_rows', session_id: 's1', rows: [{ name: 'fs', badge: 'running' }] });
    await settled();

    // Then the dash holds the one server with its badge.
    expect(store.state.dashMcp).toEqual([{ name: 'fs', badge: 'running' }]);
  });

  it('drops the statistics for a session the page has left', async () => {
    // Given a page viewing s1.
    const store = createStore();
    apply(store, snapshot({ session_id: 's1' }));
    await settled();

    // When s2's statistics arrive.
    apply(store, sessionStats({ session_id: 's2' }));
    await settled();

    // Then the dash holds nothing.
    expect(store.state.dashStats).toBeNull();
  });

  it('drops the pin rows for a session the page has left', async () => {
    // Given a page viewing s1.
    const store = createStore();
    apply(store, snapshot({ session_id: 's1' }));
    await settled();

    // When s2's pin rows arrive.
    apply(store, { type: 'dash_pin_rows', session_id: 's2', rows: [{ entry_id: 'e7', text: 'x' }] });
    await settled();

    // Then the dash holds no pins.
    expect(store.state.dashPins).toEqual([]);
  });

  it('closes the dash on a snapshot', async () => {
    // Given an open dash.
    const store = createStore();
    store.set((s) => {
      s.dashOpen = true;
    });
    await settled();

    // When a snapshot lands.
    apply(store, snapshot());
    await settled();

    // Then the dash is closed.
    expect(store.state.dashOpen).toBe(false);
  });

  it('drops the dash facts on a snapshot', async () => {
    // Given a dash holding statistics, pins, tasks and servers.
    const store = createStore();
    apply(store, sessionStats());
    apply(store, { type: 'dash_pin_rows', session_id: 's1', rows: [{ entry_id: 'e7', text: 'x' }] });
    apply(store, { type: 'dash_mcp_rows', session_id: 's1', rows: [{ name: 'fs', badge: 'running' }] });
    await settled();

    // When a snapshot lands.
    apply(store, snapshot());
    await settled();

    // Then the dash holds none of it.
    expect(store.state.dashStats).toBeNull();
    expect(store.state.dashPins).toEqual([]);
    expect(store.state.dashMcp).toEqual([]);
  });

  it('drops a pending jump on a snapshot', async () => {
    // Given a transcript waiting on a jump to e7.
    const store = createStore();
    store.set((s) => {
      s.jumpTarget = 'e7';
    });
    await settled();

    // When a snapshot lands.
    apply(store, snapshot());
    await settled();

    // Then no jump is pending.
    expect(store.state.jumpTarget).toBeNull();
  });

  it('drops a pending jump when the server refuses it', async () => {
    // Given a transcript waiting on a jump to e7.
    const store = createStore();
    apply(store, snapshot({ session_id: 's1' }));
    store.set((s) => {
      s.jumpTarget = 'e7';
    });
    await settled();

    // When the refusal arrives.
    apply(store, { type: 'error', message: 'no such entry' });
    await settled();

    // Then no jump is pending.
    expect(store.state.jumpTarget).toBeNull();
  });
});
