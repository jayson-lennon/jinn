# Sessions & Subagents

jinn runs many concurrent sessions in one process. They form a **tree**:
forks and subagents are linked to the session that created them, and the
sidebar shows children nested under their parent (subagents get a dedicated
symbol; forks get the fork origin even when forked from a subagent).

## Creating sessions

| Key | Action |
| --- | --- |
| `n` | New session (immediately active) |
| `N` | New session, choosing a **lifecycle recipe** first |
| `<c-n>` | New session from most scopes (also inside pickers) |
| `<leader>so` | Project picker — create a session in a curated project directory |
| `<leader>sl` | Session lifecycle recipe picker |

A **lifecycle recipe** is a named pair of setup/teardown shell commands
configured in `jinn.toml` (`[[session_lifecycle]]` — e.g. open a fossil branch
checkout or a git worktree). The setup command may print a path on its last
line; jinn switches the new session's cwd there. Lifecycle choices can also
take positional arguments (jinn prompts for them).

## Navigating sessions

| Key | Action |
| --- | --- |
| `<c-l>` → Sessions section | Focus the sidebar's session tree (`<M-s>` jumps straight there) |
| `j` / `k` / `J` / `K` | Move within / between sidebar sections |
| `<enter>` | Switch to the selected session (live preview while browsing) |
| `i` | Switch to the session and enter input mode |
| `<leader>ss` | Full-screen session browser (filter by name; shows date + project + tree) |

Sessions remember their own model, persona, cwd, enabled tools/skills, and
context state. Switching is instant; work continues in the background.

## Renaming, archiving, tearing down

Within the sidebar Sessions section:

| Key | Action |
| --- | --- |
| `r` | Rename the session (type, `<enter>` confirm, `<esc>` cancel) |
| `a` | Archive the session (removed from the sidebar, kept on disk) |
| `A` | Archive the session **and its entire subtree** |
| `x` | Close the selected session |
| `t` | **Tear down**: run the session's `teardown_command`, then archive |
| `X` | Tear down the session **and its whole subtree** — press again to confirm |

Notes:
- Archiving the last active session creates a fresh one, so you're never
  stranded; archiving a never-used empty session simply removes it.
- Teardown is all-or-nothing for the tree: if any member fails its teardown or
  is busy, nothing is archived.
- Archived sessions come back through the session browser (`<leader>ss`).
- A replacement session created by archiving inherits the global default
  reasoning effort.

## Continuing a session's environment

| Key | Action |
| --- | --- |
| `c` | Switch to the session **and** re-run its setup command |
| `s` | Re-run the setup command (without switching) |

Useful when a machine rebooted or you want the environment refreshed (branch
re-checkout, dev servers, etc.).

## Changing a session's cwd

| Key | Action |
| --- | --- |
| `<M-c>` | Change cwd, browsing from the session's current directory |
| `<M-d>` | Change cwd, browsing from `$HOME` |
| `<leader>cd` | Type a path directly |

## Forking

Forks copy history **up to a chosen entry** into a new sibling session — for
branching a conversation: "try it the other way from that message."

1. In normal mode, select the entry to fork from (`j`/`k` or jumps).
2. Press `f` — new session with history up to and including that entry,
   linked as a sibling in the tree.
3. `F` instead seeds a **new session with just that entry** (no inherited
   history).

Forks always get fork origin; a fork of a subagent always has the `task` tool
enabled.

## Subagents (the `task` tool)

Ask the agent to "spawn a subagent for X" — it calls the `task` tool, which
creates a **regular session** linked as a child:

- Fresh history; inherits your model, cwd, tools, skills, MCP servers, and a
  snapshot of your task list (which then evolves independently).
- The parent's `task` tool call blocks until the child finishes and forwards
  the child's final message as the tool result.
- While it runs you can steer it: switch to the child session in the sidebar
  (it's marked with the subagent symbol) and send messages — mid-turn steering
  queues into the child like any session.
- A `task` call in your history renders as a purple block; press `<enter>` on
  it to jump into that child session (archived children are unarchived
  automatically).
- Subagents cannot spawn further subagents: the `task` tool starts disabled in
  their session. Re-enable it via the tool picker (`<leader>st`) if you really
  want nested agents.
- Cancellations forward to the parent as a failure result.

## Where sessions live

Sessions and history persist to SQLite under `~/.local/share/jinn`
(`sessions.db`). Full-text search across sessions is covered in
`pickers-and-search.md`.
