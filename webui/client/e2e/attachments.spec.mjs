//! Image attachments — the three input paths and the inline render.
//!
//! Paste (the desktop screenshot flow), the file-attach button, and a
//! scanned `https://…png` URL each produce a user entry whose image
//! renders inline through the attachment route. The URL case is served
//! by a loopback static server the spec itself spins up — the tier is
//! network-free by contract; no test ever leaves the machine.
//!
//! The sample backend answers text instantly, so an image-only send's
//! assistant echo doubles as the settle convergence: after it arrives,
//! the optimistic entry has been reconciled with the transcript's
//! server render, and the surviving user entry is the served one.

import { expect, test } from "@playwright/test";
import { readFileSync } from "node:fs";
import https from "node:https";
import { openSession } from "./nav.mjs";
import { baseUrl } from "./run.mjs";
import * as sel from "./selectors.mjs";

/** A 1x1 PNG, base64 — the smallest image the sniffer accepts. */
const PNG_BASE64 =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
const PNG_BYTES = Buffer.from(PNG_BASE64, "base64");

/** The composer's chips strip and hidden file input
 * (`wire.IMAGE_CHIPS_ID` / `wire.IMAGE_FILE_INPUT_ID`). */
const CHIPS = "#image-chips";
const FILE_INPUT = "#image-file-input";

/**
 * Boots a loopback TLS server serving the spec's PNG at /x.png over
 * https — the scan gate admits only https links, so the spec's server
 * must speak TLS. `loopback-{cert,key}.pem` is a committed self-signed
 * identity for CN/IP 127.0.0.1: the browser context trusts it via
 * Playwright's `ignoreHTTPSErrors`, and jinn's fetch client trusts it
 * because the harness exports `JINN_WEBUI_TEST_ROOT_CERT` pointing at
 * the cert (a test-only env var — production never sets it).
 *
 * @returns {Promise<{url: string, close: () => Promise<void>}>}
 */
function servePng() {
  const server = https.createServer(
    {
      key: readFileSync(new URL("./loopback-key.pem", import.meta.url)),
      cert: readFileSync(new URL("./loopback-cert.pem", import.meta.url)),
    },
    (req, res) => {
      res.writeHead(200, { "content-type": "image/png" });
      res.end(PNG_BYTES);
    },
  );
  return new Promise((resolve) => {
    server.listen(0, "127.0.0.1", () => {
      const port = server.address().port;
      resolve({
        url: `https://127.0.0.1:${port}/x.png`,
        close: () => new Promise((done) => server.close(done)),
      });
    });
  });
}

/**
 * Sends whatever is pending (text and/or chips) and waits for the
 * sample backend's assistant echo — the settle convergence marker.
 *
 * @param {import("@playwright/test").Page} page
 * @param {string} text the prompt text.
 */
async function sendAndWaitForEcho(page, text) {
  await page.locator(sel.INPUT).fill(text);
  await page.locator(sel.SEND_BUTTON).click();
  await expect(
    page
      .locator(`${sel.TRANSCRIPT} ${sel.ENTRY("assistant")}`)
      .filter({ hasText: "sample response" })
      .last(),
  ).toBeVisible({ timeout: 15_000 });
}

test("a pasted screenshot attaches and renders inline", async ({ page }) => {
  // Given the boot session open.
  await openSession(page);

  // When pasting an image (the clipboard event carrying a file), the
  // paste path runs the FileReader and a chip appears before any send.
  await page.locator(sel.INPUT).evaluate((textarea, pngB64) => {
    const bytes = Uint8Array.from(atob(pngB64), (c) => c.charCodeAt(0));
    const file = new File([bytes], "pasted.png", { type: "image/png" });
    const event = new ClipboardEvent("paste", {
      clipboardData: new DataTransfer(),
      bubbles: true,
    });
    Object.defineProperty(event, "clipboardData", {
      value: { files: [file] },
    });
    textarea.dispatchEvent(event);
  }, PNG_BASE64);
  await expect(page.locator(CHIPS).locator(".chip")).toHaveCount(1);

  // And the send goes out with the chip cleared.
  await sendAndWaitForEcho(page, "pasted shot");
  await expect(page.locator(CHIPS).locator(".chip")).toHaveCount(0);

  // Then the user entry carries the image inline, served by the
  // attachment route — which returns the same bytes back.
  const userEntry = page
    .locator(`${sel.TRANSCRIPT} ${sel.ENTRY("user")}`)
    .filter({ hasText: "pasted shot" })
    .last();
  const img = userEntry.locator("img.attachment");
  await expect(img).toHaveCount(1);
  const src = await img.getAttribute("src");
  expect(src).toMatch(/^\/web\/attachment\//);
  const fetched = await page.request.get(`${baseUrl()}${src}`);
  expect(fetched.ok()).toBeTruthy();
  expect(await fetched.body()).toEqual(PNG_BYTES);
});

test("the attach button picks a file and renders it inline", async ({
  page,
}) => {
  // Given the boot session open.
  await openSession(page);

  // When choosing a file through the hidden input the attach label
  // drives (Playwright drives the input directly — the same handler
  // the label's click proxy ends at).
  await page.locator(FILE_INPUT).setInputFiles({
    name: "picked.png",
    mimeType: "image/png",
    buffer: PNG_BYTES,
  });
  await expect(page.locator(CHIPS).locator(".chip")).toHaveCount(1);

  // And removing the chip empties the strip — the splice works.
  await page.locator(CHIPS).locator(".chip-x").click();
  await expect(page.locator(CHIPS)).toBeHidden();

  // And re-adding and sending lands the attachment inline.
  await page.locator(FILE_INPUT).setInputFiles({
    name: "picked-again.png",
    mimeType: "image/png",
    buffer: PNG_BYTES,
  });
  await sendAndWaitForEcho(page, "picked shot");

  // Then the entry's image fetches through the attachment route.
  const userEntry = page
    .locator(`${sel.TRANSCRIPT} ${sel.ENTRY("user")}`)
    .filter({ hasText: "picked shot" })
    .last();
  const img = userEntry.locator("img.attachment");
  await expect(img).toHaveCount(1);
  const src = await img.getAttribute("src");
  const fetched = await page.request.get(`${baseUrl()}${src}`);
  expect(fetched.ok()).toBeTruthy();
  expect(await fetched.body()).toEqual(PNG_BYTES);
});

test("an https image URL in the text is scanned, fetched, and rendered", async ({
  page,
}) => {
  // Given a loopback server serving the PNG — never the internet.
  const image = await servePng();

  try {
    // And the boot session open.
    await openSession(page);

    // When sending text whose URL ends in an image extension.
    await sendAndWaitForEcho(page, `look at ${image.url} please`);

    // Then the URL stays visible in the delivered text...
    const userEntry = page
      .locator(`${sel.TRANSCRIPT} ${sel.ENTRY("user")}`)
      .filter({ hasText: "look at" })
      .last();
    await expect(userEntry).toContainText(image.url);

    // ...and the scanned image renders inline through the route.
    const img = userEntry.locator("img.attachment");
    await expect(img).toHaveCount(1);
    const src = await img.getAttribute("src");
    const fetched = await page.request.get(`${baseUrl()}${src}`);
    expect(fetched.ok()).toBeTruthy();
    expect(await fetched.body()).toEqual(PNG_BYTES);
  } finally {
    await image.close();
  }
});
