# Luau: embedded scripting with `jinn luau`

jinn carries its own Lua interpreter, the vendored Luau VM. A script runs
on a host with no python, node, jq, or perl, and with no jinn config,
session store, or running jinn instance. The `jinn luau` subcommand is
config-free: every knob is a CLI flag on the invocation, and it reads
nothing from `jinn.toml`, the database, or a `[luau]` config section.

Scripts are ordinary Luau (a Lua 5.1 dialect with type annotations, string
interpolation, and compound assignment). Capabilities arrive through an
injected `jinn` table plus file-based `require` rooted at the script's
directory. **Scripts run with full access**: they read and write files,
spawn processes, and make network requests. The guardrails, a wall-clock
timeout and a memory cap, kill a runaway script an agent wrote by
accident. Containing hostile code is outside their job.

## Commands

| Command                            | What it does                                                                                      |
| ---------------------------------- | ------------------------------------------------------------------------------------------------- |
| `jinn luau run <script> [args…]`   | Run a script file; trailing args become `jinn.args`                                               |
| `jinn luau serve <script> [args…]` | Serve the script's `jinn.tool` registrations as MCP tools over stdio until the client disconnects |
| `jinn luau --eval '<code>'`        | Evaluate a one-liner; its result prints                                                           |
| `jinn luau check <script>…`        | Syntax-check files without executing them                                                         |
| `jinn luau version`                | Print `jinn <version> (Luau <version>)`                                                           |

Flags (place them **before** the subcommand — `jinn luau --timeout 5 run s.luau`):

| Flag               | Meaning                                                                                                               |
| ------------------ | --------------------------------------------------------------------------------------------------------------------- |
| `--timeout <secs>` | Wall-clock budget; default 30. Under `serve` it is the **per-call** budget instead, default 60; `0` disables entirely |
| `--eval <code>`    | Evaluate a code string instead of running a file                                                                      |

## Exit codes

| Code | Meaning                                                                         |
| ---- | ------------------------------------------------------------------------------- |
| 0    | Success (or the script's own code — see below)                                  |
| 1    | The script raised (syntax, runtime, memory cap) or `check` found a syntax error |
| 2    | Harness trouble (script path unreadable, no script given)                       |
| 124  | The script exceeded its wall-clock budget (GNU timeout convention)              |

A script chooses its own exit code two ways: `jinn.exit(n)` exits immediately,
and an integer returned from the main chunk becomes the process exit code.
`return 3` at the top level makes `jinn luau run s.luau` exit 3.

## The `jinn` table

Everything is plain strings unless noted: argv comes in as strings and files
come back as strings. JSON is a library the script chooses (`jinn.json`).

| Family  | Calls                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| ------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| args    | `jinn.args` — 1-indexed array of the trailing argv after the script name                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| stdio   | `jinn.stdio.read()` (stdin to EOF), `jinn.stdio.eprint(s)` (stderr); `print` goes to stdout                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| fs      | `jinn.fs.read/write/exists/list_dir/remove(path)` — relative paths resolve against the process cwd                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| env     | `jinn.env.get(name)` (absent → nil), `jinn.env.set(name, value)`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| http    | `jinn.http.request{url=…, method=…, headers={…}, body=…}` → `{status, headers, body}`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| proc    | `jinn.proc.run{program=…, args={…}}` → `{status, stdout, stderr}` — no shell involved                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| json    | `jinn.json.encode/decode` — sequential 1..n tables are arrays; an empty table is `{}`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| toml    | `jinn.toml.encode/decode` — a table containing nil errors (TOML has no null)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| time    | `jinn.sleep(ms)`, `jinn.exit(code)`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| tool    | `jinn.tool(spec, fn)` — register an MCP tool (serve mode only; under plain `run` it fails naming the serve subcommand)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| browser | `jinn.browser.open_tab{url}/list_tabs()/navigate{tab,url}/snapshot{tab}/click{tab,ref}/click_at{tab,x,y}/type{tab,ref,text}/press{tab,key}/evaluate{tab,script}/screenshot{tab}→{path}/wait{tab,condition}/close_tab{tab}` — drives the shared browser daemon (same twelve-tool surface as the `browser` MCP server); every tool except `open_tab`/`list_tabs` requires the `tab` id; `snapshot` is a DOM-selector walk, `evaluate` returns the script's last top-level expression value and passes IIFE-tailed scripts through unwrapped, and acting on consent banners prefers snapshot `click` refs over script-side `.click()` or screenshot-measured coordinates |

`require("./module")` loads a sibling file (the `./` prefix is mandatory);
the module file returns its exports table. `require` resolves relative to
the script's directory.

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

## Serving MCP tools from Lua

One plain Lua file can be one MCP server. `jinn luau serve <script>` executes
the script once to collect `jinn.tool` registrations, then keeps the process
alive speaking MCP over stdio:

```lua
-- foomart.lua
jinn.tool(
  { name = "search_products", description = "Search the shop",
    input = { query = "string" } },
  function(args)
    local res = jinn.http.request{ url = "https://shop.foo/search?q=" .. args.query }
    return { results = jinn.json.decode(res.body) }  -- the returned table IS the result
  end
)
```

Point a `[mcp.x]` entry at it (see `mcp-servers.md`) and toggle it in the MCP
picker like any other server. Save reusable scripts — especially serve scripts,
since a served script is an MCP server every session can enable — under the
universal scripts directory, `~/.local/share/jinn/scripts`, and reference them
with the `<scripts>` token so the entry works from any session in any working
directory (`<session_cwd>` remains available for scripts you want pinned to one
session's tree):

```toml
[mcp.foomart]
command = "<jinn>"
args = ["luau", "serve", "<scripts>/foomart.lua"]
auto_enable = false
```

The scripts directory is **universal storage**. `jinn luau` leaves what
lives there alone. `jinn luau run <scripts>/summarize.luau` works by plain
path. Serve mode is where universality pays: a served script becomes a
cross-session MCP server rather than a one-off run. Keep loose single
files at the top, or one subdirectory per multi-file script. A script
`require`s modules rooted at its own directory, and subdirectories keep
two scripts' `util.luau` from colliding.

The `input` map makes every field required. A table with a top-level
`type` or `properties` key passes through verbatim as a raw JSON schema,
the escape hatch for optional fields and nested constraints. A tool's
returned table is serialized as the result's structured content, with the
same JSON as a text fallback.

Serve-mode rules:

- **Per-call budget.** `--timeout` becomes the per-call budget under
  serve, defaulting to 60 seconds. A Lua loop past it unwinds as a clean
  tool error, and the server keeps serving.
- **A call wedged inside a capability** (the interrupt can't unwind a
  blocked `jinn.http`/`jinn.sleep`) is answered as a tool error at budget
  expiry, then **poisons the server**: every later call is refused.
  `<c-r>` in the MCP picker restarts it after fixing the script.
- **`print` writes to stderr.** Stdout and stdin are the MCP transport,
  and jinn's captured stderr tail (visible via `<c-t>` in the picker) is
  the log channel. `jinn.stdio.read` refuses, since stdin is the
  transport. `jinn.exit` inside a served tool refuses, naming the rule. A
  script may still `jinn.exit` while loading to refuse serving outright.
- **Registrations run while the script loads.** A `jinn.tool` call after
  the server has started fails. A script that registers nothing fails the
  start, naming the requirement. A duplicate tool name, a missing
  name/description, or a bad type name fails the load.
- **`jinn.tool` under plain `run` fails loudly**, naming `jinn luau serve`.

## Gotchas

- **Flag position matters.** `--timeout` must precede `run`. Everything
  after the script path is script argv.
- **`jinn.exit` is immediate.** It skips unwinding; a `pcall` around it
  still terminates the process.
- **`jinn.stdio.read()` blocks to EOF.** Scripts are batch programs, and
  there is no TTY interaction.
- **JSON nil erasure.** Assigning `nil` into a table removes the key (Lua
  semantics), so absent fields simply don't serialize. The same bite hits
  served tool results: a handler assigning `nil` into its returned table
  drops that key from the structured content.
- **Non-UTF-8 files.** `jinn.fs.read` returns bytes as an 8-bit-clean Lua
  string; `jinn.proc.run` output is lossily converted to UTF-8.
- **`jinn luau check` is a syntax gate.** It checks syntax alone. The
  Luau type checker is outside it.
