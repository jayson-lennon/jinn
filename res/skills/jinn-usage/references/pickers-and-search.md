# Pickers & Search

Pickers are Telescope-style popups for changing models, sessions, themes,
tools, and more. Open them from normal mode with the `<leader>s` family
(`<leader>` = Space); every picker is also listed under the `<leader>s` group
in the which-key popup (`?`).

## The picker family

| Key | Picker | What it changes |
| --- | --- | --- |
| `<leader>sm` | Model/provider | Active model (and provider) for the session |
| `<leader>ss` | Session | Switch / resume any session (incl. archived) |
| `<leader>se` | Persona | Session persona |
| `<leader>st` | Tool | Enable/disable tools for this session |
| `<leader>sk` | Skill | Enable/disable skills; load skill bodies into context |
| `<leader>sM` | MCP server | Enable/disable/restart MCP servers (inspector) |
| `<leader>sP` | Plugin | Read-only list of loaded plugins |
| `<leader>sh` | Theme | UI theme |
| `<leader>sr` | Reasoning effort | Model reasoning-effort level |
| `<leader>sE` | Endpoint | OpenRouter routing endpoint pin |
| `<leader>so` | Project | Curated project dirs for quick session creation |
| `<leader>sc` | Compaction model | Model used for compaction summaries |
| `<leader>sl` | Lifecycle | Lifecycle recipes for new sessions |

## Inside a picker

Shared controls:

| Key | Action |
| --- | --- |
| type letters | Filter the list (bare letters go to the filter, not actions) |
| `<up>` / `<down>` | Move selection |
| `<pgup>` / `<pgdn>` | Page the list |
| `<left>` / `<right>` / `<backspace>` | Edit the filter |
| `<enter>` | Confirm |
| `<esc>` | Close |
| `<c-n>` | New session from here |

Picker-specific keys are shown in the picker's own which-key overlay — the
highlights:

- **Model**: `<Tab>` multi-selects models into an **alloy**; `<c-a>` toggles
  alloy mode; `<c-r>` refreshes the model list from the provider.
- **Tool / Skill**: `<Tab>` toggles the highlighted entry on/off for this
  session. Toggles are per-session and persist; they never write back to the
  `disabled_tools` / `disabled_skills` defaults in `jinn.toml`.
- **Skill**: `<c-l>` loads the highlighted skill's body into context as a
  pinned tool-result pair (the picker stays open, so you can load several);
  loading auto-enables a disabled skill. `<c-u>`/`<c-d>` scroll the markdown
  preview; `<c-r>` re-scans skill directories.
- **MCP server**: `<Tab>` toggles, `<c-r>` restarts, `<c-t>` flips the preview
  pane between live status/logs and the server's tool list (details in
  `mcp-servers.md`).
- **Endpoint**: `<c-r>` re-fetches OpenRouter endpoint listings (see
  `models-and-providers.md`).
- **Project**: `<c-n>` registers a new project directory, `<c-d>` removes the
  highlighted one, `<c-enter>` starts a session there with a lifecycle recipe.

## Sidebar navigation doubles as picker preview

The sidebar's Sessions and Task-list sections show live previews of the
highlighted entry; `<enter>` confirms from the sidebar just like a picker.

## Searching across sessions

jinn keeps a full-text (FTS5) index over persisted chat prose — user,
assistant, tool call/result, system, error, and compaction entries — in every
session.

- The **`session_search` tool** (ask the agent: "search my sessions for X")
  runs a ranked query over all sessions and returns the best matches with
  per-session counts; the agent can then pull a specific transcript with
  **`session_fetch`**.
- Queries go to SQLite FTS5 unmodified: plain words, `"quoted phrases"`,
  `prefix*`, `AND` / `OR` / `NOT`; malformed queries return the SQLite error
  verbatim.
- Indexing trails saves by up to ~one heartbeat (5s); a dashboard row shows
  live reindex progress.

This is jinn's cross-session memory: nothing is silently recalled — you (or
the agent) search for it explicitly.
