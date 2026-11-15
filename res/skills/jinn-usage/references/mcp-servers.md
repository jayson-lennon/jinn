# MCP Servers

jinn is an MCP **client**: it launches or connects to MCP servers and exposes
their tools to the model as `mcp__<server>__<tool>`. Configure servers once;
toggle them per session.

## Declaring servers

Servers live in `jinn.toml` under `[mcp.<name>]` blocks (see
`configuration.md` for the exact file path and the reload categories):

```toml
# stdio — jinn spawns the server (default transport)
[mcp.context7]
command = "npx"
args = ["@context7/mcp-server", "--stdio"]

# remote_http — connect to an already-running server; nothing spawned
[mcp.remote]
transport = "remote_http"
url = "http://localhost:3001/mcp"

[mcp.remote.headers]                 # local_http / remote_http only
Authorization = "Bearer ${MY_API_KEY}" # ${VAR} expanded once at startup

# Singleton servers. A server marked `singleton = true` spawns one instance
# that has a shared connection across all sessions. Use this for servers that
# consume a lot of resources, or are already designed to support multiple clients.
# Works for every transport — stdio included: one child process is shared
# no matter how the server speaks.
#
# Singleton example — one shared process speaking streamable HTTP. The
# child binds the port jinn picks; `<port>` and `<ip>` expand to the
# chosen port and loopback address in both `args` and `url`.
#
[mcp.shared_tools]
command = "some-mcp-server"
args = ["serve", "--port", "<port>", "--host", "<ip>"]
transport = "local_http"
url = "http://127.0.0.1:<port>/mcp"
singleton = true
```

Three transports. `stdio` (spawn + pipes). `local_http` (spawn, then
connect over HTTP; jinn allocates a free port and expands `<ip>`/`<port>`
tokens in `args` and `url`). `remote_http` (externally managed); jinn
connects to the URL as given. Server entries are a deferred-update
section: an edited entry reconnects that server, a removed entry keeps an
already-running connection up until session end, and new sessions pick up
the entry as configured.

- `auto_enable = true` in a server block makes new sessions start with that
  server already enabled: it stamps new sessions only; a session's enabled
  set is its own, managed in the picker.
- Header values support `${VAR}` expansion. An unset variable blocks that
  connection with an error naming the variable. jinn keeps header values
  out of logs and renders.

## Bundled servers

jinn ships three builtin servers: **overgraph**, **smolvm**, and
**browser**. Their
`[mcp.*]` blocks ship in `default_jinn.toml`, and each session starts with
them disabled until you enable them in the picker.

### Overgraph — embedded graph database

```toml
[mcp.overgraph]        # shipped in default_jinn.toml; shown here for reference
command = "<jinn>"     # <jinn> expands to the running jinn executable
args = ["mcp", "overgraph"]
singleton = true       # one shared graph process for all sessions
auto_enable = false
```

Six tools: `graph_query` (GQL: reads, writes, schema DDL),
`graph_upsert_node`, `graph_upsert_edge`, `graph_vector_search` (semantic
search over nodes carrying embeddings), `graph_ingest_file` (bulk-load a
JSONL batch file in one call), and `graph_docs` (bundled GQL reference; call
it before writing queries you are unsure of). Data persists under a
jinn-managed directory across sessions. To work against a different graph
database, add your own entry with
`args = ["mcp", "overgraph", "--db", "/path/to/dir"]`. For usage patterns,
worked examples, and batch ingestion, see `graph-database.md`.

### Smolvm — a sandboxed Linux VM per session

```toml
[mcp.smolvm]           # shipped in default_jinn.toml; shown here for reference
command = "<jinn>"
args = ["mcp", "smolvm", "--cwd", "<session_cwd>"]
auto_enable = false
```

Enabling it boots **one ephemeral Alpine Linux microVM for this session
only** — not a singleton. The session's working directory is mounted
read-write at `/workspace` inside the guest, which is also the default
working directory, so `vm_run` edits the session's real tree. Two tools:
`vm_run` (run a shell command via `bash -lc`; the guest has bash, git, and
python3; returns stdout/stderr/exit code; a command killed at the timeout —
default 60s, max 600s — returns its partial output with exit code 124) and
`vm_info` (the machine manifest plus live guest facts: distro, kernel,
CPUs, memory, the mount, uptime). The VM lives and dies with the server
process: disabling the server or quitting jinn discards the guest disk,
while writes to the mounted session directory persist on the host.

Requirements: hardware virtualization (KVM on Linux, HVF on macOS). First
boot needs the network (packages install from Alpine's index) and downloads
the VM engine once. Boot progress streams live into the server's stderr
tail in the picker preview while it connects — one line per phase (engine
download, machine create, guest boot, first-boot package install) with a
running elapsed-seconds heartbeat, ending in `ready — serving vm_run and
vm_info`. Machine manifests seed into `~/.config/jinn/smolvm/`
(`alpine.toml`
ships; your edits are never overwritten) and every boot reads the file
from disk — the server refuses to start without it. To boot a different
manifest, add your own entry:
`args = ["mcp", "smolvm", "--cwd", "<session_cwd>", "--smolfile",
"/path/to/profile.toml"]` — each `[mcp.<name>]` entry is its own
per-session machine.

### Browser — one shared headed Chrome

```toml
[mcp.browser]          # shipped in default_jinn.toml; shown here for reference
command = "<jinn>"
args = ["mcp", "browser"]
singleton = true       # one daemon client per machine; the Chrome window itself lives outside jinn
auto_enable = false
```

Eleven tools for driving a real headed Chrome: `open_tab` (returns a tab
id), `list_tabs`, `navigate`, `snapshot` (readable page tree with `e<n>`
refs over the interactive elements), `click` (by `tab` + `ref`),
`type`, `press` (key names like `"Enter"`, `"Tab"`), `evaluate` (stealth
JS eval), `screenshot` (returns a saved PNG **path** — view it with the
image-attachment built-in), `wait` (`"navigation"`, `"network_idle"`,
`{text = "…"}`, or `{ms = n}`), and `close_tab`. **Every tool except
`open_tab`/`list_tabs` requires the `tab` id** — there is no "whatever
tab is live" fallback.

The Chrome window is owned by a separate **browser daemon** (`jinn
browser daemon`), not by jinn or this server: the first tool call
auto-starts it, it outlives jinn restarts, and every jinn instance and
`jinn.browser` luau script on the machine shares the same window and
tabs. `jinn browser status|stop|restart` manage it directly. Tab ids
from before a Chrome crash are gone (`unknown tab`) — the daemon
relaunches transparently on the next command and `list_tabs` shows the
new state. Pick the browser binary with `[browser] binary = "/abs/path"`
in jinn.toml (unset = auto-detect: PATH first, then install paths;
branded Chrome preferred, Chromium fallback; a configured path that does
not resolve refuses the daemon start).

Input is stealth-tuned (trusted-input CDP paths, no automation flags,
no `Runtime.enable`/`Network.enable`), so logged-in sites treat it like
a normal user window.

## Enable for a session

Servers are **off by default** per session. The choice persists with the
session; forks and subagents inherit it.

1. Press `<leader>sM` (normal mode) to open the MCP server picker.
2. `<Tab>` stages the highlighted server's enable/disable toggle; confirming
   makes it real. Enabling spawns the server process/connection, disabling
   kills it and unregisters its tools. `<esc>` closes and reverts to the
   toggles the picker opened with.
3. `<c-t>` flips the preview pane between the server's live status/stderr
   tail and its tool list.
4. `<c-r>` restarts the selected server, useful after it dies or you edit
   its code. Restart blocks until the server is `Running` or definitively
   failed.
5. `<enter>` applies the staged toggles. `<esc>` closes and reverts them.

Status is surfaced in the sidebar's MCP servers section: connecting, running,
dead. A dead server's captured stderr explains why; view it in the picker's
preview. The agent can also restart a dead server itself via its built-in
`restart_mcp_server` tool.

## Tools in context

Enabled servers register their tools for the session automatically
(namespace `mcp__<server>__<tool>`); disabling unregisters them, keeping
them out of the model's context. Tool-call timeouts and other tool defaults
come from `default_timeout_secs` in `jinn.toml`. Disable individual MCP
tools the same way as built-ins: the tool picker (`<leader>st`), or the
`tool_filter` default in `jinn.toml` using the full namespaced name.
