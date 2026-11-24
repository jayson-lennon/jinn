//! The jinn subprocess lifecycle for the webui e2e tier.
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
//!   directory under the scratch root (`app_paths.rs`: read once at
//!   startup, the only place the workspace reads the environment for
//!   path derivation);
//! - `--db-path` and `--config` name files inside that root, so the
//!   real `~/.config/jinn` and `~/.local/share/jinn` are never opened;
//! - the working directory is inside the root, so the session-init scan
//!   reads the scratch root's project-local skills, not the checkout's;
//! - the environment is BUILT from an allowlist, never inherited, so
//!   the caller's `RUST_LOG`, provider keys, and shell variables do not
//!   reach the instance.
//!
//! Staged scratch resources (the block-less persona, the scratch skill,
//! the scratch applet) are parameters of each boot — a test that needs
//! one asks for it; a test that does not gets an instance without it.
//! There is no module-level staged state to leak between boots.

import { spawn } from "node:child_process";
import net from "node:net";
import fs from "node:fs";
import path from "node:path";
import os from "node:os";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));

/** Root of the repo (three up from webui/client/e2e). */
const REPO_ROOT = path.resolve(here, "..", "..", "..");
/** The debug binary the e2e recipes build before every run. */
const BIN_PATH = path.join(REPO_ROOT, "target", "debug", "jinn");
/** The scratch luau script the `[mcp.echo]` entry serves. */
const MCP_ECHO_SCRIPT = path.join(here, "mcp-echo-tool.lua");
/**
 * The scratch roots live one level deeper, under this worktree's slug —
 * the sweep in `just webui-e2e-clean` matches the slug-scoped cmdline
 * pattern, so it can only ever kill this worktree's processes. Two
 * worktrees running the e2e tier concurrently no longer kill each
 * other mid-suite.
 */
const WORKTREE_SLUG = path.basename(REPO_ROOT);
/** Where the scratch roots are created — per-worktree scoped. */
export const TMP_BASE = path.join(os.tmpdir(), "jinn-webui-e2e", WORKTREE_SLUG);

/** How long to wait for the bind before declaring boot failed. */
const BIND_TIMEOUT_MS = 30_000;
/** Delay between bind probes. */
const BIND_POLL_MS = 50;

/**
 * The environment every instance receives — an allowlist, not an
 * inheritance. Each entry is something the process needs to run or
 * the tier itself sets; everything else in the caller's environment
 * (`RUST_LOG`, provider key variables, shell customizations) is left
 * behind on purpose.
 *
 * `SHELL` is pinned to `/bin/sh` so `script -c` launches the binary
 * through a known shell that `exec`s it, with no rc file of the
 * caller's to source. `TERM=dumb` keeps the pty wrapper from emitting
 * terminal queries nothing answers.
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
    // The attachments spec's loopback TLS fixture serves a committed
    // self-signed cert; the image-URL fetch client trusts it in test
    // runs only (production never sets this var).
    JINN_WEBUI_TEST_ROOT_CERT: path.join(here, "loopback-cert.pem"),
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

# The attachments spec sends images; the sample model is not in the
# models.dev reference data, so the vision gate needs the explicit
# capability marker (the same lever the gate's error entry names).
[[providers.sample.model_info]]
id = "sample"
input_modalities = ["text", "image"]

[providers.ghost]
backend = "openai"
requires_key = true
api_key_env = "JINN_E2E_NEVER_SET_KEY"
models = ["phantom"]
`;

/**
 * Fills the picker's rows past the modal's 70vh cap so the rows
 * container is genuinely scrollable — the scroll spec asserts
 * scrollHeight > clientHeight, which two rows never produce. Keyless
 * static providers: they render rows, need no network, and no spec
 * addresses them by name except the text-only filler the attachment
 * gate spec switches to (`filler1`), which is kept.
 */
const FILLERS_TOML = Array.from({ length: 14 }, (_, index) => {
  const number = index + 1;
  return `
[providers.filler${number}]
backend = "sample"
requires_key = false
models = ["filler-${number}"]
`;
}).join("");

/**
 * The scratch jinn.toml body — the bind is the only knob the tier
 * turns; the port is probed so the config can name it.
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
 * The staged resources a boot may write before jinn starts. Each is
 * a plain value: the persona's full file body, the skill's name and
 * body, the applet's id and title. Discovery scans at activation, so
 * a file written after boot is invisible — they must exist first.
 */
export const staged = {
  /** The block-less scratch persona the persona-picker warning path
   * pivots on. Every shipped persona carries a `<compaction>` block,
   * so the loud-error path has no row to click without this one. */
  blocklessPersona: {
    file: "blockless-e2e.md",
    body: `+++
name = "blockless-e2e"
description = "scratch persona without a compaction handoff block"
+++

You are the e2e scratch persona. Keep replies to one line.
`,
  },
  /** The scratch skill the skills spec pivots on. Discovered from the
   * scratch root's global skills dir (under the paths-root override the
   * root IS the home). Valid frontmatter: `name` must match the parent
   * directory, and `description` is required. */
  scratchSkill: {
    name: "e2e-scratch-skill",
    body: `---
name: e2e-scratch-skill
description: scratch skill for the skills modal flow
---

# e2e scratch skill

Say the magic word: e2e-scratch-skill-body-marker.
`,
  },
  /** The scratch saved-applet the applets spec pivots on: an
   * `index.html` plus its catalog row under the scratch data dir. */
  scratchApplet: { id: "e2e-scratch-applet", title: "E2E Scratch Applet" },
};

/** Writes the config dir's two files and the staged persona. */
function writeConfig(root, port, persona) {
  const configDir = path.join(root, "config", "jinn");
  fs.mkdirSync(configDir, { recursive: true });
  fs.writeFileSync(path.join(configDir, "providers.toml"), PROVIDERS_TOML + FILLERS_TOML);
  fs.writeFileSync(path.join(configDir, "jinn.toml"), jinnTomlFor(port));
  if (persona !== undefined) {
    const personasDir = path.join(configDir, "personas");
    fs.mkdirSync(personasDir, { recursive: true });
    fs.writeFileSync(path.join(personasDir, persona.file), persona.body);
  }
}

/** Writes the staged skill under the scratch root's global skills dir. */
function writeSkill(root, skill) {
  const skillsDir = path.join(root, ".agents", "skills", skill.name);
  fs.mkdirSync(skillsDir, { recursive: true });
  fs.writeFileSync(path.join(skillsDir, "SKILL.md"), skill.body);
}

/** Writes the staged applet: its `index.html` and the catalog row. */
function writeApplet(root, applet) {
  const base = path.join(root, "data", "jinn", "applets");
  const appletDir = path.join(base, applet.id);
  fs.mkdirSync(appletDir, { recursive: true });
  fs.writeFileSync(
    path.join(appletDir, "index.html"),
    `<!DOCTYPE html><html><body><h1>${applet.title}</h1><p>e2e scratch applet ${applet.id}</p></body></html>`,
  );
  fs.writeFileSync(
    path.join(base, "applets.json"),
    JSON.stringify({
      entries: [
        { id: applet.id, title: applet.title, created_at: "2026-01-01T00:00:00Z" },
      ],
    }),
  );
}

/** The empty trees every scratch root starts with. */
function writeSkeleton(root) {
  for (const dir of ["db", "tmp", "cwd"]) {
    fs.mkdirSync(path.join(root, dir), { recursive: true });
  }
}

/**
 * Writes one scratch root: the skeleton, the config files, and
 * whichever staged resources this boot asked for.
 */
function writeScratch(root, port, stage) {
  writeSkeleton(root);
  writeConfig(root, port, stage.persona);
  if (stage.skill !== undefined) writeSkill(root, stage.skill);
  if (stage.applet !== undefined) writeApplet(root, stage.applet);
}

/**
 * Reads the tail of an instance's log for failure reports — jinn logs
 * to `<paths-root>/state/jinn/jinn.log`, inside the scratch root.
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
 * # Errors
 *
 * Rejects with the instance's log tail when the boot deadline expires.
 */
async function awaitBind(port, root) {
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
    // The comm field is parenthesized and may contain spaces; the
    // fields below start two characters after the closing paren.
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
 * line names `root`, and every descendant of one. The scratch root is
 * unique per boot and appears in the `--db-path` argument of the pty
 * wrapper and the binary; a child the binary spawns (the scratch
 * config's `[mcp.echo]` entry runs the binary again as an MCP server)
 * carries no root on its command line and lands in a session of its
 * own, so the tree is walked parent-to-child from the root-naming
 * processes rather than inferred from sessions or groups.
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
 * Kills an instance's whole tree, then waits for the kernel to reap so
 * a following scratch-root removal does not race a still-open file.
 * SIGKILL, no grace: a test instance has no durable work to finish,
 * and the pty wrapper outlives a SIGTERM that already took its child
 * down.
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
 * keeps the process (and the webui) up: the default TUI mode needs a
 * tty — without one it dies at "failed to enable raw mode" right after
 * binding — and `headless` mode runs the activation then exits. The
 * command goes through `/bin/sh -c 'exec …'` so the shell is replaced
 * by the binary: the tree is `script → jinn`, one session, one group.
 * `setsid` puts the wrapper outside the caller's session. The tree is
 * not one session — the binary calls `setsid` on the pty slave — so
 * teardown finds it by the scratch root on the command line and walks
 * it parent-to-child (`processesOfRoot`).
 *
 * @param {object} [options]
 * @param {boolean} [options.persona] stage the block-less scratch persona.
 * @param {boolean} [options.skill] stage the scratch skill.
 * @param {boolean} [options.applet] stage the scratch applet.
 * @returns {Promise<{port: number, root: string, baseUrl: string}>}

 * # Errors
 *
 * Rejects when the binary is missing, the scratch tree cannot be
 * written, or the boot deadline expires before the bind accepts. A
 * failed boot tears its own instance down before rejecting.
 */
export async function bootJinn(options = {}) {
  if (!fs.existsSync(BIN_PATH)) {
    throw new Error(
      `jinn binary not found at ${BIN_PATH} — run the tier through a recipe (just webui-e2e / just webui-e2e-one), which builds it`,
    );
  }
  const port = await probePort();
  fs.mkdirSync(TMP_BASE, { recursive: true });
  const root = fs.mkdtempSync(path.join(TMP_BASE, "run-"));
  writeScratch(root, port, {
    persona: options.persona ? staged.blocklessPersona : undefined,
    skill: options.skill ? staged.scratchSkill : undefined,
    applet: options.applet ? staged.scratchApplet : undefined,
  });

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

  const run = { port, root, baseUrl: `http://127.0.0.1:${port}` };
  try {
    await awaitBind(port, root);
  } catch (error) {
    await teardownJinn(run);
    throw error;
  }
  return run;
}

/**
 * Tears one instance down: kill its whole session, remove its scratch
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

// ── The pre-fixture lifecycle ────────────────────────────────────────
//
// The one-shared-instance boot the not-yet-migrated specs still run
// through `global-setup.mjs`. Same launch site as `bootJinn` (every
// hermetic property above holds); this wrapper only pins the staging
// set and the `.run` bookkeeping the old teardown reads. It goes away
// with the last unmigrated spec.

/** Where the pre-fixture run's bookkeeping file lives (gitignored). */
const RUN_DIR = path.join(here, ".run");
const RUN_FILE = path.join(RUN_DIR, "run.json");

/** Staged resources for the one shared pre-fixture instance. */
let preFixtureStage = {};

/**
 * Stages the scratch persona for the shared pre-fixture instance.
 * MUST run before `startJinn`: discovery scans at activation.
 */
export function writeScratchPersona(content) {
  preFixtureStage.persona = content;
}

/** Stages the scratch skill for the shared pre-fixture instance. */
export function writeScratchSkill(name, content) {
  preFixtureStage.skill = { name, content };
}

/** Stages the scratch applet for the shared pre-fixture instance. */
export function writeScratchApplet(id, title) {
  preFixtureStage.applet = { id, title };
}

/**
 * Boots the shared pre-fixture instance and records its coordinates in
 * `.run/run.json` for the old teardown. Stages every resource.
 */
export async function startJinn() {
  cleanupStaleRun();
  const run = await bootJinn({ persona: true, skill: true, applet: true });
  fs.mkdirSync(RUN_DIR, { recursive: true });
  fs.writeFileSync(RUN_FILE, JSON.stringify(run, null, 2));
  return run;
}

/**
 * Tears the shared pre-fixture instance down from its bookkeeping file.
 */
export async function teardown(run) {
  const target =
    run ?? (() => {
      try {
        return JSON.parse(fs.readFileSync(RUN_FILE, "utf8"));
      } catch {
        return null;
      }
    })();
  if (target) await teardownJinn(target);
  fs.rmSync(RUN_DIR, { recursive: true, force: true });
}

/**
 * Kills a stale shared instance left by a crashed earlier run.
 * Never rejects — a missing or already-dead stale run is a no-op.
 */
export function cleanupStaleRun() {
  let stale;
  try {
    stale = JSON.parse(fs.readFileSync(RUN_FILE, "utf8"));
  } catch {
    return;
  }
  if (typeof stale.root === "string") {
    for (const pid of processesOfRoot(stale.root)) {
      try {
        process.kill(pid, "SIGKILL");
      } catch {
        // Already gone.
      }
    }
    fs.rmSync(stale.root, { recursive: true, force: true });
  }
  fs.rmSync(RUN_DIR, { recursive: true, force: true });
}
