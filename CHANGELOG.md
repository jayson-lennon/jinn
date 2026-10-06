**(Note to agents: CHANGELOG.md is human-authored only. Do not make edits)**

## 2026-10-05 v1.5.0

- Display bug fix from v1.4.0 applied generically so all tool calls should display properly.
- Canceling via ESC ESC should now work properly under all circumstances.
- Remove `command_policy` and replaced with `stream_rules`. The new rules can trigger on anything instead of just tool calls.

```toml
# old
[[projects]]
path = "/project/path"
command_policy = [{ pattern = 'cargo\s+(test|t)\b.*\s-p\b', message = "Do not run tests on individual packages. Use `just test` or `cargo test --workspace` as indicated in AGENTS.md." }]

# new
[[stream_rules.entry]]
name = 'no-bare-find-home'
description = 'a bare find at the home root walks everything'

# trigger condition
conditions = ['\bfind\s+~(\s|$)']

# where to apply (text, thinking, tool)
scopes = ['tool:bash']

# what to do when condition is met.
# Default behavior interrupts the stream.
# `fail_tool` will cause the interrupted tool call to be failed (only applies
# for tool calls).
on_trigger = 'fail_tool'

# message sent to LLM
body = 'A bare find at the home root walks your entire home directory, including caches, build trees, and dotfile folders. Search a bounded path instead: find under the project directory with `-name`, or use `rg --files -g <glob>`. If you truly need a home-wide search, scope it with `-maxdepth` and prune build directories.'

# optional project-specific rule
project = "/glob/here"
```

## 2026-10-02 v1.4.0

- Fix display bug with `write` tool. It should now display immediately as tokens stream in.
- Attendants can now be canceled from an idle session.

## 2026-10-02 v1.3.0

- Agents should be less likely to use the `interactive_term` `max_duration_seconds` parameter as a sleep function.
- Paste now works properly across all input boxes.
- Attendant model configuration can now be edited in the property panel.
- Add work timer to session that shows total wall time that a session was processing.
  - Aggregate wall time is calculated and deduplicated across the entire tree, so it shows the total wall time for the whole tree, _not_ the sum of the individual sessions.
- Bugfix: Attendants can now re-trigger infinitely.
- Bugfix: Stall watchdog broke in the v1 migration and has been fixed.
- Improved rendering performance while streaming formatted LLM responses containing code blocks.
- Attendant picker UX changes:
  - `<c-a>` keybind will attach an attendant and keep the picker open
  - `<enter>` now keeps the current session focused instead of switching to the attendant

## 2026-09-30 v1.2.1

- Embedded resources meant to ship in v1.2.0 are now included.
  - Changed internal architecture to embed by directory instead of individual files.

## 2026-09-30 v1.2.0

- OpenRouter endpoint selection is now persisted automatically to `providers.toml`.
- Bugfix: `gci` now works when a pinned entry is selected.
- Session preview will no longer obscure the session selection cursor on small terminals.
- Session preview border is now colorized based on session type:
  - Regular session: gray
  - Subagent: purple
  - Attendant: pink
- Subagent and attendant sessions now display a colored indicator in the bottom right corner of the chat history section.
- The sidebar selection cursor now spans the entire width of the sidebar
- Canceling a session now recursively cancels child subagents and attendants.
  - The cascade skips child _forks_ under the assumption that forked sessions are now operating independently.
- Sessions that are cancelled or end in an error use a red background in the sidebar instead of a red foreground.
- Add attendants. See README for details.
- Add new builtin prompts:
  - `diverge`: find new approaches to a problem
  - `rank`: determine which is best from a set
  - `judge`: determine if one thing is any good
  - `falsify`: find the ways something goes wrong
- `jinn.toml` now supports allow or deny list for tools and skills, replacing the `disabled` key:

```toml
[tools]
# NO LONGER WORKS!
# disabled = ["..."]

# "deny" mode works the same as the previous "disabled" key. The `grep` tool will be disabled in all new sessions.
tool_filter = { mode = "deny", names = ["grep"] }

[skills]
# "allow" mode is an allow list. All tools are disabled except for the ones listed
skill_filter = { mode = "allow", names = ["read"] }
```

## 2026-09-27 v1.1.2

- Add a new "extending jinn" document. You should now be able to ask `jinn` to configure and extend itself.
  - There isn't a large customization surface, but the agent should be able to guide you to a solution.

## 2026-09-27 v1.1.1

- Todo tooling completely changed to reduce context tool block size and to more closely align with existing Claude + Codex tools.
  - `todo_set_list`: rewrites the entire todo list. Added a bunch of aliases for task states.
  - `todo_set_phase`: rewrites a single phase. Added a bunch of aliases for task states.
  - `todo_get_list`: returns the entire todo list
- Task loop skills reference `todo_*` tools generically instead of by exact name.
- The `#approve-plan` prompt should produce more detailed todo lists.
- The `RECORD.md` template has been updated, so records should be more durable across changes. This change shipped in the `#plan` prompt.
- Added a new `jinn-usage` skill.
  - This enables the agent to answer meta-questions about `jinn` usage and configuration. `jinn` should now be able to edit it's configuration on your behalf.
- Remove `jinn-plugin` skill. It will need to be manually removed from your `~/.agent/skills` directory.
- `interactive_*` family of tools and related keybinds now use session IDs instead of relying on current UI state.
  - Fix: now impossible to send keystrokes to the wrong interactive session
  - Fix: agent `interactive_*` invocations now go to the correct sessions even when forked
- Added guidelines to `interactive_term` instructing agents not to redirect it's output.
- The `#gap-analysis` prompt should produce more concise reports.
- Remove `anchor_shield` context worker.
- Add `protect_latest` option for the `[auto_prune.todo]` context worker. Setting to `true` (default) will keep the latest `todo_*` tool result in context indefinitely and should help keep agents on task.
- Fix context undercounting bug in status bar.
  - This only impacts the `<percent>/<total>` (`5%/1M`) display. It was not counting tool results nor system prompts which could lead to 10%+ undercounting.
  - Count now includes tool results, tool guidelines, tool schemas.
- Steering/queue ordering behaves as intended.
  - Previously the queue buffer would get dumped at the end of a turn even if there were messages in the steering buffer. This would occur in situations where both the queue buffer and steering buffer had messages, but the agent was already on their last message (hence no time to "steer" it). Now, anything in the steering buffer gets dumped first regardless of the state of the agent turn.
- `task` (subagent) tool changes:
  - Updated instructions to encourage concurrent sessions and discourage individual sessions.
  - Todo list no longer longer propagates subagent sessions.
  - `max_duration_seconds` is no longer presented to the model, but will still be accepted and applied if provided. This change was made because its not always clear how long a subagent task will take, and ending it prematurely throws away all the work.
- Active sessions are now progressively loaded and a spinner was added to the sidebar to indicate when sessions are loading.
- Arrows keys + a few non-printable keys now work properly in interactive terms.
  - `jinn` used codes that didn't match TERMINFO, so some applications wouldn't properly register non-printable keys.
- Overall performance improvement on Markdown rendering.
  - Chat log, session previews, and skill picker rendering is faster and uses significantly less memory.
  - There is no longer UI stuttering on gigantic sessions.
- There are now spinners while a session is being loaded.
- The sidebar is now presented as one large scrollable area that keeps the cursor bounded.
  - Removed the scrolling capability of the sessions since now the entire sidebar scrolls.
- The `jinn.toml` file was restructured dramatically. Recommend moving your existing `jinn.toml` to `jinn.toml.bak` and then running `jinn install` to get a new version.
- Initial application loading responsiveness has been improved.
- Session archival operation is now visualized in the sidebar by a gray background and distinct spinner.
- Added a new `--config` flag to load a specific `jinn.toml` file.
  - Passing `--config` to `jinn config init` will initialize the template to the provided path instead of `<config dir>/jinn`
- Add new `goal` prompt that autonomously works towards the planned goal, working similarly to the `plan` prompt. See the README for usage instructions.
- Add `/export [path]` command to export a chat.
  - Formats supported: `html`, `md`
- Dashboard changes:
  - Scrolling now pivots around the cursor
  - Actors are sorted by status and by notes
  - Notes are colorized based on severity

- These plugins were moved into the core in preparation for 1.0 release. They are now unused and will remain on-disk unless you manually delete them. Please see the next section on plugin-related TOML configuration changes.
  - Deleted `persona-loader`
  - Deleted `theme-loader`
  - Deleted `stall-watchdog`
  - Deleted `tool-call-watchdog`
  - Deleted `url-citations`

### TOML plugin-specific configuration changes

**theme-loader** is now always active. The TOML configuration is now ignored and should be deleted:

```toml
# (DELETE THIS)
[plugin.theme-loader]
wasm = "theme-loader.wasm"
enabled = true
```

**persona-loader** is now always active. The TOML configuration is now ignored and should be deleted:

```toml
# (DELETE THIS)
[plugin.persona-loader]
wasm = "persona-loader.wasm"
enabled = true
```

**stall-watchdog** is now always active. The TOML configuration CHANGED:

```toml
# (DELETE THIS)
[plugin.stall-watchdog]
wasm = "stall-watchdog.wasm"
enabled = true

# (USE THIS NOW)
[stall_watchdog]        # behavior config now lives here
# amount of time to wait for a response from a provider before restarting the stream
timeout_secs = 60
# number of restarts before aborting the stream
max_restarts = 3
```

**tool-call-watchdog** is now always active. The TOML configuration CHANGED:

```toml
# (DELETE THIS)
[plugin.tool-call-watchdog]
wasm = "tool-call-watchdog.wasm"
enabled = true

# (USE THIS NOW)
[tool_call_watchdog]    # behavior config now lives here
# stream will be terminated if there are this many tool call failures in a short window of time
max_failures = 4
```

**url-citations** is now always active. The TOML configuration is now ignored and should be deleted:

```toml
# (DELETE THIS)
[plugin.url-citations]
wasm = "url-citations.wasm"
enabled = true
```

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
