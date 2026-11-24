//! Playwright config for the webui e2e tier.
//!
//! One worker, headless chromium, the list reporter, and a trace kept
//! for every failure. Each migrated test boots its own jinn through
//! the fixture (`fixture.mjs`). The global setup/teardown hooks below
//! still boot ONE shared instance for the specs not yet migrated onto
//! the fixture; they go away with those specs' migration, once no
//! spec reads a base URL from a file.
//!
//! A failure prints the runner's full report (expected, received, the
//! call log) and leaves a trace under `test-results/<test>/trace.zip`,
//! which `playwright show-trace` replays. That report is the point of
//! the run — read it whole, never filtered.

import path from "node:path";
import { fileURLToPath } from "node:url";
import { defineConfig, devices } from "@playwright/test";

const here = path.dirname(fileURLToPath(import.meta.url));

export default defineConfig({
  testDir: here,
  fullyParallel: false,
  workers: 1,
  retries: 0,
  // Boots and tears down the one shared instance the not-yet-migrated
  // specs run against. Removed when every spec is on the fixture.
  globalSetup: path.join(here, "global-setup.mjs"),
  globalTeardown: path.join(here, "global-teardown.mjs"),
  reporter: [["list"]],
  timeout: 30_000,
  use: {
    headless: true,
    viewport: { width: 1280, height: 800 },
    ...devices["Desktop Chrome"],
    // The attachments spec's loopback TLS fixture serves a committed
    // self-signed cert; the page's own fetches must not fail on it.
    ignoreHTTPSErrors: true,
    trace: "retain-on-failure",
  },
});
