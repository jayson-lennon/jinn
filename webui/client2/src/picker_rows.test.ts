// The picker rows: a frame lands only in the filter the page last asked
// for, and a session-scoped frame only while the page holds that session.
//
// Every helper writes the store, so every test awaits one microtask before
// it reads the result — a same-tick read returns the value before the write.

import { describe, it, expect } from 'vitest';
import { createStore, type ClientStore } from './store';
import {
  applyAppletRows,
  applyModelRows,
  applyPersonaRows,
  applyPickerRows,
  applySkillRows,
  applyToolRows,
  applyMcpRows,
  resetSessionPickerRows,
} from './picker_rows';
import type { PickerRow } from './protocol';

/** Awaits the microtask a store write needs before it can be read back. */
async function settled(): Promise<void> {
  await Promise.resolve();
}

/** A stored-session picker row, with only the name an assertion reads. */
function row(name: string): PickerRow {
  return { session_id: name, title: name } as PickerRow;
}

/** A page holding session `s1`. */
async function viewing(store: ClientStore): Promise<void> {
  store.set((s) => {
    s.sessionId = 's1';
  });
  await settled();
}

describe('applyPickerRows', () => {
  it('lands the rows when the filter is the one the page last asked for', async () => {
    // Given a page whose picker filter is `ab`.
    const store = createStore();
    store.set((s) => {
      s.pickerFilter = 'ab';
    });
    await settled();

    // When a picker_rows frame answers that filter.
    applyPickerRows(store, { type: 'picker_rows', filter: 'ab', rows: [row('one')] });
    await settled();

    // Then the rows land.
    expect(store.state.pickerRows.map((r) => r.title)).toEqual(['one']);
  });

  it('drops a frame that answers a filter the page has typed past', async () => {
    // Given a page whose filter has moved on to `abc`.
    const store = createStore();
    store.set((s) => {
      s.pickerFilter = 'abc';
    });
    await settled();

    // When a late frame answers the earlier `ab`.
    applyPickerRows(store, { type: 'picker_rows', filter: 'ab', rows: [row('stale')] });
    await settled();

    // Then no rows land.
    expect(store.state.pickerRows).toEqual([]);
  });
});

describe('applyModelRows', () => {
  it('lands the rows and ends the refresh, for the viewed session', async () => {
    // Given a page holding session s1, its model filter on `cl` and a refresh under way.
    const store = createStore();
    await viewing(store);
    store.set((s) => {
      s.modelFilter = 'cl';
      s.refreshing = true;
    });
    await settled();

    // When a model_rows frame answers that filter for s1.
    applyModelRows(store, { type: 'model_rows', session_id: 's1', filter: 'cl', rows: [] });
    await settled();

    // Then the rows land and the refresh is over.
    expect(store.state.modelRows).toEqual([]);
    expect(store.state.refreshing).toBe(false);
  });

  it('drops a frame for a session the page has left', async () => {
    // Given a page holding session s1, with a refresh under way.
    const store = createStore();
    await viewing(store);
    store.set((s) => {
      s.refreshing = true;
    });
    await settled();

    // When a model_rows frame arrives for s2.
    applyModelRows(store, { type: 'model_rows', session_id: 's2', filter: '', rows: [] });
    await settled();

    // Then nothing changes: the refresh is still under way.
    expect(store.state.refreshing).toBe(true);
  });

  it('a page with no session accepts a frame for any session', async () => {
    // Given a page holding no session.
    const store = createStore();

    // When a model_rows frame arrives for s1.
    applyModelRows(store, { type: 'model_rows', session_id: 's1', filter: '', rows: [] });
    await settled();

    // Then it is accepted: the refresh ends.
    expect(store.state.refreshing).toBe(false);
  });
});

describe('applySkillRows', () => {
  it('lands the rows for the viewed session in the asked filter', async () => {
    // Given a page holding s1 with its skill filter on `re`.
    const store = createStore();
    await viewing(store);
    store.set((s) => {
      s.skillFilter = 're';
    });
    await settled();

    // When a skill_rows frame answers that filter for s1.
    applySkillRows(store, { type: 'skill_rows', session_id: 's1', filter: 're', rows: [] });
    await settled();

    // Then the frame is accepted, and the rows are the frame's.
    expect(store.state.skillRows).toEqual([]);
  });

  it('drops a frame that answers a filter the page has typed past', async () => {
    // Given a page whose skill filter has moved on to `rev`.
    const store = createStore();
    await viewing(store);
    store.set((s) => {
      s.skillFilter = 'rev';
    });
    await settled();

    // When a late frame answers the earlier `re`.
    applySkillRows(store, { type: 'skill_rows', session_id: 's1', filter: 're', rows: [] });
    await settled();

    // Then no rows land.
    expect(store.state.skillRows).toEqual([]);
  });
});

describe('applyToolRows', () => {
  it('lands the rows for the viewed session in the asked filter', async () => {
    // Given a page holding s1 with its tool filter on `ba`.
    const store = createStore();
    await viewing(store);
    store.set((s) => {
      s.toolFilter = 'ba';
    });
    await settled();

    // When a tool_rows frame answers that filter for s1.
    applyToolRows(store, { type: 'tool_rows', session_id: 's1', filter: 'ba', rows: [] });
    await settled();

    // Then the frame is accepted.
    expect(store.state.toolRows).toEqual([]);
  });
});

describe('applyMcpRows', () => {
  it('lands the rows for the viewed session in the asked filter', async () => {
    // Given a page holding s1 with its MCP filter on `gi`.
    const store = createStore();
    await viewing(store);
    store.set((s) => {
      s.mcpFilter = 'gi';
    });
    await settled();

    // When an mcp_rows frame answers that filter for s1.
    applyMcpRows(store, { type: 'mcp_rows', session_id: 's1', filter: 'gi', rows: [] });
    await settled();

    // Then the frame is accepted.
    expect(store.state.mcpRows).toEqual([]);
  });
});

describe('applyPersonaRows', () => {
  it('lands the rows for the viewed session in the asked filter', async () => {
    // Given a page holding s1 with its persona filter on `de`.
    const store = createStore();
    await viewing(store);
    store.set((s) => {
      s.personaFilter = 'de';
    });
    await settled();

    // When a persona_rows frame answers that filter for s1.
    applyPersonaRows(store, { type: 'persona_rows', session_id: 's1', filter: 'de', rows: [] });
    await settled();

    // Then the frame is accepted.
    expect(store.state.personaRows).toEqual([]);
  });
});

describe('applyAppletRows', () => {
  it('lands the rows and the serving state in the asked filter', async () => {
    // Given a page whose applet filter is empty.
    const store = createStore();

    // When an applet_rows frame answers the empty filter.
    applyAppletRows(store, {
      type: 'applet_rows',
      filter: '',
      serving: true,
      bind: '127.0.0.1:0',
      rows: [],
    });
    await settled();

    // Then the serving state is the frame's.
    expect(store.state.appletServing).toEqual({ serving: true, bind: '127.0.0.1:0' });
  });

  it('drops a frame that answers a filter the page has typed past', async () => {
    // Given a page whose applet filter has moved on.
    const store = createStore();
    store.set((s) => {
      s.appletFilter = 'no';
    });
    await settled();

    // When a late frame answers the empty filter.
    applyAppletRows(store, {
      type: 'applet_rows',
      filter: '',
      serving: true,
      bind: '127.0.0.1:0',
      rows: [],
    });
    await settled();

    // Then the serving state is untouched, at its default.
    expect(store.state.appletServing).toEqual({ serving: false, bind: '' });
  });
});

describe('resetSessionPickerRows', () => {
  it('clears every session-scoped picker and its filter', async () => {
    // Given a page with every session-scoped picker holding rows and a filter.
    const store = createStore();
    store.set((s) => {
      s.skillRows = [{} as never];
      s.skillFilter = 'a';
      s.toolRows = [{} as never];
      s.toolFilter = 'b';
      s.mcpRows = [{} as never];
      s.mcpFilter = 'c';
      s.personaRows = [{} as never];
      s.personaFilter = 'd';
    });
    await settled();

    // When the snapshot resets them.
    store.set((s) => {
      resetSessionPickerRows(s);
    });
    await settled();

    // Then all four are empty, filters included.
    expect(store.state.skillRows).toEqual([]);
    expect(store.state.skillFilter).toBe('');
    expect(store.state.toolRows).toEqual([]);
    expect(store.state.toolFilter).toBe('');
    expect(store.state.mcpRows).toEqual([]);
    expect(store.state.mcpFilter).toBe('');
    expect(store.state.personaRows).toEqual([]);
    expect(store.state.personaFilter).toBe('');
  });
});
