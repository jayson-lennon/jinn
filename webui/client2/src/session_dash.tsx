// The session dash: a card under the status bar's context figure.
//
// It lists the session's statistics, the enabled MCP servers, the pinned
// entries, and the task list. Every row is the server's own: the figures are
// pre-formatted by the same function the terminal status bar calls, the pin
// labels by the one the terminal sidebar calls, so this component renders
// strings and computes nothing. The one gesture it owns is its own dismiss;
// a tap on the MCP section hands over to the picker, a tap on a pin to the
// transcript, and both close the dash first.

import { For, Show, onCleanup } from 'solid-js';
import type { JSX } from '@solidjs/web';
import type { ClientStore } from './store';
import type { DashPinRow, DashTaskPhase, DashTasks, McpBadge, SessionStats, TaskStatus, TreeStats } from './protocol';
import { closeDash, jumpToEntry } from './dash';

type Send = (msg: import('./protocol').ClientMsg) => void;

/** The two cache-hit bands the terminal status bar colours by. */
const CACHE_HIT_HEALTHY = 95;
const CACHE_HIT_DEGRADED = 90;

function cacheHealth(percent: number): 'healthy' | 'degraded' | 'poor' {
  if (percent >= CACHE_HIT_HEALTHY) return 'healthy';
  if (percent >= CACHE_HIT_DEGRADED) return 'degraded';
  return 'poor';
}

/** The task indicator the terminal sidebar draws, from the status's own word. */
function taskIndicator(status: TaskStatus): string {
  switch (status) {
    case 'Pending':
      return '\u25CB';
    case 'Completed':
      return '\u2713';
    case 'Deferred':
      return '\u25BC';
    case 'Cancelled':
      return '\u2717';
  }
}

/** A phase's colour follows the terminal's three-way rule. */
function phaseTone(phase: DashTaskPhase): 'active' | 'pending' | 'done' {
  if (phase.is_active) return 'active';
  if (phase.has_pending_work) return 'pending';
  return 'done';
}

function StatRow(props: { readonly label: string; readonly value: string; readonly health?: 'healthy' | 'degraded' | 'poor' }): JSX.Element {
  return (
    <div class="dash-row">
      <span class="dash-row-label">{props.label}</span>
      <span class="dash-row-value" data-health={props.health}>
        {props.value}
      </span>
    </div>
  );
}

function CacheRow(props: { readonly percent: number }): JSX.Element {
  return <StatRow label="Cache hit rate" value={`${props.percent}%`} health={cacheHealth(props.percent)} />;
}

function TreeBlock(props: { readonly tree: TreeStats }): JSX.Element {
  return (
    <details class="dash-tree">
      <summary>Tree · {props.tree.session_count} sessions</summary>
      <div class="dash-tree-rows">
        <Show when={props.tree.cache_hit_percent !== null}>
          <CacheRow percent={props.tree.cache_hit_percent ?? 0} />
        </Show>
        <StatRow label="Tokens" value={`↑${props.tree.tokens_sent} ↓${props.tree.tokens_received}`} />
        <StatRow label="Cost" value={props.tree.cost} />
        <StatRow label="Turns" value={String(props.tree.turn_count)} />
        <StatRow label="Working time" value={props.tree.working} />
      </div>
    </details>
  );
}

function Statistics(props: { readonly stats: SessionStats }): JSX.Element {
  return (
    <div class="dash-section" data-testid="dash-stats">
      <h3 class="dash-section-title">Session</h3>
      <StatRow label="Model" value={props.stats.model} />
      <StatRow label="Context" value={props.stats.context_label} />
      <StatRow label="Tokens" value={`↑${props.stats.tokens_sent} ↓${props.stats.tokens_received}`} />
      <StatRow label="Cost" value={props.stats.cost} />
      <StatRow label="Turns" value={String(props.stats.turn_count)} />
      <StatRow label="Working time" value={props.stats.working} />
      <Show when={props.stats.cache_hit_percent !== null}>
        <CacheRow percent={props.stats.cache_hit_percent ?? 0} />
      </Show>
      <Show when={props.stats.tree !== null}>
        <TreeBlock tree={props.stats.tree ?? unreachableTree()} />
      </Show>
      <div class="dash-row">
        <span class="dash-row-label">Directory</span>
        <span class="dash-row-value dash-cwd">{props.stats.cwd}</span>
      </div>
    </div>
  );
}

function unreachableTree(): TreeStats {
  throw new Error('the tree block renders only with a tree');
}

function McpSection(props: { readonly store: ClientStore; readonly onOpen: () => void }): JSX.Element {
  const rows = () => props.store.state.dashMcp;
  const open = (): void => {
    closeDash(props.store);
    props.onOpen();
  };
  return (
    <button
      type="button"
      class="dash-mcp"
      data-testid="dash-mcp"
      title="MCP servers"
      onClick={open}
    >
      <h3 class="dash-section-title">MCP servers</h3>
      <Show
        when={rows().length > 0}
        fallback={<span class="dash-empty">No MCP servers enabled</span>}
      >
        <For each={rows()}>
          {(row) => (
            <div class="dash-mcp-row">
              <span>{row.name}</span>
              <span class="mcp-badge" data-badge={row.badge satisfies McpBadge}>
                {row.badge}
              </span>
            </div>
          )}
        </For>
      </Show>
    </button>
  );
}

function TasksSection(props: { readonly tasks: DashTasks | null }): JSX.Element {
  return (
    <div class="dash-section" data-testid="dash-tasks">
      <h3 class="dash-section-title">
        Tasks
        <Show when={props.tasks !== null && props.tasks.total > 0}>
          <span class="dash-task-count">
            {' '}
            {props.tasks?.done} of {props.tasks?.total} done
          </span>
        </Show>
      </h3>
      <Show
        when={props.tasks !== null && props.tasks.phases.length > 0}
        fallback={<span class="dash-empty">No tasks yet</span>}
      >
        <For each={props.tasks?.phases ?? []}>
          {(phase) => (
            <div class="dash-phase">
              <div class="dash-phase-title" data-phase={phaseTone(phase)}>
                {phase.description}
              </div>
              <For each={phase.tasks}>
                {(task) => (
                  <div class="dash-task" data-status={task.status satisfies TaskStatus}>
                    <span class="dash-task-indicator">{taskIndicator(task.status)}</span>
                    <span class="dash-task-description">{task.description}</span>
                  </div>
                )}
              </For>
            </div>
          )}
        </For>
      </Show>
    </div>
  );
}

function PinsSection(props: { readonly store: ClientStore; readonly send: Send }): JSX.Element {
  const rows = (): DashPinRow[] => props.store.state.dashPins;
  const jump = (entryId: string): void => {
    closeDash(props.store);
    jumpToEntry(props.store, props.send, entryId);
  };
  return (
    <div class="dash-section" data-testid="dash-pins">
      <h3 class="dash-section-title">Pinned</h3>
      <Show when={rows().length > 0} fallback={<span class="dash-empty">No pinned entries</span>}>
        <For each={rows()}>
          {(row) => (
            <button
              type="button"
              class="dash-pin"
              data-testid="dash-pin"
              data-entry-id={row.entry_id}
              title={row.text}
              onClick={() => jump(row.entry_id)}
            >
              {row.text}
            </button>
          )}
        </For>
      </Show>
    </div>
  );
}

/**
 * The dash. Escape and a tap on the scrim close it; every dismiss goes
 * through `closeDash`, so the page's one close path reports it to the server.
 */
export function Dash(props: {
  readonly store: ClientStore;
  readonly send: Send;
  readonly onOpenMcp: () => void;
}): JSX.Element {
  const close = (): void => closeDash(props.store);

  const onKeydown = (event: KeyboardEvent): void => {
    if (event.key !== 'Escape') return;
    event.stopPropagation();
    close();
  };
  document.addEventListener('keydown', onKeydown);
  onCleanup(() => document.removeEventListener('keydown', onKeydown));

  return (
    <>
      <div
        class="dash-scrim"
        data-testid="dash-scrim"
        onClick={(e) => {
          if (e.target === e.currentTarget) close();
        }}
      />
      <div class="dash" role="dialog" aria-label="Session dash" data-testid="dash">
        <Statistics stats={props.store.state.dashStats ?? emptyStats()} />
        <McpSection store={props.store} onOpen={props.onOpenMcp} />
        <PinsSection store={props.store} send={props.send} />
        <TasksSection tasks={props.store.state.dashTasks} />
      </div>
    </>
  );
}

/**
 * What the statistics section shows before the first frame lands: the dash
 * opens at once and the figures arrive within one tick, so the rows read as
 * unknown rather than delaying the open on a round trip.
 */
function emptyStats(): SessionStats {
  return {
    cwd: '',
    cache_hit_percent: null,
    tokens_sent: '…',
    tokens_received: '…',
    context_label: '…',
    cost: '…',
    turn_count: 0,
    working: '…',
    model: '…',
    tree: null,
  };
}
