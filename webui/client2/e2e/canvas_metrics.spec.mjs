//! The canvas-metrics block on a web UI user entry: the click events a
//! canvas page collected ride the next message as a tag block, and the page
//! shows it as one collapsed line above the user's words — never the tags.
//!
//! The block is written by the enqueue path when a canvas page has served
//! events, which no gesture on the page can produce against the sample
//! model, so each spec seeds a user entry straight into the scratch store
//! in its persisted shape and reopens the session.

import {
  expect,
  openSession,
  sel,
  seedEntry,
  sendAndWaitForEcho,
  test,
  viewedSessionId,
  viewedSessionTitle,
} from "./fixture.mjs";
import { restartJinn } from "./harness.mjs";

/** The block as the enqueue path writes it: tag, numbered lines, tag. */
const BLOCK = [
  '<canvas-metrics artifact="quiz.html">',
  '1. clicked-foo {"choice":"b"}',
  "2. legend-toggled",
  "</canvas-metrics>",
].join("\n");

/**
 * Gives the test's session a title, seeds one user entry with the given
 * `display` and `expanded` texts into it, restarts, and reopens it. Returns
 * the seeded entry's element.
 */
async function seedUser(page, jinn, display, expanded) {
  await sendAndWaitForEcho(page, "a session that gets a canvas block");
  const sessionId = await viewedSessionId(page);
  const title = await viewedSessionTitle(page);
  seedEntry(jinn, sessionId, JSON.stringify({ User: { display, expanded } }));
  await restartJinn(jinn);
  await page.goto(jinn.baseUrl);
  await expect(page.locator(sel.OVERLAY)).toHaveCount(0, { timeout: 20_000 });
  await openSession(page, sessionId, title);
  return page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("user")}`).last();
}

test("a canvas block is a collapsed line above the user's words", async ({ page, jinn }) => {
  // Given a user entry whose display and expanded texts both lead with the
  // block, then the user's words.
  const entry = await seedUser(page, jinn, `${BLOCK}\nmake it **prettier**`, `${BLOCK}\nmake it **prettier**`);

  // Then the block is a collapsed disclosure whose summary names the event
  // count and the artifact, and its body is not shown.
  const details = entry.locator('[data-testid="canvas-metrics"]');
  await expect(details).toHaveCount(1);
  await expect(details.locator("summary")).toHaveText("canvas metrics · 2 events · quiz.html");
  await expect(details).not.toHaveAttribute("open");
  await expect(entry.locator('[data-testid="canvas-metrics-body"]')).toBeHidden();

  // And the user's words render as markdown below it, with no tag text left
  // in the entry.
  const prose = entry.locator(".entry-prose");
  await expect(prose.locator("strong")).toHaveText("prettier");
  await expect(entry).not.toContainText("<canvas-metrics");
  await expect(entry).not.toContainText("</canvas-metrics");

  // And no "expanded" disclosure appears: the texts differ only by the block.
  await expect(entry.locator("details:not(.canvas-metrics)")).toHaveCount(0);
});

test("opening the canvas line shows the numbered events", async ({ page, jinn }) => {
  // Given a user entry leading with a collapsed canvas block.
  const entry = await seedUser(page, jinn, `${BLOCK}\nmake it prettier`, `${BLOCK}\nmake it prettier`);

  // When clicking the block's summary.
  await entry.locator('[data-testid="canvas-metrics"] summary').click();

  // Then the numbered event lines are shown, tags off.
  const body = entry.locator('[data-testid="canvas-metrics-body"]');
  await expect(body).toBeVisible();
  await expect(body).toHaveText(
    '1. clicked-foo {"choice":"b"}\n2. legend-toggled',
  );
});

test("a real expansion behind the block still shows the expanded text, block off", async ({ page, jinn }) => {
  // Given a user entry whose expanded text carries a real expansion after
  // the block — the shape an `@path` image token leaves.
  const entry = await seedUser(
    page,
    jinn,
    `${BLOCK}\ndescribe @img.png`,
    `${BLOCK}\ndescribe <image attached: /abs/img.png>`,
  );

  // Then the expanded disclosure is offered, and its text is the expanded
  // prompt with the block taken off.
  const expanded = entry.locator("details:not(.canvas-metrics)");
  await expect(expanded).toHaveCount(1);
  await expect(expanded.locator("summary")).toHaveText("expanded");
  await expect(expanded.locator("pre")).toHaveText("describe <image attached: /abs/img.png>");
});
