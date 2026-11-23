//! Web theme — the Light/Dark/System toggle, its persistence, and the
//! system default.
//!
//! One behavior per test, mirroring the Rust suite's discipline. The
//! spec's ordering matters: it runs after the plain specs and before
//! the z-prefixed tail (alphabetical file order, one worker), and its
//! persistence test changes server state (state.toml's web_theme) —
//! the file resets nothing, so the last test restores system to keep
//! the tail's pages on the default palette.
//!
//! The mechanics, per the two realities the other specs document:
//!
//! - **System default needs colorScheme emulation at page load** —
//!   Playwright's `colorScheme` option only affects a fresh load, so
//!   the system tests load their own pages per scheme rather than
//!   flipping the OS mid-page (which the browser does not emulate).
//! - **The mode rides `<body data-theme>` set by a Datastar signal
//!   binding** — assertions read the attribute (the hook the CSS
//!   actually keys), not computed colors; the palette values
//!   themselves are pinned by the Rust contrast tests.

import { expect, test } from "@playwright/test";
import * as sel from "./selectors.mjs";
import { baseUrl } from "./run.mjs";

/**
 * Opens the landing page with the menu panel open — the theme
 * control's host.
 *
 * @param {import("@playwright/test").Page} page the browser page.
 */
async function openMenuWithThemeControl(page) {
  await page.goto(`${baseUrl()}/web`);
  await expect(page.locator(sel.PAGE_ROOT)).toBeVisible();
  await page.locator(sel.MENU_BUTTON).click();
  await expect(page.locator(sel.MENU_ITEM_THEME)).toBeVisible();
}

test("a fresh page marks System active and carries no forced mode", async ({
  page,
}) => {
  // Given a fresh page with no persisted choice (the scratch boot).
  await openMenuWithThemeControl(page);

  // Then the System option carries the active class, and the body's
  // data-theme is "system" — the CSS default the OS query overrides.
  await expect(page.locator(sel.THEME_OPTION("system"))).toHaveClass(
    new RegExp(sel.THEME_ACTIVE_CLASS),
  );
  await expect(page.locator("body")).toHaveAttribute(
    sel.DATA_THEME_ATTR,
    "system",
  );
});

test.describe("system default, light OS", () => {
  test.use({ colorScheme: "light" });

  test("a light-OS fresh page renders the light palette", async ({ page }) => {
    // Given a browser emulating a light OS scheme.

    // When loading a fresh page.
    await page.goto(`${baseUrl()}/web`);
    await expect(page.locator(sel.PAGE_ROOT)).toBeVisible();

    // Then the body paints the light background (the media-query block
    // matched — the observable system-mode default).
    const bg = await page.evaluate(() =>
      getComputedStyle(document.body).backgroundColor,
    );
    expect(bg).toBe("rgb(236, 239, 244)");
  });
});

test.describe("system default, dark OS", () => {
  test.use({ colorScheme: "dark" });

  test("a dark-OS fresh page renders the dark palette", async ({ page }) => {
    // Given a browser emulating a dark OS scheme.

    // When loading a fresh page.
    await page.goto(`${baseUrl()}/web`);
    await expect(page.locator(sel.PAGE_ROOT)).toBeVisible();

    // Then the body paints the dark background.
    const bg = await page.evaluate(() =>
      getComputedStyle(document.body).backgroundColor,
    );
    expect(bg).toBe("rgb(37, 39, 46)");
  });
});

test("picking Light flips the live page and marks the option", async ({
  page,
}) => {
  // Given the menu open on a dark-default page.
  await openMenuWithThemeControl(page);

  // When clicking Light.
  await page.locator(sel.THEME_OPTION("light")).click();

  // Then the body's data-theme flips to light without a reload, and
  // the Light option now carries the active mark (System's cleared).
  await expect(page.locator("body")).toHaveAttribute(
    sel.DATA_THEME_ATTR,
    "light",
  );
  await expect(page.locator(sel.THEME_OPTION("light"))).toHaveClass(
    new RegExp(sel.THEME_ACTIVE_CLASS),
  );
  await expect(page.locator(sel.THEME_OPTION("system"))).not.toHaveClass(
    new RegExp(sel.THEME_ACTIVE_CLASS),
  );
});

test("a pick survives a reload", async ({ page }) => {
  // Given Light picked on the landing page.
  await openMenuWithThemeControl(page);
  await page.locator(sel.THEME_OPTION("light")).click();
  await expect(page.locator("body")).toHaveAttribute(
    sel.DATA_THEME_ATTR,
    "light",
  );

  // When reloading.
  await page.reload();
  await expect(page.locator(sel.PAGE_ROOT)).toBeVisible();

  // Then the page boots light — the persisted mode seeded the signal.
  await expect(page.locator("body")).toHaveAttribute(
    sel.DATA_THEME_ATTR,
    "light",
  );

  // Restore system so the specs after this file keep the default.
  await page.locator(sel.MENU_BUTTON).click();
  await page.locator(sel.THEME_OPTION("system")).click();
  await expect(page.locator("body")).toHaveAttribute(
    sel.DATA_THEME_ATTR,
    "system",
  );
});
