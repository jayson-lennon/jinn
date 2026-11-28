---
name: jinn-usage
description: Explain and configure jinn, the terminal agent harness: keybindings, workflows (pinning, forking, subagents, MCP, terminal overlay, dictation, Discord, canvas/applets, providers), the web UI (the browser page a running jinn serves: its sidebar and `⋮` row menu, transcript and `⋮` entry menu, composer, status bar with persona and context figure, the session dash behind the context figure, push notifications, `☰` menu, pickers, the OpenRouter model market), browser automation (headed Chrome via the browser MCP or Lua host functions), the embedded Luau scripting runtime (`jinn luau`), adding new capability (MCP servers, skills, prompts, personas, themes), system prompt assembly, and jinn.toml/providers.toml configuration. Answers "what is this on screen / where is feature X" UI-identification questions via the ui-map reference, for the terminal and the web UI. Use when the user asks how to do something in jinn, what a key does, how a feature works, what a UI element is or where it lives on screen, how to open or use the web UI in a browser, wants to add or extend a feature, or wants a behavior changed via configuration. Offer to edit the user's config when an ask maps to a TOML setting; config edits reload when saved, and the skill says when each change takes effect.
---

# jinn Usage Guide

You are explaining **jinn**, a TUI agent harness with multi-session support,
Vim-style keybinds, and fine-grained context management, to a user who is
running it right now. The same instance also serves a **web UI** — a browser
page onto the same sessions — so most features have a TUI and a web way.

## How to answer

1. **Read the one reference that covers the ask.** Paths are relative to this
   skill's base directory, given in `<available_skills>`. Don't read more than
   you need.
2. **UI-identification asks → read `references/ui-map.md` first.** "What is
   the X on screen?", "where does Y live?", "which thing is the strip on the
   right?" Identify the surface by name and position in the map, then
   cross-link to the feature reference that owns its behavior. Answer with
   the surface's name, its position, and how to interact with it.
3. **Decide which surface the user is on: the TUI or the web UI.** jinn
   serves both from one instance — same sessions, same active session. The
   web UI (a browser page, served at the root of its own port) has few keybinds: almost every
   action is a click. Say "browser", "web UI", or a URL, or name a
   web-only element (the `#` drawer button, the **Archive** button, the
   **Q** / **Cancel** / **Send** buttons, the `☰` menu, a `⋮` on a
   session row or on a message, the persona name or context figure in the
   status bar, the session dash, a push notification or the **Notifications**
   menu item, or its **Skills**, **Persona**, **Tools**, or **MCP servers**
   modal; and an ask about using jinn **on a phone or away from the
   terminal** — the web UI is that surface and its composer is where you
   reply; Discord is the alternative when the alerting must come from a
   chat app —)
   → read `references/webui.md` first. Unsaid → answer for the TUI, and when the feature also exists on
   the web, its reference names the web control and points at the section
   of `webui.md` that describes it — follow that pointer rather than
   answering from the feature reference alone, because a web behavior is
   described in exactly one place. A feature the web lacks is listed under
   "TUI-only features" there — say it is TUI-only and name the TUI
   surface that has it.
4. **Answer with real keybinds.** Every instruction cites a concrete binding:
   "press `p` in normal mode to pin", never "use the pin command". What's
   written here is the **defaults**; the user may be running different ones.
   When the exact key matters, tell them to press `?` (or `<f1>` in input
   mode) for the which-key popup, which shows the bindings live in the
   current scope.
5. **Walk workflows as sequences.** Anchor each step to its binding. Name the
   mode or scope it applies in. State the side effects: what enters or
   leaves context, what gets persisted.
6. **Configuration asks → read `references/configuration.md` first.** Follow
   its protocol: explain the mechanism, show a snippet, offer to apply it to
   the user's config file. Config edits reload when saved; say when the
   change takes effect per its category table (re-read at next use,
   deferred update, new session only).
7. **If it isn't covered, say so.** Don't invent a binding: a wrong bind
   costs the user more than an "I don't know" does.

## Adjusting your own session's capabilities

An agent running inside jinn can turn its own tools, skills, and MCP servers
on or off with two built-in tools, no trip to the pickers required.

1. Call `capabilities_query` (no arguments). It returns the session's complete
   on/off buffer as JSON: `{"tools": {...}, "skills": {...}, "mcp_servers":
   {...}}`, one entry per registered tool (including `mcp__<server>__<tool>`
   entries), per discovered skill, and per configured MCP server. `true` =
   available, `false` = withheld.
2. Flip only the states you want to change, then pass the **whole buffer** to
   `capabilities_apply` under `capabilities`. Echo the entire thing: an
   apply missing any entry, or naming an unknown one, is rejected whole. The
   current buffer rides the rejection, and nothing changes until every entry
   is present.
3. A successful apply is atomic and takes effect immediately. Off tools and
   skills vanish from the next prompt. Off MCP servers are disconnected. The
   change persists with the session. It commits deny-list filters, so a
   session under an allow-mode filter is demoted to the mask's deny list.
   Turning the adjustment tools themselves off is allowed, and honored.

If an apply is rejected because the session changed (tools registered or
removed since your query), re-run `capabilities_query` (it always returns
the current truth) and echo that.

## References

| File                                   | Covers                                                                                     |
| -------------------------------------- | ------------------------------------------------------------------------------------------ |
| `references/ui-map.md`                 | The on-screen surfaces, TUI and web UI: layout diagrams, what each element is, where it sits, its keys |
| `references/webui.md`                  | The web UI (the browser page served at the root of its own port): how to open it, its sidebar (Sessions / Library, the `⋮` row menu, the **Archive** picker), its transcript and `⋮` entry menu, its status bar (persona, context figure) and the session dash behind the figure, its `☰` menu (with its **Notifications** item), its push notifications and where they arrive, its pickers (Skills, Persona, Tools, MCP servers, Applets, the model picker, the OpenRouter model market), its composer keys, reconnecting, its TUI-only features. The one place a web behavior is described |
| `references/keybindings.md`            | Every scope's default bindings, categorized                                                |
| `references/context-management.md`     | Pinning, context toggles, isolate, compaction/pruning tuning                               |
| `references/sessions-and-subagents.md` | Sessions (create, switch, rename), the Library, forks, the session tree, subagents/tasks |
| `references/bots.md`                  | Bots: properties popup, triggers, `[[bot.entry]]`, report history, the Library               |
| `references/pickers-and-search.md`     | The `<leader>s*` pickers (incl. the web UI's Tools, Skills, Persona, and MCP modals), session search |
| `references/terminal-overlay.md`       | Interactive terminal overlay, control mode                                                 |
| `references/mcp-servers.md`            | MCP server config, enabling, the inspector                                                 |
| `references/discord.md`                | The Discord bot: forum threads as sessions, the commands, authorization, `gdc`, and how it stands beside the web UI |
| `references/chat-input-tokens.md`      | `#prompt`, `@attachment`, `/command` autocomplete tokens                                    |
| `references/dictation.md`              | Local voice input: `<M-i>` toggle, the meter, `[dictation]` config, the CLI utilities      |
| `references/builtin-prompts.md`        | The builtin `#prompts`: what each is for, how they work, choosing between them             |
| `references/personas.md`               | The builtin personas, choosing one, and guidance for writing your own                      |
| `references/workflows.md`              | Workflows: running them, building one by asking, the bundled `autodev` loop                 |
| `references/models-and-providers.md`   | Model/provider pickers, alloys, endpoint pinning, reasoning effort                         |
| `references/remote.md`                  | `jinn remote`: messaging a running instance from outside (CLI + curl HTTP surface)         |
| `references/luau.md`                    | `jinn luau`: the embedded Luau scripting runtime — run/serve/eval/check/version, the `jinn` capability table, exit codes, serving MCP tools from Lua |
| `references/canvas-and-applets.md`     | The canvas "show me" surface (live reload, click metrics) and saved applets (`<leader>sa`) |
| `references/configuration.md`          | Config file map, snippets, apply-to-user-config protocol                                   |
| `references/extending-jinn.md`         | Adding capabilities or extending jinn: MCP servers, skills, prompts, personas, themes, etc |
| `references/graph-database.md`         | The bundled overgraph graph+vector DB: usage patterns, batch ingestion, gotchas             |
