# Configuration

When a user asks for behavior jinn controls via configuration, your job is:
**explain the mechanism → show a relevant snippet → offer to apply it → note
the restart requirement.** Do not edit config files unless the user accepts
the offer.

## File map

| File             | Location                          | Contents                                                                                                                                                                           |
| ---------------- | --------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `jinn.toml`      | `~/.config/jinn/jinn.toml`        | User preferences: tools/skills defaults, session lifecycles, projects, MCP servers, compaction, auto-prune, stream rules, web fetch/search, browser, Discord, interactive terminal, remote surface |
| `providers.toml` | `~/.config/jinn/providers.toml`   | Providers, API keys, base URLs, per-model metadata                                                                                                                                 |
| themes           | `~/.config/jinn/themes/*.toml`    | Color themes (picked with `<leader>sh`)                                                                                                                                            |
| personas         | `~/.config/jinn/personas/*.md`    | Persona templates (markdown + TOML frontmatter)                                                                                                                                    |
| prompts          | `~/.config/jinn/prompts/*.md`     | Prompt templates (`#name` tokens in input)                                                                                                                                         |
| skills           | `~/.agents/skills/*/SKILL.md`     | Agent skills (project `.agents/skills/` override same-named globals)                                                                                                               |
| sessions         | `~/.local/share/jinn/sessions.db` | SQLite — sessions, history, search index (do not hand-edit)                                                                                                                        |

`jinn.toml` is auto-created with a fully commented default on first run; your
comments are preserved across saves (jinn patches documents instead of
rewriting them). Unknown keys survive upgrades — an older jinn ignores newer
keys instead of failing.

## The restart rule

**Every `jinn.toml` / `providers.toml` change takes effect on the next jinn
launch.** There is no live reload. After applying a config edit, tell the
user to quit (`q`) and relaunch. Per-session choices made in the UI (enabled
tools/skills/MCP servers, model, persona) persist in the session database and
do _not_ need a restart or config edit.

One choice is a config write rather than session state: the OpenRouter
endpoint pin made in the endpoint picker writes a `[[endpoint_defaults]]` row
to `providers.toml`, so it is live immediately (no restart) but is keyed by
model and applies to every session using that model. Hand-editing that row,
by contrast, still needs a restart.

One exception inside `jinn.toml` itself: the `[[bot.entry]]` list is
re-read every time the saved-bots picker (`<leader>sb`) opens, so
adding or editing a saved bot takes effect immediately. The shipped
default file ships one such entry (`auto-nudge`). Everything else in
`jinn.toml` follows the restart rule above.

## Offer-to-edit protocol

When the user's ask maps to a config change:

1. Name the file and section, and explain what the setting does in one or two
   sentences.
2. Show a minimal snippet reflecting their ask (values they'd actually want —
   not the doc example verbatim).
3. Ask: _"Want me to update `~/.config/jinn/jinn.toml` for you? You'll need
   to restart jinn for it to take effect."_
4. On acceptance, read the file first, apply the change surgically (preserve
   comments, ordering, and unknown keys), and confirm what you changed and
   where.
5. Remind them of the restart.

If a jinn **config subcommand** exists for the ask (e.g. `jinn install`),
prefer the command over hand-editing.

## Common asks → settings

**Disable a tool or skill by default**

```toml
[tools]
tool_filter = { mode = "deny", names = ["grep", "openrouter:web_search", "mcp__context7__lookup"] }

[skills]
skill_filter = { mode = "deny", names = ["svg-creator"] }
```

`mode = "deny"` withholds every name that matches and permits everything else;
`mode = "allow"` permits **only** the names that match. Patterns are **globs**
over the namespaced name, so one entry covers a whole MCP server
(`mcp__github__*`) without naming each of its tools. An empty `names` list in
an `allow` filter means "this session may use nothing" — different from
omitting the filter entirely.

Both shipped default tables are in `deny` mode: `[tools]` denies `grep`;
the shipped `[skills]` filter uses `deny` the same way.

Entries are **tool names**, not display labels: a builtin is its bare name
(`bash`, `grep`, `write`, `spawn_subagent`, `skill`, …), a provider-side tool keeps its
namespace (`openrouter:web_search`), and an MCP tool is its full namespaced
name (`mcp__<server>__<tool>`). A name that matches nothing has no effect.

New sessions start with these filters applied; per-session toggles
(`<leader>st`, `<leader>sk`) override and persist in the session, never
writing back here. The same two filter keys also exist per saved bot
(see `bots.md`).

**Timeouts / output caps**

```toml
[tools]
default_timeout_secs = 300       # safety ceiling for all builtin tools
max_output_lines = 2000          # what the model sees per tool call
max_output_bytes = 51200

[chat_log]
tool_entry_max_lines = 12        # how much of a tool call renders in the TUI
```

**Session lifecycles** (branch/worktree bootstrap; see
`sessions-and-subagents.md`)

```toml
[[session_lifecycle.script]]
name = "git worktree"
description = "Open a git worktree + branch"
setup_command = "cd <repo> && git worktree add -b <branch> ../<branch> && echo $(pwd)/<branch>"
teardown_command = "..."
```

**Curated projects** (appear in the `<leader>sp` picker):

```toml
[[project.entry]]
path = "~/code/myapp"
```

A project's own rules are stream rules (see **Stream rules** below) carrying
its path in their `project` field; they are configured alongside every other
rule rather than on the project entry.

**Saved bots** (`[[bot.entry]]` — see `bots.md` for the
full field reference and the feature itself):

```toml
[[bot.entry]]
name = "reviewer"
trigger = "parent_completed"
behavior = "reset"
prep_mode = false
seed_template = "The parent session's id is <parent_id>. The previous run of this bot reported: <prior_report>."
model = { single = "openrouter/deepseek/deepseek-v4.1-flash" }
tool_filter = { mode = "allow", names = ["read", "bash", "session_fetch", "session_search"] }
pins = [{ role = "user", text = "Review the parent's last turn and report one line." }]
```

`name` is required; every other field you omit is inherited from the session
the bot is created under. **`prep_mode` defaults to `true`**, so an
entry that omits it does not run until you set `prep_mode = false`. Pin
entries must carry a `role` (`"user"` or `"assistant"`) — a pin without one
fails to parse.

This list is the one `jinn.toml` surface read **live**: the saved-bots
picker (`<leader>sb`) re-reads the document every time it opens, so a hand edit
shows up without a restart. `name` is the key entries are matched by, so
saving under an existing name replaces that entry in place.

**Blocking a command before it runs** is a stream rule (see **Stream rules**
below) with a tool scope:

```toml
[[stream_rules.entry]]
name = 'no-force-push'
description = 'force-pushing main rewrites published history'
conditions = ['git push\s+.*--force']
scopes = ['tool:bash']
body = 'Force-pushing main rewrites published history — open a PR instead.'
```

A match interrupts the response as the arguments stream, so the call is never
dispatched and the command never runs. `body` comes back as guidance, and the
agent sees the abandoned call, explained, in the resumed request. It works for
any tool you name a scope for — not bash alone. `jinn.toml` rules append after
the base set (see **Stream rules** below for the shipped file and its
replace/append semantics).

> The older `[[tools.bash_command_policy]]` section is read at load time and
> converted to exactly this form, carrying its pattern and message across, so
> an existing file keeps working. `[[project.entry]].command_policy` becomes a
> rule scoped with `project`. A file still carrying the old `on_trigger` key is
> read fine — the key is ignored and the rule interrupts as normal.

A fresh install ships a whole set of command guards in an embedded
`stream_rules.toml` (compiled into the binary). That file is the base rule set:
filesystem destruction (`rm` aimed at a root, home, variable, or glob; `mkfs`,
`dd if=`, `shred`, `truncate -s`, `wipefs`, `fdisk`/`diskutil` on a device,
redirects into root-owned paths), destructive git (force-push, `clean -f`,
reflog expire, `gc --prune`, history rewrite, stash drop), publishing and cloud
destruction (`npm publish`/`unpublish`, `gh repo delete`, `gcloud`/`aws`/
`terraform destroy`/`pulumi`/`kubectl` broad deletes, Heroku/Vercel/fly
destroys, docker volume removal and `compose down -v`), database wipes (`DROP
DATABASE/TABLE`, `TRUNCATE`, `DELETE FROM` with no `WHERE`, `FLUSHALL`/
`FLUSHDB`, `dropdb`/`dropuser`), machine state (`shutdown`, `reboot`, crontab
replacement), credential and privacy reads (macOS keychain reads/dumps, shell
history files), machine-hangers (interactive editors, pagers, `tail -f`,
`gdb`, which would block the tool call until timeout), and pipe-to-shell.

The shipped guards target commands an agent emits **by accident** while doing
ordinary work, where running it once would destroy data, leak private
material, or hang the machine. Deliberate-but-destructive operations a
competent agent runs legitimately — `git reset --hard`, `git branch -D`,
`chmod -R`, `docker system prune`, `make`, a plain `rm -rf` of a named build
directory — are deliberately absent, because interrupting them routinely
would make jinn annoying to use.

`jinn install` and `jinn config init` each write that file, once, as
**`stream_rules.toml` beside `jinn.toml`**, so you can edit it:

- An existing `stream_rules.toml` **replaces the embedded set entirely**.
  An empty file therefore disables every shipped guard — that is the
  intended off switch. `jinn install` never modifies an existing one (even
  with `--force`); `jinn config init --force` overwrites it with the current
  shipped set, which is the supported path for pulling in updated guards.
- A malformed user file warns and falls back to the embedded set, so a typo
  never leaves jinn unguarded.
- `[[stream_rules.entry]]` rules in `jinn.toml` are **appended after** the
  base set, so your own rules layer on top of whichever base won.

The shipped entries also carry `positive` and `negative` fixture lists (bare
command strings that must / must not trip the declaring rule) and a
source/license comment each — jinn ignores those fields at runtime; they drive
the test suite. Each entry names its upstream source: the shipped defaults
this file grew out of, dylancaponi/claude-code-permissions,
openai/codex (Apache-2.0), SWE-agent/SWE-agent (MIT), and the command-line
folklore canon.

**MCP servers** — see `mcp-servers.md` for the full transport matrix:

```toml
[mcp.context7]
command = "npx"
args = ["@context7/mcp-server", "--stdio"]
auto_enable = true
```

**Compaction** (a backstop for full contexts — rarely fires during normal
coding):

```toml
[context_curation.compaction]
threshold = 0.7                      # usage fraction that triggers compaction
reserve_tokens = 20000               # recent history kept during compaction
fallback_context_window = 150000     # used when the provider doesn't report one
# model = "openrouter/anthropic/claude-sonnet-4"   # summarizer model
```

**Auto-prune** (context trimming workers; each has `enabled` + `min_age` and
its own thresholds):

```toml
[context_curation.auto_prune]
accumulation_threshold_tokens = 150000   # batch context-mutations to protect prefix cache

[context_curation.auto_prune.regex]
enabled = true
[[context_curation.auto_prune.regex.rules]]
pattern = "cargo test"
tool_name = "bash"
keep_last = 2
min_age = 50
```

Strategies include `edit_read`, `read_edit`, `double_edit`,
`consecutive_reads`, `tool_age_window`, `trivial_assistant`,
`anchored_assistant`, `broken_edit`, `todo`, and
`regex` — all documented with comments in the default `jinn.toml`. Each is a
`[context_curation.auto_prune.<name>]` table taking `enabled` and `min_age`;
the ones with extra tuning are listed under "Auto-prune per-strategy knobs"
below. `[context_curation.auto_prune.broken_edit]`,
`[context_curation.auto_prune.edit_read]`, and
`[context_curation.auto_prune.tool_age_window]` have no further settings.

**Web search tuning** (the provider-side `openrouter:web_search` tool):

```toml
[provider.web_search]
engine = "exa"          # "exa" | "firecrawl" | "parallel" | "native" | "auto"
# max_results = 5       # per search (provider accepts 1-25)
# max_total_results = 20        # cap across searches in one request
# search_context_size = "medium"  # "low" | "medium" | "high"; unset = adaptive
# allowed_domains = ["docs.example.com"]
# excluded_domains = ["pinterest.com"]
```

jinn passes these values straight through to the provider and validates
none of them — a typo'd `engine` or an out-of-range `max_results` is forwarded
as written and the provider decides what to do with it.

**Interactive terminal** (see `terminal-overlay.md`):

```toml
[term]
control_toggle_key = "<c-g>"         # any keybind-notation key, e.g. "<m-g>"
settle_quiet_ms = 400
settle_max_wait_ms = 3000
```

**Minimap** (the chat-log token-density map):

```toml
[ui.minimap]
max_tokens = 2000     # entries at/above this size always render lightest
```

**CWD picker command** (backs `<M-c>`/`<M-d>`; any fuzzy finder works):

```toml
[ui.cwd_selector]
command = "find -L {path} -type d 2>/dev/null | fzf --no-multi"
# `{path}` is replaced with the start dir; must print one absolute path.
```

**Chat-log rendering caps:**

```toml
[chat_log]
tool_entry_max_lines = 12     # lines of a tool call/result before truncation
min_collapse_count = 5        # smallest collapsed run of excluded entries
```

**Discord bot** (slice-owned section; see also `sessions-and-subagents.md`):

```toml
[discord]
enabled = false                    # with a token, runs a bot beside the TUI
# bot_token = "..."                # or DISCORD_BOT_TOKEN env var
# guild_id = "..."                 # optional server restriction
# forum_channel = "..."            # optional forum channel for threads
authorized_users = []              # deny-by-default; empty authorizes nobody
```

**Auto-prune per-strategy knobs** (every strategy takes `enabled` and
`min_age`; the ones with extra tuning):

```toml
[context_curation.auto_prune.double_edit]
max_file_edits = 2        # writes kept per file

[context_curation.auto_prune.consecutive_reads]
keep_last = 5             # reads kept per file

[context_curation.auto_prune.read_edit]
threshold = 2             # edits/writes before the earlier read goes stale

[context_curation.auto_prune.regex.rules]
keep_last = 2             # matching calls kept

[context_curation.auto_prune.todo]
protect_latest = true     # keep the most recent todo_* loop only

[context_curation.auto_prune.trivial_assistant]
max_tokens = 80           # "trivial" size threshold

[context_curation.auto_prune.anchored_assistant]
radius = 20               # entries near user-message anchors kept
```

**Request retries:**

```toml
[provider.request_retry]
max_retries = 5
base_delay_secs = 2
max_delay_secs = 60
```

**Remote control surface** (see `remote.md` for the CLI, the HTTP routes,
and curl examples — `jinn remote send|instances`):

```toml
[remote]
# bind = "127.0.0.1:0"   # default: loopback, OS-assigned ephemeral port
```

A non-loopback bind refuses the launch — the surface has no
authentication. The resolved port is what `jinn remote instances`
reports.

**Watchdogs** (turn health — these cancel a turn that has gone bad):

```toml
[watchdog.stall]
timeout_secs = 60     # seconds of stream silence before a stalled turn retries
max_restarts = 3      # consecutive silent-stall retries before the turn cancels

[watchdog.tool_call]
max_failures = 4      # tool failures before the turn cancels
```

`max_failures` uses a simple accumulator that rises on failure and falls on
success, so the failures need not be consecutive.

**Stream rules** (regex over the assistant's output as it is produced — the
only rule kind that can act mid-response):

```toml
[[stream_rules.entry]]
name = "ts-no-any"                  # unique; identifies the rule in logs
description = "Never widen a type to `any`"
conditions = [': any', '\bas any\b']   # regexes; first match trips the rule
scopes = ["tool:edit(*.ts)", "tool:write(*.tsx)"]
body = """
Use `unknown`, a domain type, or a type guard instead.
Never widen a type to `any` to silence an error.
"""
```

When a `conditions` regex matches the output so far, jinn stops the turn
_before_ that text reaches the chat log and resumes it with `body` injected
as guidance, so the model corrects itself instead of the user typing at
it. The interrupted text stays visible in the log but is excluded from the
resumed request, so the model does not see its own violation as context.

`scopes` decides which streams the rule is tested against:

| Scope token       | Matches                            |
| ----------------- | ---------------------------------- |
| `text`            | assistant prose                    |
| `thinking`        | reasoning output                   |
| `tool`            | any tool's serialized arguments    |
| `tool:edit(*.ts)` | `edit` calls touching a `.ts` file |

Omit `scopes` (or leave it empty) to test prose, reasoning, and tool
arguments alike. Prefer a narrow scope when the rule is about one medium — a
rule about TypeScript types belongs on tool arguments, where the source text
actually appears.

A `tool:<name>(<glob>)` token matches when the tool name is equal and any
path-like argument matches the glob. The glob decides **which files** the
rule applies to; `conditions` then tests the whole argument buffer, content
included. So `tool:edit(*.ts)` with `conditions = [': any']` fires on an edit
that writes `any` into a `.ts` file, and stays quiet for a `.py` file.

Which arguments count as paths: values under a key ending in `path`, `file`,
`dir`, or `pattern` — `edit` uses `file_path`, `read` and `write` use `path`.
The glob is tried against both the full path and the bare basename, so `*.ts`
matches `src/deep/nested/x.ts`.

A match does one thing: it interrupts the turn and resumes it with `body` as
guidance. There is no second behavior to configure. A rule scoped to
`tool:<name>` matches that call's arguments as they arrive, and interrupting
that call is what stops it running — so blocking a command before it executes
uses the same mechanism as catching a model mid-sentence.

When a rule catches a tool call, the call is paired with a result saying it did
not run and quoting the arguments the model had produced, so the model reads
its own failed attempt instead of resuming and re-emitting it.

**`[watchdog.stream_rules]`** tunes how many interrupts a session may take in a
row:

```toml
[watchdog.stream_rules]
max_failures = 4
```

Consecutive, per session, default 4. A response that completes without an
interrupt refunds one (floored at zero); an interrupted one does not, so
repeats accumulate across a turn. A value of 4 means four interrupts are
allowed and the **fifth** consecutive one cancels the stream — otherwise a
rule whose condition also matches its own injected guidance would loop
forever. A value below 1 is treated as 1.

The budget is owned by the stream-rule watchdog rather than by the matcher, so
the trip happens after the interrupting response resumes. A `jinn.toml` still
carrying the old `[stream_rules] max_interrupts` keeps its value for that
launch, logs a warning naming the move, and is never rewritten.

A rule that fails to compile, names no reachable stream, or has an empty `body`
is logged and skipped rather than breaking a turn. Use single-quoted strings for
`conditions` so regex metacharacters survive without escape processing.

Note this is distinct from `[[context_curation.auto_prune.regex.rules]]`,
which prunes completed history. A stream rule is the only one that observes
output as it is produced.

`project` is optional: a path glob selecting the sessions a rule applies in.
Absent means every project, and a project-scoped rule can add to a global
rule but never remove one.

## Coverage note

Every section in the shipped default `jinn.toml` is represented above or in a
linked reference (`mcp-servers.md`, `terminal-overlay.md`,
`context-management.md`, `sessions-and-subagents.md`, `bots.md`,
`remote.md`).
Section names are exact — `[term]`, not `[interactive_term]`; `[mcp.<name>]`,
not `[[mcp_server]]`; `[context_curation.compaction]`, not `[compaction]`.
A misspelled section is **silently ignored** (it reads as absent, not as an
error), so a typo'd key will not announce itself.

When an ask touches a
key not shown here, tell the user the full commented reference ships in the
auto-created `~/.config/jinn/jinn.toml` itself.

## Upgrades

Config keys moved under slice-owned umbrellas. An older `jinn.toml` is
not translated: jinn does not convert old keys, does not warn about them,
and does not migrate them. A file that still uses the pre-umbrella
spellings parses, and every section falls back to its default — so a
silent revert to defaults is the symptom to look for, not an error.
Check the user's file for the umbrellas listed above before
recommending an edit.

jinn reconciles the bundled themes, personas, prompts, workflows, and
skills on every launch: a file jinn wrote that a binary update made stale
is refreshed automatically, while a file that matches no jinn record is
treated as user-owned and never touched. `jinn install --builtins` is the
one command that overwrites user-edited bundled files with the shipped
copies (reclaiming them). `jinn install` without flags seeds only
`jinn.toml`, `providers.toml`, and `stream_rules.toml`, each only when
missing — it never modifies an existing file.
