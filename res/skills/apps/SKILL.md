---
name: apps
description: Save an agent-built HTML page as a durable app the user can reopen any time — a fresh artifact saved from scratch, or the current canvas artifact promoted to a saved app. The saved app is listed in the `<leader>sa` picker and served on loopback. Use when the user wants to "keep this", "save this app", "make it persistent", or reopen a page later without asking you to rebuild it.
---

# Apps — saved artifacts the user can reopen

An **app** is a snapshot of an agent-built page in
`~/.local/share/jinn/apps/<id>/`, served by jinn's apps server on
loopback and listed in the `<leader>sa` picker, where `ENTER` opens it
in the user's browser any time — even after the session that built it
is gone.

## Workflow 1 — build an app

1. **Build the page** as a self-contained HTML file, following the
   canvas skill's authoring rules (self-contained file, `/vendor/`
   library paths only, no network fetches, inline your data). Do **not**
   serve it with the `canvas` tool — the saved app is how the user
   opens it.
2. **Choose an id and a title**. The id is the directory name and URL
   segment: lowercase letters, digits, and hyphens (`sales-dashboard`,
   `ph-equilibrium-lab`). The title is what the picker shows (`Sales
   Dashboard`).
3. **Save the artifact directory** under `~/.local/share/jinn/apps/<id>/`:
   the entry page must be `index.html`; siblings (data JSON, extra CSS)
   sit beside it and are served at `<id>/<sibling>`.
4. **Register the app** by adding its entry to
   `~/.local/share/jinn/apps/apps.json` (create the file if missing):

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
5. **Tell the user**: the app is saved; open it with `<leader>sa` →
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
3. **Copy the artifact directory** to `~/.local/share/jinn/apps/<id>/`,
   renaming the entry HTML file to `index.html`. Copy every sibling the
   page fetches; siblings are served at `<id>/<sibling>`. The copy is a
   **snapshot**: later edits to the canvas file never change the saved
   app, and the saved app will not live-reload the way the canvas tab
   does — say so when you promote.
4. **Register the app** by adding its entry to
   `~/.local/share/jinn/apps/apps.json` (create the file if missing):

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
5. **Tell the user**: the app is saved; open it with `<leader>sa` →
   `ENTER` in the picker. If the canvas artifact improves later and the
   user wants the improvements, re-do the promotion (a fresh copy), not
   an edit in place.

## Rules

- **Self-contained directories.** Everything the page fetches must be in
  the app directory or under `/vendor/…` (same offline library inventory
  as canvas). No remote URLs in `src`/`href`/`fetch`.
- **`apps.json` is the catalog of record for titles and order.** A
  directory without a row is not listed. An entry whose directory is
  gone disappears at the next picker open (jinn reconciles).
- **The id is constrained**: `[a-z0-9-]+`, no dots, no slashes, not
  `apps.json` (that file is never served as an app). A malformed entry
  is silently dropped by jinn — check your id format.
- **Never write app files outside `~/.local/share/jinn/apps/`.** Never
  edit the picker, server, or bind config by hand; `[apps] bind` in
  `jinn.toml` configures the bind and needs a jinn restart.
- The user's own renames and deletes (`<c-r>` / `<c-d>` in the picker)
  rewrite `apps.json`; do not assume your last write is still current if
  the user has touched the picker since.
- **When unsure the page is ready, test it as a canvas artifact first**
  (that workflow has live reload), then promote once it works.
