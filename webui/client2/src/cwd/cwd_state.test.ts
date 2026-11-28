// The change-directory modal's state: every frame and helper, applied to a
// real store, and the state it leaves behind.
//
// Each test builds a store, reaches a starting state the way the page would,
// feeds it one frame or calls one helper, awaits one microtask for the write
// to land, and reads the result back the way a component does.

import { describe, it, expect, vi } from 'vitest';
import { apply, createStore, type ClientStore } from '../store/store';
import { BUNDLE_PROTOCOL_VERSION } from '../protocol/protocol_version';
import type { ServerMsg } from '../protocol/protocol';
import { openDash } from '../dash/dash';
import { applyCwdCheck, applySessionCwd, closeCwdModal, editCwdDraft, openCwdModal, resetCwd } from './cwd_state';

/** Awaits the microtask a store write needs before it can be read back. */
async function settled(): Promise<void> {
  await Promise.resolve();
}

type Snapshot = Extract<ServerMsg, { type: 'snapshot' }>;

/** A `snapshot` frame for `s1` with every field defaulted, overridable by
 *  name. */
function snapshot(overrides: Partial<Snapshot> = {}): Snapshot {
  return {
    type: 'snapshot',
    protocol: BUNDLE_PROTOCOL_VERSION,
    session_id: 's1',
    phase: { session_id: 's1', phase: 'Idle', queue_len: 0 },
    model_label: 'model',
    title: 'Title',
    persona: 'persona',
    context_label: '',
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

/** The page's starting point for a modal test: committed to `s1`, with the
 *  dash's directory row landed and the modal opened over it on the seed. */
async function openedOn(seed = '~/code'): Promise<ClientStore> {
  const store = createStore();
  apply(store, snapshot());
  await settled();
  apply(store, {
    type: 'session_stats',
    session_id: 's1',
    cwd: seed,
    branch: null,
    model: 'model',
    tree: null,
  } as unknown as ServerMsg);
  await settled();
  openDash(store, () => {});
  openCwdModal(store, () => {}, seed);
  await settled();
  return store;
}

describe('openCwdModal', () => {
  it('seeds the draft and asks for the first check at once', async () => {
    // Given a page viewing s1.
    const store = createStore();
    apply(store, snapshot());
    await settled();

    // When the modal is opened on the dash's directory row.
    const send = vi.fn();
    openCwdModal(store, send, '~/code');
    await settled();

    // Then the draft is the seed, and one check was asked for it.
    expect(store.state.cwdDraft).toBe('~/code');
    expect(send).toHaveBeenCalledTimes(1);
    expect(send).toHaveBeenCalledWith({ type: 'check_cwd', text: '~/code' });
  });

  it('closes the dash it was opened from', async () => {
    // Given a page with its dash open.
    const store = createStore();
    apply(store, snapshot());
    await settled();
    openDash(store, () => {});

    await settled();

    // When the modal is opened.
    openCwdModal(store, () => {}, '~/code');
    await settled();

    // Then the dash is closed and the modal is open.
    expect(store.state.dashOpen).toBe(false);
    expect(store.state.cwdOpen).toBe(true);
  });
});

describe('editCwdDraft', () => {
  it('marks the held verdict stale and keeps it for the footer', async () => {
    // Given a modal holding a green verdict for its draft.
    const store = await openedOn();
    applyCwdCheck(store, {
      type: 'cwd_check',
      session_id: 's1',
      text: '~/code',
      verdict: { kind: 'ok', path: '/home/me/code' },
    });

    // When the text moves on.
    editCwdDraft(store, '~/code/jinn');
    await settled();

    // Then the verdict is marked stale, and its green is still held.
    expect(store.state.cwdVerdict).toEqual({ verdict: { kind: 'ok', path: '/home/me/code' }, stale: true });
  });
});

describe('closeCwdModal', () => {
  it('closes the modal and leaves the draft and verdict in place', async () => {
    // Given an open modal with a green verdict.
    const store = await openedOn();
    applyCwdCheck(store, {
      type: 'cwd_check',
      session_id: 's1',
      text: '~/code',
      verdict: { kind: 'ok', path: '/home/me/code' },
    });

    // When the modal is closed.
    closeCwdModal(store);
    await settled();

    // Then it is closed, and the draft and verdict are as they were.
    expect(store.state.cwdOpen).toBe(false);
    expect(store.state.cwdDraft).toBe('~/code');
    expect(store.state.cwdVerdict).toEqual({ verdict: { kind: 'ok', path: '/home/me/code' }, stale: false });
  });
});

describe('applyCwdCheck', () => {
  it('stores a verdict whose text matches the draft and clears stale', async () => {
    // Given an open modal whose draft has moved on, so its verdict is stale.
    const store = await openedOn();
    editCwdDraft(store, '~/code/jinn');
    await settled();

    // When the answer for the draft's current text lands.
    apply(store, {
      type: 'cwd_check',
      session_id: 's1',
      text: '~/code/jinn',
      verdict: { kind: 'ok', path: '/home/me/code/jinn' },
    });
    await settled();

    // Then the verdict is stored and is current.
    expect(store.state.cwdVerdict).toEqual({ verdict: { kind: 'ok', path: '/home/me/code/jinn' }, stale: false });
  });

  it('ignores a verdict whose text is not the draft', async () => {
    // Given an open modal whose draft is ~/code/jinn with a green verdict.
    const store = await openedOn('~/code/jinn');
    applyCwdCheck(store, {
      type: 'cwd_check',
      session_id: 's1',
      text: '~/code/jinn',
      verdict: { kind: 'ok', path: '/home/me/code/jinn' },
    });

    // When a late answer for a path the user typed through lands.
    apply(store, {
      type: 'cwd_check',
      session_id: 's1',
      text: '~/code/jin',
      verdict: { kind: 'not_a_directory', path: '/home/me/code/jin' },
    });
    await settled();

    // Then the verdict is unchanged: still the green for the current text.
    expect(store.state.cwdVerdict).toEqual({ verdict: { kind: 'ok', path: '/home/me/code/jinn' }, stale: false });
  });

  it('ignores a verdict that lands after the modal closed', async () => {
    // Given a closed modal that still holds a stale verdict from its last edit.
    const store = await openedOn();
    editCwdDraft(store, '~/gone');
    closeCwdModal(store);
    await settled();

    // When the answer for that last edit lands.
    apply(store, {
      type: 'cwd_check',
      session_id: 's1',
      text: '~/gone',
      verdict: { kind: 'ok', path: '/home/me/gone' },
    });
    await settled();

    // Then nothing was stored: the verdict is still the stale one.
    expect(store.state.cwdVerdict).toEqual({ verdict: { kind: 'empty' }, stale: true });
  });
});

describe('applySessionCwd', () => {
  it("moves the dash's directory row and leaves the draft alone", async () => {
    // Given an open modal whose draft has moved on from the dash's row.
    const store = await openedOn();
    editCwdDraft(store, '~/code/jinn');
    await settled();

    // When the session's cwd changes in the terminal.
    apply(store, { type: 'session_cwd', session_id: 's1', cwd: '~/elsewhere' });
    await settled();

    // Then the dash's row shows the new cwd.
    expect(store.state.dashStats?.cwd).toBe('~/elsewhere');
    // And the draft is still the user's typed path.
    expect(store.state.cwdDraft).toBe('~/code/jinn');
  });

  it('ignores a cwd change for a session the page is not viewing', async () => {
    // Given a page committed to s1 with its dash row at ~/code.
    const store = await openedOn();
    closeCwdModal(store);
    await settled();

    // When another session's cwd changes.
    apply(store, { type: 'session_cwd', session_id: 's2', cwd: '~/elsewhere' });
    await settled();

    // Then the dash's row is unchanged.
    expect(store.state.dashStats?.cwd).toBe('~/code');
  });
});

describe('resetCwd', () => {
  it('closes the modal and drops its draft and verdict on a snapshot', async () => {
    // Given an open modal holding a green verdict.
    const store = await openedOn();
    applyCwdCheck(store, {
      type: 'cwd_check',
      session_id: 's1',
      text: '~/code',
      verdict: { kind: 'ok', path: '/home/me/code' },
    });
    await settled();

    // When a snapshot lands: the page switched sessions.
    apply(store, snapshot({ session_id: 's2' }));
    await settled();

    // Then the modal is closed and holds nothing.
    expect(store.state.cwdOpen).toBe(false);
    expect(store.state.cwdDraft).toBe('');
    expect(store.state.cwdVerdict).toEqual({ verdict: { kind: 'empty' }, stale: true });
  });
});
