//! Where the web UI lives: the page is the root of its own listener, and
//! the address it used to carry serves nothing.

import { expect, sel, test, awaitConnected } from "./fixture.mjs";

test("the root url serves the web UI and its socket connects", async ({
  page,
}) => {
  // Given the fixture's page, opened at the instance's root URL.
  // When the socket has answered.
  await awaitConnected(page);

  // Then the page is mounted and connected: no overlay, the transcript up.
  await expect(page.locator(sel.OVERLAY)).toHaveCount(0);
  await expect(page.locator(sel.TRANSCRIPT)).toBeVisible();
});

test("the old /web2 path answers 404", async ({ jinn }) => {
  // Given a running instance.
  // When its retired page path is requested.
  const response = await fetch(new URL("/web2", jinn.baseUrl));

  // Then nothing serves it: no alias, no redirect.
  expect(response.status).toBe(404);
});
