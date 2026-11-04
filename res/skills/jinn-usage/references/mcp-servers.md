# MCP Servers

jinn is an MCP **client**: it launches or connects to MCP servers and exposes
their tools to the model as `mcp__<server>__<tool>`. Configure servers once;
toggle them per session.

## Declaring servers

Servers live in `jinn.toml` under `[mcp.<name>]` blocks (see
`configuration.md` for the exact file path and the restart caveat):

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

Three transports. `stdio` — spawn + pipes. `local_http` — spawn, then
connect over HTTP. jinn allocates a free port and expands `<ip>`/`<port>`
tokens in `args` and `url`. `remote_http` — externally managed, `command`
unused. Adding, removing, or editing a server requires a **jinn restart**.

- `auto_enable = true` in a server block makes new sessions start with that
  server already enabled.
- Header values support `${VAR}` expansion. An unset variable blocks that
  connection with an error naming the variable. Header values are never
  logged or rendered.

## Bundled servers

jinn ships one builtin server: **overgraph**, an embedded graph database
speaking GQL. It is configured out of the box — no `[mcp.*]` block needed —
but disabled per session until you enable it in the picker:

```toml
[mcp.overgraph]        # shipped in default_jinn.toml; shown here for reference
command = "<jinn>"     # <jinn> expands to the running jinn executable
args = ["mcp", "overgraph"]
singleton = true       # one shared graph process for all sessions
auto_enable = false
```

Six tools: `graph_query` (GQL — reads, writes, schema DDL),
`graph_upsert_node`, `graph_upsert_edge`, `graph_vector_search` (semantic
search over nodes carrying embeddings), `graph_ingest_file` (bulk-load a
JSONL batch file in one call), and `graph_docs` (bundled GQL reference; call
it before writing queries you are unsure of). Data persists under a
jinn-managed directory across sessions. To work against a different graph
database, add your own entry with
`args = ["mcp", "overgraph", "--db", "/path/to/dir"]`. For usage patterns,
worked examples, and batch ingestion, see `graph-database.md`.

## Enable for a session

Servers are **off by default** per session. The choice persists with the
session; forks and subagents inherit it.

1. Press `<leader>sM` (normal mode) to open the MCP server picker.
2. `<Tab>` stages the highlighted server's enable/disable toggle. Nothing
   applies yet. Confirming makes it real: enabling spawns the server
   process/connection, disabling kills it and unregisters its tools. `<esc>`
   closes and reverts to the toggles the picker opened with.
3. `<c-t>` flips the preview pane between the server's live status/stderr
   tail and its tool list.
4. `<c-r>` restarts the selected server — useful after it dies or you edit
   its code. Restart blocks until the server is `Running` or definitively
   failed.
5. `<enter>` applies the staged toggles. `<esc>` closes and reverts them.

Status is surfaced in the sidebar's MCP servers section: connecting, running,
dead. A dead server's captured stderr explains why — view it in the picker's
preview. The agent can also restart a dead server itself via its built-in
`restart_mcp_server` tool.

## Tools in context

Enabled servers register their tools for the session automatically
(namespace `mcp__<server>__<tool>`); disabling unregisters them, keeping them
out of the model's context. Tool-call timeouts and other tool defaults come
from `default_timeout_secs` in `jinn.toml`. Disable individual MCP tools the
same way as built-ins: the tool picker (`<leader>st`), or the `tool_filter`
default in `jinn.toml` using the full namespaced name.
