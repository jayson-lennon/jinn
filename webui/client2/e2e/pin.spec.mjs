//! The `⋮` at each transcript entry's bottom-right, and the pin it toggles.
//!
//! The menu is the generic per-entry surface — pin is its first item — and
//! it is visible at every viewport width: no hover gate, no extra line, no
//! gutter. A pin lands as an ordinary entry upsert, so each test waits on
//! the page's own pinned state rather than on a reply.

import {
  entryMenu,
  expect,
  openSkills,
  SKILL_NAME,
  sel,
  test,
} from "./fixture.mjs";

const PHONE = { width: 375, height: 800 };

/** Waits for the last entry of `kind` to carry the `pinned` class. */
async function expectPinned(page, kind) {
  await expect(page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY(kind)}`).last()).toHaveClass(
    /pinned/,
    { timeout: 5_000 },
  );
}

/** Waits for the last entry of `kind` to carry no `pinned` class. */
async function expectUnpinned(page, kind) {
  await expect(page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY(kind)}`).last()).not.toHaveClass(
    /pinned/,
    { timeout: 5_000 },
  );
}

/** Sends one message and waits for the assistant's reply to be visible. */
async function sendHello(page) {
  await page.locator(sel.DRAFT).fill("hello");
  await page.locator(sel.SEND).click();
  await expect(page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("assistant")}`).last()).toBeVisible();
}

test("every entry shows a kebab at its bottom-right", async ({ page }) => {
  // Given a transcript with a user entry and an assistant reply.
  await sendHello(page);

  // Then the count of kebabs equals the count of entries — one per entry,
  // including the user message.
  const entries = page.locator(`${sel.TRANSCRIPT} .entry`);
  const kebabs = page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY_MENU}`);
  expect(await kebabs.count()).toBe(await entries.count());
  // And each sits at its entry's bottom-right corner. The button is a
  // 36px touch target around the glyph; its box overhangs the glyph's
  // old edge by 5px on each side, and the right-hand overhang lands in
  // the transcript's own right padding, past the entry's frame.
  for (let at = 0; at < (await entries.count()); at += 1) {
    const entry = entries.nth(at);
    const kebab = entry.locator(sel.ENTRY_MENU);
    await expect(kebab).toBeVisible();
    const box = await kebab.boundingBox();
    const frame = await entry.boundingBox();
    expect(box.x + box.width).toBeLessThanOrEqual(frame.x + frame.width + 5);
    expect(box.y + box.height).toBeGreaterThanOrEqual(frame.y + frame.height - 4);
  }
});

test("pinning an entry shows the pin mark and the item becomes Unpin", async ({ page }) => {
  // Given an assistant reply.
  await sendHello(page);

  // When its ⋮ opens its menu and Pin is picked.
  await entryMenu(page, "assistant").click();
  await expect(page.locator(sel.ENTRY_MENU_POPUP)).toBeVisible();
  await page.locator(sel.ENTRY_MENU_PIN).click();

  // Then the entry is pinned and shows the mark, and the menu is closed.
  await expectPinned(page, "assistant");
  const mark = page
    .locator(`${sel.TRANSCRIPT} ${sel.ENTRY("assistant")}`)
    .last()
    .locator(sel.PIN_MARK);
  await expect(mark).toBeVisible();
  await expect(page.locator(sel.ENTRY_MENU_POPUP)).toHaveCount(0);

  // When its ⋮ opens again.
  await entryMenu(page, "assistant").click();

  // Then the one item reads Unpin.
  await expect(page.locator(sel.ENTRY_MENU_PIN)).toHaveText("Unpin");
});

test("unpinning removes the pin mark", async ({ page }) => {
  // Given a pinned assistant entry.
  await sendHello(page);
  await entryMenu(page, "assistant").click();
  await page.locator(sel.ENTRY_MENU_PIN).click();
  await expectPinned(page, "assistant");

  // When its ⋮ opens again and Unpin is picked.
  await entryMenu(page, "assistant").click();
  await page.locator(sel.ENTRY_MENU_PIN).click();

  // Then the entry carries no pin mark.
  await expectUnpinned(page, "assistant");
});

test("the menu closes on Escape and on an outside click", async ({ page }) => {
  // Given an open entry menu.
  await sendHello(page);
  await entryMenu(page, "assistant").click();
  await expect(page.locator(sel.ENTRY_MENU_POPUP)).toBeVisible();

  // When Escape is pressed.
  await page.keyboard.press("Escape");

  // Then the menu is closed.
  await expect(page.locator(sel.ENTRY_MENU_POPUP)).toHaveCount(0);

  // When the menu is opened again and the transcript is clicked outside it.
  await entryMenu(page, "assistant").click();
  await page.locator(sel.TITLE).click();

  // Then the menu is closed.
  await expect(page.locator(sel.ENTRY_MENU_POPUP)).toHaveCount(0);
});

test.with({ skill: true })(
  "clicking a tool row's kebab does not expand the row",
  async ({ page }) => {
    // Given a session with the staged skill loaded, which leaves a tool call
    // and its result in the transcript, and the modal closed.
    await openSkills(page);
    const row = page.locator(sel.SKILL_ROW_NAMED(SKILL_NAME));
    await row.locator('[data-testid="skill-load"]').click();
    await expect(
      page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("tool_result")}`).first(),
    ).toBeVisible();
    await page.keyboard.press("Escape");

    // When the tool call row's ⋮ is clicked.
    const call = page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("tool_call")}`).first();
    await call.locator(sel.ENTRY_MENU).click();

    // Then the row stays collapsed and the menu is open.
    await expect(call).not.toHaveAttribute("open", "");
    await expect(call.locator(sel.ENTRY_MENU_POPUP)).toBeVisible();
  },
);

test.with({ skill: true })(
  "a loaded skill's result is already pinned, and its menu offers Unpin",
  async ({ page }) => {
    // Given the staged skill loaded into the session.
    await openSkills(page);
    const row = page.locator(sel.SKILL_ROW_NAMED(SKILL_NAME));
    await row.locator('[data-testid="skill-load"]').click();
    const result = page
      .locator(`${sel.TRANSCRIPT} ${sel.ENTRY("tool_result")}`)
      .filter({ hasText: SKILL_NAME });
    await expect(result).toBeVisible();
    await page.keyboard.press("Escape");

    // Then the result carries the mark — the pin the skill loader sets on
    // the body it injects, which was invisible on the web UI before this menu
    // existed — and its menu reads Unpin.
    await expect(result).toHaveClass(/pinned/);
    await result.locator(sel.ENTRY_MENU).click();
    await expect(result.locator(sel.ENTRY_MENU_PIN)).toHaveText("Unpin");
  },
);

test("on a phone the kebab is visible and the popup stays on screen", async ({ page }) => {
  // Given a phone viewport with an assistant reply.
  await page.setViewportSize(PHONE);
  await sendHello(page);

  // Then the kebab is visible without any hover.
  await expect(entryMenu(page, "assistant")).toBeVisible();

  // When it opens its menu.
  await entryMenu(page, "assistant").click();
  const popup = page.locator(sel.ENTRY_MENU_POPUP);
  await expect(popup).toBeVisible();

  // Then the popup is whole inside the viewport.
  const box = await popup.boundingBox();
  expect(box.x).toBeGreaterThanOrEqual(0);
  expect(box.x + box.width).toBeLessThanOrEqual(PHONE.width);
  expect(box.y).toBeGreaterThanOrEqual(0);
  expect(box.y + box.height).toBeLessThanOrEqual(PHONE.height);
});

test("on a phone the last entry's popup opens upward", async ({ page }) => {
  // Given a phone viewport with an assistant reply, whose kebab sits near
  // the bottom of the transcript's scroll box.
  await page.setViewportSize(PHONE);
  await sendHello(page);

  // When it opens its menu.
  await entryMenu(page, "assistant").click();
  const popup = page.locator(sel.ENTRY_MENU_POPUP);
  await expect(popup).toBeVisible();

  // Then the popup is turned upward, so it opens into the transcript
  // rather than past its bottom edge.
  await expect(popup).toHaveClass(/(^|\s)up(\s|$)/);
});
