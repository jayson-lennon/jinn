// The session-load overlay: the one wait the page raises itself, and the
// three things that end it — the right snapshot, a refusal, or a clear.
//
// `forkSource` is module-level. Every fork test opens with its own
// `beginFork`, which overwrites it, so no test inherits a fork another left
// pending. Every helper writes the store, so every test awaits one
// microtask before it reads the result.

import { describe, it, expect } from 'vitest';
import { createStore } from './store';
import {
  beginFork,
  beginSessionLoad,
  clearSessionLoad,
  resolveLoadingAfterError,
  resolveLoadingAfterSnapshot,
} from './session_load';

/** Awaits the microtask a store write needs before it can be read back. */
async function settled(): Promise<void> {
  await Promise.resolve();
}

describe('beginSessionLoad', () => {
  it('marks the session the page is waiting on', async () => {
    // Given a page with no wait.
    const store = createStore();

    // When beginning a load of s1.
    beginSessionLoad(store, 's1');
    await settled();

    // Then the overlay's cause is s1.
    expect(store.state.loadingSessionId).toBe('s1');
  });
});

describe('clearSessionLoad', () => {
  it('ends whatever wait is under way', async () => {
    // Given a page waiting on s1.
    const store = createStore();
    beginSessionLoad(store, 's1');
    await settled();

    // When clearing the load.
    clearSessionLoad(store);
    await settled();

    // Then there is no wait.
    expect(store.state.loadingSessionId).toBeNull();
  });
});

describe('resolveLoadingAfterSnapshot', () => {
  it('a snapshot naming the awaited session ends the wait', () => {
    // Given a wait on s1.
    const s = { loadingSessionId: 's1' as string | null };

    // When a snapshot names s1.
    resolveLoadingAfterSnapshot('s1', 's1', s);

    // Then the wait is over.
    expect(s.loadingSessionId).toBeNull();
  });

  it('a snapshot naming another session leaves the wait up', () => {
    // Given a wait on s1.
    const s = { loadingSessionId: 's1' as string | null };

    // When a snapshot names s2 — the old session's, landing late.
    resolveLoadingAfterSnapshot('s1', 's2', s);

    // Then the overlay stays, waiting for the real one.
    expect(s.loadingSessionId).toBe('s1');
  });

  it('no wait leaves the overlay alone', () => {
    // Given no wait.
    const s = { loadingSessionId: null as string | null };

    // When a snapshot lands.
    resolveLoadingAfterSnapshot(null, 's1', s);

    // Then there is still no wait.
    expect(s.loadingSessionId).toBeNull();
  });

  it('a fork ends on a snapshot naming a session other than its source', async () => {
    // Given a pending fork taken from s1.
    const store = createStore();
    beginFork(store, 's1');
    await settled();

    // When a snapshot names s2, the fork's own id — the loading id read
    // before the setter, as the snapshot arm reads it.
    const loadingId = store.state.loadingSessionId;
    store.set((s) => {
      resolveLoadingAfterSnapshot(loadingId, 's2', s);
    });
    await settled();

    // Then the wait is over.
    expect(store.state.loadingSessionId).toBeNull();
  });

  it('a fork survives a snapshot that names its own source', async () => {
    // Given a pending fork taken from s1.
    const store = createStore();
    beginFork(store, 's1');
    await settled();

    // When a snapshot names s1 — the source, not the child.
    const loadingId = store.state.loadingSessionId;
    store.set((s) => {
      resolveLoadingAfterSnapshot(loadingId, 's1', s);
    });
    await settled();

    // Then the overlay stays up.
    expect(store.state.loadingSessionId).not.toBeNull();
  });
});

describe('resolveLoadingAfterError', () => {
  it('ends any wait, a fork included', async () => {
    // Given a pending fork.
    const store = createStore();
    beginFork(store, 's1');
    await settled();

    // When a refusal arrives.
    store.set((s) => {
      resolveLoadingAfterError(s);
    });
    await settled();

    // Then the wait is over.
    expect(store.state.loadingSessionId).toBeNull();
  });
});
