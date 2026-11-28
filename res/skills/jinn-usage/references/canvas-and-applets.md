# Canvas and Applets

Two web servers jinn runs for agent-built pages. The **canvas** is a
scratch surface: ask for a chart, diagram, simulation, or "show me"
anything, and the agent writes an HTML page and serves it; live reload
makes it editable in place. An **applet** is the same kind of page saved
permanently, reopened any time from the `<leader>sa` picker, even after
the session that built it is gone.

## Canvas: the "show me" surface

Ask naturally: "chart my benchmark numbers", "show me how this state
machine works", "make an interactive demo of that algorithm". The agent
writes the page, serves it, and puts the URL in its reply. You click the
link to open it.

What you get:

- **Libraries included.** Chart.js, Mermaid, KaTeX, three.js, p5, Vega,
  and more are embedded in the jinn binary and served offline at
  `/vendor/…`; the agent picks one, and npm and the network stay out of
  it.
- **Live reload.** Every save to the page (or its data files) refreshes
  your tab. Say "make the bars red" and watch it change: the tab is a
  live view of the file.
- **Your clicks report back.** Interactive pages track what you click
  (quiz answers, slider positions, buttons). The clicks ride your **next
  chat message** as a numbered `<canvas-metrics>` list, so the agent sees
  what you did without you describing it.
- **One page at a time.** A second `canvas` call re-targets the same
  server: same URL, your tab follows to the new page. Artifacts live in
  your project directory as plain HTML files; edit them by hand if you
  like.

## Applets: saved pages

"Keep this" or "save this as an applet" turns the current canvas page
into a snapshot under `~/.local/share/jinn/applets/`. The agent can also
build an applet from scratch. Either way:

- Open the picker with `<leader>sa`; it shows every saved applet with a
  live filter, and a status line in its footer: `<server offline>` until
  the applets server has bound, then `serving http://127.0.0.1:<port>`
  in green. Opening the picker wakes the server (it binds on the first
  picker open, or on the first `<enter>`), so the address is visible
  without opening an applet first. `<enter>` opens it in your browser,
  `<c-enter>` opens it in a **kiosk window** — full-screen, chromeless,
  even while your normal browser is running (jinn launches a
  Chromium-family browser it finds with its own profile; set
  `[applets] kiosk_command` in `jinn.toml` to launch a specific browser
  instead — `{url}`, `{profile}`, `{id}`, and `{title}` are substituted
  into your command verbatim). `<c-r>` renames, `<c-d>` deletes (removes
  the saved copy for good), `<esc>` closes.
- In the web UI, the `☰` menu's **Applets** item opens the saved applets
  as a filterable list of new-tab links; rename, delete, and kiosk are
  the TUI's (`webui.md` → [Pickers](webui.md#pickers)).
- An applet is a **snapshot**. Later improvements to the canvas page it
  came from stay on the canvas; the applet keeps the copy it was made
  from. Ask the agent to re-promote (a fresh copy) when you want the
  improvements.
- Hand edits work too: each applet is a plain directory with
  `index.html` plus any data files, cataloged in `applets.json` beside
  them. The picker rescans when opened, so external changes show up; an
  applet whose directory is deleted disappears from the list.

Renaming and deleting in the picker rewrite the catalog. The picker is
the source of truth for titles and order.
