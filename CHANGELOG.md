**(Note to agents: CHANGELOG.md is human-authored only. Do not make edits)**

## (development)

- Todo tooling completely changed to reduce context tool block size and to more closely align with existing Claude + Codex tools.
  - `todo_set_list`: rewrites the entire todo list
  - `todo_set_phase`: rewrites a single phase
  - `todo_get_list`: returns the entire todo list
- Task loop skills now reference `todo_*` tools generically instead of by exact name.
- Added a new `jinn-usage` skill.
  - This enables the agent to answer meta-questions about `jinn` usage and configuration. `jinn` should now be able to edit it's configuration on your behalf.
- Remove `jinn-plugin` skill. It will need to be manually removed from your `~/.agent/skills` directory.
- `interactive_*` family of tools and related keybinds now use session IDs instead of relying on current UI state.
  - Fix: now impossible to send keystrokes to the wrong interactive session
  - Fix: agent `interactive_*` invocations now go to the correct sessions even when forked
- `interactive_term` tool now has guidelines instructing agents not to redirect it's output.
- The `#gap-analysis` prompt should now produce more concise reports.

## 2026-09-13 v0.118.0

- Fix build dependency error (`trouper` was not published to crates.io).
- Planning prompt updated to better accommodate absence of a RECORD.md file.
- Add project-level `bash` command blocking via regex.

### `bash` command blocking

Project-specific command blocks (`bash` only) can now be configured via `jinn.toml`:

```toml
[[projects]]
path = "/mnt/zed/repos/jinn"
command_policy = [{ pattern = 'cargo\s+(test|t)\b.*\s-p\b', message = "Do not run tests on individual packages. Use `just test` or `cargo test --workspace` as indicated in AGENTS.md." }]
```

Any `bash` command that matches the pattern will immediately fail and the `message` will be returned to the agent. Note that this feature makes _no effort_ to detect circumvention techniques. It's just a regex.

## 2026-09-13 v0.117.0

- Report app name/id to OpenRouter.
- Fix: `task` tool is now available in forked sessions at any depth and is automatically disabled for subagent sessions (unbound subagent storms are still impossible). Manually forking a subagent session re-activates the `task` tool automatically.
  - The assumption here is that a manual session fork means that the user wants to continue working with the session as if it were a normal session and therefore they probably want access to the `task` tool to spawn more subagents.
- Removed `session_query` tool and replaced with two new session searching tools:
  - `session_search`: searches all sessions by content
  - `session_fetch`: reads specific entries from a session; defaults to current session
- Added FTS indexing to support `session_search` tool. Initial indexing could take anywhere from several minutes to a half hour+ depending on number of sessions and their size.
  - Indexing status is displayed on the dashboard.
  - Indexes are large so expect database size to increase by 2x-2.5x.
  - Session search will work in a degraded state until initial indexing is complete. After that, indexes should only lag by about 10 seconds from when entries land in the chat history.
- Startup performance characteristics have changed. `jinn` should startup faster overall.
  - Already applied migrations are now skipped. Migrations get reported at startup as they are applied.
  - WASM plugin compilation is now cached. Plugins will only get compiled _once_ at startup instead of on every startup.
- Shutdown performance characteristics have changed. Shutdowns should now be immediate on average.
  - Shutting down immediately after startup has a minor slowdown. This is a known issue.
  - Shutting down during a large write has a minor slowdown (like in the middle of saving a large FTS index). This is unavoidable.
- Traces should be less noisy.
- Traces now are colorless by default since they log to files. Use `--trace-color` to re-enable colored traces.
- `-v` semantics have changed and it now manipulates `RUST_LOG`. Set `RUST_LOG=...` directly to override the new behavior.
  - `-v` now controls only the verbosity of `jinn_*` crates.
  - Third-party crates will now only display `WARN` and `ERROR` traces unless `-q` is passed.
- Use bundled SQLite3 on Windows. Should resolve issues when building from source on Windows.
- Precompiled Windows artifact will now be shipped with every version update.

## 2026-09-09 v0.116.1

- Change default "sources" section highlight to light blue.
- Reduce verbose subcommand help for `jinn install`

## 2026-09-09 v0.116.0

- Highlight "sources" section when the results of a web search or web fetch come in.

## 2026-09-02 v0.115.1

- Number of pruned token display from v0.115.0 now persists across restarts and session reloads.

## 2026-09-01 v0.115.0

- Add "research" prompt. Similar to the planning prompt: use `#research <the thing you want to research>` and the agent will guide you through it. Recommend using a third-party search service via MCP to avoid automated blocking.
- Session picker now shows tree relationships by titles (same format as the session sidebar)
- Fix stall restart handling. Should now properly restart a stream.
- Add total number of tokens pruned to the Quake bar session status area.
- Subagents now inherit a copy of the parent task list. The task lists are distinct, so changes will not impact child/parent sessions.
- Subagent sessions can now also be accessed straight from the tool call+tool result chat entry.
- Subagent invocations now use purple in the chat history.
- Update "gap-analysis" prompt to provide a list of recommendations.
- A previous change introduced `project` as an alias for `projects` in `jinn.toml`, which caused duplicate key errors. This has been fixed. The correct key for project configuration is `projects`.

## 2026-08-31 v0.114.0

- Add project + date info to session picker
- Remove experimental task reminder
- Remove experimental "todo auto steer"
- Remove customized linker config for Linux builds
- Change default pruner accumulation threshold to 150k
- `jinn install --force` will no longer overwrite `jinn.toml`
- URL sources are now collapsed by default. Press `e` keybind to toggle collapsed/expanded citations.
- (Dev note): `jinn.toml` configuration round-trip tests now use hard-coded values instead of defaults. This allows updating the values of default `jinn.toml` without causing test failures.

## 2026-08-29 v0.113.0

- Add `A` sidebar keybind to archive selected session and all of it's children.
- Add `X` sidebar keybind to teardown selected session and then archive all of it's children.
- Make archive/teardown confirmation banners use consistent wording.
- Add `micro-task-loop` skill for straightforward tasks.
- Add tool-call failure watchdog plugin.
- Move stalled stream detection to plugin.
  - Fixes issue where stall detection would trigger on long-running tool calls.
- Add subagents/spawnable subtasks.
- Add interactive TUI app driver.
- (Experimental) Add task list reminder; disabled by default.

### Stall detection

Stall detection starts a timer when sending a request to a provider which resets whenever tokens are received. If the provider doesn't send tokens for a configured amount of time, then the plugin will trigger a stream restart.

Stall detection can be enabled + configured in `jinn.toml`:

```toml
[plugin.stall-watchdog]
enabled = true
wasm = "stall-watchdog.wasm"
config = { max_restarts = 3, timeout_secs = 30}
```

### Tool call watchdog

This is a plugin that tracks how many tool calls have failed and automatically stops a session if a threshold is reached.

```toml
[plugin.tool-call-watchdog]
enabled = true
wasm = "tool-call-watchdog.wasm"
config.max_failures = 4
```

It uses a simple accumulator that increases by 1 on every tool call failure, and decreases by 1 on every tool call success. Once `max_failures` is reached, it will stop the session and reset to 0.

### Subagents

Subagents can be spawned using a new `task` tool and will appear in _purple text_ as a child session in the sidebar. The max depth is set to 1 prevent runaway cascading subagents.

Subagent sessions are regular sessions that you can load to view their progress, steer, or cancel whenever desired. Once the subagent session returns to an IDLE state, the last message in the session (regardless of what it is) is sent back to the parent as a tool result. This makes it impossible to introduce a broken program state by manually working with a subagent since it's just a parent session calling a tool and waiting for the result.

### Interactive TUI app driver

New tools make interactive terminal applications available to agents. Ask an agent to run an app "interactively" in order to launch in an interactive context. One app per session is supported (use subagents for multiple apps).

When an interactive application is running, there will be a green box displayed in the session sidebar. Interactive apps run on dedicated tasks continuously until they either exit or are replaced with a different interactive app. It's possible for you to manually work with the interactive app whenever you want. Note that an interactive app only supports one input stream at a time. If you are working with an interactive application and the agent tries to send keystrokes, the tool call will fail and the agent will be instructed to wait for you to finish.

Keybinds:

- `<m-t>`: toggle app display
- `<c-g>`: toggle app control
- `y`: yank a screenshot of the application
- `I`: send a screenshot to the agent

The key to toggle input capture for the app can be defined in `jinn.toml`:

```toml
[interactive_term]
control_toggle_key = "<m-g>"    # alt+g
```

## 2026-08-27 v0.112.1

- Add better Windows support (contributor: Jeff Mitchell <crusty.rustacean@gmail.com>)
  - Fix Kitty keyboard startup failure
  - Git Bash resolution
  - Browser version probe

## 2026-08-27 v0.112.0

- Add keybind `gci` in normal mode to "isolate" the selected chat entry.
  - The motivating use-case for this is planning -> isolate the approved plan -> implement with fresh context to maintain lifecycle.
- Changed context assembly order for system prompt.

## 2026-08-27 v0.111.0

- Add user filter for Discord bot usage. The bot will only respond to users listed in the TOML.
  - This is a minimal implementation, there is no role filtering or allow-all.

```toml
# jinn.toml
[discord]
enabled = true              # Whether the Discord bot is active.
guild_id = "123"            # Discord guild (server) ID the bot operates in.
forum_channel = "456"       # Forum channel where the bot creates session threads.
authorized_users = ["789"]  # Users allowed to interact with the bot.
```

## 2026-08-26 v0.110.0

- Move citation tracking into plugin.
  - Add support for tracking citations via ZAI `web-search-prime` MCP tool
  - Add support for tracking citations using `web-search` + `web-fetch` tool
  - Add support for tracking citations using `openrouter:web-search` tool
- Disabling an MCP server now removes it's associated tools from the session tool listing.
- Add plugin status screen under `<leader>sP`.
- Cache indicator is now colorized based on cache hit rate.
  - <90%: red
  - 90%-94%: yellow
  - 95%+: green
- Configuration update for to allow adjustment of default tools + skills + MCP servers for sessions.
- MCP servers can now use custom headers.

### TOML Config Update

The behavior of tools and skills is "enable everything" by default for every session. Configuration options have been added to disable specific tools and skills. These apply to all new sessions.

All MCP servers are disabled by default. There is now an additional `auto_enable` flag on the MCP server configuration block which will start the MCP server on every new session.

```toml
# list tools to be disabled by default
disabled_tools = ["web-search", "web-fetch", "openrouter:web_search"]

# list skills to be disabled by default
disabled_skills = ["foo", "bar"]

# MCP autostart example
[mcp_server.parallel-search]
transport = "remote_http"
url = "https://search.parallel.ai/mcp"
# Defaults to false, but when true: MCP server will start up on every session
auto_enable = true
```

### MCP Custom Headers

Many remote MCP servers require an API key. `jinn` now supports adding custom headers with environment variable token substitution.

```toml
[mcp_server.web-search-prime]
transport = "remote_http"
url = "https://api.z.ai/api/mcp/web_search_prime/mcp"

# Headers specified in another block
[mcp_server.web-search-prime.headers]
# `${FOO}` expands to an environment variable named FOO
Authorization = "Bearer ${ZAI_API_KEY}"
```

## 2026-08-25 v0.109.0

- Group tool calls atomically to prevent malformed chat history construction.
  - A side-effect of this change is that pins and context exclusion now operate on multiple entries as a group.

## 2026-08-20 v0.108.4

- Add filters to prevent invalid message sequencing being sent to providers.

## 2026-08-20 v0.108.3

- Switched tools + skills backing data to a `BTreeMap` to prevent future cache issues.

## 2026-08-19 v0.108.2

- Skill construction no longer busts cache.

## 2026-08-18 v0.108.1

- Tool construction no longer busts cache.

## 2026-08-18 v0.108.0

- `jinn install` now installs builtin plugins
- Removed hashline implementation.
- Updated read/write/edit tools to use more common harness schemas and tool descriptions.

## 2026-08-17 v0.107.0

- `jinn` will immediately exit on startup if `jinn.toml` or `providers.toml` is malformed.
- Yanking from chat history tool results will now return a complete JSON object instead of the displayed text. This is to enable piping into `jq` for processing.
- Bugfix: Tool output should no longer leak and overwrite the TUI.
- Experimental plugin system added. Plugins are written in Rust and compiled to WASM. See the jinn-plugin skill for more information.

## 2026-08-15 v0.106.0

- Add `[[providers.model_info]]` tables to `providers.toml`: per-model `context_length`, `input_modalities`, and `extra_body` overrides. Hand-authored values take precedence over API-discovered data and models.dev. Models that are only listed in `providers.toml` (never discovered) now appear in the model cache, so the status bar, compaction gate, and attachment gate resolve them; `input_modalities = ["text", "image"]` marks a local model vision-capable.
- Update learning-tutor persona.
- Skill preview rendering now uses a shared cache across all sessions.
- **Breaking:** `providers.toml` providers are now map-keyed tables (`[providers.<name>]`) instead of `[[providers]]` array-of-tables.

### Map-Keyed Providers

Nested tables are now part of the key path, so a nested table's provider is self-describing: `[providers.zai.extra_body]` and `[[providers.zai.model_info]]` unambiguously belong to zai instead of relying on which `[[providers]]` block came before them. Duplicate provider names are now rejected by TOML itself, and file order carries no meaning. Two things to know when converting an existing file: dotted names like `llama.cpp` are no longer valid as table keys (rename to something dot-free, e.g. `llamacpp`), and legacy files fail to load with an error naming the new syntax — the conversion is renaming each block header to `[providers.<its name>]` and deleting the `name =` key inside it.

```toml
# example
[providers.llamacpp]
backend = "openai"
requires_key = false
base_url = "http://127.0.0.1:8089/v1"
models = [
    "/path/to/model.gguf",
    "/foo/bar.gguf
]

[[providers.llamacpp.model_info]]
id = "/path/to/model.gguf"
context_length = 96000
input_modalities = ["text", "image"]

# the `bar.gguf` model is not listed, so it gets application defaults (unknown context + text-only modality)
```

## 2026-08-07 v0.105.0

- Add `F` keybind in Normal mode to create a _new_ session from an existing User or Assistant message _without_ existing history. Only the selected message will be present in the new session. The new session is not counted as a fork of the previous session since no history is preserved.
- Add `--dump-requests` debugging flag to get raw output of everything sent to a provider.
- Migrations should no longer fail if interrupted.
- Migrations performance improvement (single transaction).

## 2026-08-03 v0.104.1

- Simpler MCP configuration + added MCP configuration example

## 2026-08-03 v0.104.0

- MCP server sidebar entry only shows when MCP servers are enabled. Also uses consistent padding (1 cell top + 1 cell bottom).
- Uploaded tokens indicator now uses provider-returned value, falling back to local calculation.
- Cached tokens are now displayed (w/hexagon icon) next to uploaded tokens indicator as a percentage.
- Reasoning effort is now displayed upon starting `jinn`.

### OpenRouter Provider Selection

OpenRouter endpoints can now be selected using `<leader>sE` and selection is persisted per session. When using OpenRouter it's recommended to manually select a provider in order to maximize prompt cache pricing. Default behavior is unchanged and uses your OpenRouter account configuration for endpoints.

## 2026-07-27 v0.103.0

- `@` behavior now allows sending a message if the attachment cannot be found. Missing files are highlighted in red.
- `@` popup now scrolls.

## 2026-07-25 v0.102.0

- Fix slow startup performance on `install` and `config` commands.
- Add `--force` flag to `install` command to overwrite existing files.
- Fix positioning of file selection and prompt selection popups to be word-wrap aware. They now appear directly over the cursor instead of above the input box.
- Provide base directory in context for loaded skills to help agent load reference files.
- Add MCP server support.
- "Nag" system defaults changed: 200 chat entries + disabled
- Headed Chrome/Chromium instance restarts on connection lost. This happens if a `web_fetch` or `web_search` request hasn't happened for a while.

## 2026-07-23 v0.101.0

- Add ability to load skills directly via the skill picker using `<c-l>`.
- Skills that have been loaded can no longer be disabled from the skill picker.

### New Feature: Project Record

A new "record" file can be placed at `./agents/RECORD.md` which lists out current high-level facts about the project.

The idea behind the record is that it always gets managed by a human and gets surfaced during every feature change. Humans can easily read and edit the record, and the agent has been instructed to only write approved edits to the record. The agent loads it and so gains an understanding of how things are _supposed_ to work which should (in theory) speed up codebase exploration during planning. It also helps (but doesn't eliminate) the problem of docs drifting from the actual implementation since the record gets referenced regularly.

For example, a fact might be "The database backend is SQLite so the application can be ran easily standalone" and a new feature request might be "Add Postgres support". This pre-existing fact will get surfaced during planning so the developer can be made aware of potential implications of the feature.

The implementation is prompt-based and all relevant planning and analysis prompts have been updated to support managing the record. Note that this takes slightly more tokens since extra work is being performed. But the token cost shouldn't be significant since the agent will already have the relevant code loaded into context during planning and evaluation.

Currently its just a markdown file. As I experiment with using it, it might get changed to a vector search or some other lower-context mechanism.

## 2026-07-21 v0.100.0

- New "nag" system reminds agent to use the `todo_*` tools if they haven't done so after 100 chat entries.
- Add `cargo binstall` support

## 2026-07-21 v0.97.0

- Discord integration now allows selection of lifecycle script
