//! Playwright global teardown — kills the jinn process group and
//! removes the scratch tree, whatever shape the run ended in.

import fs from "node:fs";
import { teardown } from "./harness.mjs";
import { BASE_URL_FILE } from "./global-setup.mjs";

/** @returns {Promise<void>} Playwright's global-teardown contract. */
export default async function globalTeardown() {
  await teardown(null);
  fs.rmSync(BASE_URL_FILE, { force: true });
  process.stdout.write("==> jinn torn down\n");
}
