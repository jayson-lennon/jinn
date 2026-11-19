//! Playwright config for the webui e2e tier.
//!
//! One jinn per run, serial specs, headless chromium. The base URL is
//! NOT set here: the config is evaluated before globalSetup boots
//! jinn, so specs read the URL at import time from `run.mjs`.

import path from "node:path";
import { fileURLToPath } from "node:url";
import { defineConfig, devices } from "@playwright/test";

const here = path.dirname(fileURLToPath(import.meta.url));

export default defineConfig({
  testDir: here,
  fullyParallel: false,
  workers: 1,
  retries: 0,
  globalSetup: path.join(here, "global-setup.mjs"),
  globalTeardown: path.join(here, "global-teardown.mjs"),
  reporter: [["list"]],
  timeout: 30_000,
  use: {
    headless: true,
    viewport: { width: 1280, height: 800 },
    ...devices["Desktop Chrome"],
  },
});
