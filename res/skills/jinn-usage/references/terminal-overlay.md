# Interactive Terminal Overlay

jinn can run an interactive terminal program (vim, htop, a REPL, `ssh`) in a
PTY beside the agent. The agent gets the rendered screen from its tool calls.
**You** can watch, take over, and hand back. Both of you share the same live
terminal.

- One terminal per chat session. Spawning another kills the previous program.
- Sessions with a live terminal are marked in the sidebar's session tree.
- The overlay renders the active session's terminal with colors, sized to
  fit.

## Open and close it

| Key | Context | Action |
| --- | --- | --- |
| `<M-t>` | any non-terminal scope | Toggle the overlay for the **active** session |
| `T` | sidebar Sessions section | Toggle the overlay for the **selected** session |
| `<M-t>` | overlay view mode | Close the overlay (program keeps running) |

Closing the overlay leaves the program running in the background, and the
agent's tool calls keep working against it.

## Watch vs. control

The overlay has two modes:

- **View** (default). Passive. You watch the screen; keys do jinn things
  (`y`, `I`, `<M-t>`, `T`, `?`). The agent can read the screen and send
  input freely.
- **Control**. *Your* keys go straight to the program; the agent's typing
  pauses while you hold it. jinn keeps one key for itself — the toggle —
  and forwards every other keypress to the PTY.

| Key | Context | Action |
| --- | --- | --- |
| toggle (default `<c-g>`) | view | **Take control** |
| toggle (default `<c-g>`) | control | **Hand back** to the agent |

The toggle key is configurable per user — set `[term] control_toggle_key` in
`jinn.toml` (see `configuration.md`). `<c-g>` is the shipped default; just
try it.

**Handback semantics.** Agent input arriving while *you* hold control comes
back as a "user holds control" notice — the tool call returns it and the
agent waits. When you hand back, the agent's next call sees the screen as
*you* left it.

## Get the screen into the conversation

| Key | Context | Action |
| --- | --- | --- |
| `y` | view mode | Yank the visible screen to the clipboard |
| `I` | view mode | Yank the screen **and** push it to the model ("Here is the current terminal screen:") |

`I` is the fastest way to ask about what you're looking at. Open the overlay
on a failing test run, press `I`, ask "why is this failing?" — the screen
arrives with your question.

## Typical workflows

- **Long-running programs.** Start a dev server or watch process through the
  agent; it stays alive across turns. Glance with `<M-t>` any time.
- **Interactive auth / TUI apps.** Let the agent launch it. Take control to
  type passwords or drive vim, hand back when done.
- **Screen as context.** Press `I` to inject the current screen, or ask the
  agent to re-check the screen after you've poked at it in control mode.
