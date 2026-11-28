// The subscribe flow: when a subscription is sent, when it is reused, and
// when nothing is sent at all.

import { describe, it, expect } from 'vitest';
import { subscribe, requestAndSubscribe, base64UrlToBytes, bytesToBase64Url, type PushSurface, type Subscription } from './subscribe';
import type { Send } from '../pickers/model_picker';

/** A `send` that records what leaves the page. */
function recorder(): Send & { messages: unknown[] } {
  const messages: unknown[] = [];
  const send = ((msg: unknown) => {
    messages.push(msg);
  }) as unknown as Send & { messages: unknown[] };
  send.messages = messages;
  return send;
}

/** A surface that records what was asked of it. `held` is the subscription
 *  the browser already holds, or null for none; `permission` is what the
 *  browser reports, read on every call so a test can change the answer
 *  mid-flow — the way a browser does once the user answers the prompt. */
function surface(options: { held?: Subscription | null; permission?: NotificationPermission | (() => NotificationPermission | null) } = {}): PushSurface & { calls: string[] } {
  const calls: string[] = [];
  const fresh: Subscription = { endpoint: 'https://push.example/a', keys: { p256dh: 'BKey', auth: 'secret' } };
  const held = options.held ?? null;
  return {
    calls,
    register: async () => {
      calls.push('register');
    },
    current: async () => {
      calls.push('current');
      return held;
    },
    subscribe: async () => {
      calls.push('subscribe');
      return fresh;
    },
    permission: () => (typeof options.permission === 'function' ? options.permission() : (options.permission ?? 'granted')),
  };
}

describe('subscribe', () => {
  it('sends a fresh subscription when none is held', async () => {
    // Given permission granted and no subscription yet.
    const send = recorder();
    const s = surface();

    // When subscribing.
    const result = await subscribe(s, 'BPublic', send);

    // Then the browser was asked for one, and it was sent.
    expect(result).toBe('sent');
    expect(s.calls).toEqual(['register', 'current', 'subscribe']);
    expect(send.messages).toEqual([
      { type: 'push_subscribe', endpoint: 'https://push.example/a', keys: { p256dh: 'BKey', auth: 'secret' } },
    ]);
  });

  it('re-sends a held subscription without re-subscribing', async () => {
    // Given a subscription the browser already holds.
    const send = recorder();
    const s = surface({ held: { endpoint: 'https://push.example/held', keys: { p256dh: 'H', auth: 'a' } } });

    // When subscribing.
    const result = await subscribe(s, 'BPublic', send);

    // Then the browser was not asked for a new one, and the held one was sent.
    expect(result).toBe('sent');
    expect(s.calls).toEqual(['register', 'current']);
    expect(send.messages[0]).toMatchObject({ endpoint: 'https://push.example/held' });
  });

  it('sends nothing when permission is not granted', async () => {
    // Given a page whose permission is still default.
    const send = recorder();
    const s = surface({ permission: 'default' });

    // When subscribing.
    const result = await subscribe(s, 'BPublic', send);

    // Then nothing was registered or sent.
    expect(result).toBe('no-permission');
    expect(s.calls).toEqual([]);
    expect(send.messages).toEqual([]);
  });

  it('sends nothing when the server holds no key', async () => {
    // Given a server that reported no public key.
    const send = recorder();
    const s = surface();

    // When subscribing.
    const result = await subscribe(s, '', send);

    // Then nothing happened.
    expect(result).toBe('no-key');
    expect(s.calls).toEqual([]);
  });
});

describe('requestAndSubscribe', () => {
  it('subscribes once the user grants permission', async () => {
    // Given a prompt the user accepts, and a browser that then reports granted.
    const send = recorder();
    let permission: NotificationPermission = 'default';
    const s = surface({ permission: () => permission });

    // When permission is requested and granted.
    const result = await requestAndSubscribe(s, 'BPublic', async () => {
      permission = 'granted';
      return permission;
    }, send);

    // Then the subscription was sent.
    expect(result).toBe('sent');
    expect(send.messages).toHaveLength(1);
  });

  it('sends nothing when the user denies', async () => {
    // Given a prompt the user declines.
    const send = recorder();
    const s = surface();

    // When permission is requested and denied.
    const result = await requestAndSubscribe(s, 'BPublic', async () => 'denied', send);

    // Then nothing was sent.
    expect(result).toBe('denied');
    expect(send.messages).toEqual([]);
  });

  it('never prompts when the server holds no key', async () => {
    // Given a server that reported no public key.
    const send = recorder();
    const s = surface();
    let asked = false;

    // When permission would be requested.
    const result = await requestAndSubscribe(s, '', async () => {
      asked = true;
      return 'granted';
    }, send);

    // Then the user was never prompted: there is nothing to subscribe with.
    expect(result).toBe('no-key');
    expect(asked).toBe(false);
  });
});

describe('base64url', () => {
  it('decodes an unpadded key to the bytes it names', () => {
    // Given an unpadded base64url string.
    // When decoded.
    const bytes = base64UrlToBytes('AQID');

    // Then the bytes are the ones it encodes.
    expect([...bytes]).toEqual([1, 2, 3]);
  });

  it('round-trips through encode', () => {
    // Given some bytes.
    const bytes = Uint8Array.from([0, 255, 16, 32]);

    // When encoded and decoded.
    const back = base64UrlToBytes(bytesToBase64Url(bytes));

    // Then they are the same bytes.
    expect([...back]).toEqual([0, 255, 16, 32]);
  });
});
