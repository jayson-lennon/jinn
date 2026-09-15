# Interactive Terminal Overlay

jinn can run an interactive terminal program (vim, htop, a REPL, `ssh`) in a
PTY beside the agent. The agent gets the rendered screen from its tool calls;
**you** can watch, take over, and hand back — both of you share the same live
terminal.

- One terminal per chat session. Spawning another kills the previous program.
- Sessions with a live terminal are marked in the sidebar's session tree.
- The overlay renders the active session's terminal with colors, sized to
  fit.

## Opening and closing

| Key | Context | Action |
| --- | --- | --- |
| `<M-t>` | any non-terminal scope | Toggle the overlay for the **active** session |
| `T` | sidebar Sessions section | Toggle the overlay for the **selected** session |
| `<M-t>` | overlay view mode | Close the overlay (program keeps running) |
| `<Tab>` | overlay view mode | Cycle tabs (Dashboard ↔ Normal) |

Closing the overlay never stops the program — it keeps running in the
background, and the agent's tool calls keep working against it.

## Watching vs. controlling

The overlay has two modes:

- **View** (default): passive. You watch the screen; keys do jinn things
  (`y`, `I`, `<M-t>`, `T`, `<Tab>`, `?`). The agent can read the screen and
  send input freely.
- **Control**: *your* keys go straight to the program; the agent's input is
  locked out. Every key except the toggle is forwarded to the PTY.

| Key | Context | Action |
| --- | --- | --- |
| toggle (default `<c-g>`) | view | **Take control** |
| toggle (default `<c-g>`) | control | **Hand back** to the agent |

The toggle key is configurable per user — check `[interactive_term]
control_toggle_key` in `jinn.toml` (see `configuration.md`) or just try
`<c-g>`, the shipped default.

**Handback semantics (worth explaining to users):** if the agent's input lands
while *you* hold control, the tool call does not type into your program — it
resolves with a "user holds control" notice, and the agent waits. When you
hand back, the agent's next call sees the screen as *you* left it.

## Getting the screen into the conversation

| Key | Context | Action |
| --- | --- | --- |
| `y` | view mode | Yank the visible screen to the clipboard |
| `I` | view mode | Yank the screen **and** push it to the model ("Here is the current terminal screen:") |

`I` is the fastest way to ask the agent about what you're looking at:
open the overlay on a failing test run, press `I`, and ask "why is this
failing?" — no copy-pasting.

## Typical workflows

- **Long-running programs**: start a dev server or watch process via the
  agent; it stays alive across turns. Glance with `<M-t>` any time.
- **Interactive auth / TUI apps**: let the agent launch it, take control with
  the toggle to type passwords or drive vim, hand back when done.
- **Screen as context**: `I` to inject the current screen, or ask the agent to
  re-check the screen after you've poked at it in control mode.
