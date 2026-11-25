# Web UI

A running jinn serves a browser page beside the TUI: the same instance,
the same sessions, the same transcript. It is a second window onto the
one active session — a session you open in the browser becomes active
in the terminal too, and the other way round. Every web feature has a
home in the reference that owns it — `ui-map.md` maps each on-screen
element to it; this file holds only what the TUI has no counterpart for.

## Open it

Point a browser at `http://127.0.0.1:8387/` (the default bind; `/`
redirects to `/web`). The TUI's Dashboard tab names the resolved URL on
its **webui** row — the line to copy when the bind is not the default.
There is no login: the page is unauthenticated and single-user, so keep
it on loopback or behind a VPN or SSH tunnel you secure. Changing the
bind is `[webui]` in `jinn.toml` (`configuration.md`).

## The two pages

- **Landing page** (`/web`): the sidebar's session list and nothing
  else. Click a row to open that session.
- **Session page** (`/web/session/<id>`): titlebar, transcript, and the
  composer. Every open page follows its session live; close the tab
  mid-turn and nothing is lost — the turn runs in jinn, and reopening
  the page catches up.

Each page shows **one** session, the one it is viewing. Several tabs
may view different sessions; the instance still has one active session,
and clicking a row makes that session active everywhere.

## Keys and buttons

There are no keybinds on the web — every action is a click. The keys
that do exist:

| Key | Does |
| --- | --- |
| `Enter` in the composer | Inserts a newline. It never sends. |
| `Esc` | Closes the top overlay: a modal, the menu, or the session drawer |

So a message is sent only by the **Send** button. While the session is
working the composer swaps Send for **`Q`**, which queues the message
until the current turn settles, and **Cancel**, which stops the turn
(queued work is dumped into the TUI's input box as an editable draft,
not into the web composer).

## On a phone

Under 768px wide the sidebar becomes an off-canvas drawer. The titlebar
gains a `#` button that opens it in one tap; a tap on the dimmed
backdrop closes it.

## What the web does not do

Everything below exists in the TUI only. When a user asks for one of
these, say so rather than describing a control that is not there.

- Pinning, exclusion, isolate, and the per-entry audit popup.
- Steer mode (`<M-q>`): a message sent while working is always queued.
- The `#prompt`, `@attachment`, and `/command` input tokens. Text is
  sent verbatim.
- Forking, resuming (`c`), lifecycle setup (`s`), archiving, closing,
  and tearing down sessions. (The web can *open* an archived session
  through its **Archive** picker, but cannot archive one.)
- Model alloys and multi-select, reasoning-effort and endpoint pickers.
- Staged toggling in the tool and MCP pickers (the web commits per
  click), the MCP inspector, and the pickers' context-cost footers.
- The skill picker's markdown preview, the terminal overlay, dictation,
  and the task-list browser.

