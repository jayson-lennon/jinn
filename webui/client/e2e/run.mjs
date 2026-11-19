//! The run coordinates, readable by specs at import time.
//!
//! Playwright evaluates the config before `globalSetup` boots jinn,
//! so the config cannot name the base URL — but every spec module is
//! imported after setup has run, and each reads its URL from here.

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const BASE_URL_FILE = path.join(here, ".run", "base-url.txt");

/**
 * The running jinn's base URL. Import-time read: setup wrote it.
 *
 * # Errors
 *
 * Throws when global setup never ran (run specs only via Playwright).
 */
export function baseUrl() {
  return fs.readFileSync(BASE_URL_FILE, "utf8").trim();
}
