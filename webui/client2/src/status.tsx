// The session's status row: what kind of conversation is on screen, and
// whether a turn is running in it.
//
// The page's chat pane has no title of its own beyond the bar above it, and
// for a subagent or a bot that bar says nothing about what the session is —
// a conversation the user did not type into, opened for them. So the row
// names the kind at its right edge, in the kind's colour, whether the
// session is idle or busy. The terminal's streaming indicator draws the
// same label in the same place for the same reason.
//
// While a turn runs the row also carries the busy text at its left edge:
// "Working..." in the tool loop, "Streaming..." while tokens arrive. An
// ordinary idle session has nothing to say and the row is not drawn at all,
// so the layout above the composer is exactly what it was.

import { Show } from 'solid-js';
import type { JSX } from '@solidjs/web';
import type { ClientStore } from './store';
import { isBusy } from './store';

export interface SessionStatusProps {
  store: ClientStore;
}

/** The kinds a session is named by. A user or fork session is an ordinary
 *  conversation and has no label: the reader can already tell. */
type Kind = 'subagent' | 'bot';

/** The viewed session's kind, read off its row in whichever list holds it. */
function viewedKind(store: ClientStore): Kind | null {
  const s = store.state;
  if (s.sessionId === null) return null;
  const row = [...s.sessions, ...s.library].find((r) => r.session_id === s.sessionId);
  if (row === undefined) return null;
  return row.origin === 'subagent' || row.origin === 'bot' ? row.origin : null;
}

export function SessionStatus(props: SessionStatusProps): JSX.Element {
  const kind = (): Kind | null => viewedKind(props.store);
  const busy = (): boolean => isBusy(props.store.state.phase);
  // The busy text names the phase the terminal names: the tool loop is
  // "work" the user watches, and anything else that is not idle is a stream.
  const busyText = (): string =>
    props.store.state.phase === 'Working' ? 'Working...' : 'Streaming...';

  // The row is always in the document — at a fixed height, empty when idle —
  // and only its CONTENTS come and go. A row that mounted and unmounted
  // would resize the transcript's grid track under a reader, dropping them
  // out of the follow band the moment a turn started.
  return (
    <div class="session-status" data-testid="session-status">
      <Show when={busy()}>
        <span class="spinner spinner-small" aria-hidden="true" />
        <span class="session-status-text" data-testid="status-text">
          {busyText()}
        </span>
      </Show>
      {/* The callback form of `Show` hands back the narrowed `when` value —
          a boolean here, not the kind — so the kind is read from its own
          accessor, which the guard has just proved non-null. */}
      <Show when={kind() !== null}>
        <span class="session-status-kind" data-kind={kind() ?? undefined} data-testid="status-kind">
          [{kind()}]
        </span>
      </Show>
    </div>
  );
}
