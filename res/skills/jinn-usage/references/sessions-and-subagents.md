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
| `<leader>sp` | Project picker: create a session in a curated project directory |
| `<leader>sl` | Session lifecycle recipe picker                                  |

One more slash command works the same way in the input box: `/export`
(write the session to a self-contained file). Context summarization is
handoff compaction's job — see `context-management.md`.

The **web UI** creates sessions too: the menu button (top-left `☰`) carries a
**New session** item that opens a modal over the page. Pick a project (the
curated `[[project.entry]]` list — required; with none configured the modal
says so), a lifecycle recipe (blank first, its args input appears only when
the recipe's setup takes parameters), and submit — the browser navigates to
the created session's page, and every other open page's sidebar picks the
new session up without a reload. It is the same creation path the TUI
pickers and Discord `/new` ride, so the session looks identical everywhere.

Clicking a session row in the web UI navigates with targeted patches:
the sidebar never blanks, the transcript resets and fills through the
live SSE stream's catch-up a moment later, and the sidebar's
highlighted row follows the page you are viewing. The list itself
fetches once per page load; navigation costs no list retransmission
(a session switch fetches nothing, and creating a session bumps the
list's tick once so the surviving sidebar picks up the new row).

On a phone-width browser (under 768px) the session list is an off-canvas
drawer, and the titlebar's sessions button (`#`, beside the menu button)
opens it in one tap — no menu hop. The button turns yellow with a count
badge when background sessions settled work you haven't viewed (the same
unread mark that yellows the sidebar rows); activating a session clears its
mark. At desktop widths the button is hidden — the sidebar is already on
screen.

The web UI renames a session too: click the title in the session page's
titlebar, edit it, and press `Enter` (`Esc` or clicking away cancels). The
title is the only session action on the web — archiving, closing, and
tearing down are TUI-only, as is forking.

The sidebar's **Sessions | Library** toggle switches the list between
loaded sessions and Library members. The Sessions list carries only
sessions that are loaded — never archived ones, however many the store
holds — so it stays small. Membership is changed from the row's `⋮`
menu — **add to library** / **remove from library** — and the row moves
between the two lists on every open page without a reload. The Library
view keeps a title filter box; the Sessions view carries an **Archive**
button in its place.

The **Archive** button opens a picker over every stored session, loaded
and archived alike. It mirrors the TUI session picker: rows nest as a
tree (subagent children under the session that spawned them), each
with its project and a `loaded` / `archived` tag, and the filter box
narrow them with the TUI picker's own fuzzy match. It shows at most the
first 50 rows, so it stays fast on a large store. Clicking a row opens
that session. It is the only way to reach an archived session from the
web.

A **lifecycle recipe** is a named pair of setup/teardown commands configured
in `jinn.toml` (`[[session_lifecycle.script]]`, e.g. open a fossil branch
checkout or a git worktree). A command may be a shell string or a builtin
(`{ builtin = "…" }`). The setup command may print a path on its last
line; jinn switches the new session's cwd there. Lifecycle choices can
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
| `X` | Tear down the session **and its whole subtree**; press again to confirm |

Notes:

- Archiving the last active session creates a fresh one; archiving a fresh,
  unused session simply removes it.
- `X` runs the **root session's** teardown command, then archives the
  whole closure; only the busy check is tree-wide. Descendants are
  archived as-is; their own teardown commands are skipped.
- Archived sessions come back through the session browser (`<leader>ss`).
- A replacement session created by archiving inherits the archived
  session's model and reasoning effort, tool/skill toggles, and
  auto-enabled MCP servers.

## Resuming a session (`c`) and lifecycle setup (`s`)

| Key | Action                                                 |
| --- | ------------------------------------------------------ |
| `c` | Resume the session under the cursor: re-send its context to the provider (existing history only, focus unchanged) |
| `s` | Run the session's lifecycle setup command (first run only) |

`c` re-sends the session's existing history to the provider. A UI-only
"↻ session resumed" marker entry is added; focus stays where it is, and
the provider receives the existing history alone. Use it after a
rate-limit or an errored turn, or to resume a session rehydrated from
disk (in-memory sessions only). `s` runs the setup half of the session's
lifecycle recipe (`[[session_lifecycle.script]]`), leaving the model turn
alone; once setup has run, further presses are inert.

## Changing a session's cwd

| Key          | Action                                                    |
| ------------ | --------------------------------------------------------- |
| `<M-c>`      | Change cwd, browsing from the session's current directory |
| `<M-d>`      | Change cwd, browsing from `$HOME`                         |
| `<leader>cd` | Type a path directly (ghosts a fuzzy match as you type; `<tab>` accepts it) |

## Forking

Forks copy history **up to a chosen entry** into a new sibling session, to
branch a conversation at that point.

1. In normal mode, select the entry to fork from (`j`/`k` or jumps).
2. Press `f`: new session with history up to and including that entry,
   linked as a sibling in the tree.
3. `F` creates a **new session whose history is just that entry**; it
   works on user and assistant entries.

Forks always get fork origin; a fork of a subagent always has the
`spawn_subagent` tool enabled.

## Subagents (the `spawn_subagent` tool)

Ask the agent to "spawn a subagent for X"; it calls the `spawn_subagent`
tool, which creates a **regular session** linked as a child:

- **Fresh history.** It inherits your model, cwd, tools, skills, and MCP
  servers. It starts with an **empty task list** of its own: put the
  whole assignment in the prompt.
- **Give it the context it needs.** The parent's history stays in the
  parent; everything the child knows arrives in its prompt.
- **The parent blocks.** The `spawn_subagent` tool call waits until the
  child finishes and forwards the child's final message as the tool
  result.
- **You can steer it mid-run.** Switch to the child session in the
  sidebar (it's marked with the subagent symbol) and send messages:
  with the input in **Steer** mode (the default, `<M-q>` toggles) the
  message reaches the child between tool calls, your live channel to a
  running subagent. The child's turn end disconnects it from the
  parent, and a message sent in queue mode arrives after the turn. See
  `chat-input-tokens.md`.
- **The call shows in your history as a purple block.** Press `<enter>`
  on it to jump into that child session (archived children are
  unarchived automatically).
- **One level deep.** The `spawn_subagent` tool starts disabled in a
  subagent's session; re-enable it via the tool picker (`<leader>st`) if
  you want nested agents.
- **Cancellations forward to the parent** as a failure result.

## Where sessions live

Sessions and history persist to SQLite under `~/.local/share/jinn`
(`sessions.db`). Full-text search across sessions is covered in
`pickers-and-search.md`.
