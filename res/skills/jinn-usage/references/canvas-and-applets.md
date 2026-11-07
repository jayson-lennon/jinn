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
writes the page, serves it, and puts the URL in its reply. jinn does not
open your browser; you click the link.

What you get:

- **Libraries included.** Chart.js, Mermaid, KaTeX, three.js, p5, Vega,
  and more are embedded in the jinn binary and served offline at
  `/vendor/…`; the agent picks one and never needs npm or the network.
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
  live filter. `<enter>` opens it in your browser (this also wakes the
  applets server; it binds lazily on first use), `<c-r>` renames,
  `<c-d>` deletes (removes the saved copy for good), `<esc>` closes.
- An applet is a **snapshot**: later improvements to the canvas page it
  came from never change it, and it does not live-reload. Ask the agent
  to re-promote (a fresh copy) when you want the improvements.
- Hand edits work too: each applet is a plain directory with
  `index.html` plus any data files, cataloged in `applets.json` beside
  them. The picker rescans when opened, so external changes show up; an
  applet whose directory is deleted disappears from the list.

Renaming and deleting in the picker rewrite the catalog: the picker,
not the file, is the source of truth for titles and order.
