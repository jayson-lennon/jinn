//! The session status bar: the persona on the left, the context usage figure
//! on the right, both pushed by the server and both gone with the session.
//!
//! A scratch boot's session starts on `coding-assistant` and on a model
//! whose window the catalog may or may not record, so the figure is
//! asserted by its shape — `used%/max` or `used/???` — and never by a
//! number. The figure's exact value is the terminal's own function, proved
//! equal in Rust; here it is enough that the page shows one at all, and
//! that it stays well-formed once history lands.

import {
  BLOCKLESS_PERSONA,
  expect,
  sel,
  sendAndWaitForEcho,
  test,
} from "./fixture.mjs";

/** The context figure's shape: a percentage over a budget, or used tokens
 *  over an unknown window. */
const FIGURE = /%\/|\/\?\?\?/;

test("the status bar shows the session's persona", async ({ page }) => {
  // Given the boot session.
  // Then the bar sits under the title bar naming its persona. The context
  // figure beside it is pushed only once something has changed — the first
  // tick on an idle session just records a baseline — so the figure is
  // asserted after a turn, in the test below.
  await expect(page.locator(sel.STATUS_BAR)).toBeVisible();
  await expect(page.locator(sel.STATUS_PERSONA)).toHaveText("coding-assistant");
});

test.with({ persona: true })("clicking the persona opens the picker, and a pick updates the bar", async ({
  page,
}) => {
  // Given the bar, showing the boot session's default persona.
  await expect(page.locator(sel.STATUS_PERSONA)).toHaveText("coding-assistant");

  // When clicking the persona name.
  await page.locator(sel.STATUS_PERSONA).click();

  // Then the existing persona modal opens, and the current persona is
  // the row marked active.
  await expect(page.locator(sel.PERSONA_MODAL)).toBeVisible();
  const current = page.locator(sel.PERSONA_ROW_NAMED("coding-assistant"));
  await expect(current).toHaveAttribute("data-active", "true");

  // When picking the staged persona.
  const staged = page.locator(sel.PERSONA_ROW_NAMED(BLOCKLESS_PERSONA));
  await expect(staged).toBeVisible({ timeout: 15_000 });
  await staged.click();

  // Then the modal closed, and the bar shows the new name — with no
  // reload, the switch arrives as a frame on the next tick.
  await expect(page.locator(sel.PERSONA_MODAL)).toBeHidden();
  await expect(page.locator(sel.STATUS_PERSONA)).toHaveText(BLOCKLESS_PERSONA);
});

test("the context figure is shown on an idle session the moment the page lands", async ({ page }) => {
  // Given the boot session, idle, and no turn ever sent. The figure is the
  // snapshot's own: the tick that pushes it only speaks when it changes, and
  // an idle session never does, so the page must not have to wait for one.
  // Then it is already on the bar in one of the terminal's two shapes.
  await expect(page.locator(sel.STATUS_CONTEXT)).toHaveText(FIGURE);
});

test("the context figure stays well-formed once the history changes", async ({ page }) => {
  // Given the boot session's figure already on the bar.
  await expect(page.locator(sel.STATUS_CONTEXT)).toHaveText(FIGURE);

  // When a turn lands in the history.
  await sendAndWaitForEcho(page, "hello");

  // Then the figure still reads in one of the terminal's two shapes — the
  // page re-shows the server's own string, whatever it became.
  await expect(page.locator(sel.STATUS_CONTEXT)).toHaveText(FIGURE);
});
