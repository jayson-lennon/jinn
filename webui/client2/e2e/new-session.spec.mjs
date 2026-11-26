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
