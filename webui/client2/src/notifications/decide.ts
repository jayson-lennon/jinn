// What the service worker decides, with the browser taken out.
//
// The worker exists to answer three questions, and none of them needs a
// browser to answer: a push arrived — show it? a page reported a settle —
// show it? a banner was tapped — where does the page go? Keeping the
// answers here, over two small surfaces a test can fake, is what lets the
// decisions be unit-tested at all: headless Chromium never grants the
// notification permission, so `sw.ts` itself raises no banner under test
// and its behaviour cannot be asserted in a browser. The answers can.
//
// `sw.ts` wires these to the real events; it decides nothing.

/** What a push carries, decrypted: the session it names and its title. */
export interface PushPayload {
  session_id: string;
  title: string;
  body: string;
}

/** What a page posts to the worker over its `message` channel: a settle it
 *  saw arrive over the socket, for the case where the page is alive but
 *  hidden and no push was needed. */
export interface SettleMessage {
  type: 'settle';
  session_id: string;
  title: string;
  body: string;
}

/** What the worker posts to a page when a banner is tapped: the session the
 *  tap named. The page forwards it to jinn as a tap on the rail row. */
export interface NotificationOpenedMessage {
  type: 'notification_opened';
  session_id: string;
}

/** One open page of the origin, as far as these decisions care. */
export interface Client {
  visibilityState: DocumentVisibilityState;
}

/** The browser, as the worker's decisions use it. Shaped by what the three
 *  decisions need rather than by the worker globals, so `sw.ts` adapts the
 *  real browser to it and the decisions never name a worker type. */
export interface Surface {
  /** Raises a banner. Must be called before the event handler returns. */
  show(title: string, options: { body: string; tag: string; data: { session_id: string } }): Promise<void>;
  /** Every open page of the origin, controlled by this worker or not. */
  clients(): Promise<readonly Client[]>;
  /** Hands a message to one page and brings it to the front. */
  deliver(client: Client, message: NotificationOpenedMessage | null): Promise<void>;
  /** Opens a page when no page is open, at the URL a tap asks for. */
  open(url: string): Promise<void>;
}

/** The notification's tag, so two pushes for one session replace each other
 *  rather than stacking. */
export function tagOf(sessionId: string): string {
  return `session:${sessionId}`;
}

/** A push always shows. There is no visibility check on this path: a push
 *  that shows nothing is what makes Safari revoke the permission, and a
 *  push reaches a page only when no page is looking anyway. A push with no
 *  data is one this build did not send, so a plain banner stands in for
 *  it rather than a silent one. */
export async function onPush(surface: Surface, data: PushPayload | null): Promise<void> {
  // A data-less push names no session, so there is no title to put after the
  // line; it stays the bare line rather than inventing one.
  const payload = data ?? { session_id: '', title: 'jinn', body: 'Turn finished:' };
  await show(surface, payload);
}

/** A settle is shown only when no page of the origin is visible. The page
 *  that posted it was hidden, or it would not have posted; but another
 *  page may be visible, and only the worker can see every page. */
export async function onSettle(surface: Surface, message: SettleMessage): Promise<void> {
  const clients = await surface.clients();
  if (clients.some((client) => client.visibilityState === 'visible')) return;
  await show(surface, message);
}

/** A tap closes its banner and takes a page to the session it named. With
 *  a page open, that page is told the session and brought forward — it
 *  forwards the session to jinn, which activates it. With none, a page is
 *  opened with the session in its hash, which the fresh page reads once. */
export async function onTap(
  surface: Surface,
  sessionId: string | undefined,
  close: () => void,
): Promise<void> {
  close();
  const clients = await surface.clients();
  const client = clients[0];
  if (client) {
    await surface.deliver(client, sessionId ? { type: 'notification_opened', session_id: sessionId } : null);
    return;
  }
  await surface.open(sessionId ? `/#notification=${encodeURIComponent(sessionId)}` : '/');
}

async function show(surface: Surface, payload: PushPayload): Promise<void> {
  await surface.show(payload.title, {
    body: payload.body,
    tag: tagOf(payload.session_id),
    data: { session_id: payload.session_id },
  });
}
