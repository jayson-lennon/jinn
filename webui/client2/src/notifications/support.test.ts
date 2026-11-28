// @vitest-environment jsdom
// The notification support checks: every label a user can meet, and the
// feature detection that must never throw on a page without the APIs.

import { describe, it, expect, vi, afterEach } from 'vitest';
import { permissionLabel, pushSupported, type LabelState } from './support';

/** A label state with every field defaulted, overridable by name. */
function state(overrides: Partial<LabelState> = {}): LabelState {
  return {
    supported: true,
    serverKey: 'BPublic',
    permission: 'default',
    standalone: false,
    ios: false,
    ...overrides,
  };
}

afterEach(() => {
  vi.restoreAllMocks();
});

describe('permissionLabel', () => {
  it('names http as the reason when the API is absent', () => {
    // Given a page with no Push API.
    // When the label is read.
    const label = permissionLabel(state({ supported: false }));

    // Then it says so, before any permission is considered.
    expect(label).toBe('Notifications — unavailable on http');
  });

  it('names the server when it holds no key', () => {
    // Given a server whose data dir could not be written.
    // When the label is read.
    const label = permissionLabel(state({ serverKey: null }));

    // Then the server is named, not the page.
    expect(label).toBe('Notifications — unavailable on this server');
  });

  it('reads as on when permission is granted', () => {
    // Given a granted permission.
    // When the label is read.
    const label = permissionLabel(state({ permission: 'granted' }));

    // Then it says on.
    expect(label).toBe('Notifications: on');
  });

  it('names the browser when permission is denied', () => {
    // Given a denied permission.
    // When the label is read.
    const label = permissionLabel(state({ permission: 'denied' }));

    // Then the browser is named as the one blocking it.
    expect(label).toBe('Notifications: blocked by the browser');
  });

  it('asks to install on an iOS tab that has not decided', () => {
    // Given an iOS Safari tab with no permission yet.
    // When the label is read.
    const label = permissionLabel(state({ ios: true }));

    // Then installing is named, since only an installed app can push there.
    expect(label).toBe('Notifications — install the app to enable');
  });

  it('offers to enable on an installed iOS app', () => {
    // Given an installed iOS app with no permission yet.
    // When the label is read.
    const label = permissionLabel(state({ ios: true, standalone: true }));

    // Then the offer is made: installing is no longer the blocker.
    expect(label).toBe('Enable notifications');
  });

  it('offers to enable when nothing blocks', () => {
    // Given a secure page, a server with a key, and no permission yet.
    // When the label is read.
    const label = permissionLabel(state());

    // Then it offers to enable.
    expect(label).toBe('Enable notifications');
  });
});

describe('pushSupported', () => {
  it('is false when Notification is absent', () => {
    // Given a window whose Notification is deleted, as on plain http.
    vi.stubGlobal('Notification', undefined);

    // When the check runs.
    const supported = pushSupported();

    // Then it is false, and did not throw.
    expect(supported).toBe(false);
  });

  it('is false when serviceWorker is absent', () => {
    // Given a navigator without serviceWorker.
    const original = navigator.serviceWorker;
    Object.defineProperty(navigator, 'serviceWorker', { value: undefined, configurable: true });

    // When the check runs.
    const supported = pushSupported();

    // Then it is false.
    expect(supported).toBe(false);
    Object.defineProperty(navigator, 'serviceWorker', { value: original, configurable: true });
  });
});
