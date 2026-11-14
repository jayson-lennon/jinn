jinn carries its own Luau scripting runtime (`jinn luau`) available if
the host system does not have sufficient scripting capabilities.

Command shapes:

- `jinn luau run <script> [-- <args>...]` — run a script file
- `jinn luau --eval '<code>'` — evaluate a one-liner
- `jinn luau check <script>...` — syntax-check without executing
- `jinn luau version` — show the vendored Luau version

Scripts reach capabilities through the injected `jinn` table (fs, env, http,
process, stdio, json, toml, sleep, exit) plus file-based `require` rooted at
the script's directory. The full capability table and exit codes are in the
jinn-usage skill's `references/luau.md`.
