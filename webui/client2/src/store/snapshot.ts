// The snapshot: the one frame that touches every topic.
//
// A snapshot is always applied. It replaces which session the page views
// and that session's transcript; the draft and pending images are not the
// transcript's and are left as they are, so following a switch made
// elsewhere never discards what the user is typing.
//
// The whole frame lands in ONE setter. Solid batches writes, and a snapshot
// split across per-topic setters would let an effect observe a half-applied
// one — the new session's title over the old session's transcript. The
// pieces that need another topic's module state (the unread mark's
// persistence, the fork wait's source) are called from inside the setter.

import type { ClientStore } from './store';
import { indexSnapshot } from '../transcript/transcript_entries';
import { markSeenOnSnapshot, saveLastSeen } from '../rail/last_seen';
import { resolveLoadingAfterSnapshot } from '../overlays/session_load';
import { resetSessionPickerRows } from '../pickers/picker_rows';
import { resetDash } from '../dash/dash';
import type { ServerMsg } from '../protocol/protocol';
import { reloadIfStale } from '../protocol/protocol_version';

/** `snapshot`: the page's whole state for one session, from one frame. */
export function applySnapshot(store: ClientStore, msg: Extract<ServerMsg, { type: 'snapshot' }>): void {
  // First, and before any write: a snapshot from a server speaking another
  // protocol version belongs to a bundle this page is not. The page reloads
  // and this frame is not applied — a half-rendered snapshot is worse than
  // a reload, and the reload lands on the hashed URL of the new bundle.
  if (reloadIfStale(msg.protocol)) return;
  // Read before the setter: whether this snapshot is the one the page
  // is waiting on, and so must end the loading overlay.
  const loadingId = store.state.loadingSessionId;
  store.set((s) => {
    s.sessionId = msg.session_id;
    s.phase = msg.phase.phase;
    s.queueLen = msg.phase.queue_len;
    s.modelLabel = msg.model_label;
    s.title = msg.title;
    s.persona = msg.persona;
    // The figure is the snapshot's own, so the bar is whole on arrival: the
    // tick pushes it only when it changes, and an idle session never does.
    s.contextLabel = msg.context_label;
    s.revision = msg.revision;
    s.entries = msg.entries;
    s.entriesBefore = msg.entries_before;
    s.loadingMore = false;
    s.sessions = msg.sessions;
    s.library = msg.library;
    s.theme = msg.theme;
    s.synced = true;
    s.hydrating = msg.hydrating;
    // The viewed session's templates. The draft stays — it is the
    // user's — but any open `#` completion closes: the list it was
    // filtering belonged to the session the page just left.
    s.promptTemplates = msg.prompt_templates;
    s.promptCompletion = null;
    s.promptHighlight = null;
    // Not the draft or the chips: they belong to the user, not to the
    // session, and a switch must not take unsent text with it.
    // A different session's picker rows are not the ones the page was
    // looking at; an open modal re-asks for its own rows on open.
    resetSessionPickerRows(s);
    // The dash describes one session: a switch closes it and drops the
    // other session's facts and any jump still in flight.
    resetDash(s);
    // The page is now viewing this session: whatever settled before
    // this moment is seen. One arm covers a click on the page and a
    // switch made elsewhere that the page follows — both land here.
    // The time recorded is the server's own settle time for the
    // session, never the browser's clock: the two run on different
    // machines and drift, and a browser that is behind would record
    // a "seen" older than the settle and never clear the mark.
    if (msg.session_id !== null) {
      markSeenOnSnapshot(msg.session_id, msg.sessions, msg.library, s.lastSeen);
    }
    resolveLoadingAfterSnapshot(loadingId, msg.session_id, s);
  });
  // Indexed from the frame's own array, not the store's. Reading the
  // store's array here left the index naming the session just left,
  // so an upsert for the new session was appended instead of
  // replacing its copy from the snapshot, and the entry showed twice.
  indexSnapshot(msg.entries);
  if (msg.session_id !== null) saveLastSeen(store.state.lastSeen, msg.sessions, msg.library);
}
