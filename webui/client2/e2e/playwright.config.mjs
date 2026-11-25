//! Playwright config for the webui2 e2e tier.
//!
//! One worker, headless chromium, the list reporter, and a trace kept
//! for every failure. There is no global setup or teardown: every test
//! boots its own jinn through the fixture and tears it down when it
//! ends, so nothing is shared between tests and the order the tests run
//! in carries no meaning. The recipes shuffle that order and print the
//! seed, so an order dependence fails with the seed that reproduces it.

import path from "node:path";
import { fileURLToPath } from "node:url";
import { defineConfig, devices } from "@playwright/test";

const here = path.dirname(fileURLToPath(import.meta.url));

export default defineConfig({
  testDir: here,
  fullyParallel: false,
  workers: 1,
  retries: 0,
  reporter: [["list"]],
  timeout: 30_000,
  use: {
    headless: true,
    viewport: { width: 1280, height: 800 },
    ...devices["Desktop Chrome"],
    trace: "retain-on-failure",
  },
});
