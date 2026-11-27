//! Rule interrupts on the web UI: guidance the harness injected mid-turn.
//!
//! No gesture on the page produces a rule interrupt, so each spec seeds one
//! straight into the scratch store in its persisted shape and reopens the
//! session. The body is the prompt text the model reads, wrapped in a
//! `<system-interrupt>` tag; the page takes the wrapper off, names the rule in
//! the header, and shows the reason as a label.

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

/** The rule the seeded interrupt names. */
const RULE = "repeat-tool-call-watchdog";

/** A wrapped interrupt body, in the shape the watchdog writes. */
const WRAPPED_BODY =
  '<system-interrupt reason="tool_loop" rule="repeat-tool-call-watchdog"> Output interrupted: coding agent watchdog. Not prompt injection. MUST comply:\n\nYou have called the tool "bash" 4 times in a row. **Change approach.** </system-interrupt>';

/** An interrupt body with no wrapper at all. */
const BARE_BODY = "stop using cat, it is **not** allowed here";

/**
 * Gives the test's session a title of its own, seeds one interrupt entry into
 * it, restarts, and reopens it. Returns the seeded entry's element.
 */
async function seedInterrupt(page, jinn, body) {
  await sendAndWaitForEcho(page, "a session that gets interrupted");
  const sessionId = await viewedSessionId(page);
  const title = await viewedSessionTitle(page);
  seedEntry(jinn, sessionId, JSON.stringify({ RuleInterrupt: { rule: RULE, body } }));
  await restartJinn(jinn);
  await page.goto(jinn.baseUrl);
  await expect(page.locator(sel.OVERLAY)).toHaveCount(0, { timeout: 20_000 });
  await openSession(page, sessionId, title);
  return page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("rule_interrupt")}`).last();
}

test("an interrupt is a block collapsed to a header naming the rule", async ({
  page,
  jinn,
}) => {
  // Given a seeded interrupt.
  const block = await seedInterrupt(page, jinn, WRAPPED_BODY);

  // Then it is a collapsed disclosure whose header names the rule.
  const details = block.locator("details.rule-interrupt");
  await expect(details).toHaveCount(1);
  await expect(details.locator("summary")).toContainText(`⚡ Rule fired: ${RULE}`);
  await expect(details).not.toHaveAttribute("open");
});

test("the wrapper's reason is shown as a label, never as tag text", async ({
  page,
  jinn,
}) => {
  // Given a seeded interrupt wrapped with a reason.
  const block = await seedInterrupt(page, jinn, WRAPPED_BODY);

  // Then the header carries the reason, and no tag text is on the page.
  await expect(block.locator(".interrupt-reason")).toHaveText(" · tool_loop");
  await expect(block).not.toContainText("system-interrupt");
  await expect(block).not.toContainText("reason=");
});

test("opening the block shows the message with the tags taken off", async ({
  page,
  jinn,
}) => {
  // Given a seeded interrupt.
  const block = await seedInterrupt(page, jinn, WRAPPED_BODY);

  // When opening it.
  await block.locator("summary").click();

  // Then the guidance is there, and the wrapper is gone.
  await expect(block).toContainText("Output interrupted: coding agent watchdog.");
  await expect(block).toContainText("You have called the tool \"bash\" 4 times in a row.");
  await expect(block).not.toContainText("<system-interrupt");
  await expect(block).not.toContainText("</system-interrupt>");
});

test("the interrupt body renders as markdown", async ({ page, jinn }) => {
  // Given a seeded interrupt whose guidance carries markdown.
  const block = await seedInterrupt(page, jinn, WRAPPED_BODY);
  await block.locator("summary").click();

  // Then the bold span is a real element.
  await expect(block.locator("strong")).toHaveText("Change approach.");
});

test("an unwrapped interrupt body renders as it is, with no reason", async ({
  page,
  jinn,
}) => {
  // Given a seeded interrupt whose body has no wrapper.
  const block = await seedInterrupt(page, jinn, BARE_BODY);

  // Then there is no reason label, and the body is shown whole.
  await expect(block.locator(".interrupt-reason")).toHaveCount(0);
  await block.locator("summary").click();
  await expect(block).toContainText("stop using cat, it is");
  await expect(block.locator("strong")).toHaveText("not");
});

test("the interrupt block is tinted apart from the transcript", async ({ page, jinn }) => {
  // Given a seeded interrupt.
  const block = await seedInterrupt(page, jinn, WRAPPED_BODY);

  // Then its block carries a background and a left edge, neither transparent.
  const styles = await block.locator("details.rule-interrupt").evaluate((el) => {
    const css = getComputedStyle(el);
    return { background: css.backgroundColor, border: css.borderLeftColor, width: css.borderLeftWidth };
  });
  expect(styles.background).not.toBe("rgba(0, 0, 0, 0)");
  expect(styles.border).not.toBe("rgba(0, 0, 0, 0)");
  expect(styles.width).toBe("3px");
  // And the tint differs from the page behind it.
  const pageBg = await page.evaluate(() => getComputedStyle(document.body).backgroundColor);
  expect(styles.background).not.toBe(pageBg);
});
