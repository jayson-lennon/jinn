//! New-session flow — the menu item, the creation modal, and the
//! create POST's whole-page navigation.
//!
//! The `z` prefix is load-bearing: the tier's specs run in
//! alphabetical file order against ONE scratch jinn (serial workers,
//! shared db), and this spec CREATES a second session — every other
//! spec's single-boot-session pin (`sessions`, `smoke`) must have
//! run before it. Specs asserting a single boot session sort before
//! session-creating specs: `zspinner.spec.mjs` sorts after this file
//! and builds on the sessions it creates.

import { expect, test } from "@playwright/test";
import * as sel from "./selectors.mjs";
import { openSession } from "./nav.mjs";

test("menu opens the new-session modal with project and lifecycle selects", async ({
  page,
}) => {
  // Given a session page with the menu closed.
  await openSession(page);

  // When opening the menu and tapping the new-session item.
  await page.locator(sel.MENU_BUTTON).click();
  await expect(page.locator(sel.MENU_PANEL)).toBeVisible();
  await page.locator(sel.MENU_ITEM_NEW_SESSION).click();

  // Then the menu closed and the modal rendered with both selects —
  // the project select carries the scratch config's entry, and the
  // lifecycle select's first row is the blank one.
  await expect(page.locator(sel.MENU_PANEL)).toBeHidden();
  const modal = page.locator(sel.SESSION_NEW_MODAL);
  await expect(modal).toBeVisible();
  const project = page.locator(sel.SESSION_NEW_PROJECT);
  await expect(project).toBeVisible();
  await expect(project).toHaveValue("~/e2e-scratch");
  const lifecycle = page.locator(sel.SESSION_NEW_LIFECYCLE);
  await expect(lifecycle).toBeVisible();
  const firstOption = lifecycle.locator("option").first();
  await expect(firstOption).toHaveAttribute("value", "");
  // And the args input is absent for the blank lifecycle.
  await expect(page.locator(sel.SESSION_NEW_ARGS)).toHaveCount(0);
});

test("clicking the backdrop closes the new-session modal", async ({ page }) => {
  // Given the new-session modal open.
  await openSession(page);
  await page.locator(sel.MENU_BUTTON).click();
  await page.locator(sel.MENU_ITEM_NEW_SESSION).click();
  const modal = page.locator(sel.SESSION_NEW_MODAL);
  await expect(modal).toBeVisible();

  // When clicking the dark backdrop outside the card.
  const box = await modal.boundingBox();
  await page.mouse.click(box.x + 10, box.y + box.height - 10);

  // Then the modal hides — the click-away handler lives on the
  // overlay, not the card.
  await expect(modal).toBeHidden();
});

test("selecting the parameterized lifecycle re-pulls with the args input", async ({
  page,
}) => {
  // Given the modal open on the blank lifecycle.
  await openSession(page);
  await page.locator(sel.MENU_BUTTON).click();
  await page.locator(sel.MENU_ITEM_NEW_SESSION).click();
  const modal = page.locator(sel.SESSION_NEW_MODAL);
  await expect(modal).toBeVisible();

  // When selecting the scratch config's parameterized lifecycle.
  await page
    .locator(sel.SESSION_NEW_LIFECYCLE)
    .selectOption("echo-setup");

  // Then the re-pulled fragment renders the args input (one
  // parameter), the lifecycle row is selected, and the input's
  // placeholder names the replacement token the setup script wants
  // (`echo $1` → `Please enter: <1>`) — what to type, Discord-style.
  const lifecycle = page.locator(sel.SESSION_NEW_LIFECYCLE);
  await expect(lifecycle).toHaveValue("echo-setup");
  const args = page.locator(sel.SESSION_NEW_ARGS);
  await expect(args).toBeVisible();
  await expect(args).toHaveAttribute("placeholder", "Please enter: <1>");
});

test("typing into the args input keeps the modal open, Escape closes it", async ({
  page,
}) => {
  // Given the modal open on the parameterized lifecycle, so the args
  // input rendered.
  await openSession(page);
  await page.locator(sel.MENU_BUTTON).click();
  await page.locator(sel.MENU_ITEM_NEW_SESSION).click();
  const modal = page.locator(sel.SESSION_NEW_MODAL);
  await expect(modal).toBeVisible();
  await page
    .locator(sel.SESSION_NEW_LIFECYCLE)
    .selectOption("echo-setup");
  const args = page.locator(sel.SESSION_NEW_ARGS);
  await expect(args).toBeVisible();

  // When typing into the args input with real keypresses — `fill()`
  // dispatches no keydown events, which is exactly the blind spot
  // that let an every-keystroke dismissal ship unnoticed.
  await args.click();
  await page.keyboard.type("hello world");

  // Then the modal is still open and the input holds the typed text —
  // a keystroke must never dismiss the form.
  await expect(modal).toBeVisible();
  await expect(args).toHaveValue("hello world");

  // And Escape dismisses it.
  await page.keyboard.press("Escape");
  await expect(modal).toBeHidden();
});

test("creating a session lands on the new session's page", async ({ page }) => {
  // Given the modal open on the blank lifecycle with the scratch
  // project selected.
  await openSession(page);
  const beforeId = await page.locator(sel.PAGE_ROOT).getAttribute(
    sel.PAGE_SESSION_ATTR,
  );
  const rowsBefore = await page.locator(sel.SESSION_ROW).count();
  await page.locator(sel.MENU_BUTTON).click();
  await page.locator(sel.MENU_ITEM_NEW_SESSION).click();
  const modal = page.locator(sel.SESSION_NEW_MODAL);
  await expect(modal).toBeVisible();

  // When submitting the form.
  await page.locator(sel.SESSION_NEW_SUBMIT).click();

  // Then the page swapped wholesale to the created session's page:
  // the root's session id changed, the URL tracks it, the modal is
  // gone, and the sidebar grew by exactly one row (a delta, not an
  // absolute count — the serial tier's shared db may hold sessions
  // created by specs sorting before this one).
  await expect(page.locator(sel.MENU_PANEL)).toBeHidden();
  await expect(modal).toHaveCount(0);
  const root = page.locator(sel.PAGE_ROOT);
  const createdId = await root.getAttribute(sel.PAGE_SESSION_ATTR);
  if (!createdId || createdId === beforeId) {
    throw new Error(`create did not navigate; page root id: ${createdId}`);
  }
  await expect(page).toHaveURL(
    new RegExp(`${sel.SESSION_PAGE_PATH.replace("/", "/")}${createdId}`),
  );
  await expect(page.locator(sel.SESSION_ROW)).toHaveCount(rowsBefore + 1);
});
