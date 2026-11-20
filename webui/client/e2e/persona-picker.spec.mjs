//! Persona-picker flow — the menu item opens the modal, the rows are
//! TUI-parity, the filter is server-side, a switch moves the active
//! mark, the block-less persona pushes the loud error entry into the
//! transcript without a reload, and the landing page carries no
//! persona item.
//!
//! With global-setup's staged scratch persona (`blockless-e2e`, no
//! `<compaction>` block) beside the shipped personas, the warning path
//! has a row to click; every shipped persona carries the block, so the
//! silent path pivots on one of those.

import { expect, test } from "@playwright/test";
import { openSession } from "./nav.mjs";
import { baseUrl } from "./run.mjs";
import * as sel from "./selectors.mjs";
const { BLOCKLESS_PERSONA } = sel;

test("the menu item opens the modal listing personas with the active marked", async ({
  page,
}) => {
  // Given the boot session open and the menu opened.
  await openSession(page);
  await page.locator(sel.MENU_BUTTON).click();
  await expect(page.locator(sel.MENU_PANEL)).toBeVisible();

  // When clicking the Persona item.
  await page.locator(sel.MENU_ITEM_PERSONA).click();

  // Then the modal renders inside its own host span (never the menu's
  // container), the shipped personas all render as rows, and exactly
  // one row — the boot session's persona — carries the active marker.
  const modal = page.locator(sel.PERSONA_MODAL);
  await expect(modal).toBeVisible();
  const host = page.locator(sel.PERSONA_MODAL_HOST);
  await expect(host.locator(sel.PERSONA_MODAL)).toHaveCount(1);
  await expect(page.locator(sel.PERSONA_ROW("brainstorm"))).toBeVisible();
  await expect(page.locator(sel.PERSONA_ROW(BLOCKLESS_PERSONA))).toBeVisible();
  const active = page.locator(".persona-row-active");
  await expect(active).toHaveCount(1);
});

test("typing in the filter narrows the rows server-side with marked runs", async ({
  page,
}) => {
  // Given the persona modal open.
  await openSession(page);
  await page.locator(sel.MENU_BUTTON).click();
  await page.locator(sel.MENU_ITEM_PERSONA).click();
  const modal = page.locator(sel.PERSONA_MODAL);
  await expect(modal).toBeVisible();

  // When typing a query only the scratch persona's description
  // matches.
  await page.locator(sel.PERSONA_FILTER).fill("handoff");

  // Then only that row survives…
  await expect(page.locator(sel.PERSONA_ROW(BLOCKLESS_PERSONA))).toBeVisible();
  await expect(page.locator(sel.PERSONA_ROW("brainstorm"))).toHaveCount(0);
  // …and the matched runs carry `<mark>` highlights.
  await expect(
    page.locator(`${sel.PERSONA_ROW(BLOCKLESS_PERSONA)} mark`).first(),
  ).toBeVisible();
});

test("choosing a row switches the persona and marks the new row active", async ({
  page,
}) => {
  // Given the persona modal open.
  const sessionId = await openSession(page);
  await page.locator(sel.MENU_BUTTON).click();
  await page.locator(sel.MENU_ITEM_PERSONA).click();
  const modal = page.locator(sel.PERSONA_MODAL);
  await expect(modal).toBeVisible();

  // When choosing the brainstorm persona.
  await page.locator(sel.PERSONA_ROW("brainstorm")).click();

  // Then the modal closes…
  await expect(modal).not.toBeVisible();
  // …and a fresh page shows the switched persona from server state
  // alone (the client holds no persona state — the reload pin).
  await page.reload();
  await page.locator(sel.MENU_BUTTON).click();
  await page.locator(sel.MENU_ITEM_PERSONA).click();
  await expect(modal).toBeVisible();
  await expect(page.locator(sel.PERSONA_ROW("brainstorm"))).toHaveClass(
    /persona-row-active/,
  );
  // And the active row count is still exactly one.
  await expect(page.locator(".persona-row-active")).toHaveCount(1);
  expect(sessionId).toBeTruthy();
});

test("a block-less persona switch pushes the loud error entry without a reload", async ({
  page,
}) => {
  // Given the boot session open. The tier runs serially against one
  // scratch jinn, so the boot session may already carry error entries
  // (the chat spec's cancel pushes "Cancelled") — the pin is the count
  // GROWING by exactly one warning, not starting at zero.
  await openSession(page);
  const before = await page.locator(sel.ENTRY("error")).count();

  // When opening the persona modal and choosing the scratch persona
  // with no compaction block.
  await page.locator(sel.MENU_BUTTON).click();
  await page.locator(sel.MENU_ITEM_PERSONA).click();
  await expect(page.locator(sel.PERSONA_MODAL)).toBeVisible();
  await page.locator(sel.PERSONA_ROW(BLOCKLESS_PERSONA)).click();

  // Then the modal closes and the warning is already in the
  // transcript — no reload, the POST response carried the swap. The
  // warning is matched by text (the DOM's text content reads the
  // unescaped angle brackets), and the total error count grew by one.
  await expect(page.locator(sel.PERSONA_MODAL)).not.toBeVisible();
  const warnings = page
    .locator(sel.ENTRY("error"))
    .filter({ hasText: "carries no <compaction> handoff block" });
  await expect(warnings).toHaveCount(1);
  await expect(page.locator(sel.ENTRY("error"))).toHaveCount(before + 1);
  // And the switched persona is the active one on the re-rendered
  // modal the next open shows.
  await page.locator(sel.MENU_BUTTON).click();
  await page.locator(sel.MENU_ITEM_PERSONA).click();
  await expect(page.locator(sel.PERSONA_ROW(BLOCKLESS_PERSONA))).toHaveClass(
    /persona-row-active/,
  );
});

test("clicking the backdrop closes the modal", async ({ page }) => {
  // Given the persona modal open on a session page.
  await openSession(page);
  await page.locator(sel.MENU_BUTTON).click();
  await page.locator(sel.MENU_ITEM_PERSONA).click();
  const modal = page.locator(sel.PERSONA_MODAL);
  await expect(modal).toBeVisible();

  // When clicking the dark backdrop — the overlay itself, outside the
  // card (offset into the padding band, off the inner card).
  const box = await modal.boundingBox();
  await page.mouse.click(box.x + 10, box.y + box.height - 10);

  // Then the modal hides — the click-away handler lives on the
  // overlay, and a click on the card must not fire it.
  await expect(modal).toBeHidden();
});

test("clicking the modal card does not close it", async ({ page }) => {
  // Given the persona modal open on a session page.
  await openSession(page);
  await page.locator(sel.MENU_BUTTON).click();
  await page.locator(sel.MENU_ITEM_PERSONA).click();
  const modal = page.locator(sel.PERSONA_MODAL);
  await expect(modal).toBeVisible();

  // When clicking inside the card's header band (not on a row, not on
  // the overlay).
  await page.locator(`#${sel.PERSONA_MODAL.slice(1)} .persona-modal-head`).click();

  // Then the modal stays open.
  await expect(modal).toBeVisible();
});

test("the landing page's menu carries no persona item", async ({ page }) => {
  // Given the landing page (nothing viewed).
  await page.goto(`${baseUrl()}/web`);

  // When opening the hamburger menu.
  await page.locator(sel.MENU_BUTTON).click();
  await expect(page.locator(sel.MENU_PANEL)).toBeVisible();

  // Then the persona item is absent from the markup — the server
  // omitted it, and the other two items survive.
  await expect(page.locator(sel.MENU_ITEM_PERSONA)).toHaveCount(0);
  await expect(page.locator(sel.MENU_ITEM_SESSIONS)).toBeVisible();
  await expect(page.locator(sel.MENU_ITEM_NEW_SESSION)).toBeVisible();
});
