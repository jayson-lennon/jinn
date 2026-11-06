---
name: jinn-usage
description: Explain and configure jinn, the terminal agent harness — keybindings, workflows (pinning, forking, subagents, MCP, terminal overlay, dictation, providers), adding new capability (MCP servers, skills, prompts, personas, themes), and jinn.toml/providers.toml configuration. Answers "what is this on screen / where is feature X" UI-identification questions via the ui-map reference. Use when the user asks how to do something in jinn, what a key does, how a feature works, what a UI element is or where it lives on screen, wants to add or extend a feature, or wants a behavior changed via configuration. Offer to edit the user's config when an ask maps to a TOML setting; config changes require a jinn restart.
---

# jinn Usage Guide

You are explaining **jinn** — a TUI agent harness with multi-session support,
Vim-style keybinds, and fine-grained context management — to a user who is
running it right now.

## How to answer

1. **Read the one reference that covers the ask.** Paths are relative to this
   skill's base directory, given in `<available_skills>`. Don't read more than
   you need.
2. **UI-identification asks → read `references/ui-map.md` first.** "What is
   the X on screen?", "where does Y live?", "which thing is the strip on the
   right?" — identify the surface by name and position in the map, then
   cross-link to the feature reference that owns its behavior. Answer with
   the surface's name, its position, and how to interact with it.
3. **Answer with real keybinds.** Every instruction cites a concrete binding —
   "press `p` in normal mode to pin", never "use the pin command". What's
   written here is the **defaults**; the user may be running different ones.
   When the exact key matters, tell them to press `?` (or `<f1>` in input
   mode) — the which-key popup shows the bindings live in the current scope.
4. **Walk workflows as sequences.** Anchor each step to its binding. Name the
   mode or scope it applies in. State the side effects — what enters or
   leaves context, what gets persisted.
5. **Configuration asks → read `references/configuration.md` first.** Follow
   its protocol: explain the mechanism, show a snippet, offer to apply it to
   the user's config file. Config changes need a jinn restart — say so every
   time.
6. **If it isn't covered, say so.** Don't invent a binding — a wrong bind
   costs the user more than an "I don't know" does.

## Adjusting your own session's capabilities

An agent running inside jinn can turn its own tools, skills, and MCP servers
on or off with two built-in tools — no trip to the pickers required.

1. Call `capabilities_query` (no arguments). It returns the session's complete
   on/off buffer as JSON: `{"tools": {...}, "skills": {...}, "mcp_servers":
   {...}}`, one entry per registered tool (including `mcp__<server>__<tool>`
   entries), per discovered skill, and per configured MCP server. `true` =
   available, `false` = withheld.
2. Flip only the states you want to change, then pass the **whole buffer** to
   `capabilities_apply` under `capabilities`. Echo the entire thing — an
   apply missing any entry, or naming an unknown one, is rejected whole. The
   current buffer rides the rejection, and nothing changes until every entry
   is present.
3. A successful apply is atomic and takes effect immediately. Off tools and
   skills vanish from the next prompt. Off MCP servers are disconnected. The
   change persists with the session. It commits deny-list filters, so a
   session under an allow-mode filter is demoted to the mask's deny list.
   Turning the adjustment tools themselves off is allowed — and honored.

If an apply is rejected because the session changed (tools registered or
removed since your query), re-run `capabilities_query` — it always returns
the current truth — and echo that.

## References

| File                                   | Covers                                                                                     |
| -------------------------------------- | ------------------------------------------------------------------------------------------ |
| `references/ui-map.md`                 | The on-screen surfaces: layout diagram, what each element is, where it sits, its keys      |
| `references/keybindings.md`            | Every scope's default bindings, categorized                                                |
| `references/context-management.md`     | Pinning, context toggles, isolate, compaction/pruning tuning                               |
| `references/sessions-and-subagents.md` | Sessions, forks, the session tree, subagents/tasks                                         |
| `references/bots.md`                  | Bots: properties popup, triggers, `[[bot.entry]]`, report history, the Library               |
| `references/pickers-and-search.md`     | The `<leader>s*` pickers, session search                                                   |
| `references/terminal-overlay.md`       | Interactive terminal overlay, control mode                                                 |
| `references/mcp-servers.md`            | MCP server config, enabling, the inspector                                                 |
| `references/chat-input-tokens.md`      | `#prompt`, `@attachment`, `/command` autocomplete tokens                                    |
| `references/dictation.md`              | Local voice input: `<M-i>` toggle, the meter, `[dictation]` config, the CLI utilities      |
| `references/builtin-prompts.md`        | The builtin `#prompts`: what each is for, how they work, choosing between them             |
| `references/models-and-providers.md`   | Model/provider pickers, alloys, endpoint pinning, reasoning effort                         |
| `references/remote.md`                  | `jinn remote`: messaging a running instance from outside (CLI + curl HTTP surface)         |
| `references/configuration.md`          | Config file map, snippets, apply-to-user-config protocol                                   |
| `references/extending-jinn.md`         | Adding capabilities or extending jinn: MCP servers, skills, prompts, personas, themes, etc |
| `references/graph-database.md`         | The bundled overgraph graph+vector DB: usage patterns, batch ingestion, gotchas             |
