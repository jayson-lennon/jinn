# UI Map: what is on screen

Identifies every surface jinn draws, in the terminal and in the browser
web UI. When the user asks "what is the thing on the right / at the
bottom / above the input", find it here by position or appearance, then
follow the cross-link for how it behaves. If the ask does not say which
surface, check both layouts.

This file is **defaults** for the *layout*; the keys shown are the default
bindings and may be rebound. The which-key popup (`?` in normal-like scopes,
`<f1>` in input mode) shows the bindings live in the current scope, and the
live screen is the truth if this file and your build disagree. The
original web UI (`/web`) has no keybinds — it is all clicks (see
[Web UI layout](#web-ui-layout)); `/web2` has a few, all in its composer
and overlays (see [Web UI 2 layout](#web-ui-2-layout-web2)).

## Layout

Chat layout, top to bottom: main column (left), then the minimap column,
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
(displayed only while recording; idle it paints dotted cells; see
`dictation.md`).

A dashboard tab replaces the whole main column (below); full-width tabs take
the entire window, edge to edge.

## Tab bar (top strip)

The 1-row strip at the very top. One label per registered tab slice plus the
always-present **Chat** tab (first). Tab `<Tab>` cycles between them; the
active tab highlights.

- **Chat**: the conversation layout described above.
- **Dashboard**: one row per actor with a per-row lifecycle status note
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
pinning, exclusion, and the audit popup all operate here (see
`context-management.md` for the keys). The audit popup overlays this area;
see Popups below.

### Streaming indicator
The 1-row line between the chat log and the input box. A spinner plus
"Working..." while the active session is sending/streaming/compacting; idle
rows are blank. Shows a queued-message count when messages are waiting.
The right edge names the session kind (subagent / bot) in its own color at
all times, busy or idle alike.

### Input box
The message composer, above the status bar. Height grows with the message
(grows to a cap, ~half the terminal) and collapses to one row. Typing here
is input mode; `#`/`@`/`/` open the autocomplete popup (see
`chat-input-tokens.md`). The autocomplete popup hangs off this box.

### Status bar
The bottom 2 rows of the main column.

- **Line 1**: the session's working directory (shortened).
- **Line 2**: token stats (context used / budget), turn count, working time,
  and the model: `({provider})/{model}`, or "no model selected". The right
  side swaps to a transient hint when one is showing (e.g. "yanked 42
  terminal lines to the clipboard"); during a hint the model name yields
  its spot and returns after.

## Minimap column
The 1-column strip between the chat log and the border: a token-density map
of the chat log (lighter = fewer tokens, a `>` arrow pointing at the current
selection). Tuned by `[ui.minimap] max_tokens` in `jinn.toml` (see
`configuration.md`). The dictation meter replaces this column's lower part
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
popup, `P` in the Bots section; see `bots.md`). The wide-reaching popups
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

The `<leader>s` family opens centered filtered lists: models, sessions,
personas, tools, skills, MCP servers, themes, reasoning effort, endpoints,
projects, lifecycles, bots, workflows, applets, the OpenRouter model market,
stream rules. One popup each; keys and the full table live in
`pickers-and-search.md`. Picker footers show a context-cost figure where it
applies.

## Web UI layout

The browser page jinn serves beside the TUI (what it is and how to open
it: `webui.md`). At desktop widths it is a session list on the left and
the session on the right:

```text
┌──────────────┬──────────────────────────────────────────────┐
│ # ☰  Title                 [model ▾]  ← titlebar (1 row)     │
├──────────────┼──────────────────────────────────────────────┤
│ Sessions     │                                              │
│ [Sessions|   │  Transcript                                  │
│  Library]    │  (scrolling conversation)                    │
│ [filter…]    │                                              │
│              │                                              │
│ ▎Session A ⋮ │                     ↻  ← spinner, while busy  │
│  Session B ⋮ ├──────────────────────────────────────────────┤
│  Session C ⋮ │ [Message…            ] [+] [Q] [Cancel]      │
│              │                    or [+] [Send]  ← composer  │
└──────────────┴──────────────────────────────────────────────┘
  sidebar                   main
```

The landing page (`/web`) is the left column only. Everything in the
titlebar sits at the top of the main column.

| Element | What it is | See |
| --- | --- | --- |
| `☰` (top-left) | The menu button. Opens a full-height slide-in panel: New session, Persona, MCP servers, Tools, Skills, Applets, Sessions, and a Light/Dark/System control at its foot. The per-session items are hidden on the landing page. | `webui.md`, then the feature's reference |
| `#` + count (left of the title, phone widths only) | The sessions button: opens the session drawer. Turns **yellow** with a number when background sessions finished work you have not viewed. | `sessions-and-subagents.md` |
| Title | The session's title. Click it to rename. | `sessions-and-subagents.md` |
| `[model]` (titlebar, right) | The model picker button, naming the current model. | `models-and-providers.md` |
| Sidebar `Sessions \| Library` | Switches the list between loaded sessions and Library members. Archived sessions are never in the Sessions list. | `sessions-and-subagents.md`, `bots.md` |
| Sidebar `[Archive]` (Sessions view) | Opens a picker over every stored session, loaded and archived, tree-nested, with the TUI picker's fuzzy filter. | `sessions-and-subagents.md` |
| Sidebar `filter library…` box (Library view) | Narrows the Library list by title. | `sessions-and-subagents.md` |
| **Blue** session row | The session this page is viewing. | `models-and-providers.md` |
| **Yellow** session row | A session that finished work while you were not viewing it. | `models-and-providers.md` |
| `· bot` after a row's title | A bot session. | `bots.md` |
| `↻` before a row's title | That session is working right now. | `sessions-and-subagents.md` |
| `⋮` on a session row | The row's action menu: add to / remove from the Library. | `sessions-and-subagents.md` |
| Transcript | The conversation: markdown, collapsible thinking and tool blocks, a Sources list under entries that cite. | `webui.md` |
| `↻` above the composer | The session is working. | `webui.md` |
| `[+]` | Attach an image. | `chat-input-tokens.md` |
| `Message…` box | The composer. `Enter` inserts a newline; it never sends. | `webui.md` |
| `[Q]` / `[Cancel]` (while busy) | Queue the message until the turn settles / stop the turn. | `webui.md` |
| `[Send]` (while idle) | Send the message. | `webui.md` |
| "Loading session…" card | A full-page overlay while a clicked session's page loads. | `sessions-and-subagents.md` |

On a phone (under 768px) the sidebar slides off-canvas and the `#`
button appears; details are in `webui.md`.

## Web UI 2 layout (`/web2`)

A second browser page the instance serves beside the TUI and the original
web UI (what it is and how to open it: `webui2.md`). Below 768px the
sidebar is an off-canvas drawer behind a `#` button left of the `☰`
(the menu's **Sessions** item opens it too); at wider widths it is the
fixed column drawn here, and the `#` is not shown.

```text
┌──────────────┬──────────────────────────────────────────────┐
│Sessions│Libra│ ☰ Session title        [provider/model]      │ ← title bar
│ ▎Session A   ├──────────────────────────────────────────────┤
│  Session B ● │  Transcript                                  │
│        ⋮     │  (scrolling conversation)                    │
│              │  a user message with an attached image:      │
│  Archive     │    you  look at this                         │
│              │    [img]                                     │
│              ├──────────────────────────────────────────────┤
│              │ [chip: dot.png ✕]                            │ ← chips
│              │ [Message jinn…      ] [+]        [Send]      │ ← composer
│              │ [Message jinn…      ] [+]     [Q] [Cancel]   │
└──────────────┴──────────────────────────────────────────────┘
  sidebar                   main
```

| Element | What it is | See |
| --- | --- | --- |
| `Sessions` / `Library` | The sidebar's two-segment control. Chooses which list it shows: the loaded sessions that are not library members, or the library members. Exactly one shows at a time. | `webui2.md` |
| `⋮` on a session row | The row's menu. One item — **add to library** or **remove from library** — which moves the session to the other list. | `webui2.md` |
| `Archive` | Opens a picker over every stored session, loaded or archived, with a title filter. | `webui2.md` |
| A highlighted session row | The active session — the same one the TUI shows. | `webui2.md` |
| **Loading session…** card | A full-page overlay with a spinner, raised the instant a session row or an Archive row is picked and cleared when that session arrives. Covers everything; `Esc` does not close it. | `webui2.md` |
| `●` after a session title | That session's turn is running. | `webui2.md` |
| The session title | Click to rename in place: `Enter` commits, `Esc` cancels. | `webui2.md` |
| `[provider/model]` | The model chip. Opens the model picker; its **Refresh models** button asks the registry to rediscover and stays busy until the fresh rows land. | `webui2.md` |
| `[chip: … ✕]` | A pending image, one per chip, above the composer. Arrives from a paste, the `+`, or an `https://` image URL already sent. `✕` removes it. | `webui2.md` |
| `[img]` on a user message | An image the message carried. | `webui2.md` |
| `[Message jinn…]` | The composer's draft. `Enter` sends; `Shift+Enter` is a newline. An image with no text sends. | `webui2.md` |
| `[+]` | Attach an image. Opens the file picker. | `webui2.md` |
| `[Q]` / `[Cancel]` (while busy) | Queue the message until the turn settles / stop the turn. | `webui2.md` |
| `[Send]` (while idle) | Send the message. | `webui2.md` |
| `☰` | The menu button. Opens a full-height slide-in panel over a scrim. **New session** is its first item and the **Light | Dark | System** color mode group its last, at every time; **Sessions** sits just above the group, and between the first item and **Sessions**, while a session is active, **Skills**, **Persona**, **Tools**, **MCP servers**, in that order — each opens its picker modal. A scrim click, `Esc`, or picking an item closes it. | `webui2.md` |
| **New session** (menu) | A modal with a Project select, a Lifecycle select, **Setup arguments** when the lifecycle declares any, and **Create session**. The new session becomes active and the page moves to it. | `webui2.md` |
| **Light | Dark | System** (menu) | The color mode. Saved to `state.toml`; **System** follows the browser. | `webui2.md` |
| `#` (phone only) | Below 768px, at the title bar's left. Opens the sidebar as an off-canvas drawer; a scrim tap, `Esc`, or picking a row closes it. | `webui2.md` |
| The skills modal | A filter bar and the session's discovered skills — a `●` / `○` mark, name, description, a `project` badge on project skills, and a `+`. A row click toggles at once; `+` loads the body into context. | `webui2.md` |
| The tools modal | A filter bar and one row per tool the session's registry offers — a `✓` (enabled) / `✗` (disabled) mark, the name, the description's first sentence. A row click commits at once and the modal stays open; dismissing does not revert. | `webui2.md` |
| The MCP servers modal | A filter bar and one row per `[mcp.<name>]` in `jinn.toml` — a `●` (enabled) / `○` (disabled) mark, the name, the launch summary, and a status badge: `off`, `starting`, `running`, `dead`. A row click toggles at once; the badge updates live. | `webui2.md` |
| The persona modal | A filter bar and one row per persona — a `●` on the current persona / `○` on the rest, the name, the description; the current row is highlighted. A row click switches the persona and closes the modal. | `webui2.md` |
| `disconnected — reconnecting…` | A full-page overlay while the connection is down. | `webui2.md` |

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
| The web UI's address | Dashboard tab, **webui** row |
| The second web UI's (`/web2`) address | Dashboard tab, **webui2** row |
| The browser page's `☰` / `#` / `[model]` buttons | Web UI titlebar |
| A blue or yellow session row, a spinning row, `⋮` | Web UI sidebar |
| The browser page's Send / `Q` / Cancel buttons | Web UI composer |
| The `/web2` page's Archive button, title, model chip | Web UI 2 (`/web2`) title bar and sidebar |
| The `/web2` page's Send / `Q` / Cancel buttons | Web UI 2 (`/web2`) composer |
| The `/web2` page's `☰` button and its menu | Web UI 2 (`/web2`) title bar and menu |
| The `/web2` page's skills list — filter, `●` / `○` marks, `+` | Web UI 2 (`/web2`) skills modal |
| The `/web2` page's tools list — `✓` / `✗` marks | Web UI 2 (`/web2`) tools modal |
| The `/web2` page's MCP servers list — `●` / `○` marks, status badge | Web UI 2 (`/web2`) MCP servers modal |
| The `/web2` page's persona list — `●` / `○` marks | Web UI 2 (`/web2`) persona modal |
