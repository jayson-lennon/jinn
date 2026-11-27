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
- **Model market** (`<leader>so`): searchable table of OpenRouter's catalog
  with per-model performance columns — ECI (Epoch AI, bundled snapshot) and
  VALS (Vals Index, fetched live and cached). Sort with `l`/`h` (cycles
  PROJ → IN → OUT → CACHE → ECI → VALS → MODEL), reverse with `s`, `<enter>`
  switches/pins the model, `<enter>` again in detail mode shows pricing and
  performance detail, `O` opens the highlighted model's openrouter.ai page
  in your OS default browser. Rows with no score for a column show `#`.
- **Tool / Skill**: `<Tab>` toggles the highlighted entry on/off for this
  session. Toggles are per-session and persist for the session; the
  `tool_filter` / `skill_filter` defaults in `jinn.toml` stay as written.
- **Skill**: `<c-l>` loads the highlighted skill's body into context as a
  pinned tool-result pair. The picker stays open so you can load several, and
  loading auto-enables a disabled skill. `<c-u>`/`<c-d>` scroll the markdown
  preview; `<c-r>` re-scans skill directories.
- **Skill, in `/web2`**: the menu button's **Skills** item (while a
  session is active) opens the same list for the active session —
  the `●` / `○` mark, name, description, a `project` badge on a
  project-scoped skill, and a `+` on each row, with the filter bar
  narrowing it and highlighting the matches. A row click toggles the
  skill at once, with no staging. Each row's `+` loads the body into context,
  enabling the skill first if it was off; once loaded, the row is not a
  toggle and its `+` is disabled. The modal refreshes itself when a
  skill scan lands. The markdown preview and the context-cost footer
  are TUI-only.
- **Tool, in `/web2`**: the menu button's **Tools** item (while a
  session is active) opens the active session's tools — a `✓` / `✗`
  enabled mark, the name, the description's first sentence — with the
  filter bar narrowing it and highlighting the matches. Clicking a row
  commits at once, the TUI `<Tab>` + `<enter>` result with no staging,
  and the modal stays open. Dismissing with `Esc` or a backdrop click
  only closes it — there is **no revert**; undo a
  mis-click by clicking the row again.
- **MCP servers, in `/web2`**: the menu button's **MCP servers** item
  (while a session is active) opens one row per `[mcp.<name>]` in
  `jinn.toml` — a `●` / `○` enabled mark, the name, the launch summary,
  and a status badge (`off` / `starting` / `running` / `dead`). Clicking
  a row toggles that server for the session at once and the modal stays
  open; the badge updates live on its own. No stderr/log pane and no
  tool list — those are TUI-only.
- **Persona, in `/web2`**: the menu button's **Persona** item (while a
  session is active) opens the personas in their own name-sorted order
  — a `●` / `○` mark, the name, the description. Clicking a row
  switches the session's persona and **closes the modal**. A persona
  without a `<compaction>` handoff block pushes an error entry into the
  transcript, the same warning the TUI shows.
- **Applets, in `/web2`**: the menu button's **Applets** item (at every
  time — it depends on no session) opens the saved applets, newest first,
  with a status line naming the applets server's bind and the filter bar
  narrowing by title and highlighting the matches. Each row is a link that
  opens the applet in a new tab while the server is serving, and a
  disabled line while it is offline. Rename, delete, and kiosk are the
  TUI's — `/web2` opens, nothing more.
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
