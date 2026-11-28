// An opt-in trace of the token path inside the page: one console line at
// each hop a streamed chunk passes through, so a stall can be placed
// between two of them instead of guessed at.
//
// The server logs its own side of the same path (the token arriving, the
// session actor appending it, the outbound flush pushing it). This is the
// browser's half, and it exists because the server's log can only prove
// a frame was sent: whether the page received it, kept it, and drew it
// is invisible from the server.
//
// The hops, in order:
//   recv   the socket decoded a frame and handed it to the store
//   drop   the store refused a delta and returned without writing, with why
//   apply  the store wrote a delta into the transcript
//   render a view re-rendered an entry, with how much text it drew
//
// Off unless the page is opened with `?debug=stream`, for the same reason
// the unread trace is: the path is hot (several frames a second during a
// turn) and normal operation should log nothing.

/**
 * Whether the trace is on. Read once, at load, from the page's URL; a page
 * opened without the flag does no work per frame beyond this one check.
 *
 * There is no window outside a browser (the unit tests run in Node, and
 * this module is imported wherever the store is), and no window means no
 * trace: the read is guarded rather than assumed, so importing the module
 * is always safe.
 */
const STREAM_DEBUG: boolean = (() => {
  try {
    return streamDebugEnabled(window.location.search);
  } catch {
    return false;
  }
})();

/** Whether `search` turns the stream trace on. Split out so a test can ask
 *  without a window. */
export function streamDebugEnabled(search: string): boolean {
  try {
    return new URLSearchParams(search).get('debug') === 'stream';
  } catch {
    return false;
  }
}

/**
 * One line on the console for a hop on the stream path. Does nothing
 * unless the trace is on.
 */
export function debugStream(message: string, fields: Record<string, unknown>): void {
  if (!STREAM_DEBUG) return;
  console.debug(`[stream] ${message}`, fields);
}
