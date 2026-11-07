# Sessions & Subagents

jinn runs many concurrent sessions in one process. They form a **tree**:
forks and subagents are linked to the session that created them, and the
sidebar shows children nested under their parent (subagents get a dedicated
symbol; forks get the fork origin even when forked from a subagent).

## Creating sessions

| Key          | Action                                                           |
| ------------ | ---------------------------------------------------------------- |
| `n`          | New session (immediately active)                                 |
| `<c-n>`      | New session from most scopes (also inside pickers)               |
| `/new`       | Type it in the chat input                                        |
| `<leader>sp` | Project picker — create a session in a curated project directory |
| `<leader>sl` | Session lifecycle recipe picker                                  |

Three more slash commands work the same way in the input box: `/compact`
(summarize history), `/compact-all` (ignore the token reserve), and
`/export` (write the session to a self-contained file).

A **lifecycle recipe** is a named pair of setup/teardown commands configured
in `jinn.toml` (`[[session_lifecycle.script]]` — e.g. open a fossil branch
checkout or a git worktree). A command may be a shell string or a builtin
(`{ builtin = "…" }`). The setup command may print a path on its last
line — jinn switches the new session's cwd there. Lifecycle choices can
also take positional arguments (jinn prompts for them).

## Navigating sessions

| Key                        | Action                                                                    |
| -------------------------- | ------------------------------------------------------------------------- |
| `<c-l>` → Persona section  | Focus the sidebar (`<M-s>` jumps straight to the Sessions section)        |
| `j` / `k` / `J` / `K`      | Move within / between sidebar sections                                    |
| `<enter>`                  | Switch to the selected session (live preview while browsing)              |
| `i`                        | Switch to the session and enter input mode                                |
| `<leader>ss`               | Session browser popup (filter by name; shows date + project + tree) |

Sessions remember their own model, persona, cwd, enabled tools/skills, and
context state. Switching is instant; work continues in the background.

## Renaming, archiving, tearing down

Within the sidebar Sessions section:

| Key | Action                                                                   |
| --- | ------------------------------------------------------------------------ |
| `r` | Rename the session (type, `<enter>` confirm, `<esc>` cancel)             |
| `a` | Archive the session (removed from the sidebar, kept on disk; press again to confirm) |
| `A` | Archive the session **and its entire subtree** (press again to confirm)  |
| `x` | Close the selected session (press again to confirm)                      |
| `t` | **Tear down**: run the session's `teardown_command` (the session stays open) |
| `X` | Tear down the session **and its whole subtree** — press again to confirm |

Notes:

- Archiving the last active session creates a fresh one; archiving a
  never-used empty session simply removes it.
- `X` runs the **root session's** teardown command, then archives the
  whole closure; only the busy check is tree-wide. Descendants are
  archived without their own teardown commands running.
- Archived sessions come back through the session browser (`<leader>ss`).
- A replacement session created by archiving inherits the archived
  session's model and reasoning effort, tool/skill toggles, and
  auto-enabled MCP servers.

## Resuming a session (`c`) and lifecycle setup (`s`)

| Key | Action                                                 |
| --- | ------------------------------------------------------ |
| `c` | Resume the session under the cursor: re-send its context to the provider (no new message, no switch) |
| `s` | Run the session's lifecycle setup command (only when it has not run yet) |

`c` re-sends the session's existing history to the provider. A UI-only
"↻ session resumed" marker entry is added — no user or assistant message
is sent, and focus stays where it is. Use it after a rate-limit or an
errored turn, or to resume a session rehydrated from disk (`c` acts only
on in-memory sessions). `s` runs the setup half of the session's
lifecycle recipe (`[[session_lifecycle.script]]`), not the model turn —
it does nothing once setup has already run.

## Changing a session's cwd

| Key          | Action                                                    |
| ------------ | --------------------------------------------------------- |
| `<M-c>`      | Change cwd, browsing from the session's current directory |
| `<M-d>`      | Change cwd, browsing from `$HOME`                         |
| `<leader>cd` | Type a path directly                                      |

## Forking

Forks copy history **up to a chosen entry** into a new sibling session, to
branch a conversation at that point.

1. In normal mode, select the entry to fork from (`j`/`k` or jumps).
2. Press `f` — new session with history up to and including that entry,
   linked as a sibling in the tree.
3. `F` instead creates a **new session with just that entry** (no
   inherited history) — it works on user and assistant entries.

Forks always get fork origin; a fork of a subagent always has the
`spawn_subagent` tool enabled.

## Subagents (the `spawn_subagent` tool)

Ask the agent to "spawn a subagent for X" — it calls the `spawn_subagent`
tool, which creates a **regular session** linked as a child:

- **Fresh history.** It inherits your model, cwd, tools, skills, and MCP
  servers. It starts with an **empty task list** — your list is not
  copied, so the whole assignment must be in the prompt.
- **Give it the context it needs.** It cannot see what you were doing.
- **The parent blocks.** The `spawn_subagent` tool call waits until the
  child finishes and forwards the child's final message as the tool
  result.
- **You can steer it mid-run.** Switch to the child session in the
  sidebar (it's marked with the subagent symbol) and send messages —
  with the input in **Steer** mode (the default, `<M-q>` toggles) the
  message reaches the child between tool calls. This is the only way to
  steer a live subagent: its turn end disconnects it from the parent, so
  a queued message arrives too late. See `chat-input-tokens.md`.
- **The call shows in your history as a purple block.** Press `<enter>`
  on it to jump into that child session (archived children are
  unarchived automatically).
- **No nesting.** Subagents cannot spawn further subagents — the
  `spawn_subagent` tool starts disabled in their session. Re-enable it
  via the tool picker (`<leader>st`) if you really want nested agents.
- **Cancellations forward to the parent** as a failure result.

## Where sessions live

Sessions and history persist to SQLite under `~/.local/share/jinn`
(`sessions.db`). Full-text search across sessions is covered in
`pickers-and-search.md`.
