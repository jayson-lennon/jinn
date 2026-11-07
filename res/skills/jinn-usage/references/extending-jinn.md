# Extending jinn

jinn has **no plugin system**. This is a design decision, not an omission.

So "how do I add a feature to jinn" has a different answer than you'd
expect, and this document is the answer: the five surfaces you extend jinn
through, what each is good for, how to install each one.

**When the user asks to add a capability to jinn, the routing decision is
yours.** Walk the routing table in order, stop at the first fit.

---

## The routing table

| The user wants…                      | Use             | Why not something else                                   |
| ------------------------------------ | --------------- | -------------------------------------------------------- |
| the model to call a new tool/API     | **MCP server**  | Only surface that adds executable capability             |
| the agent to follow a methodology    | **Skill**       | Instructions + optional bundled scripts, no tool surface |
| a reusable instruction I type myself | **Prompt**      | Expands from `#name` in the chat input                   |
| the agent's personality/approach     | **Persona**     | Replaces the system prompt's identity block              |
| colors / visual appearance           | **Theme**       | TOML color theme, `<leader>sh` picker                    |
| a _jinn feature_ (new keybind, pane) | **None — a PR** | Not user-extensible. See "What can't be extended" below. |

MCP is the most-used surface. Most of what people imagine a plugin system
for — a Jira integration, a video editor, a database client, web search, an
issue tracker — is an MCP server, and jinn is already an MCP client.

**Name the difference when they come up together.** An MCP server gives the
model _new actions_. A skill gives it _knowledge of a procedure_. "Search
the video editor's timeline for a clip" is a tool. "How we run task loops"
is a skill. A skill can bundle shell scripts and reference material and
have the agent run them via `bash` — that's how non-tool capabilities still
work without MCP.

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
context even when unused, and most sessions need none of them. Open the
MCP picker (`<leader>sM`) and enable it in the sessions that want it;
`auto_enable = true` in the block stamps sessions created from then on.

**Case 2 — a server exists but needs a wrapper.** Some servers need an env
var that shouldn't be hardcoded, or a working directory. Add a local
script, point `command` at it:

```toml
[mcp.video_editor]
command = "/home/you/bin/video-editor-mcp.sh"
```

`${VAR}` expansion happens once at startup, for headers only. For
`command`/`args`, put the real value in the script. See `mcp-servers.md`.

**Case 2b — a plain HTTP API with no MCP server available.** Write a thin
server. See "Writing an MCP server" below.

**Case 3 — the capability is a local script with no server at all.** The
`bash` tool already exists. If the user wants "a command that does X",
don't build an MCP server — enable `bash` and put the command in a skill
so the agent knows it exists. This applies especially to one-off project
scripts: project skills live in `.agents/skills/` and get committed.

### Writing an MCP server

Only when no server exists. An MCP server is ordinary code in the user's
preferred language — no jinn-specific runtime, and it works with any MCP
client. No manifest either — if you're picturing pi's `package.json` block,
jinn doesn't have one. jinn only needs the process to speak MCP on stdio
(or HTTP). So it needs two things:

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

**Say this to the user once:** the model can only call what the server
exposes, and an MCP server is arbitrary code the user chooses to run. jinn
has no security model — see the README's Security section.

---

## 2. Skills — teaching a procedure

### What it is

A skill is a directory with a `SKILL.md`: YAML frontmatter (`name`,
`description`) plus markdown instructions. jinn lists every discovered
skill's name, description, and path in the system prompt's `<skills>`
block. Bodies load only on demand — descriptions are always in context.

This is a **cheaper surface than MCP** in many cases. A skill that bundles
a `scripts/` directory and tells the agent to run them via `bash` provides
executable capability with no protocol work — and the same skill works in
any harness using the Agent Skills format.

### Where they live and how discovery works

Precedence, highest first:

1. `<project>/.agents/skills/<name>/SKILL.md` — project-local, committable
2. `~/.agents/skills/<name>/SKILL.md` — user-global
3. `<system skills dir>` — jinn's own bundled skills

A project skill with the same name as a global shadows the global.
Discovery walks from the session's cwd up to the project root, stopping at
the VCS root (`.git`, `.hg`, `.fslckout`, `.fossil`, `.jj`) or `$HOME`.
Closer files win.

Bundled skills ship inside the binary, reconciled into
`~/.agents/skills/` on every launch. An edited bundled skill is left alone;
`jinn install --builtins` restores the shipped copies.

### Installing one

**From a registry or repo (most common).** Skills are plain files — there's
no install command. Copy the directory:

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
`#name` in the chat input and stays collapsed until sent — the on-disk
file is never mutated by use.

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

### What it is

A persona is markdown with `+++` TOML frontmatter, exactly like a prompt,
in `~/.config/jinn/personas/`. The body replaces the `<persona>` block of
the system prompt. Sessions default to `coding-assistant` and fall back to
it if their persona is deleted.

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
from a description.

### Persona vs. prompt vs. skill

|          | Persona                | Prompt                 | Skill                       |
| -------- | ---------------------- | ---------------------- | --------------------------- |
| Scope    | whole session          | one send               | on demand, any turn         |
| Replaced | identity block         | nothing, expands       | nothing, adds capability    |
| Fits     | "how the agent thinks" | "instructions I reuse" | "a procedure worth keeping" |

A persona is always in context, so keep it short. A skill's description is
always in context but the body isn't. A prompt's text only enters context
when you send it.

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
session: a one-line summary and behavioral guideline per tool, filtered to
the tools actually available (disabled tools and server tools that don't
match the provider are excluded from both the prompt and the API call).
The `<skills>` block lists name/description/path only — bodies load on
demand.

**Minimizing the prompt:** the persona, date, and cwd blocks are fixed.
Everything else shrinks by disabling it — tools off (`<leader>st`),
skills off (`<leader>sk`), MCP servers off (`<leader>sM`, or `auto_enable
= false` for new sessions), and no `AGENTS.md`/`CLAUDE.md` files in the
walk. A session with everything disabled carries just persona + date +
cwd.

---

## 5. Themes — colors

TOML color themes in `~/.config/jinn/themes/`. Picked with `<leader>sh`.

jinn ships five: `default`, `catppuccin-mocha`, `nord-light`,
`gruvbox-dark`, `sonokai` — reconciled into place on launch. See the
default theme for the format.

---

## Installation summary

| Surface | Location                       | Install        | An edit takes effect        |
| ------- | ------------------------------ | -------------- | ---------------------------- |
| MCP     | `~/.config/jinn/jinn.toml`     | config edit    | at once — an edited entry reconnects that server; `auto_enable` stamps new sessions |
| Skill   | `~/.agents/skills/<n>/`        | copy directory | at once — `<c-r>` rescan |
| Prompt  | `~/.config/jinn/prompts/*.md`  | write file     | at once — re-read at next use |
| Persona | `~/.config/jinn/personas/*.md` | write file     | at once — re-read at next use |
| Theme   | `~/.config/jinn/themes/*.toml` | write file     | at once — re-read at next use |

---

## What can't be extended

These are jinn's own code and are not user-extensible:

- **New keybinds, panes, or TUI layout** — the sidebar, pickers, and
  which-key are built in. A custom status bar, footer, or editor is not
  possible without a PR.
- **The agent loop** — turn dispatch, steering vs. queueing, the settled
  boundary, compaction triggers.
- **Tool-call interception** — jinn's only policy surface is
  `[[stream_rules.entry]]`: regex rules over the assistant's output, which
  can stop a named tool's call before it runs by interrupting its
  arguments. A `stream_rules.toml` beside `jinn.toml` replaces the shipped
  guard set wholesale (`jinn config init --force` restores it), and
  `jinn.toml` rules append after it. There's no general permission/approval
  UI.
- **Context assembly** — pinning, pruning, compaction tuning are config,
  not extensible.

If the user's ask lands here, the answer is: **that's a jinn feature
request.** Say so and move on. Don't invent a workaround and don't propose
writing a plugin — there is no plugin system.

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

Skills need no protocol — copy the directory, `<c-r>` to rescan. Prefer a
project-local skill (`.agents/skills/`) when the skill is about this
project, an MCP config edit when the capability is a tool.

---

## Coverage note

Section names in `jinn.toml` are exact. `[mcp.<name>]`, not
`[[mcp_server]]`. `[term]`, not `[interactive_term]`. **A misspelled
section is silently ignored** — it reads as absent, not as an error. When
an ask touches a key not shown in `configuration.md`, send the user to the
fully commented default `jinn.toml` jinn auto-creates on first run.

Only "Writing an MCP server" teaches something new. Everything else routes
to another reference in this skill or repeats facts already in
`configuration.md`. A typo'd key or path writes files to the wrong place
or nowhere — verify paths before applying changes.
