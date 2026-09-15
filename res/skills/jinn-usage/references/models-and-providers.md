# Models & Providers

jinn talks to Anthropic, Google, and any OpenAI-compatible API (including
OpenRouter). Providers and models are declared in `providers.toml`; the
active model is chosen per session at runtime.

## Switching the session's model

1. Press `<leader>sm` (normal mode) to open the model picker.
2. Filter by typing, `<up>`/`<down>` to select, `<enter>` to confirm.
3. `<c-r>` refreshes the model list from the provider (pick up newly released
   models without restarting).
4. `<Tab>` multi-selects: combine models from any mix of providers into an
   **alloy** — a composite "model" the session routes through. `<c-a>`
   toggles alloy mode. Alloys round-trip through config and session
   persistence; they're stored like any other model selection.

The choice persists with the session. The last-selected model is remembered
across launches.

## Reasoning effort

`<leader>sr` opens the reasoning-effort picker for models that support it
(minimal/low/medium/high-style levels, depending on the model).

## OpenRouter endpoint pinning

OpenRouter serves each model through multiple upstream **endpoints** (routing
tags) with their own pricing and uptime. For prefix-cache affinity you can pin
one:

1. Select an OpenRouter-served single model.
2. `<leader>sE` opens the endpoint picker; `<c-r>` fetches/refreshes the
   endpoint list for that model (listings are cached in memory for the
   session's lifetime).
3. `<enter>` pins an endpoint. From then on requests force that endpoint and
   disable fallbacks, maximizing KV-cache hits.

Scope rules: a pin applies only to a **single (non-alloy) model served via
OpenRouter**; it's ignored for alloys and every other backend. The pin is
per-session.

## Compaction model

`<leader>sc` picks the provider/model used for compaction summaries,
independent of the session model (unset = compaction uses the session's
model). Also settable in `jinn.toml` — see `configuration.md`.

## Provider configuration (`providers.toml`)

Lives at `~/.config/jinn/providers.toml` (API keys, base URLs, model
catalogs). Providers are map-keyed tables — order in the file carries no
meaning, duplicate names are rejected. Example shape:

```toml
[providers.openrouter]
# provider-level settings: api key env/name, base url, default model info...
```

Model metadata can be set per model in `[[providers.<name>.model_info]]`
tables (context length, image support, extra request body). Precedence for
model metadata: per-model config > provider-block config > API-discovered
cache > models.dev. `providers.toml` is hand-authored only — jinn never
writes discovered models into it. **Edits require a restart**; a whole-file
syntax error aborts launch (fail-fast, with a legible error).

See `configuration.md` for the full config-file map and the offer-to-edit
protocol.
