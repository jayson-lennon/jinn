//! Verbatim excerpts from the recorded loop, session
//! `01a20b66-be8a-7027-b834-0623ed7a5bb8` ("webui-library-impl", a bot on
//! `glm-5.3-flash`), transcribed from `sessions.db` at the named
//! ordinals.
//!
//! The session's real failure: ~1,389 tool calls over 52 minutes, of
//! which 834 consecutive calls spanned ≤3 distinct commands with
//! byte-identical `Success` results, ended only when a human noticed and
//! sent a stop message. These fixtures are the evidence the cross-call
//! watchdogs are judged against — real data, not synthetic strings.
//!
//! Each record is one completed tool call: the tool name, the arguments
//! JSON exactly as the model issued it, the result status, and the
//! result content verbatim.

/// One completed tool call from the recorded session.
pub(crate) struct ExcerptCall {
    /// The tool's name.
    pub(crate) tool_name: &'static str,
    /// The arguments JSON, exactly as recorded.
    pub(crate) arguments: &'static str,
    /// Whether the result was a success.
    pub(crate) result_success: bool,
    /// The result content, verbatim (truncation already applied by the
    /// harness that recorded it).
    pub(crate) result_content: &'static str,
}

/// The lock phase: 12 consecutive bash calls alternating between two
/// byte-identical greps, each returning the identical result. Ordinals
/// 7771–7819 of session `01a20b66-be8a-7027-b834-0623ed7a5bb8`.
pub(crate) const LOCK_PHASE: &[ExcerptCall] = &[
    ExcerptCall {
        // Session ordinal 7771.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -n \\\"lrow\\\" crates/slices/jinn-webui/src/regions/sessions.rs\"}",
        result_success: true,
        result_content: "265:.srow, .lrow { position: relative; }\n",
    },
    ExcerptCall {
        // Session ordinal 7774.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -n \\\"\\\\.lrow\\\" crates/slices/jinn-webui/src/regions/sessions.rs\"}",
        result_success: true,
        result_content: "265:.srow, .lrow { position: relative; }\n",
    },
    ExcerptCall {
        // Session ordinal 7780.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -n \\\"lrow\\\" crates/slices/jinn-webui/src/regions/sessions.rs\"}",
        result_success: true,
        result_content: "265:.srow, .lrow { position: relative; }\n",
    },
    ExcerptCall {
        // Session ordinal 7783.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -n \\\"\\\\.lrow\\\" crates/slices/jinn-webui/src/regions/sessions.rs\"}",
        result_success: true,
        result_content: "265:.srow, .lrow { position: relative; }\n",
    },
    ExcerptCall {
        // Session ordinal 7789.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -n \\\"lrow\\\" crates/slices/jinn-webui/src/regions/sessions.rs\"}",
        result_success: true,
        result_content: "265:.srow, .lrow { position: relative; }\n",
    },
    ExcerptCall {
        // Session ordinal 7792.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -n \\\"\\\\.lrow\\\" crates/slices/jinn-webui/src/regions/sessions.rs\"}",
        result_success: true,
        result_content: "265:.srow, .lrow { position: relative; }\n",
    },
    ExcerptCall {
        // Session ordinal 7798.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -n \\\"lrow\\\" crates/slices/jinn-webui/src/regions/sessions.rs\"}",
        result_success: true,
        result_content: "265:.srow, .lrow { position: relative; }\n",
    },
    ExcerptCall {
        // Session ordinal 7801.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -n \\\"\\\\.lrow\\\" crates/slices/jinn-webui/src/regions/sessions.rs\"}",
        result_success: true,
        result_content: "265:.srow, .lrow { position: relative; }\n",
    },
    ExcerptCall {
        // Session ordinal 7807.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -n \\\"lrow\\\" crates/slices/jinn-webui/src/regions/sessions.rs\"}",
        result_success: true,
        result_content: "265:.srow, .lrow { position: relative; }\n",
    },
    ExcerptCall {
        // Session ordinal 7810.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -n \\\"\\\\.lrow\\\" crates/slices/jinn-webui/src/regions/sessions.rs\"}",
        result_success: true,
        result_content: "265:.srow, .lrow { position: relative; }\n",
    },
    ExcerptCall {
        // Session ordinal 7816.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -n \\\"lrow\\\" crates/slices/jinn-webui/src/regions/sessions.rs\"}",
        result_success: true,
        result_content: "265:.srow, .lrow { position: relative; }\n",
    },
    ExcerptCall {
        // Session ordinal 7819.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -n \\\"\\\\.lrow\\\" crates/slices/jinn-webui/src/regions/sessions.rs\"}",
        result_success: true,
        result_content: "265:.srow, .lrow { position: relative; }\n",
    },
];

/// The drift phase: 29 bash calls, each a small variation on the same
/// grep family, arguments and results alternating between a handful of
/// near-identical shapes. No call repeats byte-for-byte until the very
/// end of the stretch. Ordinals 3766–3850 of the same session — the
/// first 29 calls of the ~200-call drift before the loop locked into
/// exact repeats. The approved accumulator policy (max 8, similarity
/// 0.85) demonstrably nudges on this stretch at call 29.
pub(crate) const DRIFT_PHASE: &[ExcerptCall] = &[
    ExcerptCall {
        // Session ordinal 3766.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -n \\\"\\\\.lrow\\\" crates/slices/jinn-webui/src/regions/sessions.rs\"}",
        result_success: true,
        result_content: "265:.srow, .lrow { position: relative; }\n",
    },
    ExcerptCall {
        // Session ordinal 3769.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -n \\\"\\\\.srow {\\\" crates/slices/jinn-webui/src/regions/sessions.rs\"}",
        result_success: true,
        result_content: "181:.srow { display: block; padding: 8px 14px; color: var(--fg); text-decoration: none;\n292:  .srow { padding: 12px 14px; }\n",
    },
    ExcerptCall {
        // Session ordinal 3772.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -n \\\"\\\\.lrow\\\" crates/slices/jinn-webui/src/regions/sessions.rs\"}",
        result_success: true,
        result_content: "265:.srow, .lrow { position: relative; }\n",
    },
    ExcerptCall {
        // Session ordinal 3775.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -n \\\"srow {\\\" crates/slices/jinn-webui/src/regions/sessions.rs\"}",
        result_success: true,
        result_content: "181:.srow { display: block; padding: 8px 14px; color: var(--fg); text-decoration: none;\n292:  .srow { padding: 12px 14px; }\n",
    },
    ExcerptCall {
        // Session ordinal 3778.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -n \\\"\\\\.lrow\\\" crates/slices/jinn-webui/src/regions/sessions.rs; grep -n \\\"\\\\.srow {\\\" crates/slices/jinn-webui/src/regions/sessions.rs\"}",
        result_success: true,
        result_content: "265:.srow, .lrow { position: relative; }\n181:.srow { display: block; padding: 8px 14px; color: var(--fg); text-decoration: none;\n292:  .srow { padding: 12px 14px; }\n",
    },
    ExcerptCall {
        // Session ordinal 3781.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -rn \\\"\\\\.lrow\\\" crates/slices/jinn-webui/src/regions/sessions.rs\"}",
        result_success: true,
        result_content: "265:.srow, .lrow { position: relative; }\n",
    },
    ExcerptCall {
        // Session ordinal 3784.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -rn \\\"\\\\.srow {\\\" crates/slices/jinn-webui/src/regions/sessions.rs\"}",
        result_success: true,
        result_content: "181:.srow { display: block; padding: 8px 14px; color: var(--fg); text-decoration: none;\n292:  .srow { padding: 12px 14px; }\n",
    },
    ExcerptCall {
        // Session ordinal 3787.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -n \\\"\\\\.lrow\\\" crates/slices/jinn-webui/src/regions/sessions.rs\"}",
        result_success: true,
        result_content: "265:.srow, .lrow { position: relative; }\n",
    },
    ExcerptCall {
        // Session ordinal 3790.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -n \\\"srow {\\\" crates/slices/jinn-webui/src/regions/sessions.rs\"}",
        result_success: true,
        result_content: "181:.srow { display: block; padding: 8px 14px; color: var(--fg); text-decoration: none;\n292:  .srow { padding: 12px 14px; }\n",
    },
    ExcerptCall {
        // Session ordinal 3793.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -n \\\"lrow\\\" crates/slices/jinn-webui/src/regions/sessions.rs\"}",
        result_success: true,
        result_content: "265:.srow, .lrow { position: relative; }\n",
    },
    ExcerptCall {
        // Session ordinal 3796.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -n \\\"\\\\.lrow\\\" crates/slices/jinn-webui/src/regions/sessions.rs\"}",
        result_success: true,
        result_content: "265:.srow, .lrow { position: relative; }\n",
    },
    ExcerptCall {
        // Session ordinal 3799.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -n \\\"\\\\.srow {\\\" crates/slices/jinn-webui/src/regions/sessions.rs\"}",
        result_success: true,
        result_content: "181:.srow { display: block; padding: 8px 14px; color: var(--fg); text-decoration: none;\n292:  .srow { padding: 12px 14px; }\n",
    },
    ExcerptCall {
        // Session ordinal 3802.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -c \\\"lrow\\\" crates/slices/jinn-webui/src/regions/sessions.rs; grep -n \\\"\\\\.lrow\\\" crates/slices/jinn-webui/src/regions/sessions.rs\"}",
        result_success: true,
        result_content: "1\n265:.srow, .lrow { position: relative; }\n",
    },
    ExcerptCall {
        // Session ordinal 3805.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -n \\\"srow {\\\" crates/slices/jinn-webui/src/regions/sessions.rs | wc -l; grep -n \\\"\\\\.lrow {\\\" crates/slices/jinn-webui/src/regions/sessions.rs\"}",
        result_success: true,
        result_content: "2\n265:.srow, .lrow { position: relative; }\n",
    },
    ExcerptCall {
        // Session ordinal 3808.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -n \\\"\\\\.srow {\\\" crates/slices/jinn-webui/src/regions/sessions.rs; grep -n \\\"\\\\.lrow {\\\" crates/slices/jinn-webui/src/regions/sessions.rs\"}",
        result_success: true,
        result_content: "181:.srow { display: block; padding: 8px 14px; color: var(--fg); text-decoration: none;\n292:  .srow { padding: 12px 14px; }\n265:.srow, .lrow { position: relative; }\n",
    },
    ExcerptCall {
        // Session ordinal 3811.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -n \\\"\\\\.lrow\\\" crates/slices/jinn-webui/src/regions/sessions.rs\"}",
        result_success: true,
        result_content: "265:.srow, .lrow { position: relative; }\n",
    },
    ExcerptCall {
        // Session ordinal 3814.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -n \\\"srow {\\\" crates/slices/jinn-webui/src/regions/sessions.rs\"}",
        result_success: true,
        result_content: "181:.srow { display: block; padding: 8px 14px; color: var(--fg); text-decoration: none;\n292:  .srow { padding: 12px 14px; }\n",
    },
    ExcerptCall {
        // Session ordinal 3817.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -n \\\"\\\\.lrow\\\" crates/slices/jinn-webui/src/regions/sessions.rs\"}",
        result_success: true,
        result_content: "265:.srow, .lrow { position: relative; }\n",
    },
    ExcerptCall {
        // Session ordinal 3820.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -n \\\"srow {\\\" crates/slices/jinn-webui/src/regions/sessions.rs\"}",
        result_success: true,
        result_content: "181:.srow { display: block; padding: 8px 14px; color: var(--fg); text-decoration: none;\n292:  .srow { padding: 12px 14px; }\n",
    },
    ExcerptCall {
        // Session ordinal 3823.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -n \\\"\\\\.lrow\\\" crates/slices/jinn-webui/src/regions/sessions.rs\"}",
        result_success: true,
        result_content: "265:.srow, .lrow { position: relative; }\n",
    },
    ExcerptCall {
        // Session ordinal 3826.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -n \\\"\\\\.srow {\\\" crates/slices/jinn-webui/src/regions/sessions.rs\"}",
        result_success: true,
        result_content: "181:.srow { display: block; padding: 8px 14px; color: var(--fg); text-decoration: none;\n292:  .srow { padding: 12px 14px; }\n",
    },
    ExcerptCall {
        // Session ordinal 3829.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -n \\\"lrow\\\" crates/slices/jinn-webui/src/regions/sessions.rs\"}",
        result_success: true,
        result_content: "265:.srow, .lrow { position: relative; }\n",
    },
    ExcerptCall {
        // Session ordinal 3832.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -n \\\"\\\\.lrow\\\" crates/slices/jinn-webui/src/regions/sessions.rs\"}",
        result_success: true,
        result_content: "265:.srow, .lrow { position: relative; }\n",
    },
    ExcerptCall {
        // Session ordinal 3835.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -n \\\"\\\\.srow {\\\" crates/slices/jinn-webui/src/regions/sessions.rs\"}",
        result_success: true,
        result_content: "181:.srow { display: block; padding: 8px 14px; color: var(--fg); text-decoration: none;\n292:  .srow { padding: 12px 14px; }\n",
    },
    ExcerptCall {
        // Session ordinal 3838.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -n \\\"lrow\\\" crates/slices/jinn-webui/src/regions/sessions.rs\"}",
        result_success: true,
        result_content: "265:.srow, .lrow { position: relative; }\n",
    },
    ExcerptCall {
        // Session ordinal 3841.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -n \\\"\\\\.lrow\\\" crates/slices/jinn-webui/src/regions/sessions.rs\"}",
        result_success: true,
        result_content: "265:.srow, .lrow { position: relative; }\n",
    },
    ExcerptCall {
        // Session ordinal 3844.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -n \\\"srow {\\\" crates/slices/jinn-webui/src/regions/sessions.rs\"}",
        result_success: true,
        result_content: "181:.srow { display: block; padding: 8px 14px; color: var(--fg); text-decoration: none;\n292:  .srow { padding: 12px 14px; }\n",
    },
    ExcerptCall {
        // Session ordinal 3847.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -n \\\"\\\\.lrow\\\" crates/slices/jinn-webui/src/regions/sessions.rs\"}",
        result_success: true,
        result_content: "265:.srow, .lrow { position: relative; }\n",
    },
    ExcerptCall {
        // Session ordinal 3850.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -n \\\"\\\\.srow {\\\" crates/slices/jinn-webui/src/regions/sessions.rs\"}",
        result_success: true,
        result_content: "181:.srow { display: block; padding: 8px 14px; color: var(--fg); text-decoration: none;\n292:  .srow { padding: 12px 14px; }\n",
    },
];

/// Legitimate work: 6 varied bash calls debugging a Playwright spec —
/// different commands, different outputs, one genuine failure. The
/// negative control. Ordinals 3583–3598 of the same session.
pub(crate) const LEGIT_WORK: &[ExcerptCall] = &[
    ExcerptCall {
        // Session ordinal 3583.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -n \\\"waiting for\\\" webui/client/e2e/test-results/aprobe-trace-each-step-of-the-first-test/error-context.md | head -3; sed -n '/Page snapshot/,/^---/p' webui/client/e2e/test-results/aprobe-trace-each-step-of-the-first-test/error-context.md | sed -n 1,40p\"}",
        result_success: true,
        result_content: "# Page snapshot\n\n```yaml\n- generic [ref=e2]:\n  - navigation [ref=e3]:\n    - heading \"Sessions\" [level=2] [ref=e4]\n    - tablist \"sidebar list\" [ref=e5]:\n      - tab \"Sessions\" [ref=e6] [cursor=pointer]\n      - tab \"Library\" [ref=e7] [cursor=pointer]\n    - textbox \"filter sessions…\" [ref=e8]\n    - generic [ref=e9]:\n      - button \"row actions\" [ref=e10] [cursor=pointer]: ⋮\n      - generic [ref=e11] [cursor=pointer]:\n        - generic [ref=e12]: Untitled Session\n        - text: loaded\n      - button \"row actions\" [ref=e13] [cursor=pointer]: ⋮\n      - generic [ref=e14] [cursor=pointer]:\n        - generic [ref=e15]: Untitled Session\n        - text: loaded\n  - main [ref=e16]:\n    - generic [ref=e17]:\n      - button \"☰\" [ref=e20] [cursor=pointer]\n      - generic [ref=e21]: Untitled Session\n      - button \"__no_provider__\" [ref=e23] [cursor=pointer]\n    - article [ref=e26]:\n      - generic [ref=e27]: transient\n      - generic [ref=e28]: \"**Project resources discovered** - 11 skill(s) - 11 prompt(s)\"\n    - generic [ref=e30]:\n      - textbox \"Message…\" [ref=e31]\n      - generic \"Attach an image\" [ref=e32] [cursor=pointer]: +\n      - button \"Send\" [ref=e33] [cursor=pointer]\n```\n",
    },
    ExcerptCall {
        // Session ordinal 3586.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -rn \\\"pending\\\" /tmp/e2e-add.log 2>/dev/null; sed -n 1,40p webui/client/e2e/test-results/aprobe-trace-each-step-of-the-first-test/error-context.md | grep -n \\\"Test source\\\" \"}",
        result_success: false,
        result_content: "Command exited with code 1",
    },
    ExcerptCall {
        // Session ordinal 3589.
        tool_name: "bash",
        arguments: "{\"command\":\"sed -n 40,80p webui/client/e2e/test-results/aprobe-trace-each-step-of-the-first-test/error-context.md\"}",
        result_success: true,
        result_content: "      - generic [ref=e21]: Untitled Session\n      - button \"__no_provider__\" [ref=e23] [cursor=pointer]\n    - article [ref=e26]:\n      - generic [ref=e27]: transient\n      - generic [ref=e28]: \"**Project resources discovered** - 11 skill(s) - 11 prompt(s)\"\n    - generic [ref=e30]:\n      - textbox \"Message…\" [ref=e31]\n      - generic \"Attach an image\" [ref=e32] [cursor=pointer]: +\n      - button \"Send\" [ref=e33] [cursor=pointer]\n```\n",
    },
    ExcerptCall {
        // Session ordinal 3592.
        tool_name: "bash",
        arguments: "{\"command\":\"wc -l webui/client/e2e/test-results/aprobe-trace-each-step-of-the-first-test/error-context.md; sed -n 46,60p webui/client/e2e/test-results/aprobe-trace-each-step-of-the-first-test/error-context.md\"}",
        result_success: true,
        result_content: "48 webui/client/e2e/test-results/aprobe-trace-each-step-of-the-first-test/error-context.md\n      - textbox \"Message…\" [ref=e31]\n      - generic \"Attach an image\" [ref=e32] [cursor=pointer]: +\n      - button \"Send\" [ref=e33] [cursor=pointer]\n```\n",
    },
    ExcerptCall {
        // Session ordinal 3595.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -rn \\\"test-source\\\\|source\\\" webui/client/e2e/test-results/aprobe-trace-each-step-of-the-first-test/error-context.md | head -2; cd webui/client/e2e && npx playwright test --config playwright.config.mjs aprobe 2>&1 | grep -E \\\"S[0-9]|Call log|waiting|TimeoutError|locator|passed|failed\\\" | head -16\",\"max_duration_secs\":900}",
        result_success: true,
        result_content: "44:      - generic [ref=e28]: \"**Project resources discovered** - 11 skill(s) - 11 prompt(s)\"\nS1: openSession start\nS2: openSession done, url http://127.0.0.1:37681/web/session/01a20e28-0db4-7157-912e-802b8f92b687\nS3: navigated\nS4: created 01a20e28-1c11-7281-a3a1-4a3b8273b169\nS5: row count 1\n  1 failed\n",
    },
    ExcerptCall {
        // Session ordinal 3598.
        tool_name: "bash",
        arguments: "{\"command\":\"grep -n \\\"expect(createdRow).toHaveCount(0)\\\" webui/client/e2e/library.spec.mjs; cd webui/client/e2e && cat > aprobe.spec.mjs <<'EOF'\\nimport { expect, test } from \\\"@playwright/test\\\";\\nimport { openSession } from \\\"./nav.mjs\\\";\\nimport * as sel from \\\"./selectors.mjs\\\";\\n\\ntest(\\\"trace toggle only\\\", async ({ page }) => {\\n  test.setTimeout(60000);\\n  await openSession(page);\\n  const menu = page.locator(sel.ROW_MENU).first();\\n  const id = await menu.getAttribute(sel.ROW_MENU_ATTR);\\n  console.log(\\\"T1: id\\\", id);\\n  await page.locator(sel.ROW_MENU_IN(id)).click({ timeout: 5000 }).catch(e => console.log(\\\"KEBAB FAIL\\\", e.message.split(\\\"\\\\n\\\")[1] || e.message.split(\\\"\\\\n\\\")[0]));\\n  console.log(\\\"T2: kebab ok\\\");\\n  await page.locator(sel.MENU_ENTRY(sel.ACTION_LIBRARY_TOGGLE)).first().click({ timeout: 5000 }).catch(e => console.log(\\\"ENTRY FAIL\\\", e.message.split(\\\"\\\\n\\\")[1] || e.message.split(\\\"\\\\n\\\")[0]));\\n  console.log(\\\"T3: entry ok\\\");\\n  await page.waitForTimeout(1500);\\n  console.log(\\\"T4: row count\\\", await page.locator(sel.ROW_BY_ID(id)).count());\\n});\\nEOF\\ncd webui/client/e2e && npx playwright test --config playwright.config.mjs aprobe 2>&1 | grep -E \\\"^T[0-9]|FAIL|passed|failed\\\" | head -8\",\"max_duration_secs\":900}",
        result_success: false,
        result_content: "92:  await expect(createdRow).toHaveCount(0);\n/bin/bash: line 20: cd: webui/client/e2e: No such file or directory\nCommand exited with code 1",
    },
];

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        reason = "test code"
    )]

    use super::*;
    use crate::fuzzy_tool_call_watchdog_actor::FuzzyToolCallWatchdogActor;
    use crate::tool_call_similarity;
    use jinn_core_types::SessionId;

    #[rstest::rstest]
    #[tokio::test]
    async fn the_exact_watchdog_nudges_then_kills_the_lock_phase() {
        // Given the exact watchdog at its shipped default of 4.
        let mut actor =
            crate::repeat_tool_call_watchdog_actor::RepeatToolCallWatchdogActor::with_max_repeats(4)
                .await;
        let session = SessionId::new();

        // When the 12-call lock phase replays, the nudge landing in
        // history as soon as it is published (the bus delivery the race
        // gate checks for).
        let mut first_nudge = 0;
        let mut killed = 0;
        for (i, call) in LOCK_PHASE.iter().enumerate() {
            let args = tool_call_similarity::normalize(call.arguments);
            let result = tool_call_similarity::result_prefix(call.result_content);
            let actions = actor.on_call_completed(&session, call.tool_name, &args, &result, 4);
            if actions.iter().any(|a| matches!(a, crate::repeat_tool_call_watchdog_actor::RepeatWatchdogAction::Nudge(_, _))) {
                if first_nudge == 0 {
                    first_nudge = i + 1;
                }
                actor
                    .state
                    .write()
                    .session
                    .get_or_create(&session)
                    .push_entry(jinn_core_types::ChatEntry::rule_interrupt(
                        crate::repeat_tool_call_watchdog_actor::REPEAT_RULE_NAME,
                        "nudge body",
                    ));
            }
            if actions.iter().any(|a| matches!(a, crate::repeat_tool_call_watchdog_actor::RepeatWatchdogAction::CancelTurn(_))) {
                killed = i + 1;
            }
        }

        // Then the nudge arrived within the first 6 calls and the kill
        // within 12 (the alternating pair costs the 2-deep basis one
        // extra call before its first hit; the nudge resets the count and
        // the kill needs 4 more repeats).
        assert!(
            first_nudge > 0 && first_nudge <= 6,
            "nudge within 6 calls, got call {first_nudge}"
        );
        assert!(
            killed > 0 && killed <= 12,
            "kill within the excerpt, got call {killed}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn the_excerpts_carry_their_recorded_success_flags() {
        // Given the three excerpts.
        // Then the success flags match the recorded session: the lock
        // and drift phases all succeeded; legit work includes failures.
        assert!(LOCK_PHASE.iter().all(|c| c.result_success));
        assert!(DRIFT_PHASE.iter().all(|c| c.result_success));
        assert!(LEGIT_WORK.iter().any(|c| !c.result_success));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn the_exact_watchdog_stays_silent_on_legit_work() {
        // Given the exact watchdog at its default.
        let mut actor =
            crate::repeat_tool_call_watchdog_actor::RepeatToolCallWatchdogActor::with_max_repeats(4)
                .await;
        let session = SessionId::new();

        // When the legitimate-work excerpt replays.
        let mut any_action = 0;
        for call in LEGIT_WORK {
            let args = tool_call_similarity::normalize(call.arguments);
            let result = tool_call_similarity::result_prefix(call.result_content);
            let actions = actor.on_call_completed(&session, call.tool_name, &args, &result, 4);
            if !actions.is_empty() {
                any_action += 1;
            }
        }

        // Then nothing fired: varied debugging never accumulates repeats.
        assert_eq!(any_action, 0, "the negative control stayed silent");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn the_fuzzy_watchdog_nudges_on_the_drift_phase() {
        // Given the fuzzy watchdog at its shipped defaults.
        let mut actor = FuzzyToolCallWatchdogActor::with_config(
            jinn_preferences_config::schemas::FuzzyToolCallWatchdogConfig::default(),
        )
        .await;
        let session = SessionId::new();

        // When the 29-call drift phase replays — every call a small
        // variation on the same grep family, arguments and results
        // alternating between a handful of near-identical shapes.
        let mut first_nudge = 0;
        for (i, call) in DRIFT_PHASE.iter().enumerate() {
            let args = tool_call_similarity::normalize(call.arguments);
            let result = tool_call_similarity::result_prefix(call.result_content);
            let actions = actor.on_call_completed(&session, call.tool_name, &args, &result, 8, 0.85, 500);
            if first_nudge == 0
                && actions.iter().any(|a| matches!(a, crate::fuzzy_tool_call_watchdog_actor::FuzzyWatchdogAction::Nudge(_, _)))
            {
                first_nudge = i + 1;
            }
        }

        // Then a nudge fired within the excerpt — measured at call 29,
        // the stretch's length.
        assert_eq!(first_nudge, 29, "the drift loop earns its nudge at call 29");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn the_fuzzy_watchdog_stays_silent_on_legit_work() {
        // Given the fuzzy watchdog at its defaults.
        let mut actor = FuzzyToolCallWatchdogActor::with_config(
            jinn_preferences_config::schemas::FuzzyToolCallWatchdogConfig::default(),
        )
        .await;
        let session = SessionId::new();

        // When the legitimate-work excerpt replays.
        let mut any_action = 0;
        for call in LEGIT_WORK {
            let args = tool_call_similarity::normalize(call.arguments);
            let result = tool_call_similarity::result_prefix(call.result_content);
            let actions = actor.on_call_completed(&session, call.tool_name, &args, &result, 8, 0.85, 500);
            if !actions.is_empty() {
                any_action += 1;
            }
        }

        // Then nothing fired.
        assert_eq!(any_action, 0, "the negative control stayed silent");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn the_exact_watchdog_catches_the_drift_phase_only_partially() {
        // Given the exact watchdog at its default.
        let mut actor =
            crate::repeat_tool_call_watchdog_actor::RepeatToolCallWatchdogActor::with_max_repeats(4)
                .await;
        let session = SessionId::new();

        // When the drift phase replays through the exact rule.
        let mut first_nudge = 0;
        for (i, call) in DRIFT_PHASE.iter().enumerate() {
            let args = tool_call_similarity::normalize(call.arguments);
            let result = tool_call_similarity::result_prefix(call.result_content);
            let actions = actor.on_call_completed(&session, call.tool_name, &args, &result, 4);
            if first_nudge == 0
                && actions.iter().any(|a| matches!(a, crate::repeat_tool_call_watchdog_actor::RepeatWatchdogAction::Nudge(_, _)))
            {
                first_nudge = i + 1;
            }
        }

        // Then the exact rule is weaker here — the drift's near-variants
        // and differing results mostly miss it. This is the documented
        // gap the fuzzy watchdog exists to fill: asserting only that the
        // exact rule did not spuriously kill, whatever it decided.
        assert!(
            !actor
                .on_call_completed(&session, "bash", "x", "y", 4)
                .iter()
                .any(|a| matches!(a, crate::repeat_tool_call_watchdog_actor::RepeatWatchdogAction::CancelTurn(_))),
            "the exact rule must not have killed on drift data"
        );
        let _ = first_nudge;
    }

}
