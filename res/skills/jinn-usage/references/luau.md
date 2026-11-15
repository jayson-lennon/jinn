# Luau: embedded scripting with `jinn luau`

jinn carries its own Lua interpreter — the vendored Luau VM — so a script can
run on a host with no python, node, jq, or perl, and with no jinn config,
session store, or running jinn instance. The `jinn luau` subcommand is
config-free: it never reads `jinn.toml`, never opens the database, and has no
`[luau]` config section. Every knob is a CLI flag on the invocation.

Scripts are ordinary Luau (a Lua 5.1 dialect with type annotations, string
interpolation, and compound assignment). Capabilities arrive through an
injected `jinn` table plus file-based `require` rooted at the script's
directory. This is **not a sandbox**: scripts can read and write files, spawn
processes, and make network requests. The guardrails (a wall-clock timeout
and a memory cap) exist to kill a runaway script an agent wrote by accident,
not to contain hostile code.

## Commands

| Command | What it does |
| --- | --- |
| `jinn luau run <script> [args…]` | Run a script file; trailing args become `jinn.args` |
| `jinn luau --eval '<code>'` | Evaluate a one-liner; its result prints |
| `jinn luau check <script>…` | Syntax-check files without executing them |
| `jinn luau version` | Print `jinn <version> (Luau <version>)` |

Flags (place them **before** the subcommand — `jinn luau --timeout 5 run s.luau`):

| Flag | Meaning |
| --- | --- |
| `--timeout <secs>` | Wall-clock budget; default 30; `0` disables entirely |
| `--eval <code>` | Evaluate a code string instead of running a file |

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Success (or the script's own code — see below) |
| 1 | The script raised (syntax, runtime, memory cap) or `check` found a syntax error |
| 2 | Harness trouble (script path unreadable, no script given) |
| 124 | The script exceeded its wall-clock budget (GNU timeout convention) |

A script chooses its own exit code two ways: `jinn.exit(n)` exits immediately,
and an integer returned from the main chunk becomes the process exit code.
`return 3` at the top level makes `jinn luau run s.luau` exit 3.

## The `jinn` table

Everything is plain strings unless noted — argv comes in as strings, files
come back as strings. JSON is a library the script chooses (`jinn.json`), not
a forced contract.

| Family | Calls |
| --- | --- |
| args | `jinn.args` — 1-indexed array of the trailing argv after the script name |
| stdio | `jinn.stdio.read()` (stdin to EOF), `jinn.stdio.eprint(s)` (stderr); `print` goes to stdout |
| fs | `jinn.fs.read/write/exists/list_dir/remove(path)` — relative paths resolve against the process cwd |
| env | `jinn.env.get(name)` (absent → nil), `jinn.env.set(name, value)` |
| http | `jinn.http.request{url=…, method=…, headers={…}, body=…}` → `{status, headers, body}` |
| proc | `jinn.proc.run{program=…, args={…}}` → `{status, stdout, stderr}` — no shell involved |
| json | `jinn.json.encode/decode` — sequential 1..n tables are arrays; an empty table is `{}` |
| toml | `jinn.toml.encode/decode` — a table containing nil errors (TOML has no null) |
| time | `jinn.sleep(ms)`, `jinn.exit(code)` |
| browser | `jinn.browser.open_tab{url}/list_tabs()/navigate{tab,url}/snapshot{tab}/click{tab,ref}/type{tab,ref,text}/press{tab,key}/evaluate{tab,script}/screenshot{tab}→{path}/wait{tab,condition}/close_tab{tab}` — drives the shared browser daemon (same eleven-tool surface as the `browser` MCP server); every tool except `open_tab`/`list_tabs` requires the `tab` id |

`require("./module")` loads a sibling file (the `./` prefix is mandatory);
the module file returns its exports table. `require` resolves relative to
the script's directory, not the cwd.

## Worked example

```lua
-- summarize.luau — fetch, extract, report
local res = jinn.http.request{ url = "https://api.github.com/repos/mlua/mlua" }
if res.status ~= 200 then
    jinn.stdio.eprint("fetch failed: " .. res.status)
    jinn.exit(1)
end
local repo = jinn.json.decode(res.body)
print(("{} stars").format(repo.stargazers_count))
```

```
jinn luau run summarize.luau          # runs with the default 30s budget
jinn luau check summarize.luau        # syntax-gate before running
```

## Gotchas

- **Flag position matters.** `--timeout` must precede `run`, because
  everything after the script path is script argv.
- **`jinn.exit` is immediate.** It does not unwind; a `pcall` around it
  still terminates the process.
- **`jinn.stdio.read()` blocks to EOF.** Scripts are batch programs; there
  is no TTY interaction.
- **JSON nil erasure.** Assigning `nil` into a table removes the key (Lua
  semantics), so absent fields simply don't serialize.
- **Non-UTF-8 files.** `jinn.fs.read` returns bytes as an 8-bit-clean Lua
  string; `jinn.proc.run` output is lossily converted to UTF-8.
- **Type checking is not part of `jinn luau check`.** It is a syntax gate
  only; the Luau type checker is not embedded.
