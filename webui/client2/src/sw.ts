// The web UI's service worker. It exists for three events and nothing else:
// a push arriving from jinn, a hidden page reporting a settle, and the user
// tapping the banner either of them raised.
//
// It carries no fetch handler on purpose. Without one the browser never
// routes a page load through this file, so installing it changes nothing
// about how the page, its assets, or its socket are fetched — the worker is
// a sleeping script the browser wakes for those events and ignores the rest
// of the time.
//
// This file decides nothing. What to do for each event lives in
// `notifications/decide.ts`, over two small surfaces the browser supplies
// here — which is what makes those decisions unit-testable, since headless
// Chromium never grants the notification permission and raises no banner.
//
// Whether a banner is shown is the worker's call, never a page's: only the
// worker can see every open page of the origin, and a push that shows no
// notification is what makes Safari revoke the permission.
//
// `tsconfig.json` gives this program the DOM library, where `self` is a
// `Window`; a worker's `self` is a `ServiceWorkerGlobalScope`. The reference
// to the `webworker` library below makes that type available, and the cast
// narrows `self` for this file only.

/// <reference lib="webworker" />

import {
  onPush,
  onSettle,
  onTap,
  type PushPayload,
  type SettleMessage,
  type Surface,
} from './notifications/decide';

/// The worker's own scope: `clients`, `registration`, and the events.
const worker = self as unknown as ServiceWorkerGlobalScope;

/// The browser as the decisions use it: the worker's own registration and
/// client list, nothing more.
const surface: Surface = {
  show: (title, options) => worker.registration.showNotification(title, options),
  clients: () => worker.clients.matchAll({ type: 'window', includeUncontrolled: true }),
  // `focus()` is what brings a page forward; the message has to reach it
  // first, or a page that reloads on focusing would lose it.
  deliver: async (client, message) => {
    const target = client as WindowClient;
    if (message) target.postMessage(message);
    await target.focus();
  },
  open: async (url) => {
    await worker.clients.openWindow(url);
  },
};

worker.addEventListener('push', (event: PushEvent) => {
  // `waitUntil` keeps the worker alive until the banner is up, so the
  // browser cannot suspend it between the event and `showNotification` —
  // the gap that would otherwise count as a silent push.
  event.waitUntil(onPush(surface, event.data ? (event.data.json() as PushPayload) : null));
});

worker.addEventListener('message', (event: ExtendableMessageEvent) => {
  const data = event.data as SettleMessage | undefined;
  if (data?.type !== 'settle') return;
  event.waitUntil(onSettle(surface, data));
});

worker.addEventListener('notificationclick', (event: NotificationEvent) => {
  const sessionId = (event.notification.data as { session_id?: string } | undefined)?.session_id;
  event.waitUntil(onTap(surface, sessionId, () => event.notification.close()));
});
