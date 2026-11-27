// The toast stack: refusals and failures the server reports, stacked until
// the page dismisses them.
//
// Every helper writes the store, so every test awaits one microtask before
// it reads the result — a same-tick read returns the value before the write.

import { describe, it, expect } from 'vitest';
import { createStore } from './store/store';
import { applyError, dismissToast, pushToast } from './toast_state';
import { beginSessionLoad } from './session_load';

/** Awaits the microtask a store write needs before it can be read back. */
async function settled(): Promise<void> {
  await Promise.resolve();
}

describe('pushToast', () => {
  it('stacks a notice on an empty stack', async () => {
    // Given a page with no toasts.
    const store = createStore();

    // When pushing one.
    pushToast(store, 'refused');
    await settled();

    // Then it is the only notice.
    expect(store.state.toasts.map((t) => t.message)).toEqual(['refused']);
  });

  it('a second notice joins the first, newest last', async () => {
    // Given a page already showing one notice.
    const store = createStore();
    pushToast(store, 'first');
    await settled();

    // When pushing another.
    pushToast(store, 'second');
    await settled();

    // Then both show, in the order pushed.
    expect(store.state.toasts.map((t) => t.message)).toEqual(['first', 'second']);
  });
});

describe('dismissToast', () => {
  it('removes only the notice with the id given', async () => {
    // Given two notices.
    const store = createStore();
    pushToast(store, 'first');
    pushToast(store, 'second');
    await settled();
    const [first] = store.state.toasts;

    // When dismissing the first.
    dismissToast(store, first!.id);
    await settled();

    // Then only the second remains.
    expect(store.state.toasts.map((t) => t.message)).toEqual(['second']);
  });
});

describe('applyError', () => {
  it('pushes one notice carrying the message', async () => {
    // Given a page with no toasts.
    const store = createStore();

    // When an error frame arrives.
    applyError(store, { type: 'error', message: 'no such session' });
    await settled();

    // Then one notice carries that message.
    expect(store.state.toasts.map((t) => t.message)).toEqual(['no such session']);
  });

  it('ends a pending session load', async () => {
    // Given a page waiting on a session to load.
    const store = createStore();
    beginSessionLoad(store, 's1');
    await settled();

    // When an error frame arrives.
    applyError(store, { type: 'error', message: 'no such session' });
    await settled();

    // Then the wait is over.
    expect(store.state.loadingSessionId).toBeNull();
  });
});
