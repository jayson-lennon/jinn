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
//! The web UI binds a probed port: each boot probes a free one and writes
//! the `[webui]` section, so two concurrent runs never fight over the
//! shipped default.

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

/** The scratch MCP server's script: one echo tool, served over stdio. */
const MCP_ECHO_SCRIPT = path.join(here, "mcp-echo-tool.lua");

/**
 * The block-less scratch persona the persona spec's warning path pivots
 * on. Every shipped persona carries a `<compaction>` block, so the
 * loud-error path has no row to click without this one.
 */
const blocklessPersona = {
  file: "blockless-e2e.md",
  body: `+++
name = "blockless-e2e"
description = "scratch persona without a compaction handoff block"
+++

You are the e2e scratch persona. Keep replies to one line.
`,
};

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
    // The log filter, forwarded so a kept run's `jinn.log` can carry the
    // `info!` scan lines a debugging session needs. The default floor is
    // `warn`, which hides them.
    ...keep("RUST_LOG"),
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

/**
 * The scratch providers.toml — sample-only, keyless, network-free.
 *
 * @param {boolean} textOnly
 *   write the sample model's capability marker as text-only, so an image
 *   send is blocked by the vision gate instead of going through.
 */
function providersTomlFor(textOnly) {
  // The attachments spec sends images; the sample model is not in the
  // models.dev reference data, so the vision gate needs the explicit
  // capability marker on the model's own row (the lever the gate's error
  // entry names). A
  // text-only row is the gate's blocked path: the error it names.
  const modalities = textOnly ? '["text"]' : '["text", "image"]';
  return `version = 1

default_provider = "sample"

[providers.sample]
backend = "sample"
requires_key = false
models = ["sample"]

# The attachments spec sends images; the sample model is not in the
# models.dev reference data, so the vision gate needs the explicit
# capability marker on the model's own row (the lever the gate's error
# entry names).
[[providers.sample.model_info]]
id = "sample"
input_modalities = ${modalities}

# A second, selectable, keyless provider: the model-picker spec needs a
# row that is not the current one to switch to.
[providers.second]
backend = "sample"
requires_key = false
models = ["second-model"]
`;
}

/**
 * The scratch jinn.toml body. The web UI binds a probed port, so a
 * concurrent run never collides with this one's.
 *
 * The project entry and the lifecycle script exist for the new-session
 * specs: the web UI's modal creates a session only against a configured
 * project, and offers no submit control without one. The lifecycle's
 * setup takes a NAMED argument so one spec can assert the page shows the
 * template's own spelling of it rather than a label made up from its
 * position.
 *
 * With `mcp` staged, an `[mcp.e2e-echo]` entry points at the repo's
 * debug binary serving the scratch echo tool over stdio (`jinn luau
 * serve`, network-free), so the MCP modal has a real server whose
 * coordinator can reach `running`.
 *
 * Three more lifecycle scripts exist for the teardown specs, each with a
 * setup command because a teardown is only due for a session whose setup
 * has run — a lifecycle with a teardown and no setup never reaches that
 * state, and a close archives without running anything:
 *
 * - `marker-teardown` writes a marker file under the instance's scratch
 *   home, so a spec can assert the command actually ran.
 * - `slow-teardown` sleeps before exiting, so a spec can observe the
 *   row's busy dot and a reload while the teardown is in flight.
 * - `failing-teardown` exits non-zero, so a spec can assert a failed
 *   teardown leaves the session open.
 */
function jinnTomlFor(webui2Port, mcp) {
  const mcpEntry = mcp
    ? `
[mcp.e2e-echo]
command = "${BIN_PATH}"
args = ["luau", "serve", "${MCP_ECHO_SCRIPT}"]
`
    : "";
  return `version = 1

[webui]
bind = "127.0.0.1:${webui2Port}"

[[project.entry]]
path = "~/e2e-scratch"

[[session_lifecycle.script]]
name = "echo-setup"
setup_command = "echo <greeting>"

[[session_lifecycle.script]]
name = "marker-teardown"
setup_command = "true"
teardown_command = "touch ~/.teardown-marker"

[[session_lifecycle.script]]
name = "slow-teardown"
setup_command = "true"
teardown_command = "sleep 4; touch ~/.slow-teardown-done"

[[session_lifecycle.script]]
name = "failing-teardown"
setup_command = "true"
teardown_command = "exit 1"

${mcpEntry}`;
}

/** Writes the config dir's two files. */
function writeConfig(root, webui2Port, stage) {
  const configDir = path.join(root, "config", "jinn");
  fs.mkdirSync(configDir, { recursive: true });
  fs.writeFileSync(
    path.join(configDir, "providers.toml"),
    providersTomlFor(stage.textOnly === true),
  );
  fs.writeFileSync(path.join(configDir, "jinn.toml"), jinnTomlFor(webui2Port, stage.mcp));
  if (stage.persona) {
    // Personas are discovered from the config dir's `personas/` child —
    // under the paths-root override, the scratch root's own.
    const personasDir = path.join(configDir, "personas");
    fs.mkdirSync(personasDir, { recursive: true });
    fs.writeFileSync(path.join(personasDir, blocklessPersona.file), blocklessPersona.body);
  }
}

/**
 * The empty trees every scratch root starts with.
 *
 * `e2e-scratch` is the configured project's directory. It must exist:
 * a lifecycle command is spawned with the session's cwd as its working
 * directory, so a missing project fails the setup before it runs and the
 * session's lifecycle state never advances past `nothing_ran`.
 */
function writeSkeleton(root) {
  for (const dir of ["db", "tmp", "cwd", "e2e-scratch"]) {
    fs.mkdirSync(path.join(root, dir), { recursive: true });
  }
}

/**
 * The scratch skill the skills spec pivots on. Its name and description
 * are chosen so a filter for `e2e` matches it alone among the skills a
 * scratch session discovers, and its body carries a marker a test can
 * look for once the skill is loaded into context.
 */
export const scratchSkill = {
  name: "e2e-scratch-skill",
  description: "scratch skill for the webui2 skills modal",
  body: `---
name: e2e-scratch-skill
description: scratch skill for the webui2 skills modal
---

# e2e scratch skill

Say the magic word: e2e-scratch-skill-body-marker.
`,
};

/**
 * Writes the staged skill under the scratch root's project-local skills
 * dir, so the session's project walk discovers it as a project skill.
 * Discovery scans at boot, so this has to happen before jinn starts —
 * a file written afterwards is invisible until a rescan.
 */
function writeSkill(root, skill) {
  const skillsDir = path.join(root, ".agents", "skills", skill.name);
  fs.mkdirSync(skillsDir, { recursive: true });
  fs.writeFileSync(path.join(skillsDir, "SKILL.md"), skill.body);
}

/** The scratch prompt template the prompt-popup spec pivots on. Its name is
 * chosen so a filter for `e2e-scra` matches it alone among the templates a
 * scratch session discovers — the shipped `plan`, `approve-plan`, … have no
 * such run — and its description names the feature so a test can tell the
 * row's two parts apart. */
export const scratchPrompt = {
  name: "e2e-scratch-prompt",
  description: "scratch prompt for the webui2 # popup",
  body: `+++
name = "e2e-scratch-prompt"
description = "scratch prompt for the webui2 # popup"
+++

e2e-scratch-prompt-body-marker
`,
};

/**
 * Writes the staged prompt in the scratch root's user prompts dir — the
 * same layer the bundled templates are copied into, and the layer the
 * prompt scan reads first.
 *
 * This mirrors the staged skill, which sits in the root's global skills
 * dir: with `HOME` set to the scratch root, a project-local file would
 * have to sit under the session's cwd, and the walk is bounded by `$HOME`.
 * The user layer is the one place discovery is guaranteed to look.
 * Discovery scans at boot, so this has to happen before jinn starts — a
 * file written afterwards is invisible until a rescan.
 */
function writePrompt(root, prompt) {
  const promptsDir = path.join(root, "config", "jinn", "prompts");
  fs.mkdirSync(promptsDir, { recursive: true });
  fs.writeFileSync(path.join(promptsDir, `${prompt.name}.md`), prompt.body);
}

/**
 * The staged OpenRouter market: two listings whose PROJ order is fixed
 * (the cheap one first), and the cheap one's endpoint detail.
 *
 * The scratch instance has no OpenRouter provider and the tier is
 * network-free, so the actor's disk cache is what the market serves. The
 * files are the actor's own stamped envelope (`{ fetched_at, value }`)
 * under the scratch cache's `jinn/openrouter` directory, stamped at the
 * write so they sit well inside the models TTL.
 */
export const scratchMarket = {
  /** The cheap listing — the one every market spec pivots on. */
  cheapId: "openrouter/e2e-alpha/e2e-cheap",
  cheapName: "e2e-alpha/e2e-cheap",
  /** The dear listing, with a variant suffix and no ECI score. */
  dearId: "openrouter/e2e-beta/e2e-dear:free",
  dearName: "e2e-beta/e2e-dear:free",
  /** The tag of the cheap model's one provider endpoint. */
  endpointTag: "e2e-azure/us-east",
  /** The dear model's two endpoints, named so a name sort orders them
   * one way and a latency sort the other. */
  dearFastTag: "e2e-fast",
  dearSlowTag: "e2e-slow",
};

/** A fresh stamp, in the envelope's own format. */
const nowStamp = () => new Date().toISOString();

/**
 * The listings as the actor stores them: USD/token prices, the cheap one
 * with an ECI score so an ECI sort puts it first.
 */
function marketRows() {
  return [
    {
      id: scratchMarket.cheapId,
      author: "e2e-alpha",
      slug: "e2e-cheap",
      variant: null,
      prompt_price: 0.000001,
      completion_price: 0.000001,
      cache_read_price: 0.0000001,
      has_overrides: false,
      context_length: 200000,
      eci: 50.0,
      eci_ci: null,
      vals: null,
      is_free: false,
    },
    {
      id: scratchMarket.dearId,
      author: "e2e-beta",
      slug: "e2e-dear",
      variant: "free",
      prompt_price: 0.000003,
      completion_price: 0.000015,
      cache_read_price: 0.0000003,
      has_overrides: true,
      context_length: null,
      eci: null,
      eci_ci: null,
      vals: 61.5,
      is_free: true,
    },
  ];
}

/** The cheap model's endpoint rows, as the actor stores them: a bare
 * array, one entry per provider. */
function marketEndpoints() {
  return [
    {
      tag: scratchMarket.endpointTag,
      provider_name: "E2E Azure",
      prompt_price: 0.0000012,
      completion_price: 0.0000045,
      cache_read_price: 0.00000009,
      latency_p50_ms: 1159.0,
      throughput_p50: 38.0,
      uptime_1d: 0.992,
      quantization: null,
      overrides: [],
    },
  ];
}

/** The dear model's endpoint rows: the pricier one is the faster, so a
 * PROJ sort and a LAT sort order them oppositely. */
function dearMarketEndpoints() {
  return [
    {
      tag: scratchMarket.dearSlowTag,
      provider_name: "E2E Slow",
      prompt_price: 0.000001,
      completion_price: 0.000004,
      cache_read_price: null,
      latency_p50_ms: 900.0,
      throughput_p50: 30.0,
      uptime_1d: 0.99,
      quantization: null,
      overrides: [],
    },
    {
      tag: scratchMarket.dearFastTag,
      provider_name: "E2E Fast",
      prompt_price: 0.000005,
      completion_price: 0.000020,
      cache_read_price: null,
      latency_p50_ms: 100.0,
      throughput_p50: 60.0,
      uptime_1d: 0.999,
      quantization: null,
      overrides: [],
    },
  ];
}

/** The actor's endpoint cache name: separators flattened to underscores. */
function endpointCacheName(modelId) {
  return `endpoints-${modelId.replace(/[/:]/g, "_")}.json`;
}

/**
 * Writes the market's disk cache into the scratch root: the listings and
 * the cheap model's endpoints, both freshly stamped.
 */
function writeMarket(root) {
  const dir = path.join(root, "cache", "jinn", "openrouter");
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(path.join(dir, "models.json"), JSON.stringify({ fetched_at: nowStamp(), value: marketRows() }));
  fs.writeFileSync(
    path.join(dir, endpointCacheName(scratchMarket.cheapId)),
    JSON.stringify({ fetched_at: nowStamp(), value: marketEndpoints() }),
  );
  fs.writeFileSync(
    path.join(dir, endpointCacheName(scratchMarket.dearId)),
    JSON.stringify({ fetched_at: nowStamp(), value: dearMarketEndpoints() }),
  );
}

/**
 * The scratch applet the applets spec pivots on. The title is chosen so a
 * filter for `e2e` matches it alone, and its body carries a marker so a
 * test that fetched the page can tell the artifact from an error page.
 */
export const scratchApplet = {
  id: "e2e-scratch-applet",
  title: "e2e scratch applet",
  body: `<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <title>e2e scratch applet</title>
  </head>
  <body>
    <h1>e2e-scratch-applet-marker</h1>
  </body>
</html>
`,
};

/**
 * Writes the staged applet under the scratch root's data dir, where the
 * applets slice discovers saved applets: `<data>/jinn/applets/<id>/` holds
 * the artifact, and one `applets.json` at the applets root is the
 * catalog — an entry's directory is the truth for existence, its row the
 * truth for title and recency. Written before jinn starts, like the
 * staged skill — the catalog's scan runs on the first rescan.
 */
function writeApplet(root, applet) {
  const appletsRoot = path.join(root, "data", "jinn", "applets");
  const appletDir = path.join(appletsRoot, applet.id);
  fs.mkdirSync(appletDir, { recursive: true });
  fs.writeFileSync(path.join(appletDir, "index.html"), applet.body);
  fs.writeFileSync(
    path.join(appletsRoot, "applets.json"),
    JSON.stringify({
      entries: [{ id: applet.id, title: applet.title, created_at: "2026-01-01T00:00:00Z" }],
    }),
  );
}

/** Writes one scratch root: the skeleton, the config files, and any staged resources. */
function writeScratch(root, webui2Port, stage = {}) {
  writeSkeleton(root);
  writeConfig(root, webui2Port, stage);
  if (stage.skill) writeSkill(root, scratchSkill);
  if (stage.applet) writeApplet(root, scratchApplet);
  if (stage.prompt) writePrompt(root, scratchPrompt);
  if (stage.market) writeMarket(root);
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
    const open = await fetch(`http://127.0.0.1:${port}/health`)
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
 * keeps the process (and the web UI) up: the default TUI mode needs
 * a tty. `setsid` puts the wrapper outside the caller's session, and
 * teardown finds the tree by the scratch root on the command line.
 *
 * @param {{ skill?: boolean, persona?: boolean, mcp?: boolean, textOnly?: boolean, applet?: boolean, prompt?: boolean }} [options]
 *   stage the scratch skill when `skill` is set, the block-less persona
 *   when `persona` is set, the echo MCP server when `mcp` is set, the
 *   scratch applet under the data dir when `applet` is set, and the
 *   scratch prompt template under the project-local prompts dir when
 *   `prompt` is set.
 *   With `textOnly`, the sample model's row declares text only, so an
 *   image send hits the vision gate's blocked path.
 *
 * @returns {Promise<{webui2Port: number, root: string, baseUrl: string, pid: number}>}
 *
 * # Errors
 *
 * Rejects when the binary is missing, the scratch tree cannot be
 * written, or the boot deadline expires before the bind accepts. A
 * failed boot tears its own instance down before rejecting.
 */
export async function bootJinn(options = {}) {
  if (!fs.existsSync(BIN_PATH)) {
    throw new Error(
      `jinn binary not found at ${BIN_PATH} — run the tier through a recipe (just webui2-e2e / just webui2-e2e-one), which builds it`,
    );
  }
  const webui2Port = await probePort();
  fs.mkdirSync(TMP_BASE, { recursive: true });
  const root = fs.mkdtempSync(path.join(TMP_BASE, "run-"));
  writeScratch(root, webui2Port, {
    skill: options.skill === true,
    persona: options.persona === true,
    mcp: options.mcp === true,
    textOnly: options.textOnly === true,
    applet: options.applet === true,
    prompt: options.prompt === true,
    market: options.market === true,
  });
  const run = {
    webui2Port,
    root,
    /** The web UI page itself, at the root — the fixture's page opens this. */
    baseUrl: `http://127.0.0.1:${webui2Port}/`,
    pid: 0,
  };
  await launch(run);
  return run;
}

/**
 * Starts the binary for one run's scratch root and waits for its bind.
 *
 * Both a fresh boot and a restart come through here, so the two launch
 * the same command against the same config.
 *
 * @param {{root: string, webui2Port: number}} run the run to start.
 *
 * # Errors
 *
 * Rejects when the boot deadline expires before the bind accepts. A
 * failed start tears the instance down before rejecting.
 */
async function launch(run) {
  const { root, webui2Port } = run;
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
  run.pid = child.pid;
  try {
    await awaitBind(webui2Port, root);
  } catch (error) {
    await teardownJinn(run);
    throw error;
  }
}

/**
 * Stops one instance and starts it again on the same scratch root, so
 * the next boot reads what the last one wrote to its store.
 *
 * Startup loads only the unarchived sessions, so a session whose row
 * was archived is on disk and not in memory after this returns: the
 * shape a user has when they reach for an archived session days later.
 * The page's socket drops while the instance is down and reconnects on
 * its own.
 *
 * @param {{root: string}} run the run to restart.
 *
 * # Errors
 *
 * Rejects when the restarted instance does not bind in time.
 */
export async function restartJinn(run) {
  await killTree(run.root);
  await launch(run);
}

/**
 * Tears one instance down: kill its whole tree, remove its scratch
 * root. Safe to call twice and on an instance that already died.
 *
 * With `JINN_E2E_KEEP` set, the kill still happens but the scratch root
 * is left in place — its `state/jinn/jinn.log` and staged tree are then
 * readable after the run. A debugging aid, never set by the recipes.
 *
 * # Errors
 *
 * Never rejects — teardown best-effort cleans whatever exists.
 */
export async function teardownJinn(run) {
  if (!run || typeof run.root !== "string") return;
  await killTree(run.root);
  if (process.env.JINN_E2E_KEEP !== undefined) {
    console.error(`[e2e] keeping scratch root: ${run.root}`);
    return;
  }
  fs.rmSync(run.root, { recursive: true, force: true });
}
