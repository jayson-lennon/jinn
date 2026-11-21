//! Playwright global setup — boots the scratch jinn once per run.
//!
//! After the bind accepts, the boot session is switched to the sample
//! model: a fresh db has no `last_model`, so the session would
//! otherwise sit on `__no_provider__` and the picker flows would have
//! nothing to be "current". The switch goes through the same POST the
//! page's own delegation issues, so it doubles as a boot-of-record
//! that the switch endpoint works.
//!
//! Writes the run coordinates (baseUrl especially) where both the
//! Playwright config and the teardown hook can read them.

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { startJinn, writeScratchPersona, writeScratchSkill } from "./harness.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
export const BASE_URL_FILE = path.join(here, ".run", "base-url.txt");

const BOOT_MODEL = "sample/sample";

/**
 * The block-less scratch persona the persona-picker spec pivots on.
 *
 * Every shipped persona carries a `<compaction>` block, so the
 * block-less warning path (switch allowed, loud error entry) would
 * never be exercised against shipped data alone. Written into the
 * scratch personas dir before jinn boots — the scan happens at
 * activation, so a file written after boot is invisible.
 */
const BLOCKLESS_PERSONA = `+++
name = "blockless-e2e"
description = "scratch persona without a compaction handoff block"
+++

You are the e2e scratch persona. Keep replies to one line.
`;

/**
 * The scratch skill the skills spec pivots on.
 *
 * Discovered from `<root>/.agents/skills/` (the global skills dir —
 * under `JINN_PATHS_ROOT` the root IS the home). Written before jinn
 * boots for the same reason the persona is: discovery scans at
 * activation, so a file written after boot is invisible. Valid
 * frontmatter required — the scanner parses `name` (which must match
 * the parent directory) and `description`.
 */
const SKILL = "e2e-scratch-skill";
const SKILL_BODY = `---
name: ${SKILL}
description: scratch skill for the skills modal flow
---

# e2e scratch skill

Say the magic word: e2e-scratch-skill-body-marker.
`;

/**
 * Activates the boot session's sample model over the web surface.
 *
 * # Errors
 *
 * Throws when the sessions list or the switch endpoint misbehaves —
 * a boot that cannot switch models cannot run the suite.
 */
async function switchBootModel(baseUrl) {
  const sessionsHtml = await fetch(`${baseUrl}/web/sessions`).then((r) => r.text());
  const sessionId = /data-session-id="([^"]+)"/.exec(sessionsHtml)?.[1];
  if (!sessionId) throw new Error("no session row in the freshly booted sessions list");

  const body = new URLSearchParams({
    session_id: sessionId,
    provider_id: BOOT_MODEL,
  });
  const res = await fetch(`${baseUrl}/web/model`, {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body,
  });
  if (!res.ok) {
    throw new Error(`model switch failed: ${res.status} ${await res.text()}`);
  }
  return sessionId;
}

/** @returns {Promise<void>} Playwright's global-setup contract. */
export default async function globalSetup() {
  writeScratchPersona(BLOCKLESS_PERSONA);
  writeScratchSkill(SKILL, SKILL_BODY);
  const run = await startJinn();
  const sessionId = await switchBootModel(run.baseUrl);
  fs.mkdirSync(path.dirname(BASE_URL_FILE), { recursive: true });
  fs.writeFileSync(BASE_URL_FILE, run.baseUrl);
  process.stdout.write(`==> jinn up at ${run.baseUrl} (session ${sessionId})\n`);
}
