// What this page can do with notifications, decided by asking the browser.
//
// Nothing here may assume the Notification or Push APIs exist. On a plain
// `http://` origin that is not loopback the browser exposes neither — they
// are not "denied", they are simply absent — and the web UI must keep
// working there. So every check is a feature detection, never a permission
// read: touching `Notification.permission` when `Notification` is undefined
// would throw on exactly the pages this module exists to protect.

/** Whether the browser exposes the Push API at all. False on plain http. */
export function pushSupported(): boolean {
  return typeof window !== 'undefined' && 'serviceWorker' in navigator && typeof Notification !== 'undefined';
}

/** Whether this page runs as an installed Home Screen app. On iOS, push
 *  only reaches an installed app — a Safari tab never has the Push API —
 *  so the menu names that when it is the reason. */
export function isStandalone(): boolean {
  return typeof window !== 'undefined' && window.matchMedia('(display-mode: standalone)').matches;
}

/** The permission the user has granted, or `null` when there is nothing to
 *  ask for: the API is absent, or the page is not a secure context. */
export function permission(): NotificationPermission | null {
  if (!pushSupported()) return null;
  return Notification.permission;
}

/** Whether the server can send pushes: it holds a key pair. A server whose
 *  data dir could not be written reports none, and the page cannot
 *  subscribe to it. */
export function serverSupportsPush(publicKey: string | null): boolean {
  return publicKey !== null;
}

/** Everything the label depends on, gathered by the caller. A pure input
 *  so every label is reachable from a test without a browser. */
export interface LabelState {
  /** The browser exposes the Push API. */
  supported: boolean;
  /** The server's push public key, or null when it holds none. */
  serverKey: string | null;
  /** The user's granted permission, or null when there is nothing to ask. */
  permission: NotificationPermission | null;
  /** The page runs as an installed Home Screen app. */
  standalone: boolean;
  /** The browser is iOS, where only an installed app can push. */
  ios: boolean;
}

/** Whether the browser is iOS — where only an installed app can push. */
export function isIos(): boolean {
  if (typeof navigator === 'undefined') return false;
  return /iPad|iPhone|iPod/.test(navigator.userAgent);
}

/** The ☰ menu's label for the Notifications item. Each state reads as a
 *  sentence so the user knows what clicking will do, or why it will do
 *  nothing. The order is the order a user meets them: an http page first,
 *  then a server that cannot push, then the permission itself. */
export function permissionLabel(state: LabelState): string {
  if (!state.supported) return 'Notifications: unavailable on http';
  if (!state.serverKey) return 'Notifications: unavailable on this server';
  if (state.permission === 'granted') return 'Notifications: on';
  if (state.permission === 'denied') return 'Notifications: blocked by the browser';
  if (state.permission === 'default' && !state.standalone && state.ios) {
    return 'Notifications: install the app to enable';
  }
  return 'Enable notifications';
}
