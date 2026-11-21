//! The jinn subprocess lifecycle for the webui e2e tier.
//!
//! One run of `just webui-e2e` boots one real jinn binary in a scratch
//! paths root: probe a free loopback port, write scratch config files
//! (sample-only providers, the probed port in `[webui] bind`), spawn
//! the binary detached so the whole process group dies together, poll
//! the bind until it accepts, and tear it all down. Nothing outside
//! the scratch root is read or written — `JINN_PATHS_ROOT` redirects
//! config/data/state discovery, `--db-path` and `--config` pin the
//! rest. See `.plans/webui-e2e/plan.md` for the spec.

import { spawn } from "node:child_process";
import net from "node:net";
import fs from "node:fs";
import path from "node:path";
import os from "node:os";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));

/** Where the run's pid/baseUrl bookkeeping file lives (gitignored). */
const RUN_DIR = path.join(here, ".run");
/** Bookkeeping file so teardown works even after a crashed run. */
const RUN_FILE = path.join(RUN_DIR, "run.json");

/** Root of the repo (three up from webui/client/e2e). */
const REPO_ROOT = path.resolve(here, "..", "..", "..");
/** The debug binary the recipe builds before the suite runs. */
const BIN_PATH = path.join(REPO_ROOT, "target", "debug", "jinn");
/** The scratch luau script the `[mcp.echo]` entry serves. */
const MCP_ECHO_SCRIPT = path.join(here, "mcp-echo-tool.lua");
/** Where the scratch roots are created. */
const TMP_BASE = path.join(os.tmpdir(), "jinn-webui-e2e");

/** How long to wait for the bind before declaring boot failed. */
const BIND_TIMEOUT_MS = 30_000;
/** Delay between bind probes. */
const BIND_POLL_MS = 250;
/** SIGTERM grace before escalating to SIGKILL. */
const KILL_GRACE_MS = 5_000;

/** The scratch providers.toml — sample-only, keyless, network-free.
 *
 * `ghost` exists so the picker has an unavailable row: it requires a
 * key from an env var no run sets, so it always renders with the
 * unavailable attribute and no switch action.
 */
const PROVIDERS_TOML = `version = 1

default_provider = "sample"

[providers.sample]
backend = "sample"
requires_key = false
models = ["sample"]

[providers.ghost]
backend = "openai"
requires_key = true
api_key_env = "JINN_E2E_NEVER_SET_KEY"
models = ["phantom"]
`;

/**
 * Builds the scratch jinn.toml body — the bind is the only knob the
 * tier turns; the port is probed so the config can name it.
 *
 * The `[mcp.echo]` entry points at the repo's debug binary serving the
 * scratch echo tool over stdio (`jinn luau serve`, network-free), so
 * the MCP modal has a real server whose coordinator can reach
 * `running` — the connection respawns per click on the session page.
 */
function jinnTomlFor(port) {
  return `version = 1

[webui]
bind = "127.0.0.1:${port}"

# The new-session spec needs one configured project and one
# parameterized lifecycle to create from the browser.
[[project.entry]]
path = "~/e2e-scratch"

[[session_lifecycle.script]]
name = "echo-setup"
setup_command = "echo $1"

[mcp.echo]
command = "${BIN_PATH}"
args = ["luau", "serve", "${MCP_ECHO_SCRIPT}"]
`;
}

/**
 * Probes the kernel for a free loopback port.
 *
 * # Errors
 *
 * Rejects when the kernel refuses to hand out a port.
 */
export function probePort() {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address();
      server.close(() => resolve(port));
    });
  });
}

/** Staged scratch persona body — written into the run's personas dir. */
let stagedPersona;

/**
 * Stages one scratch persona `.md` for the run's personas dir.
 *
 * MUST run before {@linkcode startJinn}: the persona scan happens at
 * activation, so a file written after boot is invisible for the whole
 * run. {@linkcode writeScratch} lands it in
 * `<root>/config/jinn/personas/` under the SAME root the run boots
 * with — a separate temp dir would be invisible (`JINN_PATHS_ROOT`).
 *
 * @param {string} content the full persona file body (frontmatter +
 *   markdown).
 */
export function writeScratchPersona(content) {
  stagedPersona = content;
}

/**
 * Writes the scratch tree: config dir, providers.toml, jinn.toml, db.
 *
 * Both files live under `<root>/config/jinn/` — `AppPaths::new_in`
 * sets the config dir to `<root>/config` and the file paths append
 * `jinn/`, so anything written one level up is invisible to the run
 * (which would then seed a default providers.toml over it).
 *
 * # Errors
 *
 * Rejects when the scratch tree cannot be created.
 */
export function writeScratch(root, port) {
  const configDir = path.join(root, "config", "jinn");
  fs.mkdirSync(configDir, { recursive: true });
  fs.mkdirSync(path.join(root, "db"), { recursive: true });
  fs.writeFileSync(path.join(configDir, "providers.toml"), PROVIDERS_TOML);
  fs.writeFileSync(path.join(configDir, "jinn.toml"), jinnTomlFor(port));
  if (stagedPersona !== undefined) {
    const personasDir = path.join(configDir, "personas");
    fs.mkdirSync(personasDir, { recursive: true });
    fs.writeFileSync(path.join(personasDir, "blockless-e2e.md"), stagedPersona);
  }
}

/**
 * Reads the last `lines` lines of the jinn log for failure reports.
 *
 * jinn writes its log to `<paths-root>/state/jinn/jinn.log` — the
 * paths root IS the scratch root, so the tail is read from there.
 *
 * # Errors
 *
 * Returns a placeholder when the log does not exist or is unreadable.
 */
function logTail(root, lines) {
  const logPath = path.join(root, "state", "jinn", "jinn.log");
  try {
    const text = fs.readFileSync(logPath, "utf8");
    const all = text.split("\n");
    return all.slice(Math.max(0, all.length - lines)).join("\n");
  } catch {
    return `(no jinn log at ${logPath})`;
  }
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/**
 * Polls the bind until a TCP connect succeeds or the deadline passes.
 *
 * `bootRoot` is the scratch root the log tail is read from on failure.
 *
 * # Errors
 *
 * Rejects with the jinn log tail when the boot deadline expires.
 */
export async function awaitBind(port, bootRoot) {
  const deadline = Date.now() + BIND_TIMEOUT_MS;
  for (;;) {
    const open = await new Promise((resolve) => {
      const socket = net.connect({ port, host: "127.0.0.1" });
      socket.once("connect", () => {
        socket.destroy();
        resolve(true);
      });
      socket.once("error", () => {
        socket.destroy();
        resolve(false);
      });
    });
    if (open) return;
    if (Date.now() > deadline) {
      throw new Error(
        `jinn did not bind 127.0.0.1:${port} within ${BIND_TIMEOUT_MS}ms\n` +
          `--- jinn.log tail ---\n${bootRoot ? logTail(bootRoot, 40) : "(no root yet)"}`,
      );
    }
    await sleep(BIND_POLL_MS);
  }
}

/**
 * Kills a stale run left behind by a crashed earlier run, if any.
 *
 * # Errors
 *
 * Never rejects — a missing or already-dead stale run is a no-op.
 */
export function cleanupStaleRun() {
  let stale;
  try {
    stale = JSON.parse(fs.readFileSync(RUN_FILE, "utf8"));
  } catch {
    return;
  }
  if (typeof stale.pid === "number") {
    try {
      process.kill(-stale.pid, "SIGKILL");
    } catch {
      // Already gone — the normal case after a clean teardown.
    }
  }
  if (typeof stale.root === "string") {
    fs.rmSync(stale.root, { recursive: true, force: true });
  }
  fs.rmSync(RUN_DIR, { recursive: true, force: true });
}

/**
 * Boots one scratch-isolated jinn and returns its run coordinates.
 *
 * The binary is spawned detached (its own process group) so teardown
 * can kill the whole group — jinn spawns children of its own. Stdout
 * and stderr stream into the run dir's jinn.log for diagnostics.
 *
 * # Errors
 *
 * Rejects when the binary is missing, the scratch tree cannot be
 * written, or the boot deadline expires before the bind accepts.
 */
export async function startJinn() {
  cleanupStaleRun();
  fs.mkdirSync(RUN_DIR, { recursive: true });
  fs.mkdirSync(TMP_BASE, { recursive: true });

  if (!fs.existsSync(BIN_PATH)) {
    throw new Error(
      `jinn binary not found at ${BIN_PATH} — build it first (cargo build --bin jinn)`,
    );
  }

  const port = await probePort();
  const root = fs.mkdtempSync(path.join(TMP_BASE, "run-"));
  writeScratch(root, port);

  // The webui binds during system activation in every mode, but only
  // a run that stays alive serves it: `headless` runs the activation,
  // then exits once settled (it is a batch runner), and the default
  // TUI mode needs a tty — without one it dies at "failed to enable
  // raw mode" right after binding. So the binary runs under
  // util-linux `script`, which hands it a pty and keeps the process
  // (and the webui) up for the whole suite. `setsid` detaches the
  // wrapper into its own session/process group so teardown can kill
  // the whole tree with one group signal.
  const child = spawn(
    "setsid",
    [
      "script",
      "-qfec",
      `${BIN_PATH} --db-path ${path.join(root, "db", "state.db")} --config ${path.join(root, "config", "jinn", "jinn.toml")}`,
      "/dev/null",
    ],
    {
      env: { ...process.env, JINN_PATHS_ROOT: root },
      detached: true,
      stdio: "ignore",
    },
  );
  child.unref();

  try {
    await awaitBind(port, root);
  } catch (err) {
    try {
      process.kill(-child.pid, "SIGKILL");
    } catch {
      // The process may have already exited on its own.
    }
    fs.rmSync(root, { recursive: true, force: true });
    throw err;
  }

  const run = {
    pid: child.pid,
    port,
    root,
    baseUrl: `http://127.0.0.1:${port}`,
  };
  fs.writeFileSync(RUN_FILE, JSON.stringify(run, null, 2));
  return run;
}

/**
 * Escalating process-group kill: SIGTERM, grace period, SIGKILL.
 *
 * # Errors
 *
 * Never rejects — killing an already-dead group is a no-op.
 */
async function killGroup(pid) {
  try {
    process.kill(-pid, "SIGTERM");
  } catch {
    return;
  }
  const deadline = Date.now() + KILL_GRACE_MS;
  for (;;) {
    let alive = false;
    try {
      process.kill(-pid, 0);
      alive = true;
    } catch {
      alive = false;
    }
    if (!alive) return;
    if (Date.now() > deadline) break;
    await sleep(100);
  }
  try {
    process.kill(-pid, "SIGKILL");
  } catch {
    // Gone between the check and the kill — fine.
  }
}

/**
 * Tears the run down: kill the process group, remove the scratch root
 * and the run bookkeeping.
 *
 * # Errors
 *
 * Never rejects — teardown best-effort cleans whatever exists.
 */
export async function teardown(run) {
  if (run && typeof run.pid === "number") {
    await killGroup(run.pid);
  } else {
    // No coordinates in hand — fall back to the bookkeeping file.
    try {
      const stale = JSON.parse(fs.readFileSync(RUN_FILE, "utf8"));
      if (typeof stale.pid === "number") await killGroup(stale.pid);
    } catch {
      // Nothing recorded; nothing to kill.
    }
  }
  if (run && typeof run.root === "string") {
    fs.rmSync(run.root, { recursive: true, force: true });
  }
  fs.rmSync(RUN_DIR, { recursive: true, force: true });
}
