//! The change-directory modal: the dash's directory row opened into a
//! field, a live red/green footer, and Accept.
//!
//! The footer's verdict is the server's own, resolved by the terminal cwd
//! popup's resolver, so a green footer here means the same thing it means
//! there. The two paths every test pivots on are the ones the harness
//! already stages under its scratch root: the session's cwd, and the
//! sibling `e2e-scratch` directory a relative path reaches from it. A spec
//! derives both from its own instance and never spells a location.
//!
//! Accept is enabled only while the verdict is green and current: an edit
//! disables it at once, and it comes back when the new verdict lands.

import { awaitConnected, expect, openDash, sel, test } from "./fixture.mjs";

/** Opens the modal from the dash, the way a user does. */
async function openCwd(page) {
  await openDash(page);
  await page.locator(sel.DASH_CWD).click();
  await expect(page.locator(sel.CWD_MODAL)).toBeVisible();
}

test("tapping the directory row opens the modal seeded with the cwd", async ({ page }) => {
  // Given the boot session's dash, and its directory row's text once the
  // statistics frame has landed.
  await openDash(page);
  await expect(page.locator(sel.DASH_CWD)).not.toHaveText("");
  const row = (await page.locator(sel.DASH_CWD).textContent()).trim();

  // When the row is tapped.
  await page.locator(sel.DASH_CWD).click();

  // Then the dash is gone and the modal is up, seeded with the row's text.
  await expect(page.locator(sel.DASH)).toBeHidden();
  await expect(page.locator(sel.CWD_MODAL)).toBeVisible();
  await expect(page.locator(sel.CWD_INPUT)).toHaveValue(row);
});

test("the seeded cwd is green and Accept is enabled once its check lands", async ({ page }) => {
  // Given the modal opened on the session's cwd.
  await openCwd(page);

  // Then the footer resolves the seed green, and Accept is enabled.
  await expect(page.locator(sel.CWD_FOOTER)).toHaveAttribute("data-tone", "ok");
  await expect(page.locator(sel.CWD_ACCEPT)).toBeEnabled();
});

/** Routes the page's socket through a pass-through that records every frame
 *  the page sends, and reloads so the socket goes through it. Every frame
 *  still reaches the server unchanged. */
async function recordSentFrames(page) {
  const sent = [];
  await page.routeWebSocket(/\/ws$/, (ws) => {
    const server = ws.connectToServer();
    ws.onMessage((message) => {
      sent.push(String(message));
      server.send(message);
    });
    server.onMessage((message) => ws.send(message));
  });
  await page.reload();
  return sent;
}

test("typing replaces the seed and a burst asks for one check", async ({ page }) => {
  // Given the modal open with its seed selected, and a recorder on the wire.
  const sent = await recordSentFrames(page);
  await openCwd(page);
  const seedChecks = sent.filter((f) => f.includes('"check_cwd"')).length;

  // When a path is typed one character at a time, faster than the debounce.
  await page.locator(sel.CWD_INPUT).pressSequentially("../e2e-scratch", { delay: 20 });

  // Then the typing replaced the seed rather than prepending to it.
  await expect(page.locator(sel.CWD_INPUT)).toHaveValue("../e2e-scratch");

  // And exactly one more check went out for the whole burst, naming the
  // finished text, and its verdict is green.
  await expect
    .poll(() => sent.filter((f) => f.includes('"check_cwd"')).length - seedChecks)
    .toBe(1);
  const burst = sent.filter((f) => f.includes('"check_cwd"')).slice(seedChecks);
  expect(burst).toEqual(['{"type":"check_cwd","text":"../e2e-scratch"}']);
  await expect(page.locator(sel.CWD_FOOTER)).toHaveAttribute("data-tone", "ok");
});

test("a missing path turns the footer red and keeps Accept disabled", async ({ page }) => {
  // Given the modal open on a green seed.
  await openCwd(page);
  await expect(page.locator(sel.CWD_FOOTER)).toHaveAttribute("data-tone", "ok");

  // When the text becomes a path that does not exist.
  await page.locator(sel.CWD_INPUT).fill("../no-such-dir-e2e");

  // Then the footer turns red naming the path, and Accept is disabled.
  await expect(page.locator(sel.CWD_FOOTER)).toHaveAttribute("data-tone", "bad");
  await expect(page.locator(sel.CWD_FOOTER)).toContainText("not a directory");
  await expect(page.locator(sel.CWD_ACCEPT)).toBeDisabled();
});

test("an edit disables Accept at once and the next green re-enables it", async ({ page }) => {
  // Given the modal open on a green seed, Accept enabled.
  await openCwd(page);
  await expect(page.locator(sel.CWD_ACCEPT)).toBeEnabled();
  const greenBefore = await page.locator(sel.CWD_FOOTER).textContent();

  // When one character is typed: the text is now ahead of its verdict.
  await page.locator(sel.CWD_INPUT).pressSequentially("x", { delay: 0 });

  // Then Accept is disabled at once, and the footer still holds its green.
  await expect(page.locator(sel.CWD_ACCEPT)).toBeDisabled();
  await expect(page.locator(sel.CWD_FOOTER)).toHaveAttribute("data-tone", "ok");
  expect((await page.locator(sel.CWD_FOOTER).textContent()).trim()).toBe(greenBefore.trim());

  // When the text becomes a real directory and its verdict lands.
  await page.locator(sel.CWD_INPUT).fill("../e2e-scratch");

  // Then the footer is green for it, and Accept is enabled again.
  await expect(page.locator(sel.CWD_FOOTER)).toHaveAttribute("data-tone", "ok");
  await expect(page.locator(sel.CWD_FOOTER)).toContainText("e2e-scratch");
  await expect(page.locator(sel.CWD_ACCEPT)).toBeEnabled();
});

test("an empty field shows the muted hint and keeps Accept disabled", async ({ page }) => {
  // Given the modal open.
  await openCwd(page);

  // When the text is cleared.
  await page.locator(sel.CWD_INPUT).fill("");

  // Then the footer is the muted hint, and Accept is disabled.
  await expect(page.locator(sel.CWD_FOOTER)).toHaveAttribute("data-tone", "none");
  await expect(page.locator(sel.CWD_FOOTER)).toContainText("type a path");
  await expect(page.locator(sel.CWD_ACCEPT)).toBeDisabled();
});

test("Accept changes the cwd and closes the modal", async ({ page }) => {
  // Given the modal open on a real sibling directory, green.
  await openCwd(page);
  await page.locator(sel.CWD_INPUT).fill("../e2e-scratch");
  await expect(page.locator(sel.CWD_ACCEPT)).toBeEnabled();

  // When Accept is tapped.
  await page.locator(sel.CWD_ACCEPT).click();

  // Then the modal closes.
  await expect(page.locator(sel.CWD_MODAL)).toBeHidden();

  // And the dash's directory row shows the new directory.
  await openDash(page);
  await expect(page.locator(sel.DASH_CWD)).toContainText("e2e-scratch");
});

test("Cancel closes the modal without changing the cwd", async ({ page }) => {
  // Given the modal open with a real path typed and green.
  await openCwd(page);
  await page.locator(sel.CWD_INPUT).fill("../e2e-scratch");
  await expect(page.locator(sel.CWD_ACCEPT)).toBeEnabled();

  // When Cancel is tapped.
  await page.locator(sel.CWD_CANCEL).click();

  // Then the modal closes, and the cwd is unchanged.
  await expect(page.locator(sel.CWD_MODAL)).toBeHidden();
  await openDash(page);
  await expect(page.locator(sel.DASH_CWD)).not.toContainText("e2e-scratch");
});

test("a tap on the backdrop closes the modal without changing the cwd", async ({ page }) => {
  // Given the modal open with a real path typed and green.
  await openCwd(page);
  await page.locator(sel.CWD_INPUT).fill("../e2e-scratch");
  await expect(page.locator(sel.CWD_ACCEPT)).toBeEnabled();

  // When the backdrop is tapped, outside the card.
  await page.locator(".modal-backdrop").click({ position: { x: 2, y: 2 } });

  // Then the modal closes, and the cwd is unchanged.
  await expect(page.locator(sel.CWD_MODAL)).toBeHidden();
  await openDash(page);
  await expect(page.locator(sel.DASH_CWD)).not.toContainText("e2e-scratch");
});

test("Escape closes the modal without changing the cwd", async ({ page }) => {
  // Given the modal open with a real path typed and green.
  await openCwd(page);
  await page.locator(sel.CWD_INPUT).fill("../e2e-scratch");
  await expect(page.locator(sel.CWD_ACCEPT)).toBeEnabled();

  // When Escape is pressed.
  await page.keyboard.press("Escape");

  // Then the modal closes, and the cwd is unchanged.
  await expect(page.locator(sel.CWD_MODAL)).toBeHidden();
  await openDash(page);
  await expect(page.locator(sel.DASH_CWD)).not.toContainText("e2e-scratch");
});
