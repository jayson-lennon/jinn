//! The hamburger menu's Persona item: the modal it opens over the
//! persona catalog, the switch that closes the modal, and the loud
//! error a block-less persona pushes into the transcript.
//!
//! A scratch boot's catalog holds the shipped personas as well as
//! anything staged, so the rows a test sees are never only its own.
//! Every test that pivots on a row names the one scratch persona it
//! stages, and never asserts on the catalog's size.

import {
  BLOCKLESS_PERSONA,
  expect,
  openMenu,
  openPersona,
  sel,
  test,
} from "./fixture.mjs";

test("the menu offers a Persona item while a session is active", async ({
  page,
}) => {
  // Given an open menu.
  await openMenu(page);

  // Then it lists Persona.
  await expect(page.locator(sel.MENU_ITEM_PERSONA)).toBeVisible();
});

test.with({ persona: true })("the Persona item opens the modal listing the catalog", async ({
  page,
}) => {
  // Given the boot session, whose catalog holds the staged persona.
  // When opening the persona modal through the menu.
  await openPersona(page);

  // Then the menu closed, and the modal lists that persona — not
  // marked active, as the session is still on its default persona,
  // `coding-assistant`, which a scratch catalog also holds.
  await expect(page.locator(sel.MENU_PANEL)).toBeHidden();
  const row = page.locator(sel.PERSONA_ROW_NAMED(BLOCKLESS_PERSONA));
  await expect(row).toBeVisible({ timeout: 15_000 });
  await expect(row).toHaveAttribute("data-active", "false");
});

test.with({ persona: true })("clicking a persona switches to it and closes the modal", async ({
  page,
}) => {
  // Given the open persona modal listing the staged persona.
  await openPersona(page);
  const row = page.locator(sel.PERSONA_ROW_NAMED(BLOCKLESS_PERSONA));
  await expect(row).toBeVisible({ timeout: 15_000 });

  // When clicking the row.
  await row.click();

  // Then the modal closed, and reopening it shows that persona marked
  // active.
  await expect(page.locator(sel.PERSONA_MODAL)).toBeHidden();
  await openPersona(page);
  await expect(page.locator(sel.PERSONA_ROW_NAMED(BLOCKLESS_PERSONA))).toHaveAttribute(
    "data-active",
    "true",
  );
});

test.with({ persona: true })("the switched-to persona is the only row marked active", async ({
  page,
}) => {
  // Given the open persona modal listing the staged persona.
  await openPersona(page);
  const row = page.locator(sel.PERSONA_ROW_NAMED(BLOCKLESS_PERSONA));
  await expect(row).toBeVisible({ timeout: 15_000 });

  // When clicking the row, then reopening the modal.
  await row.click();
  await expect(page.locator(sel.PERSONA_MODAL)).toBeHidden();
  await openPersona(page);

  // Then exactly one row is marked active, and it is that one.
  const active = page.locator(`${sel.PERSONA_ROW}[data-active="true"]`);
  await expect(active).toHaveCount(1);
  await expect(active).toHaveAttribute("data-persona", BLOCKLESS_PERSONA);
});

test.with({ persona: true })("Escape closes the persona modal", async ({ page }) => {
  // Given the open persona modal, with its filter focused.
  await openPersona(page);

  // When pressing Escape.
  await page.keyboard.press("Escape");

  // Then the modal is gone.
  await expect(page.locator(sel.PERSONA_MODAL)).toBeHidden();
});

test.with({ persona: true })("switching to a block-less persona pushes the warning into the transcript", async ({
  page,
}) => {
  // Given the open persona modal listing the staged block-less persona,
  // with an empty transcript.
  await openPersona(page);
  const row = page.locator(sel.PERSONA_ROW_NAMED(BLOCKLESS_PERSONA));
  await expect(row).toBeVisible({ timeout: 15_000 });

  // When clicking the row.
  await row.click();

  // Then the modal closed, and the transcript shows an error entry
  // naming the persona and its missing handoff block — without a reload.
  await expect(page.locator(sel.PERSONA_MODAL)).toBeHidden();
  const entry = page
    .locator(`${sel.TRANSCRIPT} ${sel.ENTRY("error")}`)
    .filter({ hasText: BLOCKLESS_PERSONA });
  await expect(entry).toBeVisible();
});
