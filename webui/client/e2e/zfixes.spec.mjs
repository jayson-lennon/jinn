//! Regression specs for the webui bugfix batch — the self-healing
//! switching overlay, the create spinner, the picker's scroll and
//! right alignment, and the composer's full-height controls.
//!
//! Sorts after `zswitch.spec.mjs` on purpose: it reuses the multi-
//! session world the earlier specs establish (createSession is the
//! znew-session flow, condensed). Every route intercept scopes to the
//! one endpoint it gates — the page's SSE `@get` retries infinitely
//! and a broad pattern poisons the page.
//!
//! Geometry assertions (scrollability, alignment, heights) read
//! bounding boxes and scroll dimensions off the live page — computed
//! truth, not style strings.

import { expect, test } from "@playwright/test";
import * as sel from "./selectors.mjs";
import { openSession } from "./nav.mjs";
import { baseUrl } from "./run.mjs";

/** The phone viewport the mobile tier runs at (≤767px breakpoint). */
const PHONE = { width: 390, height: 844 };
/** The desktop viewport (the default, named for the height specs). */
const DESKTOP = { width: 1280, height: 800 };

/**
 * Creates a session through the new-session modal and lands on its
 * page — the znew-session flow, condensed to the id it returns.
 *
 * @param {import("@playwright/test").Page} page the browser page.
 * @returns {Promise<string>} the created session's id.
 */
async function createSession(page) {
  const beforeId = await page
    .locator(sel.PAGE_ROOT)
    .getAttribute(sel.PAGE_SESSION_ATTR);
  await page.locator(sel.MENU_BUTTON).click();
  await page.locator(sel.MENU_ITEM_NEW_SESSION).click();
  const modal = page.locator(sel.SESSION_NEW_MODAL);
  await expect(modal).toBeVisible();
  await page.locator(sel.SESSION_NEW_SUBMIT).click();
  const root = page.locator(sel.PAGE_ROOT);
  await expect(root).not.toHaveAttribute(sel.PAGE_SESSION_ATTR, beforeId ?? "");
  await expect(page.locator(sel.MENU_PANEL)).toBeHidden();
  const createdId = await root.getAttribute(sel.PAGE_SESSION_ATTR);
  if (!createdId) {
    throw new Error("create did not navigate; page root carries no session id");
  }
  return createdId;
}

test("a failed activate clears the overlay and surfaces the error", async ({
  page,
}) => {
  // Given two sessions — A viewed, B left behind — and an activate
  // POST that fails with a 500 (gated, so the overlay's stuck window
  // is observable before the failure lands).
  await openSession(page);
  const sessionA = await page
    .locator(sel.PAGE_ROOT)
    .getAttribute(sel.PAGE_SESSION_ATTR);
  const sessionB = await createSession(page);
  await page.goto(`${baseUrl()}${sel.SESSION_PAGE_PATH}${sessionA}`);
  await expect(page.locator(sel.PAGE_ROOT)).toHaveAttribute(
    sel.PAGE_SESSION_ATTR,
    sessionA,
  );

  let releaseActivate;
  const gate = new Promise((resolve) => {
    releaseActivate = resolve;
  });
  await page.route("**/activate", async (route) => {
    await gate;
    await route.fulfill({ status: 500, body: "switch failed" });
  });

  // When clicking B's row: the overlay raises synchronously…
  await page.locator(sel.SESSION_ROW_BY_ID(sessionB)).click();
  await expect(page.locator(sel.SWITCH_OVERLAY)).toBeVisible();

  // …and the held response lands as a failure.
  releaseActivate();

  // Then the page is NOT frozen: the overlay cleared (the page root's
  // fetch-error listener re-seeded the signal), the error is visible
  // on the current page, and A is still the viewed session.
  await expect(page.locator(sel.SWITCH_OVERLAY)).toBeHidden();
  await expect(page.locator(sel.PAGE_ROOT)).toHaveAttribute(
    sel.PAGE_SESSION_ATTR,
    sessionA,
  );
  const notice = page.locator(sel.ERROR_NOTICE);
  await expect(notice).toBeVisible();
  await expect(notice).toContainText("500");

  // And the page recovered for real use: with the failure route
  // gone, the viewed row activates normally again.
  await page.unroute("**/activate");
  await page.locator(sel.SESSION_ROW_BY_ID(sessionA)).click();
  await expect(page.locator(sel.SWITCH_OVERLAY)).toBeHidden();
});

test("creating a session raises the interstitial until the swap lands", async ({
  page,
}) => {
  // Given a session page and a create POST whose fulfillment is held.
  await openSession(page);
  let releaseCreate;
  const gate = new Promise((resolve) => {
    releaseCreate = resolve;
  });
  await page.route("**/session-new", async (route) => {
    if (route.request().method() !== "POST") {
      return route.continue();
    }
    await gate;
    await route.continue();
  });

  // When submitting the create form.
  await page.locator(sel.MENU_BUTTON).click();
  await page.locator(sel.MENU_ITEM_NEW_SESSION).click();
  await expect(page.locator(sel.SESSION_NEW_MODAL)).toBeVisible();
  await page.locator(sel.SESSION_NEW_SUBMIT).click();

  // Then the same interstitial the activate POST raises is visible
  // while the create is in flight — creation is a navigation.
  await expect(page.locator(sel.SWITCH_OVERLAY)).toBeVisible();

  // When the held response lands.
  releaseCreate();

  // Then the swap lands and the overlay cleared by the same rule.
  await expect(page.locator(sel.SWITCH_OVERLAY)).toBeHidden();
  await expect(page.locator(sel.PAGE_ROOT)).not.toHaveAttribute(
    sel.PAGE_SESSION_ATTR,
    "",
  );
});

test("cancel crossing a settle boundary settles the composer, never pins it", async ({
  page,
}) => {
  // Given a session page with a real turn running — the busy
  // fragment (cancel + queue) is up.
  await openSession(page);
  await page.locator(sel.PICKER_BUTTON).click();
  await expect(page.locator(sel.PICKER_MODAL)).toBeVisible();
  await page.locator(sel.PICKER_ROW_MODEL_SPAN(sel.MODEL_ID)).click();
  await expect(page.locator(sel.PICKER_BUTTON)).toHaveText(sel.MODEL_ID);
  await page.locator(sel.INPUT).fill("!response");
  await page.locator(sel.SEND_BUTTON).click();
  await expect(page.locator(sel.CANCEL_BUTTON)).toBeVisible({
    timeout: 10_000,
  });

  // And a cancel POST held at the gate — the press fires now, while
  // the busy fragment is up, but the request hangs until released.
  let releaseCancel;
  const gate = new Promise((resolve) => {
    releaseCancel = resolve;
  });
  await page.route("**/cancel", async (route) => {
    await gate;
    await route.continue();
  });
  await page.locator(sel.CANCEL_BUTTON).click();

  // When the turn settles by itself (the stream runs ~1s; the
  // ordinary phase wave restores send)…
  await expect(page.locator(sel.SEND_BUTTON)).toBeVisible({
    timeout: 15_000,
  });

  // …and only then is the held cancel released — it lands on a
  // ledger with no live turn, the shape whose settle used to publish
  // into silence and pin a busy composer forever.
  releaseCancel();

  // Then the composer ends idle and interactive — the settle push
  // re-rendered the idle controls (a harmless no-op here), and the
  // page is not frozen by the out-of-band cancel.
  await expect(page.locator(sel.SEND_BUTTON)).toBeVisible();
  await expect(page.locator(sel.INPUT)).toBeEditable();
  await page.locator(sel.INPUT).fill("still here");
  await expect(page.locator(sel.INPUT)).toHaveValue("still here");
});

test("the picker's rows scroll inside the modal and the button right-aligns", async ({
  page,
}) => {
  // Given a session page. The scratch config fills the picker with
  // enough provider rows to overflow the modal's 70vh card.
  await openSession(page);

  // Then the titlebar button's right edge sits at the titlebar's
  // right padding edge — the dead-CSS repair's alignment pin.
  const titlebar = await page.locator("#titlebar").boundingBox();
  const button = await page.locator(sel.PICKER_BUTTON).boundingBox();
  if (!titlebar || !button) {
    throw new Error("titlebar or picker button rendered no box");
  }
  const titlebarRightPad = 18;
  expect(Math.abs(button.x + button.width - (titlebar.x + titlebar.width - titlebarRightPad)))
    .toBeLessThan(2);

  // When opening the picker.
  await page.locator(sel.PICKER_BUTTON).click();
  await expect(page.locator(sel.PICKER_MODAL)).toBeVisible();

  // Then the rows container genuinely scrolls: its content is taller
  // than its box and the box is capped inside the modal.
  const scroll = await page
    .locator(sel.PICKER_ROWS)
    .evaluate((el) => ({
      scrollHeight: el.scrollHeight,
      clientHeight: el.clientHeight,
      overflowY: getComputedStyle(el).overflowY,
    }));
  expect(scroll.overflowY).toBe("auto");
  expect(scroll.scrollHeight).toBeGreaterThan(scroll.clientHeight);
});

test.describe("composer control heights", () => {
  test("send button and attach control match the textarea height", async ({
    page,
  }) => {
    // Given a desktop session page.
    await page.setViewportSize(DESKTOP);
    await openSession(page);

    // Then the send button and the attach control occupy the
    // textarea's full height.
    for (const controlSel of [sel.SEND_BUTTON, sel.ATTACH_BUTTON]) {
      const ta = await page.locator(sel.INPUT).boundingBox();
      const ctl = await page.locator(controlSel).boundingBox();
      if (!ta || !ctl) {
        throw new Error(`composer control ${controlSel} rendered no box`);
      }
      expect(
        Math.abs(ctl.height - ta.height),
        `${controlSel} height vs textarea at desktop`,
      ).toBeLessThan(2);
    }
  });

  test("queue and cancel match the textarea height at the phone breakpoint", async ({
    page,
  }) => {
    // Given a phone-width session page mid-turn — the busy controls
    // (queue + cancel) are the fragment the mobile media query used
    // to pin short. A real turn runs: the send POST starts it, the
    // busy controls push swaps the fragment in.
    await page.setViewportSize(PHONE);
    await page.goto(`${baseUrl()}/web`);
    await expect(page.locator(sel.SESSION_ROW).first()).toBeAttached();
    const rowId = await page
      .locator(sel.SESSION_ROW)
      .first()
      .getAttribute(sel.SESSION_ID_ATTR);
    await page.goto(`${baseUrl()}${sel.SESSION_PAGE_PATH}${rowId}`);
    await expect(page.locator(sel.PAGE_ROOT)).toHaveAttribute(
      sel.PAGE_SESSION_ATTR,
      rowId,
    );
    await page.locator(sel.PICKER_BUTTON).click();
    await expect(page.locator(sel.PICKER_MODAL)).toBeVisible();
    await page.locator(sel.PICKER_ROW_MODEL_SPAN(sel.MODEL_ID)).click();
    await expect(page.locator(sel.PICKER_BUTTON)).toHaveText(sel.MODEL_ID);
    await page.locator(sel.INPUT).fill("!response");
    await page.locator(sel.SEND_BUTTON).click();
    await expect(page.locator(sel.QUEUE_BUTTON)).toBeVisible();

    // Then the queue and cancel buttons occupy the textarea's full
    // height at the mobile breakpoint too.
    for (const controlSel of [sel.QUEUE_BUTTON, sel.CANCEL_BUTTON]) {
      const ta = await page.locator(sel.INPUT).boundingBox();
      const ctl = await page.locator(controlSel).boundingBox();
      if (!ta || !ctl) {
        throw new Error(`composer control ${controlSel} rendered no box`);
      }
      expect(
        Math.abs(ctl.height - ta.height),
        `${controlSel} height vs textarea at phone width`,
      ).toBeLessThan(2);
    }

    // When the turn settles (cancel ends it — the sample backend's
    // reply lands either way).
    await page.locator(sel.CANCEL_BUTTON).click();

    // Then the composer returned to send — the settle restored it.
    await expect(page.locator(sel.SEND_BUTTON)).toBeVisible();
  });
});

/** A 1x1 PNG, base64 — the smallest image the sniffer accepts. */
const PNG_BASE64 =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
/** The composer's chips strip (`wire.IMAGE_CHIPS_ID`). */
const CHIPS = "#image-chips";

/**
 * Attaches `count` images via the paste path (the clipboard event
 * carrying files) — the same lever attachments.spec uses, without a
 * file chooser.
 *
 * @param {import("@playwright/test").Page} page the browser page.
 * @param {number} count how many images to attach.
 * @returns {Promise<void>} resolves once all chips are visible.
 */
async function attachImages(page, count) {
  await page.locator(sel.INPUT).evaluate((textarea, { pngB64, n }) => {
    const bytes = Uint8Array.from(atob(pngB64), (c) => c.charCodeAt(0));
    const files = Array.from(
      { length: n },
      (_, i) => new File([bytes], `shot-${i}.png`, { type: "image/png" }),
    );
    const event = new ClipboardEvent("paste", {
      clipboardData: new DataTransfer(),
      bubbles: true,
    });
    Object.defineProperty(event, "clipboardData", { value: { files } });
    textarea.dispatchEvent(event);
  }, { pngB64: PNG_BASE64, n: count });
  await expect(page.locator(CHIPS).locator(".chip")).toHaveCount(count);
}

test("attached chips stack above the input row without horizontal overflow", async ({
  page,
}) => {
  // Given a phone-width session page (the broken shape: the chips row
  // used to share the flex row and push the textarea off-screen).
  await page.setViewportSize(PHONE);
  await page.goto(`${baseUrl()}/web`);
  await expect(page.locator(sel.SESSION_ROW).first()).toBeAttached();
  const rowId = await page
    .locator(sel.SESSION_ROW)
    .first()
    .getAttribute(sel.SESSION_ID_ATTR);
  await page.goto(`${baseUrl()}${sel.SESSION_PAGE_PATH}${rowId}`);
  await expect(page.locator(sel.PAGE_ROOT)).toHaveAttribute(
    sel.PAGE_SESSION_ATTR,
    rowId,
  );

  // When attaching several images (enough chips to wrap inside the
  // strip and prove the vertical stacking).
  await attachImages(page, 6);

  // Then the chips strip sits entirely ABOVE the textarea: its bottom
  // edge is at or above the input's top edge (own row, no sharing).
  const chips = await page.locator(CHIPS).boundingBox();
  const ta = await page.locator(sel.INPUT).boundingBox();
  if (!chips || !ta) {
    throw new Error("chips strip or textarea rendered no box");
  }
  expect(chips.y + chips.height, "chips bottom vs input top").toBeLessThanOrEqual(
    ta.y + 1,
  );

  // And the strip is bounded and vertically scrollable (the "scroll
  // through it easier since it will be vertical" directive).
  const strip = await page.locator(CHIPS).evaluate((el) => ({
    overflowY: getComputedStyle(el).overflowY,
    scrollHeight: el.scrollHeight,
    clientHeight: el.clientHeight,
  }));
  expect(strip.overflowY).toBe("auto");

  // And the composer body never overflows the viewport horizontally
  // with chips present — the defect's headline symptom.
  const overflow = await page.evaluate(() => ({
    body: document.body.scrollWidth,
    viewport: window.innerWidth,
  }));
  expect(overflow.body).toBeLessThanOrEqual(overflow.viewport);

  // And the textarea/buttons row kept its geometry: the textarea is
  // full-width inside the composer padding, not squeezed rightward.
  expect(ta.x, "textarea starts at the composer's left padding").toBeLessThan(30);
  expect(ta.width + ta.x * 2).toBeLessThanOrEqual(overflow.viewport);
});
