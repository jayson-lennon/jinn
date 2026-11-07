# UI Map — what is on screen

Identifies every surface jinn draws. When the user asks "what is the thing on
the right / at the bottom / above the input", find it here by position or
appearance, then follow the cross-link for how it behaves.

This file is **defaults** for the *layout*; the keys shown are the default
bindings and may be rebound. The which-key popup (`?` in normal-like scopes,
`<f1>` in input mode) shows the bindings live in the current scope, and the
live screen is the truth if this file and your build disagree.

## Layout

Chat layout, top to bottom — main column (left), then the minimap column,
the border column, and the full-height sidebar:

```text
┌──────────────────────────────┬─┬─┬──────────────┐
│ Tab bar (1 row)              │m│││              │
├──────────────────────────────┤i│││              │
│                              │n│││              │
│  Chat log                    │i│b│   Sidebar    │
│  (scrolling conversation)    │m│o│  (full       │
│                              │a│r│   height)    │
│                              │p│d│              │
│                              │ │e│              │
├──────────────────────────────┤ │r│              │
│ Streaming indicator (1 row)  │ │ │              │
├──────────────────────────────┤M│ │              │
│ Input box (dynamic height)   │e│ │              │
├──────────────────────────────┤t│ │              │
│ Status bar (2 rows)          │e│ │              │
└──────────────────────────────┴─┴─┴──────────────┘
   main column                    │ border
                    minimap ──────┘
```

The minimap column and the dictation meter strip share the same column: the
minimap runs the height of the chat log, and the meter strip occupies the
same column from the streaming-indicator row down to the terminal bottom
(displayed only while recording; idle it paints dotted cells — see
`dictation.md`).

A dashboard tab replaces the whole main column (below); full-width tabs take
the entire window, edge to edge.

## Tab bar (top strip)

The 1-row strip at the very top. One label per registered tab slice plus the
always-present **Chat** tab (first). Tab `<Tab>` cycles between them; the
active tab highlights.

- **Chat** — the conversation layout described above.
- **Dashboard** — one row per actor with a per-row lifecycle status note
  (e.g. the canvas server's URL, the session pruner's progress, the
  dictation model's phase). Navigate rows with `j`/`k`, jump with `g`/`G`.
  Rows publish their own status; several references mention "the dashboard
  row" as where a subsystem's live state shows (dictation, canvas, session
  pruning).

The label is derived from the slice's scope id (e.g. `dashboard` →
"Dashboard"), so new tab slices appear here automatically.

## Main column

### Chat log
The scrolling conversation history, center of the content area. Selection,
pinning, exclusion, and the audit popup all operate here — see
`context-management.md` for the keys. The audit popup overlays this area;
see Popups below.

### Streaming indicator
The 1-row line between the chat log and the input box. A spinner plus
"Working..." while the active session is sending/streaming/compacting; idle
rows are blank. Shows a queued-message count when messages are waiting.
The right edge names the session kind (subagent / bot) in its own color at
all times — busy or idle alike.

### Input box
The message composer, above the status bar. Height grows with the message
(grows to a cap, ~half the terminal) and collapses to one row. Typing here
is input mode; `#`/`@`/`/` open the autocomplete popup — see
`chat-input-tokens.md`. The autocomplete popup hangs off this box.

### Status bar
The bottom 2 rows of the main column.

- **Line 1**: the session's working directory (shortened).
- **Line 2**: token stats (context used / budget), turn count, working time,
  and the model — `({provider})/{model}`, or "no model selected". The right
  side swaps to a transient hint when one is showing (e.g. "yanked 42
  terminal lines to the clipboard"); during a hint the model name yields
  its spot and returns after.

## Minimap column
The 1-column strip between the chat log and the border: a token-density map
of the chat log (lighter = fewer tokens, a `>` arrow pointing at the current
selection). Tuned by `[ui.minimap] max_tokens` in `jinn.toml` — see
`configuration.md`. The dictation meter replaces this column's lower part
while recording.

## Border column
The 1-column vertical line between the main column and the sidebar. Its
color follows focus: the focused surface's accent, dimmed when focus is
elsewhere.

## Sidebar (right column, full height)

Sections render top-down in this order; empty sections are skipped, so the
visible stack is always the full set of populated ones. `<c-l>` focuses the
sidebar, `<M-s>` jumps straight to Sessions, `<c-h>`/`<esc>` leaves.
`<c-w>` enters resize mode (`h`/`l`).

| Section | Shows | See |
| --- | --- | --- |
| Persona | The session's active persona | `personas.md` |
| Bots | Child bot sessions and their latest reports | `bots.md` |
| Pins | Pinned context entries | `context-management.md` |
| Task list | The session's phases/tasks, with a preview pane | `sessions-and-subagents.md` |
| MCP servers | Per-session enabled MCP servers + live status (read-only here) | `mcp-servers.md` |
| Library | Sessions moved out of the main tree with `L` | `sessions-and-subagents.md` |
| Sessions | Open sessions (the main list) | `sessions-and-subagents.md` |

Sidebar overlays: the rename-session input, close/archive confirmation
prompts (anchored to the sessions cursor row), and popups that hang off the
sidebar but may reach left across the chat column (e.g. the bots properties
popup, `P` in the Bots section — see `bots.md`). The wide-reaching popups
paint on a top layer (the code's "floating surfaces" region), so they draw
over everything on screen.

## Popups and overlays

| Surface | Opens with | What it is | See |
| --- | --- | --- | --- |
| Which-key popup | `?` (normal-like scopes) / `<f1>` (input mode) | Bindings for the current scope, bottom-right | `keybindings.md` |
| Audit popup | `a` on a selected entry | Per-entry context state (included/excluded, tokens) over the chat log | `context-management.md` |
| Autocomplete popup | typing `#`, `@`, or `/` in input | Prompt/attachment/command completions anchored to the input box | `chat-input-tokens.md` |
| Terminal overlay | `<M-t>` | The interactive PTY overlay; `<c-g>` control mode | `terminal-overlay.md` |
| Quake bar | ``<M-`>`` | Full-width drop-down console over the whole screen: session + global command log, with inline submit | `keybindings.md` |
| Dictation meter | `<M-i>` arms/records | The minimap column from the streaming-indicator row down: dotted idle strip, cyan level bars while recording | `dictation.md` |
| CWD input | `<M-c>` / `<M-d>` | Type-a-path input for changing the session working directory | `keybindings.md` |
| Lifecycle arg input | launching a workflow with args | Input overlay collecting the recipe's arguments | `extending-jinn.md` |

Two full-screen surfaces complete the set alongside the overlays: the
**dashboard tab** (see Tab bar) and the **task-list browser** (`s` in the
sidebar's task-list section), which replaces the main column content.

## Pickers

The `<leader>s` family opens centered filtered lists — models, sessions,
personas, tools, skills, MCP servers, themes, reasoning effort, endpoints,
projects, lifecycles, bots, workflows, applets, the OpenRouter model market,
stream rules. One popup each; keys and the full table live in
`pickers-and-search.md`. Picker footers show a context-cost figure where it
applies.

## Feature → screen location (reverse lookup)

| If asked about… | It lives in |
| --- | --- |
| Model name / token count / turn count | Status bar, line 2 |
| Session cwd | Status bar, line 1 |
| "Working…" / queue count / session kind | Streaming indicator row |
| Yank / hint messages ("yanked …") | Status bar, line 2 right side |
| Token-density strip / the `>` arrow | Minimap column |
| The colored vertical line | Border column (focus accent) |
| Pinned entries / isolate / audit | Chat log (selection) + Pins sidebar section + audit popup |
| Task phases / task list | Task-list sidebar section + task-list browser (`s`) + tab |
| Child bots / bot reports | Bots sidebar section + properties popup (`P`) |
| Archived sessions | Library sidebar section |
| MCP server status | MCP servers sidebar section (read-only) + MCP picker/inspector |
| Voice recording level | Dictation meter strip (minimap column, lower part) |
| Command log / inline command submit | Quake bar (``<M-`>``) |
| Actor lifecycle / canvas status / prune progress | Dashboard tab |
| Interactive shell | Terminal overlay (`<M-t>`) |
| Persona in use | Persona sidebar section |
| Every `<leader>s*` chooser | Pickers (see `pickers-and-search.md`) |
