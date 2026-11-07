# Pickers & Search

Pickers change models, sessions, themes, tools, and more. Open one from
normal mode with the `<leader>s` family (`<leader>` = Space). Every picker is
also listed under the `<leader>s` group in the which-key popup (`?`).

## The picker family

| Key          | Picker           | What it changes                                       |
| ------------ | ---------------- | ----------------------------------------------------- |
| `<leader>sm` | Model/provider   | Active model (and provider) for the session           |
| `<leader>ss` | Session          | Switch / resume any session (incl. archived); `<c-enter>` opens the highlighted session's **whole tree** |
| `<leader>se` | Persona          | Session persona (builtin personas: `personas.md`)     |
| `<leader>st` | Tool             | Enable/disable tools for this session                 |
| `<leader>sk` | Skill            | Enable/disable skills; load skill bodies into context |
| `<leader>sM` | MCP server       | Enable/disable/restart MCP servers (inspector)        |
| `<leader>sh` | Theme            | UI theme                                              |
| `<leader>sr` | Reasoning effort | Model reasoning-effort level                          |
| `<leader>sE` | Endpoint         | OpenRouter routing endpoint pin                       |
| `<leader>sp` | Project          | Curated project dirs for quick session creation       |
| `<leader>sl` | Lifecycle        | Lifecycle recipes for new sessions                    |
| `<leader>sb` | Bot              | Saved bots from `jinn.toml` (`[[bot.entry]]`)         |
| `<leader>sw` | Workflow         | Discovered workflows; enter loads one into the current session, ctrl+enter starts a new session |
| `<leader>sa` | Applets          | Saved applet artifacts; opens one in the browser (see `canvas-and-applets.md`) |
| `<leader>so` | OpenRouter model market | Browse/search OpenRouter's model catalog        |
| `<leader>sx` | Stream rules     | The enforced stream-rule set (`stream_rules.toml` + `jinn.toml`) |

## Inside a picker

Shared controls:

| Key                                  | Action                                                       |
| ------------------------------------ | ------------------------------------------------------------ |
| type letters                         | Filter the list (every bare letter lands in the filter)      |
| `<up>` / `<down>`                    | Move selection                                               |
| `<pgup>` / `<pgdn>`                  | Page the list                                                |
| `<left>` / `<right>` / `<backspace>` | Edit the filter                                              |
| `<enter>`                            | Confirm                                                      |
| `<esc>`                              | Close                                                        |
| `<c-c>`                              | Clear the filter, or close when it is empty                  |
| `<c-n>`                              | New session from here (Session, Persona, Tool, Skill, Endpoint, Theme, Reasoning, Lifecycle, Project, and Task-list pickers)   |

Each picker shows its extra keys in its own which-key overlay. The
highlights:

- **Model**: `<Tab>` multi-selects models into an **alloy**, `<c-a>` toggles
  alloy mode, `<c-r>` refreshes the model list from the provider.
- **Tool / Skill**: `<Tab>` toggles the highlighted entry on/off for this
  session. Toggles are per-session and persist for the session; the
  `tool_filter` / `skill_filter` defaults in `jinn.toml` stay as written.
- **Skill**: `<c-l>` loads the highlighted skill's body into context as a
  pinned tool-result pair. The picker stays open so you can load several, and
  loading auto-enables a disabled skill. `<c-u>`/`<c-d>` scroll the markdown
  preview; `<c-r>` re-scans skill directories.
- **MCP server**: `<Tab>` stages enable/disable. `<enter>` applies the staged
  toggles (enabling spawns the server, disabling kills it); `<esc>` reverts
  to how the picker opened. `<c-r>` restarts, `<c-t>` flips the preview pane
  between live status/logs and the server's tool list (details in
  `mcp-servers.md`).
- **Endpoint**: `<c-r>` re-fetches OpenRouter endpoint listings (see
  `models-and-providers.md`).
- **Bot**: `<enter>` attaches the highlighted saved bot to the active
  session. `<c-a>` attaches and keeps the picker open. `<c-enter>` spawns it
  as a **standalone** bot (roots its own tree) in its own session and
  switches to it (see `bots.md`).
- **Project**: `<c-n>` starts a new session, `<c-d>` removes the highlighted
  one, `<c-enter>` starts a session there with a lifecycle recipe.
- **Workflow**: `<c-r>` re-scans the workflows directory.
- **Task list**: `<c-n>` starts a new session.

### Context-cost footers

The tool, skill, and MCP pickers show a context-cost figure in the footer,
counted with the same builders the prompt assembler uses:

- **Tool**: the enabled toolset's context block (names, schemas,
  descriptions, guidelines), including MCP tools.
- **Skill**: a *list* term (names + descriptions of enabled skills) plus a
  *loaded* term (pinned skill bodies). A disabled skill that is still loaded
  keeps its body cost until you unload it.
- **MCP**: the registered **tool definitions of enabled servers only**. The
  other MCP cost is advertising the server itself. MCP tools also count
  inside the tool picker's figure, so the two figures overlap; read them as
  two views of one shared set.
- **A disabled entry costs zero** (it's marked with an `x`), except a skill
  that is still loaded.

### Filter ranking

Filter, and flat pickers sort by fuzzy-match score: strongest first,
position tiebreak. Tree-mode pickers (the session picker, for instance) sort
whole trees by their best-matching member, so an exact title match ranks
above weaker matches. An empty filter keeps recency order.

## Sidebar navigation doubles as picker preview

The sidebar's Sessions and Task-list sections preview the highlighted entry
live. `<enter>` confirms from the sidebar just like a picker.

## Search across sessions

jinn keeps a full-text (FTS5) index over persisted chat prose (user,
assistant, tool call/result, system, error, and compaction entries) in
every session.

- **`session_search` runs over it.** Ask the agent "search my sessions for
  X": a ranked query across all sessions, returning the best matches with
  per-session counts. The agent then pulls a specific transcript with
  **`session_fetch`**.
- **Queries reach SQLite FTS5 unmodified**: plain words, `"quoted phrases"`,
  `prefix*`, `AND` / `OR` / `NOT`. A malformed query returns the SQLite
  error verbatim.
- **Indexing trails saves by up to ~one heartbeat (5s)**; a dashboard row
  shows live reindex progress.

Every cross-session fact arrives through this index: you (or the agent)
search for it explicitly.
