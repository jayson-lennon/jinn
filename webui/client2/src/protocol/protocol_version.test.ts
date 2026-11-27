// The bundle's protocol version: locked to the server's constant, and the
// reload it asks for when the server speaks another.

import { describe, it, expect, vi, afterEach } from 'vitest';
import { BUNDLE_PROTOCOL_VERSION, reloadIfStale, setReload, type Reload } from './protocol_version';
// The server's own constant, straight from its Rust source. A `?raw` import
// is typed by `vite/client` and needs no Node types — and it is the one
// place the two numbers can be seen to agree: a bumped server constant with
// a stale client literal fails here, and this file runs inside `build.rs`,
// so it fails the cargo build.
import serversSource from '../../../../crates/slices/jinn-webui2-msg/src/lib.rs?raw';

/** The server's protocol version, read from its source. */
function serversProtocolVersion(): number {
  const line = serversSource
    .split('\n')
    .find((candidate) => candidate.includes('pub const WEBUI2_PROTOCOL_VERSION: u16 ='));
  const digits = line?.slice(line.lastIndexOf('=') + 1).replace(/[;\s]/g, '');
  return Number(digits);
}

describe('the bundle protocol version', () => {
  // Every reload test swaps the real reload for a spy and puts it back.
  let restore: Reload | null = null;
  afterEach(() => {
    if (restore !== null) setReload(restore);
    restore = null;
  });

  it('equals the server constant', () => {
    // Given the server's constant, read from its source.
    const servers = serversProtocolVersion();

    // When compared with the bundle's own.
    const equal = BUNDLE_PROTOCOL_VERSION === servers;

    // Then they agree — the drift guard.
    expect(equal).toBe(true);
  });

  it('does not reload on a matching protocol version', () => {
    // Given a snapshot speaking the bundle's own version, and a reload the
    // test can see.
    const reload = vi.fn();
    restore = setReload(reload);

    // When the check runs.
    const requested = reloadIfStale(BUNDLE_PROTOCOL_VERSION);

    // Then no reload is requested.
    expect(requested).toBe(false);
    expect(reload).not.toHaveBeenCalled();
  });

  it('reloads once on a different protocol version', () => {
    // Given a snapshot speaking another version, and a reload the test can see.
    const reload = vi.fn();
    restore = setReload(reload);

    // When the check runs.
    const requested = reloadIfStale(BUNDLE_PROTOCOL_VERSION + 1);

    // Then exactly one reload is requested.
    expect(requested).toBe(true);
    expect(reload).toHaveBeenCalledTimes(1);
  });
});
