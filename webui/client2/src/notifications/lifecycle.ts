// The page's side of the service worker's life: registering it, keeping the
// server's subscription row current, and forwarding a banner tap to jinn.
//
// Called once from `App.tsx` when the push key lands and permission is
// already granted, and again the moment the user grants it from the menu.
// Nothing here runs on a page without the Push API — the support checks in
// `support.ts` are the gate, and every function below is reached only
// after `pushSupported()` said yes.

import type { Send } from '../pickers/model_picker';
import { isStandalone, pushSupported } from './support';
import { requestAndSubscribe, subscribe, browserSurface, type PushSurface } from './subscribe';

/** Starts the worker and subscribes, when this page can. Called on mount
 *  and whenever the permission changes. Resolves to what happened, so a
 *  caller can name it. */
export async function start(
  publicKey: string | null,
  send: Send,
): Promise<'sent' | 'unsupported' | 'no-key' | 'no-permission'> {
  if (!pushSupported()) return 'unsupported';
  if (publicKey === null) return 'no-key';
  const result = await subscribe(browserSurface, publicKey, send);
  return result === 'sent' ? 'sent' : 'no-permission';
}

/** The menu item's action: asks permission from inside the click, then
 *  subscribes. The request is the first `await`, so the user gesture is
 *  still live when the browser checks for it. */
export async function enableFromMenu(
  publicKey: string | null,
  send: Send,
): Promise<'sent' | 'unsupported' | 'no-key' | 'denied' | 'no-permission'> {
  if (!pushSupported()) return 'unsupported';
  if (publicKey === null) return 'no-key';
  return requestAndSubscribe(browserSurface, publicKey, () => Notification.requestPermission(), send);
}

/** Listens for the worker's posts. A `notification_opened` is a banner tap:
 *  the page forwards it to jinn as the activation it is, the same message a
 *  click on the rail row sends. Returns an unsubscribe. */
export function listenForTaps(send: Send): () => void {
  if (!pushSupported()) return () => undefined;
  const onMessage = (event: MessageEvent): void => {
    const data = event.data as { type?: string; session_id?: string } | undefined;
    if (data?.type !== 'notification_opened' || !data.session_id) return;
    send({ type: 'notification_opened', session_id: data.session_id });
  };
  navigator.serviceWorker.addEventListener('message', onMessage);
  return () => navigator.serviceWorker.removeEventListener('message', onMessage);
}

/** The `#notification=<id>` a fresh page may have been opened with, when a
 *  banner was tapped and no page was open. Read once, then cleared, so a
 *  reload does not re-activate it. */
export function takeHashActivation(): string | null {
  const hash = window.location.hash;
  const prefix = '#notification=';
  if (!hash.startsWith(prefix)) return null;
  const id = decodeURIComponent(hash.slice(prefix.length));
  history.replaceState(null, '', window.location.pathname + window.location.search);
  return id || null;
}

/** Re-exported for the menu's label: whether this page is an installed app. */
export { isStandalone };

/** The surface in use, for a test that needs to reach it. */
export type { PushSurface };
