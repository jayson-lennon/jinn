//! Fetch helpers for the web UI's action endpoints.
//!
//! Every helper returns the response body as text and lets non-2xx
//! statuses THROW — a failed action must surface, never be swallowed
//! into a silent no-op (the shipped failure mode this module exists
//! to prevent). Callers decide what a failure looks like; the
//! default is an unhandled rejection the console shows.

import * as wire from "./wire.js";

/** The headers every action post carries. */
const FORM_HEADERS: HeadersInit = {
  "content-type": "application/x-www-form-urlencoded",
};

/**
 * Posts a urlencoded body, asserting a 2xx response.
 *
 * @param path - The action endpoint.
 * @param body - The urlencoded form fields.
 * @returns The response body text.
 * @throws Error on network failure or a non-2xx status.
 */
async function postForm(path: string, body: string): Promise<string> {
  const response = await fetch(path, {
    method: "POST",
    headers: FORM_HEADERS,
    body,
  });
  if (!response.ok) {
    throw new Error(`${path} failed: HTTP ${response.status}`);
  }
  return response.text();
}

/** Percent-encodes one form field value. */
function encodeField(name: string, value: string): string {
  return `${encodeURIComponent(name)}=${encodeURIComponent(value)}`;
}

/**
 * Pulls the sessions-list fragment, narrowed by an optional filter.
 * The filter text is percent-encoded here and matched server-side —
 * the client never scores or compares titles itself.
 *
 * @param filter - The title fragment to filter by, or null for the
 * unfiltered list.
 * @returns The server-rendered sessions markup.
 * @throws Error on network failure or a non-2xx status.
 */
export function pullSessions(filter: string | null): Promise<string> {
  const query = filter === null || filter.length === 0
    ? ""
    : `?q=${encodeURIComponent(filter)}`;
  return fetch(`${wire.SESSIONS_ROUTE}${query}`).then((response) => {
    if (!response.ok) {
      throw new Error(`${wire.SESSIONS_ROUTE} failed: HTTP ${response.status}`);
    }
    return response.text();
  });
}

/**
 * Activates a session: POSTs to its activate endpoint, which answers
 * with the WHOLE page for that session (the server's `Page` type —
 * the response-shape contract that once shipped wrong).
 *
 * @param sessionId - The session to activate.
 * @returns The full page HTML.
 * @throws Error on network failure or a non-2xx status.
 */
export function activateSession(sessionId: string): Promise<string> {
  return postForm(
    `${wire.SESSION_PAGE_PATH}${encodeURIComponent(sessionId)}/activate`,
    "",
  );
}

/**
 * Renames a session. The response is the re-rendered sessions list
 * (the same markup the sessions GET pull serves); the SSE `sessions`
 * event corrects the OTHER pages viewing the session within one
 * batch window.
 *
 * @param sessionId - The session to rename.
 * @param title - The new title, applied trimmed server-side (an
 * empty-after-trim value is a 400).
 * @returns The replacement sessions-list fragment.
 * @throws Error on network failure or a non-2xx status.
 */
export function renameSession(sessionId: string, title: string): Promise<string> {
  return postForm(
    `${wire.SESSION_PAGE_PATH}${encodeURIComponent(sessionId)}/rename`,
    encodeField("title", title),
  );
}

/**
 * Sends a chat message into a session. The response replaces the
 * composer controls (busy state), and the SSE `controls` event
 * corrects any race within one batch window.
 *
 * @param sessionId - The session receiving the text.
 * @param text - The message, delivered verbatim.
 * @returns The replacement controls fragment.
 * @throws Error on network failure, a non-2xx status, or the
 * server's refusal (an empty text is a 400).
 */
export function sendMessage(sessionId: string, text: string): Promise<string> {
  return postForm(
    wire.SEND_PATH,
    [encodeField("session_id", sessionId), encodeField("text", text)].join("&"),
  );
}

/**
 * Cancels a session's live turn.
 *
 * @param sessionId - The session whose turn to cancel.
 * @returns The replacement controls fragment (still-busy until the
 * SSE `controls` event settles it).
 * @throws Error on network failure or a non-2xx status.
 */
export function cancelTurn(sessionId: string): Promise<string> {
  return postForm(wire.CANCEL_PATH, encodeField("session_id", sessionId));
}

/**
 * Switches a session's model.
 *
 * @param sessionId - The session to switch.
 * @param modelId - The model id, as a `Single` selection.
 * @returns The replacement model-picker fragment.
 * @throws Error on network failure or a non-2xx status.
 */
export function switchModel(
  sessionId: string,
  modelId: string,
): Promise<string> {
  return postForm(
    wire.MODEL_PATH,
    [encodeField("session_id", sessionId), encodeField("provider_id", modelId)].join(
      "&",
    ),
  );
}
