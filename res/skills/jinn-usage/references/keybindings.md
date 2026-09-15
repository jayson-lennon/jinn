# Default Keybindings

> These are the **default** bindings for the built-in keymap. Press `?`
> (normal-like scopes) or `<f1>` (input mode) to open the which-key popup —
> it always shows the bindings live in the current scope and is the source of
> truth if this file and your build disagree.

## Notation

| Form | Meaning |
| --- | --- |
| `<c-x>` | Ctrl + x |
| `<M-x>` | Alt + x |
| `<leader>` | Space |
| `<enter>`, `<esc>`, `<tab>`, `<up>`… | Enter, Escape, Tab, arrow keys |
| `gci` | A sequence: press `g`, then `c`, then `i` |
| `<leader>sm` | Space, then `s`, then `m` |

jinn is modal: keys mean different things depending on where focus is
(normal mode, input mode, a sidebar section, a picker, the terminal overlay).
The which-key popup shows the active scope's bindings.

## Normal mode (chat focus)

General / app:

| Key | Action |
| --- | --- |
| `q`, `<c-c>` | Quit jinn |
| `?` | Toggle the which-key popup |
| `i`, `<c-j>` | Enter input mode (compose a message) |
| `<esc>` | Cancel selection / dismiss prompts |
| `n` | New session |
| `N` | New session with a lifecycle recipe |
| `<Tab>` | Cycle tabs (Dashboard ↔ Normal) |

Navigation:

| Key | Action |
| --- | --- |
| `j` / `k` | Select next / previous chat entry |
| `<c-d>` / `<c-u>` | Scroll down / up half a page |
| `gg` / `G` | Jump to top / bottom of history |
| `]c` / `[c` | Jump to next / previous compaction summary |
| `]u` / `[u` | Jump to next / previous user message |
| `]p` / `[p` | Jump to next / previous pinned entry |
| `]s` / `[s` | Jump to next / previous Sources (annotation) entry |
| `<enter>` | Open the selected `task` call's subagent session |

Context (see `context-management.md`):

| Key | Action |
| --- | --- |
| `p` | Pin the selected entry to context |
| `x` | Exclude the selected entry from context |
| `r` | Reset the selected entry to default context handling |
| `gci` | Isolate: force-include the selected entry's tool loop, exclude the rest |
| `gcp` | Edit the auto-pruner accumulation token threshold |
| `e` | Expand / collapse the selected tool entry |
| `h` | Toggle visibility of excluded (collapsed) blocks |
| `a` | Toggle the audit popup for the selected entry |

Chat content:

| Key | Action |
| --- | --- |
| `y` | Yank (copy) the selected entry to the clipboard |
| `f` | Fork a new session from the selected entry (history kept up to it) |
| `F` | New session seeded with the selected entry (no inherited history) |

Model / provider (see `models-and-providers.md`):

| Key | Action |
| --- | --- |
| `gmr` | Refresh the model list |

Misc:

| Key | Action |
| --- | --- |
| `gcr` | Re-scan prompt templates |
| `<M-c>` | Change session cwd (search from the session's directory) |
| `<M-d>` | Change session cwd (search from `$HOME`) |
| `<M-t>` | Toggle the interactive terminal overlay |
| `<c-l>` | Focus the sidebar |
| `<M-s>` | Focus the sidebar's Sessions section |

### `<leader>s` — pickers

| Key | Opens |
| --- | --- |
| `<leader>sm` | Model/provider picker |
| `<leader>ss` | Session browser |
| `<leader>se` | Persona picker |
| `<leader>st` | Tool toggle picker |
| `<leader>sk` | Skill picker |
| `<leader>sM` | MCP server picker/inspector |
| `<leader>sP` | Plugin list (read-only) |
| `<leader>sh` | Theme picker |
| `<leader>sr` | Reasoning-effort picker |
| `<leader>sE` | OpenRouter endpoint picker |
| `<leader>so` | Project picker |
| `<leader>sc` | Compaction model picker |
| `<leader>sl` | Session lifecycle recipe picker |
| `<leader>cd` | CWD input (type a path directly) |

## Input mode (typing a message)

| Key | Action |
| --- | --- |
| `<enter>` | Submit the message |
| `<s-enter>` / `<c-enter>` / `<c-j>` | Insert a newline |
| `<esc>` / `<c-k>` | Back to normal mode |
| `<M-q>` | Toggle input mode behavior |
| `<c-e>` | Open the message in `$EDITOR` |
| `<c-c>` | Clear the input buffer |
| `<tab>` | Confirm the active autocomplete popup |
| `<backspace>` / `<delete>` | Delete backward / forward one grapheme |
| `<left>` / `<right>` / `<up>` / `<down>` | Move the cursor |
| `<c-left>` / `<c-right>` | Move by word |
| `<home>` / `<end>` | Jump to start / end |
| `<f1>` | Toggle the which-key popup |
| `<c-u>` / `<c-d>` | Scroll chat history while typing |
| `<c-l>` / `<M-s>` | Focus sidebar / sidebar Sessions |
| `<M-c>` / `<M-d>` | Change cwd (session root / home) |
| `<M-t>` | Toggle the terminal overlay |

Typing `#`, `@`, or `/` at the start of a token opens autocomplete popups —
see `chat-input-tokens.md`.

## Sidebar

Press `<c-l>` (or `<M-s>` for Sessions) from normal mode to enter it. Common
to every section:

| Key | Action |
| --- | --- |
| `j` / `k` | Move down / up within the section |
| `J` / `K` | Next / previous section (wraps) |
| `<esc>` / `<c-h>` | Leave the sidebar, back to chat |
| `<c-w>` | Enter sidebar resize mode (`h`/`l` widen/narrow, `<esc>` done) |
| `i` | Enter input mode |
| `q`, `<c-c>`, `?` | Quit / quit / which-key |
| `<M-t>` | Toggle the terminal overlay |

### Persona section

| Key | Action |
| --- | --- |
| `c` | Edit the selected persona |

### Pins section (pin management — see `context-management.md`)

| Key | Action |
| --- | --- |
| `u` | Unpin the selected pin |
| `t` | Move the pin to the top of context |
| `b` | Move the pin to the bottom of context |
| `r` | Move the pin to a relative position |
| `m` | Cycle the pin's position |
| `<enter>` | Leave the sidebar at the pin's position in history |

### Sessions section

| Key | Action |
| --- | --- |
| `<enter>` | Switch to the selected session |
| `i` | Switch and enter input mode |
| `r` | Rename the session |
| `a` / `A` | Archive the session / archive it and its subtree |
| `x` | Close (archive) the selected session |
| `t` | Tear down the session (run its teardown command) |
| `X` | Tear down the session **and its whole subtree** (press again to confirm) |
| `c` | Continue: switch to the session and re-run its setup command |
| `s` | Re-run the session's setup command |
| `n` / `N` | New session / new with lifecycle recipe |
| `T` | Toggle the terminal overlay for the selected session |
| `p` | Prefix group for session actions (see the which-key popup) |

### Task-list section

| Key | Action |
| --- | --- |
| `s` | Open the full-screen task-list browser |
| `<pgup>` / `<pgdn>` | Scroll the task-list preview |

### MCP servers section

Read-only navigation (status view). Manage servers via the MCP picker — see
`mcp-servers.md`.

## Pickers (shared)

| Key | Action |
| --- | --- |
| `<esc>` | Close the picker |
| `<enter>` | Confirm the selection |
| `<up>` / `<down>` | Move selection |
| `<pgup>` / `<pgdn>` | Page the list |
| `<left>` / `<right>` / `<backspace>` | Edit the filter |
| `<c-n>` | New session |
| any letter | Type into the filter |

### Picker-specific keys

| Picker | Key | Action |
| --- | --- | --- |
| Model | `<Tab>` | Toggle alloy/multi-model selection mode |
| Model | `<c-a>` | Toggle alloy mode |
| Model | `<c-r>` | Refresh models |
| Tool | `<Tab>` | Enable/disable the selected tool |
| Skill | `<Tab>` | Enable/disable the selected skill |
| Skill | `<c-l>` | Load the highlighted skill into context (stays open) |
| Skill | `<c-u>` / `<c-d>` | Scroll the preview pane |
| Skill | `<c-r>` | Re-scan skills |
| Endpoint | `<c-r>` | Refresh endpoint listings |
| MCP server | `<Tab>` | Enable/disable for this session |
| MCP server | `<c-r>` | Restart the selected server |
| MCP server | `<c-t>` | Toggle preview pane (status/log ↔ tool list) |
| Project | `<c-n>` | Register a new project directory |
| Project | `<c-d>` | Remove the highlighted project |
| Project | `<c-enter>` | New session at the highlighted project with a lifecycle |
| Task list | — | Read-only browser |

## Text-input scopes

Arg input (lifecycle arguments), session rename, CWD input, project-add, and
pruner-threshold input share a pattern:

| Key | Action |
| --- | --- |
| `<enter>` | Confirm |
| `<esc>` | Cancel |
| letters / `<backspace>` / `<delete>` / arrows | Edit the text (numeric scopes accept digits only) |
| `<c-j>` | Insert a newline (multiline scopes) |

## Terminal overlay

See `terminal-overlay.md` for the full workflow. Scope-specific keys:

| Scope | Key | Action |
| --- | --- | --- |
| Terminal view | `<M-t>` | Close the overlay |
| Terminal view | `<tab>` | Cycle tabs |
| Terminal view | `y` | Yank the visible screen to the clipboard |
| Terminal view | `I` | Yank the screen and push it to the model |
| Terminal view | `T` | Toggle overlay for the selected session |
| Terminal view | configured toggle (default `<c-g>`) | Take control of the program |
| Terminal control | configured toggle (default `<c-g>`) | Release control back to the agent |
| Terminal control | any other key | Forwarded to the running program |

## Slice-provided keys (feature bindings)

Some keys come from feature slices rather than the static keymap and are only
bound when the feature is active:

| Key | Feature | Action |
| --- | --- | --- |
| `gdc` | Discord bridge | Continue the current session in a Discord thread |

## Mouse

| Input | Action |
| --- | --- |
| Scroll wheel | Scroll chat history up / down |
