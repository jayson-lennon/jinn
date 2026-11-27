// The dash: the session statistics, enabled MCP servers, task list, and
// pinned entries the page shows under the status bar's context figure.
//
// Every row is the server's own. The figures are pre-formatted by the same
// function the terminal status bar calls, the pin labels by the one the
// terminal sidebar calls, so the page renders strings and computes nothing.
// Only the statistics are gated on an open dash: the server pushes them
// while any page has one open and a displayed value changed. The rows ride
// their own events and land whether or not a dash is open, so the next open
// paints at once.

import type { ClientStore } from '../store/store';
import { belongsToViewedSession } from '../store/store';
import type { ClientMsg, ServerMsg } from '../protocol/protocol';

type Send = (msg: ClientMsg) => void;

/** `session_stats`: the dash's figures, from the shared statistics function. */
export function applySessionStats(
  store: ClientStore,
  msg: Extract<ServerMsg, { type: 'session_stats' }>,
): void {
  if (!belongsToViewedSession(store, msg.session_id)) return;
  const stats = {
    cwd: msg.cwd,
    cache_hit_percent: msg.cache_hit_percent,
    tokens_sent: msg.tokens_sent,
    tokens_received: msg.tokens_received,
    context_label: msg.context_label,
    cost: msg.cost,
    turn_count: msg.turn_count,
    working: msg.working,
    model: msg.model,
    tree: msg.tree,
  };
  store.set((s) => {
    s.dashStats = stats;
  });
}

/** `dash_pin_rows`: the session's pins, in the sidebar's order and labels. */
export function applyDashPinRows(
  store: ClientStore,
  msg: Extract<ServerMsg, { type: 'dash_pin_rows' }>,
): void {
  if (!belongsToViewedSession(store, msg.session_id)) return;
  const rows = msg.rows.map((row) => ({ entry_id: row.entry_id, text: row.text }));
  store.set((s) => {
    s.dashPins = rows;
  });
}

/** `dash_tasks`: every phase with the tasks beneath it. */
export function applyDashTasks(
  store: ClientStore,
  msg: Extract<ServerMsg, { type: 'dash_tasks' }>,
): void {
  if (!belongsToViewedSession(store, msg.session_id)) return;
  const tasks = {
    phases: msg.phases.map((phase) => ({
      description: phase.description,
      tasks: phase.tasks.map((task) => ({ description: task.description, status: task.status })),
      is_active: phase.is_active,
      has_pending_work: phase.has_pending_work,
    })),
    done: msg.done,
    total: msg.total,
  };
  store.set((s) => {
    s.dashTasks = tasks;
  });
}

/** `dash_mcp_rows`: the enabled servers with their live status. */
export function applyDashMcpRows(
  store: ClientStore,
  msg: Extract<ServerMsg, { type: 'dash_mcp_rows' }>,
): void {
  if (!belongsToViewedSession(store, msg.session_id)) return;
  const rows = msg.rows.map((row) => ({ name: row.name, badge: row.badge }));
  store.set((s) => {
    s.dashMcp = rows;
  });
}

/**
 * Opens the dash and tells the server, whose tick then pushes the statistics
 * when they change until the dash closes.
 */
export function openDash(store: ClientStore, send: Send): void {
  if (store.state.dashOpen) return;
  store.set((s) => {
    s.dashOpen = true;
  });
  send({ type: 'dash_open' });
}

/**
 * Closes the dash. The server is told by the page's one close path, which
 * watches the open flag fall, so every dismiss — a tap, Escape, a pin, a
 * session switch — reports the same way.
 */
export function closeDash(store: ClientStore): void {
  if (!store.state.dashOpen) return;
  store.set((s) => {
    s.dashOpen = false;
  });
}

/**
 * Asks the transcript to scroll to `entryId` once it is held. The server
 * streams older pages until the entry arrives; the transcript clears the
 * target when it scrolls.
 */
export function jumpToEntry(store: ClientStore, send: Send, entryId: string): void {
  store.set((s) => {
    s.jumpTarget = entryId;
    s.navigating = true;
  });
  send({ type: 'jump_to_entry', entry_id: entryId });
}

/** The transcript reached the entry a pin asked for, so the jump is over. */
export function clearJump(store: ClientStore): void {
  if (store.state.jumpTarget === null) return;
  store.set((s) => {
    s.jumpTarget = null;
    s.navigating = false;
  });
}

/** Clears a pending jump — when it completed, or when it was refused. */
export function clearJumpTarget(store: ClientStore): void {
  if (store.state.jumpTarget === null) return;
  store.set((s) => {
    s.jumpTarget = null;
    s.navigating = false;
  });
}

/**
 * Stops the jump under way. Closes the overlay and tells the server to stop
 * sending the jump's pages; the ones already received stay, and the reader
 * is left exactly where they are. A cancel that reaches the server after
 * its jump finished is simply ignored there.
 */
export function cancelJump(store: ClientStore, send: Send): void {
  store.set((s) => {
    s.jumpTarget = null;
    s.navigating = false;
  });
  send({ type: 'cancel_jump' });
}

/**
 * Resets everything the dash holds. Called from the snapshot's setter, so a
 * session switch closes an open dash and drops another session's facts.
 */
export function resetDash(s: {
  dashOpen: boolean;
  dashStats: unknown;
  dashPins: unknown[];
  dashTasks: unknown;
  dashMcp: unknown[];
  jumpTarget: string | null;
  navigating: boolean;
}): void {
  s.dashOpen = false;
  s.dashStats = null;
  s.dashPins = [];
  s.dashTasks = null;
  s.dashMcp = [];
  s.jumpTarget = null;
  s.navigating = false;
}
