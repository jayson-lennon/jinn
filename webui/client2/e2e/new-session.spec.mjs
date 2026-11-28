//! Creating a session from the page: the menu item, the modal, and the
//! page landing on the new session.

import {
  createSession,
  expect,
  openNewSession,
  sel,
  sendAndWaitForEcho,
  test,
} from "./fixture.mjs";

test("the menu offers a New session item while a session is active", async ({
  page,
}) => {
  // Given a page with an active session.

  // When opening the menu.
  await page.locator(sel.MENU_BUTTON).click();

  // Then the item is listed.
  await expect(page.locator(sel.MENU_ITEM_NEW_SESSION)).toBeVisible();
});

test("the New session item opens the modal", async ({ page }) => {
  // Given a page with an active session.

  // When picking the item.
  await openNewSession(page);

  // Then the modal shows a project select and a Create session button.
  await expect(page.locator(sel.NEW_SESSION_MODAL)).toBeVisible();
  await expect(page.locator(sel.NEW_SESSION_PROJECT)).toBeVisible();
  await expect(page.locator(sel.CREATE_SESSION)).toBeVisible();
});

test("the modal lists the configured project", async ({ page }) => {
  // Given an open new-session modal.

  // When reading the project select.
  await openNewSession(page);
  const options = await page.locator(`${sel.NEW_SESSION_PROJECT} option`).allTextContents();

  // Then the scratch config's project is the only choice.
  expect(options).toEqual(["~/e2e-scratch"]);
});

test("the modal hides the setup arguments until a lifecycle declares them", async ({
  page,
}) => {
  // Given an open new-session modal with the blank lifecycle selected.
  await openNewSession(page);

  // Then there is no arguments field.
  await expect(page.locator(sel.NEW_SESSION_ARGS)).toHaveCount(0);

  // When picking the one-argument lifecycle.
  await page.locator(sel.NEW_SESSION_LIFECYCLE).selectOption("echo-setup");

  // Then the field appears.
  await expect(page.locator(sel.NEW_SESSION_ARGS)).toBeVisible();
});

test("Create session closes the modal and lands the page on the new session", async ({
  page,
}) => {
  // Given a page whose active session holds a marker message.
  const before = await page
    .locator(sel.ACTIVE_SESSION_ROW)
    .getAttribute("data-session-id");
  await sendAndWaitForEcho(page, "a marker only the first session holds");

  // When creating a session.
  const id = await createSession(page);

  // Then the page views a different session, with an empty transcript.
  expect(id).not.toBe(before);
  await expect(page.locator(sel.ACTIVE_SESSION_ROW)).toHaveAttribute("data-session-id", id);
  await expect(page.locator(sel.ENTRY("user"))).toHaveCount(0);
});

test("the new session appears in the rail without a reload", async ({ page }) => {
  // Given the rail lists one session.
  await expect(page.locator(sel.SESSION_ROW)).toHaveCount(1);

  // When creating a session.
  await createSession(page);

  // Then the rail lists two, with no reload.
  await expect(page.locator(sel.SESSION_ROW)).toHaveCount(2);
});

test("the setup arguments field names each argument as the template spells it", async ({
  page,
}) => {
  // Given the modal, with the staged lifecycle — `echo <greeting>` — chosen.
  await openNewSession(page);
  await page.locator(sel.NEW_SESSION_LIFECYCLE).selectOption("echo-setup");

  // Then the field's placeholder is the template's own placeholder: named,
  // not a `$1` the page derived from the argument's position.
  await expect(page.locator(sel.NEW_SESSION_ARGS)).toHaveAttribute(
    "placeholder",
    "Please enter: <greeting>",
  );
});

/* ---- the sidebar's `+` ----
 * The rail's header holds two controls on one row: Archive, and a square
 * `+` beside it that opens the same New session modal the menu's item
 * does. The shape is a requirement, and only a real browser can measure
 * it, so each viewport pins it separately. */

/** Narrower than the 768px breakpoint. */
const PHONE = { width: 390, height: 800 };
/** At the breakpoint. */
const DESKTOP = { width: 1280, height: 800 };

test("the sidebar's + button opens the New session modal", async ({ page }) => {
  // Given a page with the rail in place.
  await page.setViewportSize(DESKTOP);

  // When pressing the `+` beside Archive.
  await page.locator(sel.NEW_SESSION_BUTTON).click();

  // Then the modal is up, with its Create button.
  await expect(page.locator(sel.NEW_SESSION_MODAL)).toBeVisible();
  await expect(page.locator(sel.CREATE_SESSION)).toBeVisible();
});

test("the sidebar's + button closes the phone drawer behind the modal", async ({
  page,
}) => {
  // Given the phone's drawer, slid in.
  await page.setViewportSize(PHONE);
  await page.locator(sel.SESSIONS_BUTTON).click();
  await expect(page.locator(sel.RAIL)).toBeVisible();

  // When pressing the `+`.
  await page.locator(sel.NEW_SESSION_BUTTON).click();

  // Then the modal is up and the drawer is gone from behind it.
  await expect(page.locator(sel.CREATE_SESSION)).toBeVisible();
  await expect(page.locator(sel.RAIL)).toBeHidden();
});

/** The bounding box of one rail control, in page pixels. */
async function controlBox(page, selector) {
  return page.locator(selector).evaluate((node) => {
    const box = node.getBoundingClientRect();
    return { x: box.x, y: box.y, width: box.width, height: box.height };
  });
}

test("the sidebar's + button is square at desktop width", async ({ page }) => {
  // Given the rail in place.
  await page.setViewportSize(DESKTOP);

  // Then the `+` is as wide as it is tall.
  const plus = await controlBox(page, sel.NEW_SESSION_BUTTON);
  expect(Math.abs(plus.width - plus.height)).toBeLessThan(1);
});

test("the sidebar's + button is square at phone width", async ({ page }) => {
  // Given the phone's drawer, slid in.
  await page.setViewportSize(PHONE);
  await page.locator(sel.SESSIONS_BUTTON).click();
  await expect(page.locator(sel.RAIL)).toBeVisible();

  // Then the `+` is as wide as it is tall.
  const plus = await controlBox(page, sel.NEW_SESSION_BUTTON);
  expect(Math.abs(plus.width - plus.height)).toBeLessThan(1);
});

test("the sidebar's + button is as tall as Archive at desktop width", async ({
  page,
}) => {
  // Given the rail in place.
  await page.setViewportSize(DESKTOP);

  // Then the two controls share a height.
  const [plus, archive] = [
    await controlBox(page, sel.NEW_SESSION_BUTTON),
    await controlBox(page, sel.OPEN_PICKER),
  ];
  expect(Math.abs(plus.height - archive.height)).toBeLessThan(1);
});

test("the sidebar's + button is as tall as Archive at phone width", async ({
  page,
}) => {
  // Given the phone's drawer, slid in.
  await page.setViewportSize(PHONE);
  await page.locator(sel.SESSIONS_BUTTON).click();
  await expect(page.locator(sel.RAIL)).toBeVisible();

  // Then the two controls share a height.
  const [plus, archive] = [
    await controlBox(page, sel.NEW_SESSION_BUTTON),
    await controlBox(page, sel.OPEN_PICKER),
  ];
  expect(Math.abs(plus.height - archive.height)).toBeLessThan(1);
});

test("the sidebar's + button sits to the right of Archive on one row", async ({
  page,
}) => {
  // Given the rail in place.
  await page.setViewportSize(DESKTOP);

  // Then the `+` begins where Archive ends, and the two share a centre line.
  const [plus, archive] = [
    await controlBox(page, sel.NEW_SESSION_BUTTON),
    await controlBox(page, sel.OPEN_PICKER),
  ];
  expect(plus.x).toBeGreaterThanOrEqual(archive.x + archive.width);
  const centres = plus.y + plus.height / 2 - (archive.y + archive.height / 2);
  expect(Math.abs(centres)).toBeLessThan(1);
});
