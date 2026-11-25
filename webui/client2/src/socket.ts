// The socket: the browser's one connection to the server.
//
// Every byte that crosses it is a `ClientMsg` going up or a `ServerMsg`
// coming down. `send` is the single way a command leaves the page, and
// `onFrame` is the single place a fact arrives — so there is exactly one
// of each, mirroring the server's single `match`.
//
// On every (re)connect the page says `hello` with the newest entry id and
// revision it holds in memory. The server answers incrementally when it
// can, or with a whole snapshot when it cannot — the page never has to
// know which happened, because both are applied the same way.

import type { ClientMsg, ServerMsg } from './protocol';

export type ConnectionState = 'connecting' | 'open' | 'closed';

export interface SocketHandlers {
  /** Applies one inbound frame. */
  readonly onFrame: (msg: ServerMsg) => void;
  /** Observes the connection state, for the reconnect overlay. */
  readonly onState: (state: ConnectionState) => void;
  /** The cursor to resume from, read fresh on every connect. */
  readonly cursor: () => { last_entry_id: string | null; revision: number | null };
}

/** Reconnect delays, tried in order; the last one repeats. */
const BACKOFF_MS = [250, 500, 1000, 2000, 4000, 8000];

/** How long to wait for `hello` to be answered before assuming a dead socket. */
const HANDSHAKE_TIMEOUT_MS = 10_000;

export interface Socket {
  /** Sends one command. A no-op while disconnected. */
  readonly send: (msg: ClientMsg) => void;
  /** Closes the socket and stops reconnecting. */
  readonly close: () => void;
}

/** Builds the socket URL for the page it is served from. */
export function socketUrl(): string {
  const scheme = location.protocol === 'https:' ? 'wss' : 'ws';
  return `${scheme}://${location.host}/web2/ws`;
}

export function openSocket(handlers: SocketHandlers): Socket {
  let socket: WebSocket | null = null;
  let attempt = 0;
  let closedByPage = false;
  let retryTimer: number | null = null;
  let handshakeTimer: number | null = null;

  const connect = (): void => {
    handlers.onState('connecting');
    const ws = new WebSocket(socketUrl());
    socket = ws;

    ws.onopen = () => {
      attempt = 0;
      handlers.onState('open');
      // The first frame on every connection, before anything else.
      const cursor = handlers.cursor();
      ws.send(JSON.stringify({ type: 'hello', ...cursor } satisfies ClientMsg));
      handshakeTimer = window.setTimeout(() => {
        // No answer to `hello`: treat the socket as dead and try again.
        if (ws.readyState === WebSocket.OPEN) ws.close();
      }, HANDSHAKE_TIMEOUT_MS);
    };

    ws.onmessage = (event: MessageEvent<string>) => {
      let msg: ServerMsg;
      try {
        msg = JSON.parse(event.data) as ServerMsg;
      } catch {
        // A frame that is not JSON is not something the page can act on.
        return;
      }
      if (handshakeTimer !== null) {
        window.clearTimeout(handshakeTimer);
        handshakeTimer = null;
      }
      handlers.onFrame(msg);
    };

    ws.onclose = () => {
      if (socket === ws) socket = null;
      handlers.onState('closed');
      if (closedByPage) return;
      scheduleRetry();
    };

    // `onerror` always precedes `onclose` for a failed connection; the
    // close handler owns the retry so it happens exactly once.
    ws.onerror = () => {};
  };

  const scheduleRetry = (): void => {
    const delay = BACKOFF_MS[Math.min(attempt, BACKOFF_MS.length - 1)] ?? 4000;
    attempt += 1;
    retryTimer = window.setTimeout(() => {
      retryTimer = null;
      connect();
    }, delay);
  };

  connect();

  return {
    send(msg: ClientMsg): void {
      if (socket !== null && socket.readyState === WebSocket.OPEN) {
        socket.send(JSON.stringify(msg));
      }
    },
    close(): void {
      closedByPage = true;
      if (retryTimer !== null) window.clearTimeout(retryTimer);
      if (handshakeTimer !== null) window.clearTimeout(handshakeTimer);
      socket?.close();
      socket = null;
    },
  };
}
