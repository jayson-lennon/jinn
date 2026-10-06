# Extending jinn

jinn has **no plugin system**. This is a design decision, not an omission.

So "how do I add a feature to jinn" has a different answer than you'd expect, and
this document is the answer. It covers the five surfaces you extend jinn through,
what each is good for, and how to install each one.

**When the user asks to add a capability to jinn, the routing decision is
yours.** Work through it in order and stop at the first fit.

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

MCP is the load-bearing one. Most of what people imagine a plugin system for —
a Jira integration, a video editor, a database client, web search, an issue
tracker — is an MCP server, and jinn is already an MCP client.

**MCP vs. skills is the common confusion.** An MCP server gives the model
_new actions_. A skill gives it _knowledge of a procedure_. "Search the video
editor's timeline for a clip" is a tool. "How we run task loops" is a
skill. A skill can bundle shell scripts and reference material and have the
agent use `bash` to run them, which is how non-tool capabilities still work
without MCP.

---

## 1. MCP servers — adding executable capability

### What it is

An MCP server is a program that exposes tools over the Model Context Protocol.
jinn launches or connects to it, then exposes its tools to the model as
`mcp__<server>__<tool>`. Full config, transports, and the picker protocol live
in `mcp-servers.md` — read it before answering a config question.

The key fact for routing: **an MCP tool is a plain function** —
`name` / `description` / JSON Schema / `execute`. That's the entire contract.
Anything expressible as "the model calls this with these arguments" is an MCP
server.

### Installing one

Three cases. Confirm which before writing anything.

**Case 1 — an MCP server already exists (most common).**

Find it, then add a three-line block to `~/.config/jinn/jinn.toml`:

```toml
[mcp.video_editor]
command = "npx"
args = ["video-editor-mcp", "--stdio"]
```

A restart is required. The server is **off by default per session** — after
restarting, open the MCP picker (`<leader>sM`) and enable it for the session.
It's off by default because enabled tools occupy context even when unused, and
most sessions need none of them.

**Case 2 — a server exists but needs a wrapper.** Some servers need an env var
that shouldn't be hardcoded, or a working directory. Add a local script and
point `command` at it:

```toml
[mcp.video_editor]
command = "/home/you/bin/video-editor-mcp.sh"
```

`${VAR}` expansion happens once at startup for headers; for `command`/`args`,
put the real value in the script. See `mcp-servers.md`.

**Case 2b — a plain HTTP API with no MCP server available.** Write a thin
server. See "Writing an MCP server" below.

**Case 3 — the capability is a local script with no server at all.** The `bash`
tool already exists. If the user wants "a command that does X," don't build an
MCP server; make sure `bash` is enabled and put the command in a skill so the
agent knows it exists. Especially true for one-off project scripts: project
skills live in `.agents/skills/` and get committed.

### Writing an MCP server

Only when no server exists. An MCP server is ordinary code in the user's
preferred language — there's no jinn-specific runtime, and it works with any
MCP client. It needs three things:

1. **A manifest.** In TypeScript/JS: `package.json` with
   `{"pi": {...}}` — no, that's pi. For jinn there's no manifest at all.
   jinn only needs the server process to speak MCP on stdio (or HTTP).
2. **A process that speaks MCP** on the chosen transport, exposing a handful of
   tools.
3. **A `jinn.toml` block** to launch it.

For an HTTP-only capability, the minimal shape is a stdio server that shells
out to `curl` or uses your language's HTTP client, wrapping one endpoint as one
tool. In TypeScript the fast route is the official SDK:
`@modelcontextprotocol/sdk`. Minimum viable server, in TypeScript:

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

**Say this to the user plainly:** the model can only call what the server
exposes, and an MCP server is arbitrary code the user chooses to run. jinn has
no security model — see the README's Security section. Mention it once; don't
re-litigate it.

---

## 2. Skills — teaching a procedure

### What it is

A skill is a directory with a `SKILL.md`: YAML frontmatter (`name`,
`description`) plus markdown instructions. jinn lists every discovered skill's
name, description, and path in the system prompt's `<skills>` block; the full
body loads when the agent decides to read it. That's progressive disclosure —
descriptions always in context, bodies on demand.

This is a **cheaper and more portable surface than MCP** than you might expect.
A skill that bundles a `scripts/` directory and tells the agent to run them via
`bash` gives you executable capability with zero protocol work, and the same
skill works in any harness using the Agent Skills format.

### Where they live and how discovery works

Precedence, highest first:

1. `<project>/.agents/skills/<name>/SKILL.md` — project-local, committable
2. `~/.agents/skills/<name>/SKILL.md` — user-global
3. `<system skills dir>` — jinn's own bundled skills

A project skill with the same name as a global shadows the global. Discovery
walks from the session's cwd up to the project root, stopping at the VCS root
(`.git`, `.fslckout`) or `$HOME`. Closer files win.

`jinn install` writes bundled skills to `~/.agents/skills/`. The four it ships:
`jinn-usage`, `simple-task-loop`, `phased-task-loop`, `micro-task-loop`.

### Installing one

**From a registry or repo (most common).** Skills are plain files — there's no
install command. Copy the directory:

```sh
# user-global
git clone <repo> /tmp/s && cp -r /tmp/s/my-skill ~/.agents/skills/

# project-local, committable
cp -r my-skill <project>/.agents/skills/
```

A skill directory may contain anything else it needs: `references/*.md`,
`scripts/*.sh`, `assets/`. jinn only reads `SKILL.md`; the agent reads the rest
on demand.

**Picking up a new skill without restarting.** The skill picker
(`<leader>sk`, `<c-r>` to rescan) lets you enable/disable per session. `<c-r>`
forces a rescan of the skills directory, so a freshly copied directory is
picked up without a restart. That's the difference from MCP, which needs one.

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

The `description` is the only part always in context — write it for retrieval,
with the words a user would actually type. Everything else is read on demand.

---

## 3. Prompts — reusable instructions you type

### What it is

A prompt is a markdown file with `+++` TOML frontmatter. It expands from `#name`
in the chat input. Prompts stay collapsed as `#name` in the input and history
until sent, and the on-disk file is never mutated by use.

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

`jinn install` seeds this directory with eight: `plan`, `approve-plan`,
`gap-analysis`, `research`, `goal`, `meta-prompt`, `generate-persona`,
`_compaction`. Type `#` in the input to see what's available, and `#foo#` to
expand `foo` early for editing before sending.

---

## 4. Personas — the agent's identity

### What it is

A persona is markdown with `+++` TOML frontmatter, exactly like a prompt, in
`~/.config/jinn/personas/`. The body replaces the `<persona>` block of the
system prompt. Sessions default to `coding-assistant` and fall back to it if
their persona is deleted.

### Installing one

```toml
# ~/.config/jinn/personas/senior-architect.md
+++
name = "senior-architect"
description = "Reviews before writing, challenges scope."
+++
```

Select with the persona picker (`<leader>se`, or `c` in the sidebar's persona
section). There's a `#generate-persona` prompt for deriving one from a
description.

### Persona vs. prompt vs. skill

|          | Persona                | Prompt                 | Skill                       |
| -------- | ---------------------- | ---------------------- | --------------------------- |
| Scope    | whole session          | one send               | on demand, any turn         |
| Replaced | identity block         | nothing, expands       | nothing, adds capability    |
| Fits     | "how the agent thinks" | "instructions I reuse" | "a procedure worth keeping" |

A persona is always in context, so keep it short. A skill's description is
always in context but the body isn't. A prompt's text only enters context when
you send it.

---

## 5. Themes — colors

TOML color themes in `~/.config/jinn/themes/`. Picked with `<leader>sh`.

`jinn install` seeds four: `catppuccin-mocha`, `nord-light`, `gruvbox-dark`,
`sonokai`. See the default theme for the format.

---

## Installation summary

| Surface | Location                       | Install        | Live reload?         |
| ------- | ------------------------------ | -------------- | -------------------- |
| MCP     | `~/.config/jinn/jinn.toml`     | config edit    | no — jinn restart    |
| Skill   | `~/.agents/skills/<n>/`        | copy directory | yes — `<c-r>` rescan |
| Prompt  | `~/.config/jinn/prompts/*.md`  | write file     | no — jinn restart    |
| Persona | `~/.config/jinn/personas/*.md` | write file     | no — jinn restart    |
| Theme   | `~/.config/jinn/themes/*.toml` | write file     | no — jinn restart    |

---

## What can't be extended

These are jinn's own code and are not user-extensible:

- **New keybinds, panes, or TUI layout** — the sidebar, pickers, and which-key
  are built in. A custom status bar, footer, or editor is not possible without
  a PR.
- **The agent loop** — turn dispatch, steering vs. queueing, the settled
  boundary, compaction triggers.
- **Tool-call interception** — jinn's only policy surface is
  `[[stream_rules.entry]]`: regex rules over the assistant's output, which can
  stop a named tool's call before it runs by interrupting its arguments.
  There's no general permission/approval UI.
- **Context assembly** — pinning, pruning, compaction tuning are config, not
  extensible.

If the user's ask lands here, the honest answer is: **that's a jinn feature
request.** Say it plainly and move on. Don't invent a workaround and don't
propose writing a plugin — there is no plugin system.

---

## The offer-to-edit protocol

Follow `configuration.md` for anything touching `jinn.toml`. Summary:

1. Name the file and section; explain the setting in one or two sentences.
2. Show a snippet with values the user would actually want.
3. Ask: _"Want me to add this to `~/.config/jinn/jinn.toml`? You'll need to
   restart jinn for it to take effect."_
4. On acceptance, read the file, apply surgically (preserve comments, ordering,
   unknown keys), confirm what changed.
5. Remind them of the restart.

For skills, no protocol is needed — copy the directory, no restart, `<c-r>` to
rescan. Prefer a project-local skill (`.agents/skills/`) when the skill is
about this project, or an MCP config edit when the capability is a tool.

---

## Coverage note

Section names in `jinn.toml` are exact. `[mcp.<name>]`, not `[[mcp_server]]`.
`[term]`, not `[interactive_term]`. A misspelled section is **silently
ignored** — it reads as absent, not as an error. When an ask touches a key not
shown in `configuration.md`, point the user at the fully commented default
`jinn.toml` that jinn auto-creates on first run.

The "Writing an MCP server" section is the only place this document teaches
something new. Everything else routes to another reference in this skill or
repeats facts already in `configuration.md`. A typo'd key or path writes files
to the wrong place or no place at all, so verify paths before applying changes.
