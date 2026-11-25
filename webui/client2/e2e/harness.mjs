//! The jinn subprocess lifecycle for the webui2 e2e tier.
//!
//! Every e2e test boots its own jinn against its own scratch root — a
//! fresh database, config, data, cache, and state tree — and tears it
//! down when the test ends, whatever way it ends. Nothing a test does
//! can be seen by another test, because nothing outlives the test.
//!
//! One launch site. `bootJinn` is the only place the binary is
//! spawned, and it is hermetic by construction:
//!
//! - the paths-root override (`JINN_PATHS_ROOT`) rebinds every base
//!   directory under the scratch root;
//! - `--db-path` and `--config` name files inside that root, so the
//!   real `~/.config/jinn` and `~/.local/share/jinn` are never opened;
//! - the working directory is inside the root, so the session-init scan
//!   reads the scratch root's project-local skills, not the checkout's;
//! - the environment is BUILT from an allowlist, never inherited.
//!
//! Both browser surfaces bind: a jinn always boots the original web UI
//! too, and it is fail-fast on its port, so a scratch config naming only
//! `[webui2]` would die at launch on the fixed default. Each boot probes
//! a free port for each and writes both sections.

import { spawn } from "node:child_process";
import net from "node:net";
import fs from "node:fs";
import path from "node:path";
import os from "node:os";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));

/** Root of the repo (three up from webui/client2/e2e). */
const REPO_ROOT = path.resolve(here, "..", "..", "..");
/** The debug binary the e2e recipes build before every run. */
const BIN_PATH = path.join(REPO_ROOT, "target", "debug", "jinn");
/**
 * The scratch roots live one level deeper, under this worktree's slug —
 * the sweep in `just webui2-e2e-clean` matches the slug-scoped cmdline
 * pattern, so it can only ever kill this worktree's processes.
 */
const WORKTREE_SLUG = path.basename(REPO_ROOT);
/** Where the scratch roots are created — per-worktree scoped. */
export const TMP_BASE = path.join(os.tmpdir(), "jinn-webui2-e2e", WORKTREE_SLUG);

/** How long to wait for the bind before declaring boot failed. */
const BIND_TIMEOUT_MS = 30_000;
/** Delay between bind probes. */
const BIND_POLL_MS = 50;

/**
 * The environment every instance receives — an allowlist, not an
 * inheritance. `SHELL` is pinned to `/bin/sh` so `script -c` launches
 * the binary through a known shell that `exec`s it. `TERM=dumb` keeps
 * the pty wrapper from emitting terminal queries nothing answers.
 */
function allowedEnv(root) {
  const keep = (name) => (process.env[name] === undefined ? {} : { [name]: process.env[name] });
  return {
    PATH: process.env.PATH ?? "/usr/local/bin:/usr/bin:/bin",
    HOME: root,
    TMPDIR: path.join(root, "tmp"),
    SHELL: "/bin/sh",
    TERM: "dumb",
    LANG: process.env.LANG ?? "C.UTF-8",
    ...keep("LC_ALL"),
    // The paths-root override — the isolation lever. Rebinds config,
    // data, cache, state, and home under the scratch root.
    JINN_PATHS_ROOT: root,
  };
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

/** The scratch providers.toml — sample-only, keyless, network-free. */
const PROVIDERS_TOML = `version = 1

default_provider = "sample"

[providers.sample]
backend = "sample"
requires_key = false
models = ["sample"]

# A second, selectable, keyless provider: the model-picker spec needs a
# row that is not the current one to switch to.
[providers.second]
backend = "sample"
requires_key = false
models = ["second-model"]
`;

/**
 * The scratch jinn.toml body. Both browser surfaces bind a probed
 * port; the original UI is fail-fast on a taken port and every jinn
 * boots it.
 *
 * The project entry and the lifecycle script exist for one spec: a
 * session-activation test needs a second session in the store, `/web2`
 * has no new-session message, and the original UI's new-session modal
 * creates one — but only against a configured project, offering no
 * submit control without it.
 */
function jinnTomlFor(port, webui2Port) {
  return `version = 1

[webui]
bind = "127.0.0.1:${port}"

[webui2]
bind = "127.0.0.1:${webui2Port}"

[[project.entry]]
path = "~/e2e-scratch"

[[session_lifecycle.script]]
name = "echo-setup"
setup_command = "echo $1"
`;
}

/** Writes the config dir's two files. */
function writeConfig(root, port, webui2Port) {
  const configDir = path.join(root, "config", "jinn");
  fs.mkdirSync(configDir, { recursive: true });
  fs.writeFileSync(path.join(configDir, "providers.toml"), PROVIDERS_TOML);
  fs.writeFileSync(path.join(configDir, "jinn.toml"), jinnTomlFor(port, webui2Port));
}

/** The empty trees every scratch root starts with. */
function writeSkeleton(root) {
  for (const dir of ["db", "tmp", "cwd"]) {
    fs.mkdirSync(path.join(root, dir), { recursive: true });
  }
}

/** Writes one scratch root: the skeleton and the config files. */
function writeScratch(root, port, webui2Port) {
  writeSkeleton(root);
  writeConfig(root, port, webui2Port);
}

/**
 * Reads the tail of an instance's log for failure reports — jinn logs
 * to `<paths-root>/state/jinn/jinn.log`, inside the scratch root.
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
 * Polls the bind until an HTTP probe of the health route succeeds or
 * the deadline passes.
 *
 * # Errors
 *
 * Rejects with the instance's log tail when the boot deadline expires.
 */
async function awaitBind(port, root) {
  const deadline = Date.now() + BIND_TIMEOUT_MS;
  for (;;) {
    const open = await fetch(`http://127.0.0.1:${port}/health2`)
      .then((r) => r.ok)
      .catch(() => false);
    if (open) return;
    if (Date.now() > deadline) {
      throw new Error(
        `jinn did not bind 127.0.0.1:${port} within ${BIND_TIMEOUT_MS}ms\n` +
          `--- jinn.log tail ---\n${logTail(root, 40)}`,
      );
    }
    await sleep(BIND_POLL_MS);
  }
}

/** One /proc stat record: the parent pid, process group, and session. */
function statOf(pid) {
  try {
    const stat = fs.readFileSync(`/proc/${pid}/stat`, "utf8");
    const fields = stat.slice(stat.lastIndexOf(")") + 2).split(" ");
    return { ppid: Number(fields[1]), pgid: Number(fields[2]), sid: Number(fields[3]) };
  } catch {
    return undefined;
  }
}

/** Every live process's stat, keyed by pid, read from /proc once. */
function processTable() {
  const table = new Map();
  for (const entry of fs.readdirSync("/proc")) {
    if (!/^\d+$/.test(entry)) continue;
    const stat = statOf(entry);
    if (stat !== undefined) table.set(Number(entry), stat);
  }
  return table;
}

/**
 * The instance's whole process tree: every live process whose command
 * line names `root`, and every descendant of one.
 */
function processesOfRoot(root) {
  const table = processTable();
  const seeds = [];
  for (const pid of table.keys()) {
    try {
      const cmdline = fs.readFileSync(`/proc/${pid}/cmdline`, "utf8");
      if (cmdline.includes(root)) seeds.push(pid);
    } catch {
      // The process exited between the scan and the read — not a seed.
    }
  }
  const tree = new Set(seeds);
  let grew = true;
  while (grew) {
    grew = false;
    for (const [pid, stat] of table) {
      if (!tree.has(pid) && tree.has(stat.ppid)) {
        tree.add(pid);
        grew = true;
      }
    }
  }
  return [...tree];
}

/**
 * Kills an instance's whole tree, then waits for the kernel to reap.
 *
 * # Errors
 *
 * Never rejects — killing an already-dead instance is a no-op.
 */
async function killTree(root) {
  for (const pid of processesOfRoot(root)) {
    try {
      process.kill(pid, "SIGKILL");
    } catch {
      // Exited between the scan and the signal — fine.
    }
  }
  const reaped = Date.now() + 2_000;
  while (processesOfRoot(root).length > 0) {
    if (Date.now() > reaped) return;
    await sleep(50);
  }
}

/**
 * Boots one scratch-isolated, hermetically-launched jinn and returns
 * its run coordinates.
 *
 * The binary runs under util-linux `script`, which hands it a pty and
 * keeps the process (and both web UIs) up: the default TUI mode needs
 * a tty. `setsid` puts the wrapper outside the caller's session, and
 * teardown finds the tree by the scratch root on the command line.
 *
 * @returns {Promise<{port: number, webui2Port: number, root: string, baseUrl: string, pid: number}>}
 *
 * # Errors
 *
 * Rejects when the binary is missing, the scratch tree cannot be
 * written, or the boot deadline expires before the bind accepts. A
 * failed boot tears its own instance down before rejecting.
 */
export async function bootJinn() {
  if (!fs.existsSync(BIN_PATH)) {
    throw new Error(
      `jinn binary not found at ${BIN_PATH} — run the tier through a recipe (just webui2-e2e / just webui2-e2e-one), which builds it`,
    );
  }
  const port = await probePort();
  const webui2Port = await probePort();
  fs.mkdirSync(TMP_BASE, { recursive: true });
  const root = fs.mkdtempSync(path.join(TMP_BASE, "run-"));
  writeScratch(root, port, webui2Port);

  const child = spawn(
    "setsid",
    [
      "script",
      "-qfec",
      `exec ${BIN_PATH} --db-path ${path.join(root, "db", "state.db")} --config ${path.join(root, "config", "jinn", "jinn.toml")}`,
      "/dev/null",
    ],
    {
      env: allowedEnv(root),
      cwd: path.join(root, "cwd"),
      detached: true,
      stdio: "ignore",
    },
  );
  child.unref();

  const run = {
    port,
    webui2Port,
    root,
    /** The `/web2` page itself — the fixture's page opens this. */
    baseUrl: `http://127.0.0.1:${webui2Port}/web2`,
    /** The original web UI on the same instance; `createSession` drives it. */
    legacyUrl: `http://127.0.0.1:${port}/web`,
    pid: child.pid,
  };
  try {
    await awaitBind(webui2Port, root);
  } catch (error) {
    await teardownJinn(run);
    throw error;
  }
  return run;
}

/**
 * Tears one instance down: kill its whole tree, remove its scratch
 * root. Safe to call twice and on an instance that already died.
 *
 * # Errors
 *
 * Never rejects — teardown best-effort cleans whatever exists.
 */
export async function teardownJinn(run) {
  if (!run || typeof run.root !== "string") return;
  await killTree(run.root);
  fs.rmSync(run.root, { recursive: true, force: true });
}
