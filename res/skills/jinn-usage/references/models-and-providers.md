# Models & Providers

jinn talks to Anthropic, Google, and any OpenAI-compatible API (including
OpenRouter). Providers and models are declared in `providers.toml`; the
active model is chosen per session at runtime.

## Switch the session's model

1. Press `<leader>sm` (normal mode) to open the model picker.
2. Filter by typing, `<up>`/`<down>` to select, `<enter>` to confirm.
3. `<c-r>` refreshes the model list from the provider; pick up newly
   released models mid-session.
4. `<Tab>` multi-selects. Combine models from any mix of providers into an
   **alloy**, a composite "model" the session routes through. `<c-a>`
   toggles alloy mode. Alloys round-trip through config and session
   persistence; they're stored like any other model selection.

The choice persists with the session. The last-selected model is remembered
across launches.

## The OpenRouter model market

`<leader>so` opens a searchable table of OpenRouter's catalog with per-model
pricing and performance columns. ECI (Epoch Capabilities Index, from Epoch
AI — a snapshot bundled at build time) and VALS (Vals Index, from Vals AI —
fetched live on open and cached on disk) are the performance columns. `l`/`h`
cycle the sort column, `s` reverses it, `<enter>` switches the session to the
highlighted model (and pins OpenRouter endpoint routing from detail mode), `m`
toggles the pricing mix, `O` opens the highlighted model's openrouter.ai page
in your OS default browser (in detail mode it opens the pinned model's page),
`<c-r>` refetches everything, `?` shows the help card.

## Reasoning effort

`<leader>sr` opens the reasoning-effort picker for models that support it.
Levels are none/minimal/low/medium/high/xhigh/max-style, depending on the
model.

## OpenRouter endpoint pinning

OpenRouter serves each model through multiple upstream **endpoints** (routing
tags) with different pricing and uptime. Pinning one endpoint improves
prefix-cache hits:

1. Select an OpenRouter-served single model.
2. `<leader>sE` opens the endpoint picker; `<c-r>` fetches/refreshes the
   endpoint list for that model (listings are cached in memory for the
   session's lifetime).
3. `<enter>` pins an endpoint. Requests then go to that endpoint alone;
   that's what maximizes KV-cache hits. `<enter>` on the "Default
   (auto-route)" row clears the pin.

A pin is a **per-model default**: confirming writes a `[[endpoint_defaults]]`
row into `providers.toml`, so the choice applies to that model in every
session, including sessions you create later. The block is hand-editable; a
model absent from it auto-routes. `providers.toml` edits apply when the next
stream opens, so a hand-edit of this row reaches every session using that
model from its next request. A pin made in the picker takes effect on the
session's next request too.

Scope rules: a pin applies only to a **single (non-alloy) model served via
OpenRouter**; alloys and every other backend keep their usual auto-routing.

## Provider configuration (`providers.toml`)

Lives at `~/.config/jinn/providers.toml` (API keys, base URLs, model
catalogs). Providers are map-keyed tables: list them in any order;
duplicate names are rejected. Example shape:

```toml
[providers.openrouter]
# provider-level settings: api key env/name, base url, default model info...
```

Model metadata can be set per model in `[[providers.<name>.model_info]]`
tables (context length, image support, extra request body). Precedence for
model metadata: per-model config > provider-block config > API-discovered
cache > models.dev.

`providers.toml` stays hand-authored: discovered models are served from the
API-discovered cache, and the endpoint picker is the one automated writer;
it adds your `[[endpoint_defaults]]` row when you confirm a pin and deletes
it when you choose auto-route. Everything else in the file is left alone,
comments included. **Edits apply when the next stream opens.** A whole-file
syntax error aborts launch (fail-fast, with a legible error); an edit made
while running is refused and the previous registry stays in effect.

See `configuration.md` for the full config-file map and the offer-to-edit
protocol.

## The web UI's model picker

The title bar's model chip opens the web UI's model picker: a filter bar
over rows the TUI picker's own builder made, a row click to switch, and a
**Refresh models** button that reruns discovery. Alloys and multi-select
are the TUI's. `webui.md` → [What it does](webui.md#what-it-does), the
**Model** entry.
