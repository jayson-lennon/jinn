---
name: jinn-usage
description: Explain and configure jinn, the terminal agent harness — keybindings, workflows (pinning, forking, subagents, MCP, terminal overlay, providers), and jinn.toml/providers.toml configuration. Use when the user asks how to do something in jinn, what a key does, how a feature works, or wants a behavior changed via configuration. Offer to edit the user's config when an ask maps to a TOML setting; config changes require a jinn restart.
---

# jinn Usage Guide

You are explaining **jinn** — a TUI agent harness with multi-session support,
Vim-style keybinds, and fine-grained context management — to the user, who is
running it right now.

## How to answer

1. **Identify the topic.** Pick the most relevant reference file below and read
   it (paths are relative to this skill's base directory, given in
   `<available_skills>`). Read only what you need.
2. **Answer with real keybinds.** Every instruction must cite concrete
   bindings, e.g. "press `p` in normal mode to pin". Bindings below are the
   **defaults**; the user may be running different ones. When the exact key
   matters, tell the user to press `?` (or `<f1>` in input mode) to open the
   which-key popup, which shows the bindings that are live in the current
   scope.
3. **Describe workflows as sequences.** Anchor each step to its binding, state
   the mode/scope it applies in, and explain side effects (what enters or leaves
   context, what gets persisted).
4. **Configuration asks** → read `references/configuration.md` first, follow
   its protocol: explain the mechanism, show a snippet, offer to apply it to
   the user's config file, and note that config changes need a jinn restart.
5. **If the answer isn't covered**, say so rather than inventing bindings —
   wrong binds are worse than no answer.

## References

| File | Covers |
| --- | --- |
| `references/keybindings.md` | Every scope's default bindings, categorized |
| `references/context-management.md` | Pinning, context toggles, isolate, compaction/pruning tuning |
| `references/sessions-and-subagents.md` | Sessions, forks, the session tree, subagents/tasks |
| `references/pickers-and-search.md` | The `<leader>s*` pickers, session search |
| `references/terminal-overlay.md` | Interactive terminal overlay, control mode |
| `references/mcp-servers.md` | MCP server config, enabling, the inspector |
| `references/chat-input-tokens.md` | `#prompt`, `@attachment`, `//` autocomplete tokens |
| `references/models-and-providers.md` | Model/provider pickers, alloys, endpoint pinning, reasoning effort |
| `references/configuration.md` | Config file map, snippets, apply-to-user-config protocol |
