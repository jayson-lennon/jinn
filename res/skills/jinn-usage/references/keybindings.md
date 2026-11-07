# Default Keybindings

> **Default** bindings for the built-in keymap. Press `?` (normal-like scopes)
> or `<f1>` (input mode) for the which-key popup — it shows the bindings live
> in the current scope and is the source of truth if this file and your build
> disagree.

## Notation

| Form                                 | Meaning                                   |
| ------------------------------------ | ----------------------------------------- |
| `<c-x>`                              | Ctrl + x                                  |
| `<M-x>`                              | Alt + x                                   |
| `<leader>`                           | Space                                     |
| `<enter>`, `<esc>`, `<tab>`, `<up>`… | Enter, Escape, Tab, arrow keys            |
| `gci`                                | A sequence: press `g`, then `c`, then `i` |
| `<leader>sm`                         | Space, then `s`, then `m`                 |

jinn is modal — keys mean different things depending on where focus is:
normal mode, input mode, a sidebar section, a picker, the terminal overlay.
The which-key popup shows the bindings for the scope you're in.

## Normal mode (chat focus)

General / app:

| Key          | Action                               |
| ------------ | ------------------------------------ |
| `q`, `<c-c>` | Quit jinn                            |
| `?`          | Toggle the which-key popup           |
| `i`, `<c-j>` | Enter input mode (compose a message) |
| `<esc>`      | Cancel selection / dismiss prompts   |
| `n`          | New session                          |
| `<Tab>`      | Cycle tabs (Dashboard ↔ Normal)      |

`gci` and `gcp` are prefixes — press `g`, then `c`, then the final key.
Sequences compose — `gcip` is isolate-and-pin: `gci`, then `p`. `<c-j>` enters
input mode from normal mode. The newline keys live in input mode.

Navigation:

| Key               | Action                                             |
| ----------------- | -------------------------------------------------- |
| `j` / `k`         | Select next / previous chat entry                  |
| `<c-d>` / `<c-u>` | Scroll down / up half a page                       |
| `gg` / `G`        | Jump to top / bottom of history                    |
| `]c` / `[c`       | Jump to next / previous compaction summary         |
| `]u` / `[u`       | Jump to next / previous user message               |
| `]p` / `[p`       | Jump to next / previous pinned entry               |
| `]s` / `[s`       | Jump to next / previous Sources (annotation) entry |
| `<enter>`         | Open the selected `spawn_subagent` call's subagent session   |

Context (see `context-management.md`):

| Key   | Action                                                                  |
| ----- | ----------------------------------------------------------------------- |
| `p`   | Pin the selected entry to context                                       |
| `x`   | Exclude the selected entry from context                                 |
| `r`   | Reset the selected entry to default context handling                    |
| `gci` | Isolate: force-include the selected entry's tool loop, exclude the rest |
| `gcp` | Edit the auto-pruner accumulation token threshold                       |
| `e`   | Expand / collapse the selected tool entry                               |
| `h`   | Toggle visibility of excluded (collapsed) blocks                        |
| `a`   | Toggle the audit popup for the selected entry                           |

Chat content:

| Key | Action                                                             |
| --- | ------------------------------------------------------------------ |
| `y` | Yank (copy) the selected entry to the clipboard                    |
| `f` | Fork a new session from the selected entry (history kept up to it) |
| `F` | New session seeded with the selected entry (no inherited history)  |

Model / provider (see `models-and-providers.md`):

| Key   | Action                 |
| ----- | ---------------------- |
| `gmr` | Refresh the model list |

Misc:

| Key     | Action                                                   |
| ------- | -------------------------------------------------------- |
| `gcr`   | Re-scan prompt templates                                 |
| `<M-c>` | Change session cwd (search from the session's directory) |
| `<M-d>` | Change session cwd (search from `$HOME`)                 |
| `<M-t>` | Toggle the interactive terminal overlay                  |
| `<c-l>` | Focus the sidebar                                        |
| `<M-s>` | Focus the sidebar's Sessions section                     |

### `<leader>s` — pickers

| Key          | Opens                            |
| ------------ | -------------------------------- |
| `<leader>sm` | Model/provider picker            |
| `<leader>ss` | Session browser                  |
| `<leader>se` | Persona picker                   |
| `<leader>st` | Tool toggle picker               |
| `<leader>sk` | Skill picker                     |
| `<leader>sM` | MCP server picker/inspector      |
| `<leader>sh` | Theme picker                     |
| `<leader>sr` | Reasoning-effort picker          |
| `<leader>sE` | OpenRouter endpoint picker       |
| `<leader>sp` | Project picker                   |
| `<leader>sl` | Session lifecycle recipe picker  |
| `<leader>sb` | Saved-bots picker                |
| `<leader>sw` | Workflow picker                  |
| `<leader>sa` | Applets picker                   |
| `<leader>so` | OpenRouter model market          |
| `<leader>sx` | Stream-rule picker               |
| `<leader>cd` | CWD input (type a path directly) |

## Input mode (typing a message)

| Key                                      | Action                                 |
| ---------------------------------------- | -------------------------------------- |
| `<enter>`                                | Submit the message                     |
| `<s-enter>` / `<c-enter>`                | Insert a newline                       |
| `<esc>` / `<c-k>`                        | Back to normal mode                    |
| `<M-q>`                                  | Toggle input mode behavior             |
| `<c-e>`                                  | Open the message in `$EDITOR`          |
| `<c-c>`                                  | Clear the input buffer                 |
| `<tab>`                                  | Confirm the active autocomplete popup  |
| `<backspace>` / `<delete>`               | Delete backward / forward one grapheme |
| `<left>` / `<right>` / `<up>` / `<down>` | Move the cursor                        |
| `<c-left>` / `<c-right>`                 | Move by word                           |
| `<home>` / `<end>`                       | Jump to start / end                    |
| `<f1>`                                   | Toggle the which-key popup             |
| `<c-u>` / `<c-d>`                        | Scroll chat history while typing       |
| `<c-l>` / `<M-s>`                        | Focus sidebar / sidebar Sessions       |
| `<M-c>` / `<M-d>`                        | Change cwd (session root / home)       |
| `<M-t>`                                  | Toggle the terminal overlay            |

Type `#`, `@`, or `/` to open autocomplete popups — `/` only works as the
buffer's very first character. See `chat-input-tokens.md`.

## Sidebar

Enter it from normal mode with `<c-l>` — or `<M-s>` to land straight on
Sessions. Common to every section:

| Key               | Action                                                         |
| ----------------- | -------------------------------------------------------------- |
| `j` / `k`         | Move down / up within the section                              |
| `J` / `K`         | Next / previous section (stops at the ends)                     |
| `<esc>` / `<c-h>` | Leave the sidebar, back to chat                                |
| `<c-w>`           | Enter sidebar resize mode (`h`/`l` widen/narrow, `<esc>`/`<c-c>` done) |
| `q`, `?`          | Quit / which-key                                               |
| `<M-t>`           | Toggle the terminal overlay                                    |

### Persona section

| Key | Action                    |
| --- | ------------------------- |
| `c` | Edit the selected persona |

### Pins section (pin management — see `context-management.md`)

| Key       | Action                                             |
| --------- | -------------------------------------------------- |
| `u`       | Unpin the selected pin                             |
| `t`       | Move the pin to the top of context                 |
| `b`       | Move the pin to the bottom of context              |
| `r`       | Move the pin to a relative position                |
| `m`       | Cycle the pin's position                           |
| `<enter>` | Leave the sidebar at the pin's position in history |

### Sessions section

| Key       | Action                                                                   |
| --------- | ------------------------------------------------------------------------ |
| `<enter>` | Switch to the selected session                                           |
| `i`       | Switch and enter input mode                                              |
| `y`       | Yank (copy) the session's ID to the clipboard                            |
| `r`       | Rename the session                                                       |
| `a`       | Archive the session (press again to confirm)                             |
| `A`       | Archive the session **and its subtree** (press again to confirm)         |
| `x`       | Close the selected session (press again to confirm)                      |
| `t`       | Tear down the session: run its teardown command (leaves it open)         |
| `X`       | Tear down the session **and its whole subtree** (press again to confirm) |
| `c`       | Resume the session: re-send its context to the provider (no new message, no switch) |
| `s`       | Run the session's lifecycle setup command (only when it has not run yet) |
| `b`       | Create a bot of the selected session (prep mode)                         |
| `B`       | Create a standalone bot (no parent) from the session's environment       |
| `G`       | Re-run the selected bot (cancels its in-flight turn)                     |
| `R`       | Reset the selected bot's context without dispatching                     |
| `L`       | Move the session into the **Library** sidebar section                    |
| `P`       | Open the selected bot's properties popup                                 |
| `T`       | Toggle the terminal overlay for the selected session                     |

- **`b`, `B`, `G`, `R`, and `P` only act when the selected session is a bot** — on a plain session, nothing.
- **`L` has one refusal — a bot that has a parent.** Everything else moves into the Library.

The properties popup and the pickers have their own keys — see `bots.md`.

Section navigation doesn't wrap — `J` on the last section (Sessions) and `K`
on the first (Persona) do nothing. Sections with nothing in them are skipped
over, so a section you never see is still reachable.

### Bots section

Sessions that watch another session. `bots.md` has what a bot is, plus the
properties popup's and the pickers' own keys.

| Key       | Action                                             |
| --------- | --------------------------------------------------- |
| `<enter>` | Switch to the highlighted bot                       |
| `i`       | Switch to it and enter input mode                   |
| `y`       | Yank (copy) the bot's session ID to the clipboard   |
| `G`       | Re-run the highlighted bot (cancels a busy turn)    |
| `R`       | Reset the bot's context without dispatching         |
| `c`       | Resume the bot: re-send its context to the provider |
| `P`       | Open the bot's properties popup                     |
| `s`       | Browse the highlighted bot's report history         |

`b` (new bot) and `B` (standalone bot) don't work here — only in **Sessions**,
on the session selected there.

### Library section

A flat list of sessions bookmarked with `L` in Sessions. Archived members
render dimmed — `<enter>` loads one and unarchives it. Members hide from the
Sessions list while they're here, and a bot that has a parent can't be moved in.

| Key       | Action                                             |
| --------- | --------------------------------------------------- |
| `<enter>` | Load and switch to the member (unarchives it)       |
| `i`       | Switch to the member and enter input mode           |
| `y`       | Yank (copy) the member's session ID to the clipboard |
| `G`       | Re-run the member bot (cancels a busy turn)         |
| `R`       | Reset the member bot's context without dispatching  |
| `c`       | Resume the member (loaded members only)             |
| `L`       | Remove the member from the Library                  |
| `a`       | Archive the member                                  |
| `P`       | Open a member bot's properties popup                |
| `r`       | Rename the member                                   |

### Task-list section

| Key                 | Action                                 |
| ------------------- | -------------------------------------- |
| `s`                 | Open the full-screen task-list browser |
| `<pgup>` / `<pgdn>` | Scroll the task-list preview           |

### MCP servers section

Read-only navigation (status view). Manage servers via the MCP picker — see
`mcp-servers.md`.

## Pickers (shared)

| Key                                  | Action                |
| ------------------------------------ | --------------------- |
| `<esc>`                              | Close the picker      |
| `<enter>`                            | Confirm the selection |
| `<up>` / `<down>`                    | Move selection        |
| `<pgup>` / `<pgdn>`                  | Page the list         |
| `<left>` / `<right>` / `<backspace>` | Edit the filter       |
| `<c-c>`                              | Clear the filter, or close when it is empty |
| `<c-n>`                              | New session (Session, Persona, Tool, Skill, Endpoint, Theme, Reasoning, Lifecycle, Project, and Task-list pickers) |
| any letter                           | Type into the filter  |

### Picker-specific keys

| Picker     | Key               | Action                                                  |
| ---------- | ----------------- | ------------------------------------------------------- |
| Model      | `<Tab>`           | Toggle alloy/multi-model selection mode                 |
| Model      | `<c-a>`           | Toggle alloy mode                                       |
| Model      | `<c-r>`           | Refresh models                                          |
| Tool       | `<Tab>`           | Enable/disable the selected tool                        |
| Skill      | `<Tab>`           | Enable/disable the selected skill                       |
| Skill      | `<c-l>`           | Load the highlighted skill into context (stays open)    |
| Skill      | `<c-u>` / `<c-d>` | Scroll the preview pane                                 |
| Skill      | `<c-r>`           | Re-scan skills                                          |
| Endpoint   | `<c-r>`           | Refresh endpoint listings                               |
| MCP server | `<Tab>`           | Stage enable/disable — `<enter>` applies it, `<esc>` reverts to how the picker opened |
| MCP server | `<c-r>`           | Restart the selected server                             |
| MCP server | `<c-t>`           | Toggle preview pane (status/log ↔ tool list)            |
| Project    | `<c-n>`           | Start a new session (leaves the picker)              |
| Project    | `<c-d>`           | Remove the highlighted project                          |
| Project    | `<c-enter>`       | New session at the highlighted project with a lifecycle |
| Workflow   | `<enter>`         | Load the highlighted workflow into the current session |
| Workflow   | `<c-enter>`       | New session running the highlighted workflow          |
| Workflow   | `<c-r>`           | Re-scan the workflows directory                         |
| Task list  | `<c-n>`           | Start a new session                                     |

## Text-input scopes

Arg input (lifecycle arguments), session rename, CWD input, project-add, and
pruner-threshold input share a pattern:

| Key                                           | Action                                            |
| --------------------------------------------- | ------------------------------------------------- |
| `<enter>`                                     | Confirm                                           |
| `<esc>`                                       | Cancel                                            |
| letters / `<backspace>` / `<delete>` / arrows | Edit the text (numeric scopes accept digits only) |

## Terminal overlay

See `terminal-overlay.md` for the full workflow. Scope-specific keys:

| Scope            | Key                                 | Action                                   |
| ---------------- | ----------------------------------- | ---------------------------------------- |
| Terminal view    | `<M-t>`                             | Close the overlay                        |
| Terminal view    | `y`                                 | Yank the visible screen to the clipboard |
| Terminal view    | `I`                                 | Yank the screen and push it to the model |
| Terminal view    | `T`                                 | Toggle overlay for the selected session  |
| Terminal view    | configured toggle (default `<c-g>`) | Take control of the program              |
| Terminal control | configured toggle (default `<c-g>`) | Release control back to the agent        |
| Terminal control | any other key                       | Forwarded to the running program         |

## Slice-provided keys (feature bindings)

Some keys come from feature slices, not the static keymap — they're only
bound while the feature is active:

| Key      | Feature        | Action                                           |
| -------- | -------------- | ------------------------------------------------ |
| `gdc`    | Discord bridge | Continue the current session in a Discord thread (see `discord.md`) |
| `<M-i>`  | Dictation      | Toggle voice recording (see `dictation.md`)      |
| ``<M-`>`` | Quake bar     | Toggle the drop-down command console (inside it: ``<M-`>`` or `<esc>` closes, `<enter>` submits a command, `<pgup>`/`<pgdn>` scroll the log, `<c-c>` clears the input) |

## Mouse

| Input        | Action                        |
| ------------ | ----------------------------- |
| Scroll wheel | Scroll chat history up / down |
