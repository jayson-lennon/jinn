---
name: jinn-usage
description: Explain and configure jinn, the terminal agent harness — keybindings, workflows (pinning, forking, subagents, MCP, terminal overlay, dictation, providers), adding new capability (MCP servers, skills, prompts, personas, themes), and jinn.toml/providers.toml configuration. Use when the user asks how to do something in jinn, what a key does, how a feature works, wants to add or extend a feature, or wants a behavior changed via configuration. Offer to edit the user's config when an ask maps to a TOML setting; config changes require a jinn restart.
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

## Adjusting your own session's capabilities

An agent running inside jinn can turn its own tools, skills, and MCP servers
on or off with two built-in tools — no user trip to the pickers required.

1. Call `capabilities_query` (no arguments). It returns the session's complete
   on/off buffer as JSON: `{"tools": {...}, "skills": {...}, "mcp_servers":
   {...}}`, one entry per registered tool (including `mcp__<server>__<tool>`
   entries), per discovered skill, and per configured MCP server, `true` =
   available, `false` = withheld.
2. Edit that buffer — flip only the states you want to change — and pass the
   WHOLE thing to `capabilities_apply` under `capabilities`. **Echo the entire
   buffer**: an apply missing any entry, or naming an unknown one, is rejected
   whole with the current buffer attached to the rejection; nothing changes
   until every entry is present.
3. A successful apply is atomic and takes effect immediately: off tools and
   skills vanish from the next prompt, off MCP servers are disconnected, and
   the change persists with the session. It commits deny-list filters, so a
   session under an allow-mode filter is demoted to the mask's deny list.
   Turning the adjustment tools themselves off is allowed and honored.

If an apply is rejected because the session changed (tools registered or
removed since your query), re-run `capabilities_query` — it always returns
the current truth — and echo that.

## References

| File                                   | Covers                                                                                     |
| -------------------------------------- | ------------------------------------------------------------------------------------------ |
| `references/keybindings.md`            | Every scope's default bindings, categorized                                                |
| `references/context-management.md`     | Pinning, context toggles, isolate, compaction/pruning tuning                               |
| `references/sessions-and-subagents.md` | Sessions, forks, the session tree, subagents/tasks                                         |
| `references/bots.md`                  | Bots: properties popup, triggers, `[[bot.entry]]`, report history, the Library               |
| `references/pickers-and-search.md`     | The `<leader>s*` pickers, session search                                                   |
| `references/terminal-overlay.md`       | Interactive terminal overlay, control mode                                                 |
| `references/mcp-servers.md`            | MCP server config, enabling, the inspector                                                 |
| `references/chat-input-tokens.md`      | `#prompt`, `@attachment`, `//` autocomplete tokens                                         |
| `references/dictation.md`              | Local voice input: `<M-i>` toggle, the meter, `[dictation]` config, the CLI utilities      |
| `references/builtin-prompts.md`        | The builtin `#prompts`: what each is for, how they work, choosing between them             |
| `references/models-and-providers.md`   | Model/provider pickers, alloys, endpoint pinning, reasoning effort                         |
| `references/remote.md`                  | `jinn remote`: messaging a running instance from outside (CLI + curl HTTP surface)         |
| `references/configuration.md`          | Config file map, snippets, apply-to-user-config protocol                                   |
| `references/extending-jinn.md`         | Adding capabilities or extending jinn: MCP servers, skills, prompts, personas, themes, etc |
| `references/graph-database.md`         | The bundled overgraph graph+vector DB: usage patterns, batch ingestion, gotchas             |
