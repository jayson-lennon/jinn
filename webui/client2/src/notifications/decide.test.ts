// The worker's three decisions, each over a fake browser. One question per
// test: the test name alone says what broke.
//
// These are the decisions `sw.ts` cannot be asked in a browser, because
// headless Chromium never grants the notification permission.

import { describe, expect, it } from 'vitest';
import { onPush, onSettle, onTap, type Surface, type Client, type NotificationOpenedMessage } from './decide';

/** A banner the fake browser was asked to raise. */
interface Banner {
  title: string;
  body: string;
  tag: string;
  session_id: string;
}

/** The browser as a recording fake: every banner, message and open is kept. */
function fakeBrowser(clients: Client[] = []): Surface & { banners: Banner[]; posted: (NotificationOpenedMessage | null)[]; opened: (string | null)[] } {
  const banners: Banner[] = [];
  const posted: (NotificationOpenedMessage | null)[] = [];
  const opened: (string | null)[] = [];
  return {
    banners,
    posted,
    opened,
    show: async (title, options) => {
      banners.push({ title, body: options.body, tag: options.tag, session_id: options.data.session_id });
    },
    clients: async () => clients,
    deliver: async (client, message) => {
      posted.push(message);
      await (client as PageFake).focus();
    },
    open: async (url) => {
      opened.push(url);
    },
  };
}

/** An open page, in the visibility a test names. The browser's own client
 *  type also focuses; the fake keeps that so a test can count focusings. */
interface PageFake extends Client {
  focused: number;
  focus(): Promise<void>;
}

/** An open page, in the visibility a test names. */
function page(visible: boolean): PageFake {
  const client: PageFake = {
    visibilityState: visible ? 'visible' : 'hidden',
    focused: 0,
    focus: async () => {
      client.focused += 1;
    },
  };
  return client;
}

describe('onPush', () => {
  it('shows the banner the push named', async () => {
    // Given a push carrying a session and its title.
    const browser = fakeBrowser();

    // When it arrives.
    await onPush(browser, { session_id: 'abc', title: 'jinn', body: 'Turn finished: Other' });

    // Then one banner is raised, tagged to that session.
    expect(browser.banners).toEqual([
      { title: 'jinn', body: 'Turn finished: Other', tag: 'session:abc', session_id: 'abc' },
    ]);
  });

  it('shows a plain banner for a push with no data', async () => {
    // Given a push that carries nothing.
    const browser = fakeBrowser();

    // When it arrives.
    await onPush(browser, null);

    // Then a banner is still raised: a silent push would cost the permission.
    expect(browser.banners).toEqual([
      { title: 'jinn', body: 'Turn finished:', tag: 'session:', session_id: '' },
    ]);
  });

  it('shows the banner even while a page is visible', async () => {
    // Given a push, and a page that is visible.
    const browser = fakeBrowser([page(true)]);

    // When the push arrives.
    await onPush(browser, { session_id: 'abc', title: 'jinn', body: 'Turn finished: Other' });

    // Then the banner is raised anyway: this path has no visibility check.
    expect(browser.banners).toHaveLength(1);
  });
});

describe('onSettle', () => {
  it('raises no banner while a page is visible', async () => {
    // Given a settle, and a page that is visible.
    const browser = fakeBrowser([page(true)]);

    // When the page reports the settle.
    await onSettle(browser, { type: 'settle', session_id: 'abc', title: 'jinn', body: 'Turn finished: Other' });

    // Then no banner is raised: the gold row already says it.
    expect(browser.banners).toHaveLength(0);
  });

  it('raises the banner when every page is hidden', async () => {
    // Given a settle, and a page that is hidden.
    const browser = fakeBrowser([page(false)]);

    // When the page reports the settle.
    await onSettle(browser, { type: 'settle', session_id: 'abc', title: 'jinn', body: 'Turn finished: Other' });

    // Then one banner is raised, naming the session.
    expect(browser.banners).toEqual([
      { title: 'jinn', body: 'Turn finished: Other', tag: 'session:abc', session_id: 'abc' },
    ]);
  });

  it('raises no banner when one page is visible and another is hidden', async () => {
    // Given a settle, a hidden page that reported it, and a visible one.
    const browser = fakeBrowser([page(false), page(true)]);

    // When the settle is reported.
    await onSettle(browser, { type: 'settle', session_id: 'abc', title: 'jinn', body: 'Turn finished: Other' });

    // Then no banner is raised: someone is looking at a page.
    expect(browser.banners).toHaveLength(0);
  });
});

describe('onTap', () => {
  const settle = { session_id: 'abc' };

  it('hands the session to the open page and brings it forward', async () => {
    // Given a tap naming a session, and a page that is open.
    const client = page(false);
    const browser = fakeBrowser([client]);

    // When the banner is tapped.
    await onTap(browser, settle.session_id, () => undefined);

    // Then the page is told the session, and is focused.
    expect(browser.posted).toEqual([{ type: 'notification_opened', session_id: 'abc' }]);
    expect(client.focused).toBe(1);
  });

  it('closes the banner', async () => {
    // Given a tap, and a page that is open.
    const browser = fakeBrowser([page(true)]);
    let closed = 0;

    // When the banner is tapped.
    await onTap(browser, settle.session_id, () => {
      closed += 1;
    });

    // Then the banner is closed.
    expect(closed).toBe(1);
  });

  it('opens a page on the session when none is open', async () => {
    // Given a tap naming a session, and no page open.
    const browser = fakeBrowser([]);

    // When the banner is tapped.
    await onTap(browser, settle.session_id, () => undefined);

    // Then a page is opened with the session in its hash.
    expect(browser.opened).toEqual(['/#notification=abc']);
  });

  it('opens a page at the root when the tap names no session', async () => {
    // Given a tap that names nothing, and no page open.
    const browser = fakeBrowser([]);

    // When the banner is tapped.
    await onTap(browser, undefined, () => undefined);

    // Then a page is opened at the root.
    expect(browser.opened).toEqual(['/']);
  });
});
