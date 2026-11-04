---
name: apps
description: Save an agent-built HTML page as a durable app the user can reopen any time — a fresh artifact saved from scratch, or the current canvas artifact promoted to a saved app. The saved app is served verbatim on loopback and listed in the `<leader>sa` picker. Use when the user wants to "keep this", "save this app", "make it persistent", or reopen a page later without asking you to rebuild it.
---

# Apps — saved artifacts the user can reopen

An **app** is a snapshot of an agent-built page in
`~/.local/share/jinn/apps/<id>/`, served verbatim by jinn's apps server
(loopback only, **no script injection** — no reload, no telemetry) and
listed in the `<leader>sa` picker, where `ENTER` opens it in the user's
browser any time, even after the session that built it is gone.

Two workflows:

1. **Build an app** — a fresh page saved directly as an app.
2. **Promote the canvas** — the user liked a canvas artifact and wants
   it to survive the session. Promotion is a **file copy**: the saved
   app is a snapshot, not a symlink. Evolving it is a re-save, not an
   auto-sync.

## Workflow 1 — build an app

1. **Build the page** as a self-contained HTML file, following the
   canvas skill's authoring rules (self-contained file, `/vendor/`
   library paths only, no network fetches, inline your data). Do **not**
   serve it with the `canvas` tool — that is a different server with
   different behavior (it injects reload + telemetry; the apps server
   must not).
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

1. Locate the canvas artifact you are serving (the path from the
   original `canvas` tool call) and its directory.
2. Copy the artifact **directory** (the HTML plus any siblings it
   fetches) to `~/.local/share/jinn/apps/<id>/index.html`, same rules as
   workflow 1.
3. Register it in `apps.json` exactly as in workflow 1. The canvas file
   keeps its reload/telemetry snippets in the CWD original; the **copy**
   is what gets served, and the apps server never injects or strips —
   the served bytes are whatever you copied. That means the promoted app
   has **no live reload and no click tracking**; it is a static
   snapshot. Say so when you promote.
4. If the user later wants the improvements: re-do the promotion after
   editing the canvas artifact (a fresh copy), not an edit in place.

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
