// The protocol version this bundle was built against, and what the page
// does when the server speaks another.
//
// The server stamps its version into every snapshot. A page that never
// reloaded after a jinn update keeps running the previous bundle against
// the new server; the hashed asset URLs protect a fresh load, not a tab
// that was backgrounded across a restart. The check runs on the one moment
// a restart can be detected — the reconnect that answers `hello` with a
// snapshot — and never on a timer: a connected page is already in step.

/** The protocol version this bundle was built against.
 *
 *  A literal, locked to the server's `WEBUI2_PROTOCOL_VERSION` by
 *  `protocol_version.test.ts`, which reads the Rust source. Bumping the
 *  server's constant without this one fails the cargo build, because the
 *  client's unit tests run inside `build.rs`. */
export const BUNDLE_PROTOCOL_VERSION = 3;

/** What the page does when the server speaks another protocol version.
 *
 *  A real reload, by default. `applySnapshot` reaches it through
 *  `reloadIfStale`, which takes the reload as a parameter so a test can
 *  observe the request instead of performing it. */
export type Reload = () => void;

/** The reload a stale page performs. A module-level seam so the one caller
 *  (`applySnapshot`) needs no parameter, and a test can swap it for one it
 *  can see. */
let reloadPage: Reload = (): void => location.reload();

/** Replaces the reload for a test. Returns the previous one, to restore. */
export function setReload(reload: Reload): Reload {
  const previous = reloadPage;
  reloadPage = reload;
  return previous;
}

/** Reloads the page when the server's protocol version is not this
 *  bundle's. Returns whether a reload was requested. */
export function reloadIfStale(serverProtocol: number): boolean {
  if (serverProtocol === BUNDLE_PROTOCOL_VERSION) return false;
  reloadPage();
  return true;
}
