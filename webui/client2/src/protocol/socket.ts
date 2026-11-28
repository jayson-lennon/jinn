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
import { debugStream } from './debug_stream';

/**
 * What a received frame is, for the stream trace. Every frame is named by
 * its type; the two that carry transcript text also carry enough to follow
 * one entry across frames: a delta names its entry, its field, and how
 * much text it brings, and an upsert names its entry and its kind.
 */
function frameSummary(msg: ServerMsg): Record<string, unknown> {
  switch (msg.type) {
    case 'entry_delta':
      return { type: msg.type, entry: msg.entry_id, field: msg.field, at: msg.offset_bytes, len: msg.chunk.length };
    case 'entry_upsert':
      return { type: msg.type, index: msg.index, entry: msg.entry.entry.id };
    default:
      return { type: msg.type };
  }
}

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

/** How long to wait for `hello` to be answered before assuming a dead socket.
 *
 * The answer is the first frame the server sends for it — a snapshot, an
 * upsert, or the `current` frame when the page missed nothing. A heartbeat
 * is not an answer: a server that pings but never answers must still be
 * given up on, so the wait is not ended by one. */
const HANDSHAKE_TIMEOUT_MS = 10_000;

/** Whether a frame is an answer to `hello`: the first frame the server
 * sends for it, which only a hello answer or a session-follow snapshot
 * produces. The `current` frame counts, so a page that missed nothing is
 * answered rather than left to time out on an empty reply. */
function answersHello(msg: ServerMsg): boolean {
  return (
    msg.type === 'snapshot' ||
    msg.type === 'entry_upsert' ||
    msg.type === 'entries_page' ||
    msg.type === 'current'
  );
}

/**
 * How long a connection may stay silent before the page decides it is dead.
 *
 * A browser cannot see a connection drop on its own: a socket the network
 * lost stays `OPEN` until a send fails, and an idle page never sends. The
 * server beats every few seconds, so a healthy connection is never this
 * quiet — and one that is gets closed here, which the close handler treats
 * like any other drop and reconnects.
 */
const LIVENESS_TIMEOUT_MS = 30_000;

export interface Socket {
  /** Sends one command. A no-op while disconnected. */
  readonly send: (msg: ClientMsg) => void;
  /** Closes the socket and stops reconnecting. */
  readonly close: () => void;
}

/** Builds the socket URL for the page it is served from. */
export function socketUrl(): string {
  const scheme = location.protocol === 'https:' ? 'wss' : 'ws';
  return `${scheme}://${location.host}/ws`;
}

export function openSocket(handlers: SocketHandlers): Socket {
  let socket: WebSocket | null = null;
  let attempt = 0;
  /** Whether this connection has been answered yet. The backoff resets only
   *  when it is — never on open — so a socket that opens and stalls on its
   *  first frame backs off instead of retrying at the shortest delay
   *  forever. */
  let answered = false;
  let closedByPage = false;
  let retryTimer: number | null = null;
  let handshakeTimer: number | null = null;
  let livenessTimer: number | null = null;

  const armLiveness = (): void => {
    if (livenessTimer !== null) window.clearTimeout(livenessTimer);
    livenessTimer = window.setTimeout(() => {
      // Silent past the deadline: close, and let the close handler reconnect.
      livenessTimer = null;
      if (socket !== null && socket.readyState === WebSocket.OPEN) socket.close();
    }, LIVENESS_TIMEOUT_MS);
  };

  const connect = (): void => {
    handlers.onState('connecting');
    const ws = new WebSocket(socketUrl());
    socket = ws;
    answered = false;

    ws.onopen = () => {
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
      if (answersHello(msg)) {
        // Answered: end the handshake wait, and start the backoff over —
        // the next drop is a fresh one, not another failed attempt.
        if (handshakeTimer !== null) {
          window.clearTimeout(handshakeTimer);
          handshakeTimer = null;
        }
        if (!answered) {
          answered = true;
          attempt = 0;
        }
      }
      armLiveness();
      debugStream('recv', frameSummary(msg));
      handlers.onFrame(msg);
    };

    ws.onclose = () => {
      if (socket === ws) socket = null;
      if (livenessTimer !== null) {
        window.clearTimeout(livenessTimer);
        livenessTimer = null;
      }
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

  // The browser announcing a network change is the other way a connection
  // dies. `offline` is noticed at once — no frame needs to time out — and
  // `online` brings the retry forward instead of waiting out the backoff.
  const onOffline = (): void => {
    socket?.close();
  };
  const onOnline = (): void => {
    if (socket === null && retryTimer !== null) {
      window.clearTimeout(retryTimer);
      retryTimer = null;
      attempt = 0;
      connect();
    }
  };
  window.addEventListener('offline', onOffline);
  window.addEventListener('online', onOnline);

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
      if (livenessTimer !== null) window.clearTimeout(livenessTimer);
      window.removeEventListener('offline', onOffline);
      window.removeEventListener('online', onOnline);
      socket?.close();
      socket = null;
    },
  };
}
