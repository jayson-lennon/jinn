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
web UI (`/web2`) has a few keybinds, all in its composer and overlays
(see [Web UI layout](#web-ui-layout-web2)).

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

## Web UI layout (`/web2`)

The browser page the instance serves beside the TUI (what it is and how
to open it: `webui2.md`). Below 768px the
sidebar is an off-canvas drawer behind a `#` button left of the `☰`
(the menu's **Sessions** item opens it too); at wider widths it is the
fixed column drawn here, and the `#` is not shown.

```text
┌──────────────┬──────────────────────────────────────────────┐
│Sessions│Libra│ ☰ Session title        [provider/model]      │ ← title bar
│ ▎Session A   ├──────────────────────────────────────────────┤
│  Session B ● │ coding-assistant                   12.3k/??? │ ← status bar
│        ⋮     ├──────────────────────────────────────────────┤
│              │  Transcript                                  │
│        ⋮     │  (scrolling conversation)              [⇱]   │ ← jump to start (top-right, when off the top)
│              │  a user message with an attached image:      │
│  Archive     │    you  look at this                         │
│              │    [img]                                     │
│              │                                        [⇲]   │ ← jump to end (bottom-right, when off the end)
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
| `⋮` on a session row | The row's menu. **add to library** / **remove from library** moves the session to the other list; **archive** archives that session. | `webui2.md` |
| `Archive` | Opens a picker over every stored session, loaded or archived, with a title filter. | `webui2.md` |
| A highlighted session row | The active session — the same one the TUI shows. | `webui2.md` |
| **Loading session…** card | A full-page overlay with a spinner, raised the instant a session row or an Archive row is picked and cleared when that session arrives. Covers everything; `Esc` does not close it. | `webui2.md` |
| A gold session row | That session finished a turn while the page viewed another one. Viewing it clears the mark. | `webui2.md` |
| A gold Sessions / Library tab | Its list holds an unread session, so the mark is never behind the other list. | `webui2.md` |
| `●` after a session title | That session's turn is running. | `webui2.md` |
| The session title | Click to rename in place: `Enter` commits, `Esc` cancels. | `webui2.md` |
| `[provider/model]` | The model chip. Opens the model picker; its **Refresh models** button asks the registry to rediscover and stays busy until the fresh rows land. | `webui2.md` |
| The persona name / `12.3k/???` | The status bar, under the title bar. Left: the session's persona — click it to open the persona picker. Right: the context usage figure, the same one the TUI status bar shows (`used%/max` or `used/???` when the model's window is unknown). | `webui2.md` |
| `[chip: … ✕]` | A pending image, one per chip, above the composer. Arrives from a paste, the `+`, or an `https://` image URL already sent. `✕` removes it. | `webui2.md` |
| `[img]` on a user message | An image the message carried. | `webui2.md` |
| `[⇱]` top-right of the transcript | **Jump to start** — an up arrow against a bar above it. Shown only while scrolled away from the top; click to land on the first entry. | `webui2.md` |
| `[⇲]` bottom-right of the transcript | **Jump to end** — a down arrow against a bar below it. Shown only while scrolled away from the end; click to land on the newest entry and follow the stream again. | `webui2.md` |
| `⋮` at an entry's bottom-right | Every transcript entry's menu, always visible at every width. **Pin** pins the entry at its relative position and it shows a 📌; pinned, the item reads **Unpin**. **Fork** forks the session at that entry, as the TUI's `f` does, and the page moves onto the new session. `Esc`, an outside click, or picking an item closes it. | `webui2.md` |
| 📌 at an entry's left edge | A pinned entry — always in context, and pinned at its relative position. | `webui2.md` |
| `[Message jinn…]` | The composer's draft. `Enter` sends; `Shift+Enter` is a newline. An image with no text sends. | `webui2.md` |
| `[+]` | Attach an image. Opens the file picker. | `webui2.md` |
| `[Q]` / `[Cancel]` (while busy) | Queue the message until the turn settles / stop the turn. | `webui2.md` |
| `[Send]` (while idle) | Send the message. | `webui2.md` |
| `☰` | The menu button. Opens a full-height slide-in panel over a scrim. **New session** is its first item and the **Light | Dark | System** color mode group its last, at every time; **Sessions** sits just above the group; **Applets** sits just above **Sessions**, at every time; and between the first item and **Applets**, while a session is active, **Archive session**, **Skills**, **Persona**, **Tools**, **MCP servers**, in that order — **Archive session** archives the session in view, and each picker opens its modal. A scrim click, `Esc`, or picking an item closes it. | `webui2.md` |
| **Archive session** (menu) | Archives the session in view. Its row leaves the rail and the page moves to another session; a session running a turn is refused with a toast. | `webui2.md` |
| **Applets** (menu) | A modal over the saved applets, newest first, each a new-tab link while the applets server is serving and a disabled line while it is offline; a status line names the serving bind, and a filter narrows by title. | `webui2.md` |
| `serving http://<bind>` / `<server offline>` | The applets modal's status line: where the applets server is reachable, or that it is down (rows disabled). | `webui2.md` |
| **New session** (menu) | A modal with a Project select, a Lifecycle select, **Setup arguments** when the lifecycle declares any, and **Create session**. The new session becomes active and the page moves to it. | `webui2.md` |
| **Light | Dark | System** (menu) | The color mode. Saved to `state.toml`; **System** follows the browser. | `webui2.md` |
| `#` (phone only) | Below 768px, at the title bar's left. Opens the sidebar as an off-canvas drawer; a scrim tap, `Esc`, or picking a row closes it. Gold with a count while sessions are unread. | `webui2.md` |
| The skills modal | A filter bar and the session's discovered skills — a `●` / `○` mark, name, description, a `project` badge on project skills, and a `+`. A row click toggles at once; `+` loads the body into context. | `webui2.md` |
| The tools modal | A filter bar and one row per tool the session's registry offers — a `✓` (enabled) / `✗` (disabled) mark, the name, the description's first sentence. A row click commits at once and the modal stays open; dismissing does not revert. | `webui2.md` |
| The MCP servers modal | A filter bar and one row per `[mcp.<name>]` in `jinn.toml` — a `●` (enabled) / `○` (disabled) mark, the name, the launch summary, and a status badge: `off`, `starting`, `running`, `dead`. A row click toggles at once; the badge updates live. | `webui2.md` |
| The persona modal | A filter bar and one row per persona — a `●` on the current persona / `○` on the rest, the name, the description; the current row is highlighted. A row click switches the persona and closes the modal. | `webui2.md` |
| `disconnected — reconnecting…` | A full-page overlay while the connection is down. | `webui2.md` |
| A toast (bottom right) | Every refusal and failure the server reports — a busy archive, a refused image, an unknown skill. Dismisses itself after a few seconds; a tap dismisses it at once. | `webui2.md` |

## Feature → screen location (reverse lookup)

| If asked about… | It lives in |
| --- | --- |
| Model name / token count / turn count | Status bar, line 2 |
| Session cwd | Status bar, line 1 |
| "Working…" / queue count / session kind | Streaming indicator row |
| Yank / hint messages ("yanked …") | Status bar, line 2 right side |
| Token-density strip / the `>` arrow | Minimap column |
| The colored vertical line | Border column (focus accent) |
| Pinned entries / isolate / audit | Chat log (selection) + Pins sidebar section + audit popup; on `/web2`, a 📌 at the entry's left edge and the entry's `⋮` → Pin |
| The `/web2` page's per-entry `⋮` and its Pin / Unpin / Fork | Web UI (`/web2`) transcript, at each entry's bottom-right |
| A web-search reply's sources (`Sources (N)`) | Web UI (`/web2`) transcript, the collapsed line under the reply |
| A rule interrupt / watchdog guidance (`⚡ Rule fired: <rule>`) | Web UI (`/web2`) transcript, the yellow-tinted collapsed block where the turn changed course; in the TUI the same entry is the yellow `⚡ Rule fired` block |
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
| The web UI's (`/web2`) address | Dashboard tab, **webui2** row |
| The `/web2` page's `☰` / `#` / `[provider/model]` buttons | Web UI (`/web2`) title bar |
| The `/web2` page's title (click to rename) | Web UI (`/web2`) title bar |
| The `/web2` page's persona name and context usage figure | Web UI (`/web2`) status bar, under the title bar |
| The `/web2` page's `☰` button and its menu | Web UI (`/web2`) title bar and menu |
| A blue or gold session row, a `●` running row, a row's `⋮` | Web UI (`/web2`) sidebar |
| The `/web2` page's Archive button, model chip | Web UI (`/web2`) title bar and sidebar |
| The `/web2` page's unread session mark — gold row, gold tab, phone `#` count | Web UI (`/web2`) sidebar and title bar |
| The `/web2` page's Send / `Q` / Cancel buttons | Web UI (`/web2`) composer |
| The `/web2` page's per-entry `⋮` and its Pin / Unpin | Web UI (`/web2`) transcript, at each entry's bottom-right |
| A web-search reply's sources (`Sources (N)`) | Web UI (`/web2`) transcript, the collapsed line under the reply |
| The `/web2` page's skills list — filter, `●` / `○` marks, `+` | Web UI (`/web2`) skills modal |
| The `/web2` page's tools list — `✓` / `✗` marks | Web UI (`/web2`) tools modal |
| The `/web2` page's MCP servers list — `●` / `○` marks, status badge | Web UI (`/web2`) MCP servers modal |
| The `/web2` page's persona list — `●` / `○` marks | Web UI (`/web2`) persona modal |
