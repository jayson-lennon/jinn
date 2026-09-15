# Configuration

When a user asks for behavior jinn controls via configuration, your job is:
**explain the mechanism → show a relevant snippet → offer to apply it → note
the restart requirement.** Do not edit config files unless the user accepts
the offer.

## File map

| File | Location | Contents |
| --- | --- | --- |
| `jinn.toml` | `~/.config/jinn/jinn.toml` | User preferences: tools/skills defaults, session lifecycles, projects, MCP servers, plugins, compaction, auto-prune, web fetch/search, browser, Discord, interactive terminal |
| `providers.toml` | `~/.config/jinn/providers.toml` | Providers, API keys, base URLs, per-model metadata |
| themes | `~/.config/jinn/themes/*.toml` | Color themes (picked with `<leader>sh`) |
| personas | `~/.config/jinn/personas/*.md` | Persona templates (markdown + TOML frontmatter) |
| prompts | `~/.config/jinn/prompts/*.md` | Prompt templates (`#name` tokens in input) |
| skills | `~/.agents/skills/*/SKILL.md` | Agent skills (project `.agents/skills/` override same-named globals) |
| sessions | `~/.local/share/jinn/sessions.db` | SQLite — sessions, history, search index (do not hand-edit) |

`jinn.toml` is auto-created with a fully commented default on first run; your
comments are preserved across saves (jinn patches documents instead of
rewriting them). Unknown keys survive upgrades — an older jinn ignores newer
keys rather than breaking.

## The restart rule

**Every `jinn.toml` / `providers.toml` change takes effect on the next jinn
launch.** There is no live reload. After applying a config edit, tell the
user to quit (`q`) and relaunch. Per-session choices made in the UI (enabled
tools/skills/MCP servers, model, persona) persist in the session database and
do *not* need a restart or config edit.

## Offer-to-edit protocol

When the user's ask maps to a config change:

1. Name the file and section, and explain what the setting does in one or two
   sentences.
2. Show a minimal snippet reflecting their ask (values they'd actually want —
   not the doc example verbatim).
3. Ask: *"Want me to update `~/.config/jinn/jinn.toml` for you? You'll need
   to restart jinn for it to take effect."*
4. On acceptance, read the file first, apply the change surgically (preserve
   comments, ordering, and unknown keys), and confirm what you changed and
   where.
5. Remind them of the restart.

If a jinn **config subcommand** exists for the ask (e.g. `jinn install`,
`jinn plugin add`), prefer the command over hand-editing.

## Common asks → settings

**Disable a tool or skill by default**
```toml
# Top of jinn.toml (before any [section]).
disabled_tools = ["web-search", "mcp__context7__lookup"]
disabled_skills = ["svg-creator"]
```
New sessions start with these disabled; per-session toggles (`<leader>st`,
`<leader>sk`) override and persist in the session, never writing back here.

**Timeouts / output caps**
```toml
tool_default_timeout_secs = 300      # safety ceiling for all builtin tools
max_tool_output_lines = 2000         # what the model sees per tool call
max_tool_output_bytes = 51200
tool_entry_max_lines = 12            # how much of a tool call renders in the TUI
```

**Session lifecycles** (branch/worktree bootstrap; see
`sessions-and-subagents.md`)
```toml
[[session_lifecycle]]
name = "git worktree"
description = "Open a git worktree + branch"
setup_command = "cd <repo> && git worktree add -b <branch> ../<branch> && echo $(pwd)/<branch>"
teardown_command = "..."
```

**Curated projects** (appear in the `<leader>so` picker) — optionally with a
command policy that blocks bash commands by regex inside that project:
```toml
[[projects]]
path = "~/code/myapp"
command_policy = [{ pattern = 'rm\s+-rf\s+/', message = "Never rm -rf from root here." }]
```

**MCP servers** — see `mcp-servers.md` for the full transport matrix:
```toml
[mcp_server.context7]
command = "npx"
args = ["@context7/mcp-server", "--stdio"]
auto_enable = true
```

**Compaction** (a backstop — should almost never fire while coding):
```toml
[compaction]
threshold = 0.7                      # usage fraction that triggers compaction
reserve_tokens = 20000               # recent history kept during compaction
fallback_context_window = 150000     # used when the provider doesn't report one
# model = "openrouter/anthropic/claude-sonnet-4"   # summarizer model
```

**Auto-prune** (context trimming workers; each has `enabled` + `min_age` and
its own thresholds):
```toml
[auto_prune]
accumulation_threshold_tokens = 150000   # batch context-mutations to protect prefix cache

[auto_prune.regex]
enabled = true
[[auto_prune.regex.rules]]
pattern = "cargo test"
tool_name = "bash"
keep_last = 2
min_age = 50
```
Strategies include `edit_read`, `read_edit`, `double_edit`,
`consecutive_reads`, `tool_age_window`, `trivial_assistant`,
`anchored_assistant`, `anchor_shield`, `broken_edit`, `todo`, and
`regex` — all documented with comments in the default `jinn.toml`.

**Web fetch / search** (browser-backed; needs Chrome/Chromium):
```toml
[web_fetch]
backend = "headless-chrome"          # "http" | "headless-chrome" | "headed-chrome"

[web_search]
backend = "http"                     # DuckDuckGo; switch backend if blocked

[browser]
binary = "auto"                      # "auto" | "chrome" | "chromium"
anubis_timeout_secs = 30
# challenge_wait_secs = 120          # headed mode: time to solve a challenge by hand
```
Headed Chrome keeps a visible window with persistent cookies — solve a
Cloudflare challenge once and it stays solved.

**Interactive terminal** (see `terminal-overlay.md`):
```toml
[interactive_term]
control_toggle_key = "<c-g>"         # any keybind-notation key, e.g. "<m-g>"
settle_quiet_ms = 400
settle_max_wait_ms = 3000
```

**Request retries:**
```toml
[request_retry]
max_retries = 5
base_delay_secs = 2
max_delay_secs = 60
```

## Upgrades

Users should run `jinn install --force` after updating jinn — it refreshes
bundled themes, personas, prompts, and skills (skipping files only when not
forced). jinn never modifies an existing `jinn.toml` on install.
