# Web UI 2 (`/web2`)

A running jinn instance serves a **second** browser UI at `/web2`, beside
the original at `/web`. It is a mirror of the TUI with web UX: the same
sessions, the same transcript, the same single active session, and every
action publishes the same message the TUI's keybind does.

The original web UI is unchanged and still served. `/web2` is the newer
surface; it covers the core loop — sessions and the library, images,
themes, model refresh, and creating a session — and leaves out what
[What `/web2` does not do](#what-web2-does-not-do) lists.

Like `/web`, it is a **single-user, unauthenticated** surface — reach it
over loopback or a VPN you secure.

## Open it

Point a browser at `http://127.0.0.1:8388/web2` (or the `[webui2]` bind
you configured — see the **Web UI 2** entry under
[Common asks → settings](configuration.md#common-asks--settings)).

The TUI's Dashboard tab names the address the server is actually running
on, on its **webui2** row — the line to copy when the bind is not the
default or uses port `0`. A wildcard bind (`0.0.0.0` / `::`) has no
single address, so that row reads `serving on port <port> on all
interfaces (http://<your-ip>:<port>/web2)` and you substitute your
machine's IP.

One browser tab is the supported model. Two tabs on `/web2` both work,
but nothing coordinates them.

## What it does

- **Sessions | Library** — the sidebar's two-segment control chooses which
  list it shows: the loaded sessions that are not library members, or the
  library members. Exactly one shows at a time. In either list the active
  session is highlighted and clicking a row activates that session (the
  TUI follows). Each row's `⋮` offers **add to library** or **remove from
  library**, which moves it to the other list.
- **Session loading** — clicking a session row, or picking one from the
  **Archive** picker, raises a full-page **Loading session…** overlay at
  once: a card with a spinner, over everything. It clears when that
  session's transcript arrives — in a few milliseconds for a session
  already in memory, longer for one read from disk. It covers the whole
  page and nothing dismisses it (`Esc` does nothing): a second click
  cannot fire a second switch under it. If no answer comes within ten
  seconds the overlay clears and the composer names the timeout; the page
  stays on the session it never left.
- **Archive** — the sidebar's **Archive** button opens a picker over every
  stored session, loaded or archived, as a tree: a forked session is
  indented under its parent, and a filter that matches a child keeps its
  parent listed above it. Each row shows its title over a smaller, muted
  `date · project` line (the browser's local time); an archived row is
  dimmed. Choosing a row activates it; an archived session is loaded from
  disk and leaves the archive.
- **New session** — the menu's **New session** item opens a modal with a
  Project select, a Lifecycle select (blank by default), and **Setup
  arguments** when the chosen lifecycle declares any. **Create session**
  makes the new session active and the page moves to it. With no
  `[[project.entry]]` configured the modal says so instead of showing a
  form.
- **Follows the active session** — when the active session changes
  anywhere (another `/web2` page, `/web`, the TUI), every `/web2` page
  switches to it. Nothing the user has typed holds the page back: an
  unsent draft and any pending images belong to the user, not to the
  session, and are still there after the switch.
- **Transcript** — the full history, streamed live: markdown, thinking,
  tool calls and results, errors, and system messages all render as they
  arrive. Two floating buttons sit at the transcript's right edge, each
  shown only while you are away from that end: an up-arrow-to-line button
  in the top-right corner jumps to the start, a down-arrow-to-line button
  in the bottom-right corner jumps to the end. At the end, the transcript
  follows a live stream on its own; scroll up and it holds still until you
  jump back.
- **Composer** — one row: the draft, a `+` to attach an image, and the
  buttons, all on the same line. Sends a message, queues one while a turn
  is running, or cancels the running turn. An image can arrive three
  ways — pasted into the draft, picked through the `+`, or named by an
  `https://` image URL in the text — and each one shows as a chip above
  the draft, with an `✕` to remove it, until the message is sent. A sent
  image renders on its message.
- **Model** — the titlebar's model chip opens a picker of the available
  models. Choosing one switches the session's model. **Refresh models**
  asks the registry to rediscover them; it is disabled and busy until the
  fresh rows arrive.
- **Title** — click the title to rename the session in place; `Enter`
  commits and `Esc` cancels.
- **Menu** — the `☰` at the title bar's left opens a full-height panel
  over a dimmed page. **New session** is its first item and the
  **Light | Dark | System** color mode group its last, at every time.
  Just above the group, at every time, is **Sessions**, which opens
  the session list — on a phone the drawer, at wider widths nothing,
  since the sidebar is already in place.
  Between them, while a session is active, it lists **Skills**,
  **Persona**, **Tools**, and **MCP servers**, in that order; each opens
  its picker. With no active session those four are omitted. **Applets**
  sits between those four and **Sessions**, at every time — the saved-app
  catalog depends on no session. A scrim
  click, `Esc`, or picking an item closes it.
- **Applets** — the menu's **Applets** item (at every time) opens the
  saved applets, newest first, each a new-tab link to
  `http://<bind>/applets/<id>/` while the applets server is serving and a
  disabled line while it is offline. A status line above the list names
  the serving bind (`serving http://<bind>`) or reads `<server offline>`.
  A filter bar narrows the list by title as you type, with the matches
  highlighted. An empty catalog says **No applets saved yet**. The modal
  refreshes itself when an applet is saved or the server's serving phase
  changes.
- **Color mode** — the menu's **Light | Dark | System** group. The choice
  is saved (`state.toml`) and survives a reload and a restart; **System**
  follows the browser. It is applied before the transcript's first paint.
- **Phone layout** — below 768px the sidebar becomes an off-canvas drawer
  opened by the small `#` button left of the `☰` in the title bar, or by
  the menu's **Sessions** item. A scrim tap, `Esc`, or
  picking a row closes it. At wider widths the `#` is not shown and the
  sidebar is a fixed column.
- **Skills** — the menu's **Skills** item (only while a session is
  active) opens the session's discovered skills: a `●` / `○` enabled
  mark, the name and description, a `project` badge on a project-scoped
  skill, and a `+` on each row. A filter bar narrows the list as you
  type, with the matches highlighted. The modal refreshes itself when a
  skill scan lands.
  - Clicking a row toggles that skill **at once** for the session —
    there is no staging step. A loaded skill's row is not a toggle.
  - `+` loads the skill's body into context as a pinned tool result,
    enabling the skill first if it was off. A skill already in context
    has its `+` disabled, so nothing is loaded twice.
- **Persona** — the menu's **Persona** item opens the personas, in their
  own name-sorted order: a `●` on the session's current persona and a
  `○` on the rest, the name and description. The current persona's row
  is highlighted.
  - Clicking a row switches the session's persona and **closes the
    modal** — unlike Tools and MCP servers, which stay open.
  - If the chosen persona carries no `<compaction>` handoff block, an
    error entry is pushed into the transcript — the same warning the
    TUI shows. It appears without a reload.
- **Tools** — the menu's **Tools** item opens one row per tool the
  session's registry offers, sorted case-insensitively by name: a `✓`
  (enabled) or `✗` (disabled) mark, the tool name, and the
  description's first sentence. Disabled rows are muted.
  - Clicking a row **writes the change at once** — the session's tool
    filter becomes a deny list over the disabled rows. The mark flips
    immediately and the modal **stays open**.
  - Dismissing the modal (`Esc` or a backdrop click) only closes it.
    There is **no revert**: a mis-click is undone by clicking the row
    again.
- **MCP servers** — the menu's **MCP servers** item opens one row per
  `[mcp.<name>]` entry in `jinn.toml`, sorted case-insensitively by
  name: a `●` (enabled) or `○` (disabled) mark, the server name, its
  launch summary (command + args), and a status badge reading `off`,
  `starting`, `running`, or `dead`. A disabled server reads `off` no
  matter what its connection last reported.
  - Clicking a row enables or disables that server for the session **at
    once** — the same toggle as the TUI's MCP picker — and the modal
    **stays open**.
  - The badge updates live on its own: an enabled server's badge moves
    to `running` with no reload and no re-open.
  - This is the simple toggle list — no stderr/log pane and no tool
    list. Those are in the TUI's inspector; `/web2` does not have them.

Every picker modal here covers the full viewport with a backdrop and a
card: a filter input on top, a scrollable list below. `Esc` or a
backdrop click closes it. The filter is the terminal's own — run on the
server, with whitespace-separated terms each matched (so `gpt 4` finds
`gpt-4`), the closest match ranked first, and the matched runs
highlighted as you type (debounced).

## Keys and buttons

There are no keybinds beyond these:

| Key | Does |
| --- | --- |
| `Enter` in the composer | Sends the message. |
| `Shift+Enter` in the composer | Inserts a newline. |
| `Enter` in a picker's filter | Does nothing; a row is chosen by click. |
| `Esc` | Closes the top overlay: the menu, any of the five picker modals (Skills, Persona, Tools, MCP servers, Applets), the New session modal, another picker, the title editor, or the phone drawer. It does **not** close the **Loading session…** overlay — that ends only when the load ends. |

The composer's buttons follow the session's phase:

- **Idle** — **Send** only.
- **Working** — **Q** and **Cancel**; there is no Send. `Q` queues the
  draft behind the running turn, and any number of messages can wait
  there. A count beside the `Q` shows how many are queued, since a
  queued message does not appear in the transcript until its turn runs.

`Enter` follows the same rule: idle it sends, working it queues. An
empty draft with no image does nothing either way; an image with no text
sends. Images are limited to 5 MB each, and a refused one is named in a
message under the composer while the draft and every chip stay put.

**Cancel** stops the running turn. Queued work is dumped into the TUI's
input box as an editable draft — not into the web composer.

## Reconnecting

If the connection drops, a `disconnected — reconnecting…` overlay with a
spinner covers the page and it retries on its own; the overlay clears when
it is back. An unsent draft survives the reconnect. A restarted jinn is a
normal event — the page comes back with the whole transcript.

## What `/web2` does not do

Everything below exists on the TUI, and most of it on `/web` too. When a
user asks for one of these on `/web2`, say so and point at the surface
that has it.

- Forking, archiving, or tearing down sessions. `/web2` can *create* and
  *open* sessions, but has no fork, archive, or teardown gesture.
- Renaming, deleting, or opening an applet in kiosk mode. The `/web2`
  applets picker opens one in a new tab; those three gestures are the
  TUI's.
- Pinning, exclusion, isolate, and the per-entry audit popup.
- Steer mode. A message sent while working is queued; to steer, cancel
  and send.
- The `#prompt`, `@path`, and `/command` input tokens. Text is sent
  verbatim; `@path` is the TUI's way to attach a file, and `/web2` has the
  `+` instead.
- Model alloys, reasoning-effort and endpoint pickers.
- Unread marks on session rows, and an unread badge on the phone
  drawer's `#` button.

## How it works

The page holds all view state and talks to the server over a single
WebSocket using two message sets — one for commands the page sends, one
for facts the server sends back. The server renders no HTML fragments and
reads nothing back from the page, which is what keeps the two in step.

On every (re)connect the page sends the newest entry id it holds. The
server continues from there when it safely can and otherwise resends the
whole transcript, so a restart can never leave a gap.
