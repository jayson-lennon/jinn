# MCP Servers

jinn is an MCP **client**: it launches or connects to MCP servers and exposes
their tools to the model as `mcp__<server>__<tool>`. Servers are configured
once and toggled per session.

## Declaring servers

Servers live in `jinn.toml` under `[[mcp_server]]` blocks (see
`configuration.md` for the exact file path and the restart caveat):

```toml
# stdio — jinn spawns the server (default transport)
[mcp_server.context7]
command = "npx"
args = ["@context7/mcp-server", "--stdio"]

# remote_http — connect to an already-running server; nothing spawned
[mcp_server.remote]
transport = "remote_http"
url = "http://localhost:3001/mcp"

[mcp_server.remote.headers]            # local_http / remote_http only
Authorization = "Bearer ${MY_API_KEY}" # ${VAR} expanded once at startup
```

Transport options: `stdio` (spawn + pipes), `local_http` (spawn, then connect
over HTTP — jinn allocates a free port and expands `<ip>`/`<port>` tokens in
`args` and `url`), and `remote_http` (externally managed, `command` unused).
Adding, removing, or editing a server requires a **jinn restart**.

- `auto_enable = true` in a server block makes new sessions start with that
  server already enabled.
- Header values support `${VAR}` expansion; an unset variable blocks that
  connection with an error naming the variable, and header values are never
  logged or rendered.

## Enabling for a session

Servers are **off by default** per session (the choice persists with the
session; forks and subagents inherit it):

1. Press `<leader>sM` (normal mode) to open the MCP server picker.
2. `<Tab>` toggles the highlighted server for this session — enabling spawns
   the server process/connection; disabling kills it and unregisters its
   tools immediately.
3. `<c-t>` flips the preview pane between the server's live status/stderr
   tail and its tool list.
4. `<c-r>` restarts the selected server (useful after it dies or you edited
   its code). Restart blocks until it's `Running` or definitively failed —
   the model can't race a half-started server.
5. `<enter>` confirms / `<esc>` closes.

Status is surfaced in the sidebar's MCP servers section: connecting, running,
dead. A dead server's captured stderr explains why (view it in the picker's
preview). The agent can also restart a dead server itself via its built-in
`restart_mcp_server` tool.

## Tools in context

Enabled servers register their tools for the session automatically (namespace
`mcp__<server>__<tool>`); disabling unregisters them, so the model's context
stays clean. Tool-call timeouts and other tool defaults come from
`tool_default_timeout_secs` in `jinn.toml`. Disable individual MCP tools the
same way as built-ins: the tool picker (`<leader>st`), or the
`disabled_tools` default in `jinn.toml` using the full namespaced name.
