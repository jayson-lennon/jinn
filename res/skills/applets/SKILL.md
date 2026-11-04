---
name: applets
description: Save an agent-built HTML page as a durable applet — a small standalone web page jinn serves and the user reopens any time. Build a fresh applet from scratch, or promote the current canvas artifact by copying it onto the shelf of saved applets. Listed in the `<leader>sa` picker. Use when the user wants to "keep this", "save this app/applet", "make it persistent", or reopen a page later without asking you to rebuild it — but not for ordinary "build me an app/CLI/web app" requests, which are just coding tasks.
---

# Applets — saved pages the user can reopen

An **applet** is a snapshot of an agent-built page in
`~/.local/share/jinn/applets/<id>/`, served by jinn's applets server on
loopback and listed in the `<leader>sa` picker, where `ENTER` opens it
in the user's browser any time — even after the session that built it
is gone.

An applet is a *saved, re-openable* page — a small standalone web page
hosted by jinn. Ordinary app-building requests ("make me a web app")
are just coding tasks and do not involve applets; only saving and
re-opening pages does.

## Workflow 1 — build an applet

1. **Build the page** as a self-contained HTML file, following the
   canvas skill's authoring rules (self-contained file, `/vendor/`
   library paths only, no network fetches, inline your data). Do **not**
   serve it with the `canvas` tool — the saved applet is how the user
   opens it.
2. **Choose an id and a title**. The id is the directory name and URL
   segment: lowercase letters, digits, and hyphens (`sales-dashboard`,
   `ph-equilibrium-lab`). The title is what the picker shows (`Sales
   Dashboard`).
3. **Save the artifact directory** under
   `~/.local/share/jinn/applets/<id>/`: the entry page must be
   `index.html`; siblings (data JSON, extra CSS) sit beside it and are
   served at `<id>/<sibling>`.
4. **Register the applet** by adding its entry to
   `~/.local/share/jinn/applets/applets.json` (create the file if
   missing):

   ```json
   {
     "entries": [
       {
         "id": "sales-dashboard",
         "title": "Sales Dashboard",
         "created_at": "<current UTC time, ISO 8601>"
       }
     ]
   }
   ```

   Keep the array sorted newest-first (highest `created_at` first).
5. **Tell the user**: the applet is saved; open it with `<leader>sa` →
   `ENTER` in the picker. Do not paste a URL — the server binds lazily
   on the first open, so the port does not exist until then.

## Workflow 2 — promote the current canvas

1. **Locate the canvas artifact** you are serving: the path from the
   original `canvas` tool call. The artifact is an HTML file; any
   siblings it fetches sit beside it in the same directory.
2. **Choose an id and a title**. The id is the directory name and URL
   segment: lowercase letters, digits, and hyphens (`sales-dashboard`,
   `ph-equilibrium-lab`). The title is what the picker shows (`Sales
   Dashboard`).
3. **Copy the artifact directory** to
   `~/.local/share/jinn/applets/<id>/`, renaming the entry HTML file to
   `index.html`. Copy every sibling the page fetches; siblings are
   served at `<id>/<sibling>`. The copy is a **snapshot**: later edits
   to the canvas file never change the saved applet, and the saved
   applet will not live-reload the way the canvas tab does — say so
   when you promote.
4. **Register the applet** by adding its entry to
   `~/.local/share/jinn/applets/applets.json` (create the file if
   missing):

   ```json
   {
     "entries": [
       {
         "id": "sales-dashboard",
         "title": "Sales Dashboard",
         "created_at": "<current UTC time, ISO 8601>"
       }
     ]
   }
   ```

   Keep the array sorted newest-first (highest `created_at` first).
5. **Tell the user**: the applet is saved; open it with `<leader>sa` →
   `ENTER` in the picker. If the canvas artifact improves later and the
   user wants the improvements, re-do the promotion (a fresh copy), not
   an edit in place.

## Rules

- **Self-contained directories.** Everything the page fetches must be in
  the applet directory or under `/vendor/…` (same offline library
  inventory as canvas). No remote URLs in `src`/`href`/`fetch`.
- **`applets.json` is the catalog of record for titles and order.** A
  directory without a row is not listed. An entry whose directory is
  gone disappears at the next picker open (jinn reconciles).
- **The id is constrained**: `[a-z0-9-]+`, no dots, no slashes, not
  `applets.json` (that file is never served as an applet). A malformed
  entry is silently dropped by jinn — check your id format.
- **Never write applet files outside `~/.local/share/jinn/applets/`.**
  Never edit the picker, server, or bind config by hand; `[applets]
  bind` in `jinn.toml` configures the bind and needs a jinn restart.
- The user's own renames and deletes (`<c-r>` / `<c-d>` in the picker)
  rewrite `applets.json`; do not assume your last write is still current
  if the user has touched the picker since.
- **When unsure the page is ready, test it as a canvas artifact first**
  (that workflow has live reload), then promote once it works.
