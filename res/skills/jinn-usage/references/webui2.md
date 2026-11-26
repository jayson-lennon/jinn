# Web UI 2 (`/web2`)

A running jinn instance serves a **second** browser UI at `/web2`, beside
the original at `/web`. It is a mirror of the TUI with web UX: the same
sessions, the same transcript, the same single active session, and every
action publishes the same message the TUI's keybind does.

The original web UI is unchanged and still served. `/web2` is the newer
surface; it covers the core loop only (see [What `/web2` does not
do](#what-web2-does-not-do)).

Like `/web`, it is a **single-user, unauthenticated** surface — reach it
over loopback or a VPN you secure.

## Open it

Point a browser at `http://127.0.0.1:8388/web2` (or the `[webui2]` bind
you configured — see the **Web UI 2** entry under
[Common asks → settings](configuration.md#common-asks--settings)).

One browser tab is the supported model. Two tabs on `/web2` both work,
but nothing coordinates them.

## What it does

- **Sessions** — the sidebar lists the loaded sessions; the active one is
  highlighted. Clicking a row activates that session (the TUI follows).
- **Archive** — the sidebar's **Archive** button opens a picker over every
  stored session, loaded or archived, with a title filter. Choosing a row
  activates it; an archived session is loaded from disk and leaves the
  archive.
- **Transcript** — the full history, streamed live: markdown, thinking,
  tool calls and results, errors, and system messages all render as they
  arrive.
- **Composer** — sends a message, queues one while a turn is running, or
  cancels the running turn.
- **Model** — the titlebar's model chip opens a picker of the available
  models. Choosing one switches the session's model.
- **Title** — click the title to rename the session in place; `Enter`
  commits and `Esc` cancels.

## Keys and buttons

There are no keybinds beyond these:

| Key | Does |
| --- | --- |
| `Enter` in the composer | Sends the message. |
| `Shift+Enter` in the composer | Inserts a newline. |
| `Enter` in a picker's filter | Does nothing; a row is chosen by click. |
| `Esc` | Closes the top overlay: a picker, or the title editor. |

The composer's buttons follow the session's phase:

- **Idle** — **Send** only.
- **Working** — **Queue** and **Cancel**; there is no Send.

`Enter` follows the same rule: idle it sends, working it queues. An
empty draft does nothing either way.

**Cancel** stops the running turn. Queued work is dumped into the TUI's
input box as an editable draft — not into the web composer.

## Reconnecting

If the connection drops, a `disconnected — reconnecting…` overlay covers
the page and it retries on its own; the overlay clears when it is back.
An unsent draft survives the reconnect. A restarted jinn is a normal
event — the page comes back with the whole transcript.

## What `/web2` does not do

Everything below exists on the TUI, and most of it on `/web` too. When a
user asks for one of these on `/web2`, say so and point at the surface
that has it.

- Creating, forking, archiving, or tearing down sessions. `/web2` can
  *open* an archived session through its picker, but cannot create one.
- Personas, MCP servers, tools, skills, and applets — any of the menu's
  pickers.
- Pinning, exclusion, isolate, and the per-entry audit popup.
- Steer mode. A message sent while working is queued; to steer, cancel
  and send.
- The `#prompt`, `@attachment`, and `/command` input tokens, and image
  attachments. Text is sent verbatim.
- Model alloys, reasoning-effort and endpoint pickers.
- Unread marks on session rows.
- A theme switcher, a phone drawer layout, and a menu.

## How it works

The page holds all view state and talks to the server over a single
WebSocket using two message sets — one for commands the page sends, one
for facts the server sends back. The server renders no HTML fragments and
reads nothing back from the page, which is what keeps the two in step.

On every (re)connect the page sends the newest entry id it holds. The
server continues from there when it safely can and otherwise resends the
whole transcript, so a restart can never leave a gap.
