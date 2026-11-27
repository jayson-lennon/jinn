// The picker rows: what every filterable modal lists, for the filter the
// page last asked for.
//
// Each modal's rows live in the store and are the server's own — a frame
// lands them, the page only renders them. Every frame carries the filter it
// answers, so a late answer to text the user has typed past is dropped by
// the filter echo; the session-scoped pickers carry a session too, and a
// frame for a session the page has left is not the view it is holding.

import type { ClientStore } from './store/store';
import type { ViewState } from './store/store';
import type { ServerMsg } from './protocol/protocol';

/** A frame for a session the page has left is not the view it holds. A page
 *  with no session yet accepts anything. */
function matchesViewedSession(store: ClientStore, sessionId: string): boolean {
  return store.state.sessionId === null || sessionId === store.state.sessionId;
}

type RowsMsg<Kinds extends ServerMsg['type']> = Extract<ServerMsg, { type: Kinds }>;

/** `picker_rows`: the stored-session picker. Instance-wide — no session to match. */
export function applyPickerRows(store: ClientStore, msg: RowsMsg<'picker_rows'>): void {
  if (msg.filter !== store.state.pickerFilter) return;
  store.set((s) => {
    s.pickerRows = msg.rows;
  });
}

/** `model_rows`: the model picker. A push is also how the page learns its
 *  refresh finished. */
export function applyModelRows(store: ClientStore, msg: RowsMsg<'model_rows'>): void {
  if (!matchesViewedSession(store, msg.session_id)) return;
  if (msg.filter !== store.state.modelFilter) return;
  store.set((s) => {
    s.modelRows = msg.rows;
    s.refreshing = false;
  });
}

/** `skill_rows`: the skills modal. */
export function applySkillRows(store: ClientStore, msg: RowsMsg<'skill_rows'>): void {
  if (!matchesViewedSession(store, msg.session_id)) return;
  if (msg.filter !== store.state.skillFilter) return;
  store.set((s) => {
    s.skillRows = msg.rows;
  });
}

/** `tool_rows`: the tools modal. */
export function applyToolRows(store: ClientStore, msg: RowsMsg<'tool_rows'>): void {
  if (!matchesViewedSession(store, msg.session_id)) return;
  if (msg.filter !== store.state.toolFilter) return;
  store.set((s) => {
    s.toolRows = msg.rows;
  });
}

/** `mcp_rows`: the MCP servers modal. */
export function applyMcpRows(store: ClientStore, msg: RowsMsg<'mcp_rows'>): void {
  if (!matchesViewedSession(store, msg.session_id)) return;
  if (msg.filter !== store.state.mcpFilter) return;
  store.set((s) => {
    s.mcpRows = msg.rows;
  });
}

/** `persona_rows`: the persona modal. */
export function applyPersonaRows(store: ClientStore, msg: RowsMsg<'persona_rows'>): void {
  if (!matchesViewedSession(store, msg.session_id)) return;
  if (msg.filter !== store.state.personaFilter) return;
  store.set((s) => {
    s.personaRows = msg.rows;
  });
}

/** `applet_rows`: the applets modal. The saved-app catalog is instance-wide,
 *  so there is no session to match; the filter alone says whether this is the
 *  view the page is holding. A push from an open modal re-asks in the same
 *  filter, so it is never dropped by this guard. */
export function applyAppletRows(store: ClientStore, msg: RowsMsg<'applet_rows'>): void {
  if (msg.filter !== store.state.appletFilter) return;
  store.set((s) => {
    s.appletRows = msg.rows;
    s.appletServing = { serving: msg.serving, bind: msg.bind };
  });
}

/** The snapshot's reset of the session-scoped pickers: a different session's
 *  rows are not the ones the page was looking at; an open modal re-asks for
 *  its own rows on open. Called inside the snapshot's setter. */
export function resetSessionPickerRows(s: ViewState): void {
  s.skillRows = [];
  s.skillFilter = '';
  s.toolRows = [];
  s.toolFilter = '';
  s.mcpRows = [];
  s.mcpFilter = '';
  s.personaRows = [];
  s.personaFilter = '';
}
