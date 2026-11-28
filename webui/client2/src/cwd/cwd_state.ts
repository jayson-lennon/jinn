// The change-directory modal's state: the draft path, its verdict, and
// whether the modal is open.
//
// The verdict is the server's own — resolved per edit through the terminal
// cwd popup's resolver — and this file only stores it. Two rules shape
// everything here. A reply is applied only when its text is the draft's
// current text: two checks can be in flight under a fast typer, and an
// older answer must not overwrite a newer one. And an edit marks the held
// verdict stale without clearing it: the footer keeps showing it (so it
// does not blink on every keystroke) while Accept ignores it until the
// current text's verdict lands. The server re-resolves on Accept, so the
// disabled button is the only guard the page needs.
//
// A cwd change that lands from elsewhere — the terminal, another page —
// moves the dash's row and nothing else. An open modal's draft is the
// user's half-typed path and is left exactly as typed.

import type { ClientStore } from '../store/store';
import { belongsToViewedSession } from '../store/store';
import type { CwdVerdict, ServerMsg } from '../protocol/protocol';
import { closeDash } from '../dash/dash';

type Send = (msg: import('../protocol/protocol').ClientMsg) => void;

/** How long after the last edit the check is asked. Long enough that a
 *  fast typer pays one round trip per pause rather than per keystroke. */
export const CWD_CHECK_DEBOUNCE_MS = 100;

/** What the modal holds about its draft: the last verdict to land, and
 *  whether the text has moved on since it did. */
export interface CwdVerdictState {
  /** The verdict the footer shows — the last one to land. Kept across an
   *  edit so the footer holds steady while the next one is in flight. */
  readonly verdict: CwdVerdict;
  /** True once the text has changed since this verdict landed. The footer
   *  keeps showing the verdict; Accept is disabled until a fresh one lands. */
  readonly stale: boolean;
}

/** What an open modal holds before any verdict has landed: nothing typed,
 *  nothing checked, and nothing to accept. */
const NO_VERDICT: CwdVerdictState = { verdict: { kind: 'empty' }, stale: true };

/** `cwd_check`: the answer to one check. A reply for text the user has
 *  typed past is dropped — it describes a path the field no longer holds.
 *  A reply for a modal that has closed is dropped for the same reason. */
export function applyCwdCheck(store: ClientStore, msg: Extract<ServerMsg, { type: 'cwd_check' }>): void {
  if (!store.state.cwdOpen) return;
  if (msg.text !== store.state.cwdDraft) return;
  store.set((s) => {
    s.cwdVerdict = { verdict: msg.verdict, stale: false };
  });
}

/** `session_cwd`: the session's cwd changed — here or in the terminal. The
 *  dash's row follows it; an open modal's draft is the user's and is left
 *  alone. A frame for a session the page has left is not the row it shows. */
export function applySessionCwd(store: ClientStore, msg: Extract<ServerMsg, { type: 'session_cwd' }>): void {
  if (!belongsToViewedSession(store, msg.session_id)) return;
  store.set((s) => {
    // No statistics have landed yet, so there is no row to move: the next
    // `session_stats` frame carries the new cwd on its own.
    if (s.dashStats !== null) s.dashStats = { ...s.dashStats, cwd: msg.cwd };
  });
}

/**
 * Opens the modal on the dash's directory row. The dash closes first — one
 * overlay at a time — and the draft is seeded with the current cwd, the way
 * the terminal popup seeds it. The first check is asked at once, so the
 * footer is green on the first frame and Accept works untouched.
 */
export function openCwdModal(store: ClientStore, send: Send, cwd: string): void {
  closeDash(store);
  store.set((s) => {
    s.cwdOpen = true;
    s.cwdDraft = cwd;
    s.cwdVerdict = NO_VERDICT;
  });
  send({ type: 'check_cwd', text: cwd });
}

/** An edit: the text moves on and the held verdict goes stale. The verdict
 *  itself stays — the footer keeps showing it — and the caller asks the
 *  server for the new one after its debounce. */
export function editCwdDraft(store: ClientStore, value: string): void {
  store.set((s) => {
    s.cwdDraft = value;
    s.cwdVerdict = { ...s.cwdVerdict, stale: true };
  });
}

/** Closes the modal. The draft and verdict are kept until the next open
 *  reseeds them; nothing reads them while closed. */
export function closeCwdModal(store: ClientStore): void {
  store.set((s) => {
    s.cwdOpen = false;
  });
}

/**
 * Resets everything the modal holds. Called from the snapshot's setter, so
 * a session switch closes an open modal and drops its state: the draft was
 * a path for the session the page just left.
 */
export function resetCwd(s: {
  cwdOpen: boolean;
  cwdDraft: string;
  cwdVerdict: CwdVerdictState;
}): void {
  s.cwdOpen = false;
  s.cwdDraft = '';
  s.cwdVerdict = NO_VERDICT;
}
