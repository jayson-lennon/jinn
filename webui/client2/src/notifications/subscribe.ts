// Subscribing this browser to push, and telling jinn about it.
//
// The subscription is made once per permission grant and re-sent on every
// load: a subscription is bound to the exact origin the page was served
// from and to the key it was made under, so a page that changed hosts, or a
// server that lost its key pair, replaces the stale row simply by sending
// the one that works now. The server keys by endpoint, so a re-send is an
// update, never a duplicate.
//
// Every browser API this module touches is a parameter, so the flow is
// testable without a real PushManager. In production they are the page's.

import type { Send } from '../pickers/model_picker';

/** The page's push surface, as the browser exposes it. */
export interface PushSurface {
  /** Registers the worker at `/sw.js` and resolves once it is active. */
  register(): Promise<void>;
  /** The subscription the browser already holds for this origin, if any. */
  current(): Promise<Subscription | null>;
  /** Asks the browser for a new subscription under the server's key. */
  subscribe(publicKey: string): Promise<Subscription>;
  /** The permission the user has granted, or null when the API is absent. */
  permission(): NotificationPermission | null;
}

/** A push subscription, as the browser issues it. */
export interface Subscription {
  endpoint: string;
  keys: { p256dh: string; auth: string };
}

/** The production surface: the page's own service worker and push manager. */
export const browserSurface: PushSurface = {
  async register(): Promise<void> {
    await navigator.serviceWorker.register('/sw.js', { scope: '/' });
    await navigator.serviceWorker.ready;
  },
  async current(): Promise<Subscription | null> {
    const registration = await navigator.serviceWorker.ready;
    const held = await registration.pushManager.getSubscription();
    return held ? toSubscription(held) : null;
  },
  async subscribe(publicKey: string): Promise<Subscription> {
    const registration = await navigator.serviceWorker.ready;
    const subscription = await registration.pushManager.subscribe({
      userVisibleOnly: true,
      applicationServerKey: base64UrlToBytes(publicKey),
    });
    return toSubscription(subscription);
  },
  permission(): NotificationPermission | null {
    return typeof Notification === 'undefined' ? null : Notification.permission;
  },
};

/** A browser subscription in the shape the wire carries. The keys ride as
 *  unpadded base64url, which is what the server decodes. */
export function toSubscription(held: PushSubscription): Subscription {
  const p256dh = held.getKey('p256dh');
  const auth = held.getKey('auth');
  if (!p256dh || !auth) throw new Error('the subscription carries no keys');
  return {
    endpoint: held.endpoint,
    keys: { p256dh: bytesToBase64Url(new Uint8Array(p256dh)), auth: bytesToBase64Url(new Uint8Array(auth)) },
  };
}

/** Decodes an unpadded base64url string, as `applicationServerKey` takes it. */
export function base64UrlToBytes(value: string): Uint8Array<ArrayBuffer> {
  const padding = '='.repeat((4 - (value.length % 4)) % 4);
  const base64 = (value + padding).replace(/-/g, '+').replace(/_/g, '/');
  const raw = atob(base64);
  // Copied into a fresh ArrayBuffer: `applicationServerKey` wants a view
  // over a plain buffer, not a shared one.
  return Uint8Array.from(raw, (char) => char.charCodeAt(0)).slice() as Uint8Array<ArrayBuffer>;
}

/** Encodes bytes as unpadded base64url. */
export function bytesToBase64Url(bytes: Uint8Array<ArrayBufferLike>): string {
  let binary = '';
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

/** Registers the worker and sends whatever subscription this browser holds
 *  or can obtain. Called on every load once permission is granted, and once
 *  more in the moment the user grants it — so the server holds a current
 *  row as soon as one exists, never only at the next load. */
export async function subscribe(
  surface: PushSurface,
  publicKey: string,
  send: Send,
): Promise<'sent' | 'no-permission' | 'no-key'> {
  if (!publicKey) return 'no-key';
  if (surface.permission() !== 'granted') return 'no-permission';
  await surface.register();
  const held = await surface.current();
  const subscription = held ?? (await surface.subscribe(publicKey));
  send({ type: 'push_subscribe', endpoint: subscription.endpoint, keys: subscription.keys });
  return 'sent';
}

/** Requests permission from inside a user gesture, then subscribes. The
 *  gesture is what makes the prompt allowed; the await keeps it alive. */
export async function requestAndSubscribe(
  surface: PushSurface,
  publicKey: string,
  request: () => Promise<NotificationPermission>,
  send: Send,
): Promise<'sent' | 'denied' | 'no-key' | 'no-permission'> {
  if (!publicKey) return 'no-key';
  const granted = await request();
  if (granted !== 'granted') return 'denied';
  const result = await subscribe(surface, publicKey, send);
  return result === 'sent' ? 'sent' : 'no-permission';
}
