# The web UI

A running jinn instance serves a browser UI at the root of its own port.
It is a mirror of the TUI with web UX: the same sessions, the same
transcript, the same single active session, and every action publishes
the same message the TUI's keybind does.

It covers the core loop — sessions and the library, the transcript and
its entry menu, the status bar and its session dash, images, the pickers,
themes, model refresh, and creating a session — and leaves out what
[What the web UI does not do](#what-the-web-ui-does-not-do) lists.

It is a **single-user, unauthenticated** surface — reach it over loopback
or a VPN you secure.

## Open it

Point a browser at the port's root, `http://127.0.0.1:8388/` by default
(or the `[webui]` bind
you configured — see the **Web UI** entry under
[Common asks → settings](configuration.md#common-asks--settings)).

The TUI's Dashboard tab names the address the server is actually running
on, on its **webui** row — the line to copy when the bind is not the
default or uses port `0`. A wildcard bind (`0.0.0.0` / `::`) has no
single address, so that row reads `serving on port <port> on all
interfaces (http://<your-ip>:<port>)` and you substitute your
machine's IP.

The same row is the slice's health line: after the address it carries
`· N page(s) · last hello X ms` — how many browser pages are connected
and how long the last connect took to be answered. The row turns to a
warning tone and names the stall when a connect's reply runs past five
seconds or a write to a page stalls, so a stuck web UI is visible from
the TUI without opening a log.

One browser tab is the supported model. Two tabs both work,
but nothing coordinates them.

## Install it

The page is an installable web app. A browser's **Add to Home Screen**
(Android Chrome) or **Add to Home Screen** (iOS Safari, under **Share**) or
**Install app** (desktop Chrome/Edge, in the address-bar menu) installs it
as a standalone app with its own icon and no URL bar. The installed app
opens at the instance's root, exactly as the browser tab does — same
sessions, same single active session, same messages over the socket.

The bar above the page follows the chosen colour mode: the dark palette's
background in dark mode, the light one's in light, and the OS's in system
mode. An installed app has no reload button, so it picks up a jinn update
by reloading itself — see [Reconnecting](#reconnecting).

## What it does

- **Sessions | Library** — the sidebar's two-segment control chooses which
  list it shows: the loaded sessions that are not library members, or the
  library members. Exactly one shows at a time. In either list the active
  session is highlighted and clicking a row activates that session (the
  TUI follows). Each row's `⋮` offers **add to library** or **remove from
  library**, which moves it to the other list; **teardown + archive**, which
  runs that session's teardown lifecycle and then archives it (the terminal's
  `x` — a session with nothing to tear down is archived at once); and
  **archive**, which archives without running the teardown lifecycle. While
  the teardown runs, the row shows its busy mark — a dot with a halo
  that expands out of it and fades.
  While the page is still waiting for its first list, and again while the server is
  still loading its saved sessions at startup, whichever list is showing
  ends with a muted **loading sessions…** row — a spinner, below the rows
  already loaded. It has no timeout: it clears when the server says the
  list is complete.

  The **Sessions** list is a tree: a session forked from, spawned under, or
  bootstrapped as a bot under another is indented one level under its
  parent, as the terminal's sidebar draws it. A **subagent**'s or a **bot**'s
  row also carries a second line under its title — `subagent` or `bot`, in
  that kind's colour — and the viewed session's kind is named the same way
  above the composer (`[subagent]` / `[bot]`, at the right edge, idle or
  busy). The **Library** list stays flat.
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
  dimmed. The list is capped at 50 rows, taken as whole trees in
  rank order, so a filter never leaves a parent whose children were cut
  off. Choosing a row activates it; an archived session is loaded from
  disk and leaves the archive. It is the only way to reach an archived
  session from the web — the sidebar lists loaded sessions only.
- **New session** — the menu's **New session** item opens a modal with a
  Project select, a Lifecycle select (blank by default), and **Setup
  arguments** when the chosen lifecycle declares any. **Create session**
  makes the new session active and the page moves to it. With no
  `[[project.entry]]` configured the modal says so instead of showing a
  form.
- **Follows the active session** — when the active session changes
  anywhere (another web UI page, the TUI), every web UI page
  switches to it. Nothing the user has typed holds the page back: an
  unsent draft and any pending images belong to the user, not to the
  session, and are still there after the switch.
- **Unread marks** — a session that finishes a turn while the page is
  viewing another one is marked: its row turns gold, and so does the
  **Sessions** or **Library** tab whose list holds it, so a mark is never
  hidden behind the other list. On a phone the `#` button turns gold and
  shows how many sessions are unread across both lists. Viewing the
  session clears its mark — by clicking its row or by the page following
  a switch made elsewhere. The mark is this browser's own record and
  survives a reload; a restart of jinn forgets every mark. A subagent's
  session is never marked.
- **Transcript** — a page of history, streamed live: markdown, thinking,
  tool calls and results, errors, and system messages all render as they
  arrive. The page opens on the **newest 200 entries or 1 MB** of the
  session, whichever comes first, so a long session connects at once
  instead of on its whole history. Scrolling to the top loads the page
  before it — a spinner sits at the top of the transcript while it
  loads, and the entry you were reading stays in place as the older ones
  land above it. Markdown covers footnotes, task lists, and tables; a bare
  `http(s)` address or an email in the prose becomes a link that opens in
  a new tab, while `www.`, `ftp:`, and protocol-relative addresses stay
  text. Your own messages render as markdown too. A web-search reply's
  sources are a collapsed `Sources (N)` line — open it for each source's
  linked title and address. Guidance the harness injects mid-turn — a rule
  interrupt, such as a watchdog stopping a repeated tool call — is a
  yellow-tinted block collapsed to `⚡ Rule fired: <rule> · <reason>`;
  open it to read the guidance, shown without the `<system-interrupt>`
  wrapper the model sees. A model's thinking is a single collapsed
  `Thinking` line — click it to read the block; the text still streams in
  while it is collapsed, so opening it mid-turn shows what has arrived so
  far. Two floating buttons sit at the transcript's
  right edge, each shown only while you are away from that end: an
  up-arrow-to-line button in the top-right corner jumps to the start, a
  down-arrow-to-line button in the bottom-right corner jumps to the end.
  At the end, the transcript follows a live stream on its own; scroll up
  and it holds still until you jump back.
  - A tool result that was truncated carries a **full output** disclosure
    under its text. Its full text is fetched from the server when the
    disclosure is opened — a spinner while it loads — and kept for the
    next open, so the untruncated output never rides on the connect path.
- **Composer** — one row: the draft, a `+` to attach an image, and the
  buttons, all on the same line. Sends a message, queues one while a turn
  is running, or cancels the running turn. An image can arrive three
  ways — pasted into the draft, picked through the `+`, or named by an
  `https://` image URL in the text — and each one shows as a chip above
  the draft, with an `✕` to remove it, until the message is sent. A sent
  image renders on its message.
- **Model** — the title bar's model chip opens a two-row chooser:
  **OpenRouter Model Market** (below) or **Provider Model Picker**. The
  provider picker lists the available models, built by the TUI picker's own row builder: available models
  first alphabetically with the current one promoted to the top. Each row
  is the model name over its provider name, with `· current` after the
  one in use and `· unavailable` after one whose provider has no API
  key — that row is disabled and cannot be clicked. (The TUI's `✗`,
  `→`, and `*` marks are not drawn here.) The filter is the server-side
  fuzzy match. Choosing a row switches the session's model and closes
  the picker. **Refresh models** asks the registry to rediscover them;
  it is disabled and busy until the fresh rows arrive, and the list then
  updates in place without losing the typed filter. Alloys and
  multi-select are the TUI's.
- **OpenRouter Model Market** — the same catalog the TUI's `<leader>so`
  shows (see [models and providers](models-and-providers.md#the-openrouter-model-market)),
  as a table built for a phone. In portrait each model is two lines: the
  name with **PROJ** at the right, then a strip of IN/M, OUT/M, CACH/M,
  ECI, VALS in fixed columns, so every metric sits under the same header
  in every row; on a wide screen the same row is one line with every
  column beside the name. The header is the sort control: tap a column to
  sort by it (prices ascending, scores descending), tap it again to
  reverse; PROJ ascending is the default. **mix** toggles the pricing mix
  between the fixed `10M·98%·50k` and the session tree's measured usage;
  **⟳** refetches everything. The status line carries the catalog's age,
  the active mix, and the row count, and a **stale** note when a fetch
  fails (the rows stand). The filter is the server-side fuzzy match. Tap a
  row for its **detail page**: a **←** back button, a ⧉ link opening the
  model's openrouter.ai page in a new tab, a facts line (context window,
  free, pricing overrides), and its endpoints — the **Default
  (auto-route)** row first, then each provider's, with its own prices,
  latency, throughput and uptime. Tap an endpoint row to highlight it, then
  **Use this model** switches the session to the model and pins the
  highlighted endpoint (the Default row clears the pin). The session's
  current model is marked **· current** in the list, and its **Use this
  model** is disabled. The sort, direction and mix are remembered across
  opens in the browser; the filter is cleared on every open. `Esc` steps
  back from the detail page, then closes the market.
- **Title** — click the title to rename the session in place; `Enter`
  commits and `Esc` cancels.

## Status bar

The row under the title bar, shown while a session is active. It has
two buttons, and each opens something.

- **The persona name (left)** — the session's current persona. Click it
  to open the persona picker, the same modal the menu's **Persona** item
  opens: the shortest way to switch persona is one click on the name and
  one click on a row.
- **The context usage figure (right)** — `used%/max` when the model's
  context window is known, `used/???` when it is not. It is the
  terminal's own figure, so the two surfaces always agree. It follows
  every history change, pin, context toggle and model switch without a
  reload. Click it to open the [session dash](#session-dash).

With no session active the whole bar is not rendered.

## Session dash

A card that drops down from the status bar's right edge, under the
context figure, with a scrim behind it. Not the TUI's dashboard: it is
the session's own facts, each computed by the function the terminal
uses, so every figure matches the TUI's. From top to bottom:

- **Session** — the model, context figure, tokens sent and received,
  cost, turns, working time, cache hit rate (when the provider reports
  one), and the directory, one per line. While the session tree holds
  more than one session, a **Tree** disclosure carries the whole tree's
  totals. The figures update live while the dash is open.
- **MCP servers** — the session's enabled servers with their live
  status, or **No MCP servers enabled**. Clicking anywhere in the
  section closes the dash and opens the MCP picker.
- **Pinned** — the session's pinned entries, labeled as the TUI sidebar
  labels them, or **No pinned entries**. Clicking one closes the dash
  and scrolls the transcript to that entry, loading older history until
  it is held. A jump that takes more than a moment covers the page with
  a **Navigating…** overlay that blocks the UI until the entry is
  reached; its **Cancel** button closes the overlay and stops the load,
  leaving the transcript where it was. A jump that lands at once never
  shows the overlay.
- **Tasks** — every phase with its tasks beneath it, the active phase
  accented and completed tasks struck through, or **No tasks yet**.

`Esc` or a click outside the card closes it; switching sessions closes
it too.

## Entry menu

Every transcript entry has a `⋮` at its bottom-right, always visible at
every width — there is no hover-reveal, so it works on a phone. It opens
the popup with the things you can do to that one entry. `Esc`, a click
outside, or picking an item closes it; the last entry's popup opens
upward so the composer never cuts it off.

- **Pin / Unpin** — pins the entry at its relative position, the same
  pin the TUI's `p` sets, and the entry shows a 📌 on its left edge.
  Pinned, the item reads **Unpin**. A pin takes the whole tool loop:
    pinning a tool call or result pins its loop, and each member shows the
    mark. Pins made in the TUI appear on the web UI at once. Top and bottom
    positioning stay in the TUI's Pins sidebar.
  - **Exclude from context / Include in context** — flips the entry between
    forced-exclude and forced-include, the same override the TUI's `x` sets,
    and takes the whole tool loop like a pin does. The item names the action
    it will take: an entry in context reads **Exclude from context**, one
    out of it reads **Include in context**. An excluded entry is drawn
    dimmed. Excludes made in the TUI appear on the web UI at once. The item is
    absent on a pinned entry, since a pin overrides exclusion, and the web
    cannot return an entry to the default — the TUI's `r` does.
  - **Fork** — forks the session at that entry into a new session holding
    the history up to and including it, the same fork the TUI's `f` makes;
    the new session is linked as the source's child and every page moves
    onto it. A "Loading session…" overlay covers the page while it is
    created. Offered while a turn is running too, like the TUI's `f`.

The `⋮` on a **session row** in the sidebar is a different menu: see
**Sessions | Library** under [What it does](#what-it-does).

## Menu

The `☰` at the title bar's left opens a full-height panel over a dimmed
page. **New session** is its first item and the **Light | Dark | System**
color mode group its last, at every time. Just above the group, at every
time, is **Sessions**, which opens the session list — on a phone the
drawer, at wider widths nothing, since the sidebar is already in place.
Between them, while a session is active, it lists **Archive session**,
**Skills**, **Persona**, **Tools**, and **MCP servers**, in that order.
**Archive session** archives the session in view; a session running a
turn is refused with a toast. Each of the four pickers opens its picker.
With no active session those five are omitted. **Applets** sits between
those five and **Notifications**, at every time — the saved-app catalog
depends on no session. **Notifications** sits just above **Sessions**, at
every time; what it does is under **Notifications** below. A scrim click,
`Esc`, or picking an item closes it.

## Notifications

A turn that ends while no page of this jinn is visible raises a banner
titled **jinn** with the body **Turn finished: <session title>**. It
carries no message content. Tapping it opens or focuses the page on that
session, and activates it — the terminal switches too.

The menu's **Notifications** item is the only place permission is asked
for; browsers require a click for the prompt, so nothing is asked on load.
Its label names the state:

- **Enable notifications** — the page can ask; clicking it prompts.
- **Notifications: on** — granted, and this browser is subscribed.
- **Notifications: blocked by the browser** — refused; the browser's own
  site settings are the only way back.
- **Notifications — unavailable on http** — the page was opened over
  plain `http://` on an address other than `127.0.0.1` or `localhost`.
  Browsers expose the Notification and Push APIs only on a secure context,
  so the banner cannot exist there; the rest of the page is unaffected and
  the gold unread row still marks a finished turn. Reaching it means `https`
  — a reverse proxy, a tunnel, or a VPN that terminates TLS — or loopback.
- **Notifications — unavailable on this server** — jinn's data dir could
  not be written, so it holds no key to sign with.
- **Notifications — install the app to enable** — iOS, in a Safari tab:
  only a Home Screen web app can receive a push there (iOS 16.4+).

Nothing is registered with any push service. Each jinn generates its own
signing key pair on first use and keeps it with every browser's
subscription in `webui-push.toml` under its data dir; the contact address
a push service may use is `[webui] push_contact` in `jinn.toml`, and the
shipped default is the project's repository. Deleting that file makes
every browser resubscribe on its next load.

Where a push arrives depends on the OS, because the browser vendor's push
service is what wakes the service worker:

- **Android** — the OS wakes the browser for a push even when the app is
  swiped away or the browser is closed, as long as the browser has not
  been force-stopped and battery optimization is not restricting it.
- **iOS** — only an installed Home Screen web app (iOS 16.4+), which a
  suspended app or a Safari tab cannot do.
- **Desktop (Linux, macOS, Windows)** — only while the browser is still
  running, in the background if need be. A fully quit browser receives
  nothing until it is opened again, and the push expires if its hour
  passes first. Safari on macOS is the exception: it receives while
  closed.

In every case the gold unread row is never lost — it is derived from the
settle time when the page reconnects. Only the timely banner is at stake.

## Pickers

Every picker modal here covers the full viewport with a backdrop and a
card: a filter input on top, a scrollable list below. `Esc` or a
backdrop click closes it. The filter is the terminal's own — run on the
server, with whitespace-separated terms each matched (so `gpt 4` finds
`gpt-4`), the closest match ranked first, and the matched runs
highlighted as you type (debounced).

- **Applets** — the menu's **Applets** item (at every time) opens the
  saved applets, newest first, each a new-tab link to
  `http://<bind>/applets/<id>/` while the applets server is serving and a
  disabled line while it is offline. A status line above the list names
  the serving bind (`serving http://<bind>`) or reads `<server offline>`.
  A filter bar narrows the list by title as you type, with the matches
  highlighted. An empty catalog says **No applets saved yet**. Opening
  the modal wakes the applets server; its rows refresh by themselves
  when the server's serving phase changes or an applet is saved.
- **Color mode** — the menu's **Light | Dark | System** group. The choice
  is saved (`state.toml`) and survives a reload and a restart; **System**
  follows the browser. It is applied before the transcript's first paint.
- **Notices** — every refusal and failure the server reports is a toast at
  the bottom right: a busy archive, a refused image, an unknown skill. It
  dismisses itself after a few seconds, and a tap dismisses it at once.
  There is no message under the composer; the toast is the one surface.
- **Phone layout** — below 768px the sidebar becomes an off-canvas drawer
  opened by the small `#` button left of the `☰` in the title bar, or by
  the menu's **Sessions** item. A scrim tap, `Esc`, or
  picking a row closes it. At wider widths the `#` is not shown and the
  sidebar is a fixed column.
  - Pulling down from the top of the page does **not** refresh it — on
    purpose. Everything arrives live over the socket, so a mid-session
    reload would only discard an unsent draft. For a clean page, reload
    by hand (or in an installed app, close and reopen it).
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
- **Persona** — the menu's **Persona** item, or the persona name in the
  [status bar](#status-bar) under the title bar, opens the personas, in
  their own name-sorted order: a `●` on the session's current persona and a
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
    list. Those are in the TUI's inspector; the web UI does not have them.

This is the one place the web UI's behavior is described. The other
references name a web control and point back here; the web UI's settings
— its `[webui]` bind, its theme persistence, its image limits — stay with
the other settings in `configuration.md`, `extending-jinn.md`, and
`chat-input-tokens.md`.

## Keys and buttons

There are no keybinds beyond these:

| Key | Does |
| --- | --- |
| `Enter` in the composer | Inserts a newline. |
| `Ctrl+Enter` in the composer | Sends the message; queues it while a turn is running. |
| `Shift+Enter` in the composer | Sends the message; queues it while a turn is running. |
| `Tab` with the `#` prompt popup open | Inserts the highlighted template as a bare `#name`. |
| `↑` / `↓` with the `#` prompt popup open | Moves the highlight; the best match starts highlighted, at the bottom. |
| `Enter` in a picker's filter | Does nothing; a row is chosen by click. |
| `Esc` | Closes the top overlay, one at a time: the `#` prompt popup, the title editor, an entry's `⋮` popup, a session row's `⋮` popup, the session dash, any picker modal (Skills, Persona, Tools, MCP servers, Applets, the model chooser, the model picker, the model market, the Archive picker), the New session modal, the `☰` menu, or the phone drawer. |
| `Esc` on the **Loading session…** overlay | Does nothing. That overlay ends only when the load ends. |
| `Esc` on the **Navigating…** overlay | Does nothing. Its **Cancel** button is the one way out. |

The composer's buttons follow the session's phase:

- **Idle** — **Send** only.
- **Working** — **Q** and **Cancel**, both filled buttons; there is no Send.
  `Q` queues the
  draft behind the running turn, and any number of messages can wait
  there. A count beside the `Q` shows how many are queued, since a
  queued message does not appear in the transcript until its turn runs.

`Ctrl+Enter` and `Shift+Enter` follow the same rule: idle they send,
working they queue. `Enter` inserts a newline, so a phone's return key
never sends — the **Send** button does. An empty draft with no image does
nothing either way; an image with no text sends. Images are limited to
5 MB each, and a refused one is named in a toast while the draft and
every chip stay put.

**Prompt templates.** Typing `#` at a word boundary in the draft — the
start, or right after a space or newline — opens a popup above the
composer listing the session's templates as `name - description`, at most
20 rows, narrowing as you type. `Tab` inserts the bare `#name`, which
expands at send exactly as the TUI's does; a tap picks a row on a phone.
Moving the caret out of the token closes it. The list is the session's
own, delivered with the snapshot and refreshed when a rescan finishes,
and filtered in the browser — the server is asked nothing per keystroke.

**Cancel** stops the running turn. Queued work is dumped into the TUI's
input box as an editable draft — not into the web composer.

## Reconnecting

If the connection drops, a `disconnected — reconnecting…` overlay with a
spinner covers the page and it retries on its own; the overlay clears when
it is back. An unsent draft survives the reconnect. A restarted jinn is a
normal event — the page comes back on the newest page of the transcript,
and the rest loads on scroll.

The retry delay grows on every unanswered connect (250 ms doubling to 8 s)
and resets only once a connect has been answered, so a server that is
stalling is left alone rather than hammered. A reconnect that missed
nothing is answered at once, so a brief drop on an idle page clears in a
moment instead of returning. It keeps retrying for as long as the page is
open; there is no give-up state.

If the page reconnects to a jinn that was **updated** while it was open —
a tab left in the background across a restart, or an installed app resumed
after one — it reloads itself once to pick up the new bundle, rather than
running the old client against the new server. That reload lands on the
hashed address of the new bundle, and the draft does not survive it; an
ordinary reconnect (same jinn) keeps the draft.

## What the web UI does not do

Everything below exists on the TUI. When a user asks for one of these on
the web UI, say so and point at the surface that has it.

- Archiving a whole subtree. The web UI can *create*, *open*, *fork*, *teardown +
  archive*, and *archive* single sessions, but has no archive-tree gesture.
- Renaming, deleting, or opening an applet in kiosk mode. The web UI
  applets picker opens one in a new tab; those three gestures are the
  TUI's.
- Returning an entry to its default context state, isolate, and the
  per-entry audit popup. The web UI's entry `⋮` has a two-way
  **Exclude from context / Include in context** toggle, and **Pin**;
  the TUI's `r` reset, top/bottom pin positioning, and isolate are not
  there.
- Steer mode. A message sent while working is queued; to steer, cancel
  and send.
- The `@path` and `/command` input tokens. That text is sent verbatim;
  `@path` is the TUI's way to attach a file, and the web UI has the `+`
  instead. The `#prompt` token does work — see **Prompt templates** above.
- Model alloys, reasoning-effort and endpoint pickers.

## How it works

The page holds all view state and talks to the server over a single
WebSocket using two message sets — one for commands the page sends, one
for facts the server sends back. The server renders no HTML fragments and
reads nothing back from the page, which is what keeps the two in step.

On every (re)connect the page sends the newest entry id it holds and the
revision it saw. The server continues from there when it safely can — the
entries are still in the session, the revision still matches, and what was
missed fits one page — and otherwise resends the newest page, so a restart
or a long absence can never leave a gap or replay an unbounded history.
Image bytes and a tool result's untruncated output never travel over the
socket: the page fetches an image from its attachment route and a full
output from its own route, on demand.
