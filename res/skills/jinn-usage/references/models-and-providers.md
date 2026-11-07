# Models & Providers

jinn talks to Anthropic, Google, and any OpenAI-compatible API (including
OpenRouter). Providers and models are declared in `providers.toml`; the
active model is chosen per session at runtime.

## Switch the session's model

1. Press `<leader>sm` (normal mode) to open the model picker.
2. Filter by typing, `<up>`/`<down>` to select, `<enter>` to confirm.
3. `<c-r>` refreshes the model list from the provider — pick up newly
   released models without restarting.
4. `<Tab>` multi-selects. Combine models from any mix of providers into an
   **alloy** — a composite "model" the session routes through. `<c-a>`
   toggles alloy mode. Alloys round-trip through config and session
   persistence; they're stored like any other model selection.

The choice persists with the session. The last-selected model is remembered
across launches.

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
3. `<enter>` pins an endpoint. Requests then force that endpoint and disable
   fallbacks — that's what maximizes KV-cache hits. `<enter>` on the "Default
   (auto-route)" row clears the pin.

A pin is a **per-model default**, not per-session state. Confirming writes a
`[[endpoint_defaults]]` row into `providers.toml`, so the choice applies to
that model in every session — including sessions you create later. The block
is hand-editable; no row for a model means that model auto-routes.
`providers.toml` edits apply when the next stream opens, so a hand-edit of
this row reaches every session using that model from its next request. A
pin made in the picker takes effect on the session's next request too.

Scope rules: a pin applies only to a **single (non-alloy) model served via
OpenRouter**. It's ignored for alloys and every other backend.

## Compaction model

The summarizer model is configured in `jinn.toml` only — there is no picker:

```toml
[context_curation.compaction]
# model = "openrouter/anthropic/claude-sonnet-4"
```

When unset, compaction summaries use the session's own model.

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
cache > models.dev.

`providers.toml` is hand-authored — jinn never writes discovered models into
it. One exception: the endpoint picker writes your `[[endpoint_defaults]]`
row when you confirm a pin, and deletes it when you choose auto-route.
Everything else in the file is left alone, comments included. **Edits apply
when the next stream opens.** A whole-file syntax error aborts launch
(fail-fast, with a legible error); an edit made while running is refused
and the previous registry stays in effect.

See `configuration.md` for the full config-file map and the offer-to-edit
protocol.
