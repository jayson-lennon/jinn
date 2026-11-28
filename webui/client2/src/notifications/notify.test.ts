// The live-socket settle path: when a hidden page posts a banner, and when
// it shows nothing.

import { describe, it, expect, beforeEach } from 'vitest';
import { notifySettles, reset, type WorkerChannel } from './notify';
import type { SessionRow } from '../protocol/protocol';

/** A channel that records what was posted and answers a fixed visibility. */
function channel(hidden: boolean): WorkerChannel & { posted: unknown[] } {
  const posted: unknown[] = [];
  return {
    posted,
    post: async (message: unknown) => {
      posted.push(message);
    },
    hidden: () => hidden,
  };
}

/** A session row with every field defaulted, overridable by name. */
function row(overrides: Partial<SessionRow> = {}): SessionRow {
  return {
    session_id: 's1',
    title: 'Session',
    is_active: false,
    is_busy: false,
    is_tearing_down: false,
    in_library: false,
    last_settled_at: null,
    parent_session: null,
    origin: 'root',
    depth: 0,
    ...overrides,
  } as SessionRow;
}

beforeEach(() => {
  reset();
});

describe('notifySettles', () => {
  it('posts a banner when a settle is new and the page is hidden', async () => {
    // Given a hidden page that has seen no settle for the session.
    const c = channel(true);
    notifySettles([row()], c);

    // When a settle arrives that is newer than anything seen.
    const posted = notifySettles([row({ last_settled_at: '2026-01-01T12:00:00Z' })], c);

    // Then one banner was posted, naming the session.
    expect(posted).toBe(1);
    expect(c.posted).toEqual([
      { type: 'settle', session_id: 's1', title: 'jinn', body: 'Turn finished: Session' },
    ]);
  });

  it('posts nothing when the page is visible', async () => {
    // Given a visible page that has seen no settle for the session.
    const c = channel(false);
    notifySettles([row()], c);

    // When a settle arrives.
    const posted = notifySettles([row({ last_settled_at: '2026-01-01T12:00:00Z' })], c);

    // Then nothing was posted: the gold row already says it.
    expect(posted).toBe(0);
    expect(c.posted).toEqual([]);
  });

  it('does not post twice for the same settle time', async () => {
    // Given a hidden page that has already posted for a settle.
    const c = channel(true);
    notifySettles([row()], c);
    notifySettles([row({ last_settled_at: '2026-01-01T12:00:00Z' })], c);

    // When the same list arrives again.
    const posted = notifySettles([row({ last_settled_at: '2026-01-01T12:00:00Z' })], c);

    // Then nothing new was posted.
    expect(posted).toBe(0);
    expect(c.posted).toHaveLength(1);
  });

  it('posts nothing on a first sighting, even when hidden', async () => {
    // Given a hidden page seeing a settled row for the first time — the
    //  snapshot on load carries every past settle, and none is news.
    const c = channel(true);

    // When that list arrives.
    const posted = notifySettles([row({ last_settled_at: '2026-01-01T12:00:00Z' })], c);

    // Then nothing was posted.
    expect(posted).toBe(0);
  });

  it('does not replay a settle the page later becomes hidden for', async () => {
    // Given a visible page that saw a settle land and showed nothing.
    const visible = channel(false);
    notifySettles([row()], visible);
    notifySettles([row({ last_settled_at: '2026-01-01T12:00:00Z' })], visible);

    // When the same list arrives again, now hidden.
    const hidden = channel(true);
    const posted = notifySettles([row({ last_settled_at: '2026-01-01T12:00:00Z' })], hidden);

    // Then it is not replayed.
    expect(posted).toBe(0);
  });

  it('forgets a row that dropped off the list', async () => {
    // Given a hidden page that has seen one settle for a session.
    const c = channel(true);
    notifySettles([row()], c);
    notifySettles([row({ last_settled_at: '2026-01-01T12:00:00Z' })], c);

    // When the session is delisted, then relisted with the same time.
    notifySettles([], c);
    const posted = notifySettles([row({ last_settled_at: '2026-01-01T12:00:00Z' })], c);

    // Then it is a first sighting again: nothing is posted.
    expect(posted).toBe(0);
  });

  it('posts for a second, newer settle', async () => {
    // Given a hidden page that has posted for one settle.
    const c = channel(true);
    notifySettles([row()], c);
    notifySettles([row({ last_settled_at: '2026-01-01T12:00:00Z' })], c);

    // When a newer settle arrives.
    const posted = notifySettles([row({ last_settled_at: '2026-01-01T13:00:00Z' })], c);

    // Then a second banner is posted.
    expect(posted).toBe(1);
    expect(c.posted).toHaveLength(2);
  });
});
