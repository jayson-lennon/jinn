# Extending jinn

jinn extends through a set of surfaces, each adding a different kind of
capability:

- **MCP servers** — executable capability: tools the model calls
- **Skills** — procedures and knowledge, loaded on demand
- **Prompts** — reusable instructions you type with `#name`
- **Personas** — the session's identity
- **Themes** — colors
- **Workflows** — repeatable procedures, from checklists to multi-session
  orchestration
- **Canvas & applets** — pages the agent builds and the user opens in the
  browser

The set is complete by design: user additions ride the surfaces, and a
change to jinn's own behavior is a PR against jinn.

So "how do I add a feature to jinn" has a precise answer, and this
document is that answer: what each surface is good for, and how to
install through each one.

**When the user asks to add a capability to jinn, the routing decision is
yours.** Walk the routing table in order, stop at the first fit.

---

## The routing table

| The user wants…                      | Use                | Why not something else                                  |
| ------------------------------------ | ------------------ | -------------------------------------------------------- |
| the model to call a new tool/API     | **MCP server**     | Only surface that adds executable capability             |
| the agent to follow a methodology    | **Skill**          | Instructions + optional bundled scripts, run via `bash`  |
| a reusable instruction I type myself | **Prompt**         | Expands from `#name` in the chat input                   |
| the agent's personality/approach     | **Persona**        | Replaces the system prompt's identity block              |
| colors / visual appearance           | **Theme**          | TOML color theme, `<leader>sh` picker                    |
| a repeatable task pipeline           | **Workflow**       | Steps as editable markdown; bots for concurrency (`workflows.md`) |
| a page or visual the user keeps      | **Canvas / applet** | Agent-built HTML, live reload; applets persist (`canvas-and-applets.md`) |
| a _jinn feature_ (new keybind, pane) | **None — a PR**    | Jinn's own code. See "What can't be extended" below.     |

MCP is the most-used surface. Most of what people imagine a plugin system
for — a Jira integration, a video editor, a database client, web search, an
issue tracker — is an MCP server, and jinn is already an MCP client.

**Name the difference when they come up together.** An MCP server gives the
model _new actions_. A skill gives it _knowledge of a procedure_. "Search
the video editor's timeline for a clip" is a tool. "How we run task loops"
is a skill. A skill can bundle shell scripts and reference material and
have the agent run them via `bash` — executable capability straight from
files.

---

## 1. MCP servers — adding executable capability

### What it is

An MCP server is a program that exposes tools over the Model Context
Protocol. jinn launches or connects to it, then exposes its tools as
`mcp__<server>__<tool>`. Full config, transports, and the picker protocol
live in `mcp-servers.md` — read it before answering a config question.

The key fact for routing: **an MCP tool is a plain function** — `name` /
`description` / JSON Schema / `execute`. That is the entire contract.
Anything expressible as "the model calls this with these arguments" is an
MCP server.

### Installing one

Three cases. Confirm which before writing anything.

**Case 1 — an MCP server already exists (most common).** Find it, add a
three-line block to `~/.config/jinn/jinn.toml`:

```toml
[mcp.video_editor]
command = "npx"
args = ["video-editor-mcp", "--stdio"]
```

The server is **off by default per session** — enabled tools occupy
context even when unused, and most sessions run fine with it off. Open
the MCP picker (`<leader>sM`) and enable it in the sessions that want it;
`auto_enable = true` in the block stamps sessions created from then on.

**Case 2 — a server exists but needs a wrapper.** Some servers need an
env var supplied at launch, or a working directory. Add a local script,
point `command` at it:

```toml
[mcp.video_editor]
command = "/home/you/bin/video-editor-mcp.sh"
```

`${VAR}` expansion happens once at startup, for headers only. For
`command`/`args`, put the real value in the script. See `mcp-servers.md`.

**Case 2b — a plain HTTP API worth a thin server.** Write one. See
"Writing an MCP server" below.

**Case 3 — the capability is a local script.** The `bash` tool already
covers it. If the user wants "a command that does X", enable `bash` and
put the command in a skill so the agent knows it exists. This applies
especially to one-off project scripts: project skills live in
`.agents/skills/` and get committed.

### Writing an MCP server

Search for an existing server first — Case 1 is the common path. When you
write one, an MCP server is ordinary code in the user's preferred
language: it speaks MCP on stdio (or HTTP), works with any MCP client,
and registers entirely through its `jinn.toml` block. So it needs two
things:

1. **A process that speaks MCP** on the chosen transport, exposing a
   handful of tools.
2. **A `jinn.toml` block** to launch it.

For an HTTP-only capability, the minimal shape is a stdio server that
shells out to `curl` or uses your language's HTTP client — one endpoint,
one tool. The fast TypeScript route is the official SDK:
`@modelcontextprotocol/sdk`. Minimum viable server:

```ts
import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { z } from "zod";

const server = new McpServer("video-editor");

server.tool(
  "find_clips",
  "Find video clips matching a description",
  { query: z.string(), start: z.number().optional() },
  async ({ query, start = 0 }) => ({
    content: [{ type: "text", text: await searchClips(query, start) }],
  }),
);

await server.connect(new StdioServerTransport());
```

Then register it:

```toml
[mcp.video_editor]
command = "npx"
args = ["-y", "@acme/video-editor-mcp"]
```

**Say this to the user once:** the model calls only what the server
exposes, and an MCP server is arbitrary code the user chooses to run —
security is the user's call. See the README's Security section.

---

## 2. Skills — teaching a procedure

### What it is

A skill is a directory with a `SKILL.md`: YAML frontmatter (`name`,
`description`) plus markdown instructions. jinn lists every discovered
skill's name, description, and path in the system prompt's `<skills>`
block. Bodies load only on demand — descriptions are always in context.

This is a **cheaper surface than MCP** in many cases. A skill that
bundles a `scripts/` directory and tells the agent to run them via `bash`
provides executable capability from plain files — and the same skill
works in any harness using the Agent Skills format.

### Where they live and how discovery works

Precedence, highest first:

1. `<project>/.agents/skills/<name>/SKILL.md` — project-local, committable
2. `~/.agents/skills/<name>/SKILL.md` — user-global
3. `<system skills dir>` — jinn's own bundled skills

A project skill with the same name as a global shadows the global.
Discovery walks from the session's cwd up to the project root, stopping
at the VCS root (`.git`, `.hg`, `.fslckout`, `.fossil`, `.jj`) or `$HOME`.
Closer files win.

Bundled skills ship inside the binary, reconciled into
`~/.agents/skills/` on every launch. Your edits to a bundled skill
persist across launches; `jinn install --builtins` restores the shipped
copies.

### Installing one

**From a registry or repo (most common).** A skill installs by copying
its directory into place:

```sh
# user-global
git clone <repo> /tmp/s && cp -r /tmp/s/my-skill ~/.agents/skills/

# project-local, committable
cp -r my-skill <project>/.agents/skills/
```

A skill directory may contain anything else it needs: `references/*.md`,
`scripts/*.sh`, `assets/`. jinn only reads `SKILL.md`; the agent reads the
rest on demand.

**Picking up a new skill.** The skill picker
(`<leader>sk`, `<c-r>` to rescan) enables/disables per session. `<c-r>`
forces a rescan, so a freshly copied directory is picked up at once —
the same is true of MCP, whose config watcher picks up entry edits.

**Writing one from scratch.** Create the directory and a `SKILL.md`:

```markdown
---
name: my-skill
description: What it does, in one line, with trigger words. Use when the user asks to <X>.
---

# My Skill

Instructions the agent follows. Reference bundled files by path.

Run `bash scripts/do-thing.sh` to perform the action.
```

The `description` is the only part always in context — write it for
retrieval, with the words a user would actually type. Everything else is
read on demand.

---

## 3. Prompts — reusable instructions you type

### What it is

A prompt is a markdown file with `+++` TOML frontmatter. It expands from
`#name` in the chat input and stays collapsed until sent — use reads the
file and leaves it untouched.

### Installing one

Drop a `.md` file in `~/.config/jinn/prompts/`:

```toml
# ~/.config/jinn/prompts/review.md
+++
name = "review"
description = "Review the current change."
+++

Review the pending diff. Focus on correctness and clarity. ...
```

jinn ships twelve prompt templates — `plan`, `approve-plan`, `_compaction`
among them — reconciled into place on launch. Type `#` in the input to see
what's available, and `#foo#` to expand `foo` early for editing before
sending.

---

## 4. Personas — the agent's identity

The catalog of builtin personas and guidance for writing your own live in
`personas.md`. Mechanics here: the file format and the picker.

### What it is

A persona is markdown with `+++` TOML frontmatter, exactly like a prompt,
in `~/.config/jinn/personas/`. The body replaces the `<persona>` block of
the system prompt. Sessions default to `coding-assistant` and fall back
to it if their persona is deleted.

### Installing one

```toml
# ~/.config/jinn/personas/senior-architect.md
+++
name = "senior-architect"
description = "Reviews before writing, challenges scope."
+++
```

Select with the persona picker (`<leader>se`, or `c` in the sidebar's
persona section). There's a `#generate-persona` prompt for deriving one
from a description. The builtin personas and writing guidance live in
`personas.md`.

### Persona vs. prompt vs. skill

|          | Persona                | Prompt                 | Skill                       |
| -------- | ---------------------- | ---------------------- | --------------------------- |
| Scope    | whole session          | one send               | on demand, any turn         |
| Replaced | identity block         | expands inline         | adds capability             |
| Fits     | "how the agent thinks" | "instructions I reuse" | "a procedure worth keeping" |

A persona rides in context for the whole session; a skill keeps its
description in context and loads its body on demand; a prompt's text
enters context when you send it.

### System prompt assembly

Every session's system prompt is built from six blocks, in order:

```
<persona>       # the active persona — every session has one
<AGENTS.md>     # discovered project-context files, under # Project Context
<tool context>  # available tools + tool guidelines
<skills>        # every discovered skill's name, description, path
<date>          # current date, injected at assembly time
<cwd>           # the session's working directory
```

AGENTS.md discovery: each directory from the session's working directory
up to the project root may contribute one file — the first of `AGENTS.md`
or `CLAUDE.md` found (case-insensitive) wins per directory, and the walk
stops at the VCS root (`.git`, `.fslckout`, …) or `$HOME`. Closer files
override ancestors. Rescans happen on session create/load and on cwd
change (`<c-r>` in the skill picker forces one).

The tool-context block is generated from the tools registered in the
session: a one-line summary and behavioral guideline per tool, filtered
to the tools actually available (disabled tools and provider-mismatched
server tools are excluded from both the prompt and the API call). The
`<skills>` block lists name/description/path only — bodies load on
demand.

**Minimizing the prompt:** the persona, date, and cwd blocks are fixed.
Everything else shrinks by disabling it — tools off (`<leader>st`),
skills off (`<leader>sk`), MCP servers off (`<leader>sM`, or `auto_enable
= false` for new sessions), and a project directory free of
`AGENTS.md`/`CLAUDE.md`. A session with everything disabled carries just
persona + date + cwd.

---

## 5. Themes — colors

TOML color themes in `~/.config/jinn/themes/`. Picked with `<leader>sh`.

jinn ships five: `default`, `catppuccin-mocha`, `nord-light`,
`gruvbox-dark`, `sonokai` — reconciled into place on launch. See the
default theme for the format.

---

## 6. Workflows — repeatable procedures

A workflow is a named procedure stored under `~/.config/jinn/workflows/`,
run through the session: steps as editable markdown, bot definitions for
concurrent work, lifecycle scripts for branch isolation. Build one by
asking ("create a workflow that…"), tune it by editing the step files.
`workflows.md` covers the file anatomy and the bundled `autodev`.

---

## 7. Canvas & applets — pages the agent builds

Ask for a chart, diagram, simulation, or any "show me", and the agent
writes an HTML page and serves it — live reload makes it editable in
place. "Keep this" turns it into an applet: a snapshot the user reopens
from the `<leader>sa` picker any time. `canvas-and-applets.md` is the
user guide; the `canvas` and `applets` skills carry the authoring
workflows the agent follows.

---

## Installation summary

| Surface  | Location                          | Install        | An edit takes effect        |
| -------- | --------------------------------- | -------------- | ---------------------------- |
| MCP      | `~/.config/jinn/jinn.toml`        | config edit    | at once — an edited entry reconnects that server; `auto_enable` stamps new sessions |
| Skill    | `~/.agents/skills/<n>/`           | copy directory | at once — `<c-r>` rescan |
| Prompt   | `~/.config/jinn/prompts/*.md`     | write file     | at once — re-read at next use |
| Persona  | `~/.config/jinn/personas/*.md`    | write file     | at once — re-read at next use |
| Theme    | `~/.config/jinn/themes/*.toml`    | write file     | at once — re-read at next use |
| Workflow | `~/.config/jinn/workflows/<n>/`   | ask jinn, or write the files | at once — `<c-r>` rescan |
| Canvas artifact | anywhere writable          | the `canvas` tool serves it | live reload on every save |
| Applet   | `~/.local/share/jinn/applets/<id>/` | copy directory + register in `applets.json` | picker rescans on open |

---

## What can't be extended

These live in jinn's own code; extending them starts with a PR:

- **New keybinds, panes, or TUI layout** — the sidebar, pickers, and
  which-key are built in. A custom status bar, footer, or editor starts
  with a PR.
- **The agent loop** — turn dispatch, steering vs. queueing, the settled
  boundary, compaction triggers.
- **Tool-call interception** — jinn's policy surface is
  `[[stream_rules.entry]]`: regex rules over the assistant's output,
  which can stop a named tool's call before it runs by interrupting its
  arguments. A `stream_rules.toml` beside `jinn.toml` replaces the
  shipped guard set wholesale (`jinn config init --force` restores it),
  and `jinn.toml` rules append after it. That is the entire policy
  surface.
- **Context assembly** — pinning, pruning, and compaction tuning are
  config knobs, and their logic is jinn's own.

If the user's ask lands here, the answer is: **that's a jinn feature
request.** Say so and move on — the surfaces above are the complete set,
so the request belongs with jinn itself.

---

## The offer-to-edit protocol

Follow `configuration.md` for anything touching `jinn.toml`. Summary:

1. Name the file and section. Say what the setting does in one or two
   sentences.
2. Show a snippet with values the user would actually want.
3. Ask: _"Want me to add this to `~/.config/jinn/jinn.toml`?"_ Say when it
   takes effect, per the categories in `configuration.md`.
4. On acceptance, read the file. Apply surgically — preserve comments,
   ordering, unknown keys. Confirm what changed.
5. Point at the config dashboard row: it shows the reload land, or the
   refusal if the file broke.

Skills install by copying the directory — `<c-r>` rescans, and that is
the whole protocol. Prefer a project-local skill (`.agents/skills/`) when
the skill is about this project, an MCP config edit when the capability
is a tool.

---

## Coverage note

Section names in `jinn.toml` are exact: `[mcp.<name>]`, not
`[[mcp_server]]`; `[term]`, not `[interactive_term]`. **A misspelled
section reads as absent** — jinn applies the rest of the file and keeps
the built-in behavior for that section. When an ask reaches beyond the
key list in `configuration.md`, send the user to the fully commented
default `jinn.toml` jinn auto-creates on first run.

Only "Writing an MCP server" teaches something new. Everything else routes
to another reference in this skill or repeats facts already in
`configuration.md`. A typo'd key or path sends files to the wrong
directory or drops them entirely — verify paths before applying changes.
