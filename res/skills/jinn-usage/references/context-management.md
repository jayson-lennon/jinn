# Context Management

jinn gives you direct control over what enters the LLM's context window. Every
chat entry (user message, assistant reply, tool call, tool result) can be
pinned, excluded, or reset individually — and background workers prune
automatically. All bindings below are normal mode unless stated.

## Selecting an entry

- `j` / `k` move the selection down / up the chat history.
- Jumps: `]u`/`[u` (user messages), `]p`/`[p` (pinned), `]c`/`[c`
  (compaction summaries), `]s`/`[s` (Sources annotations), `gg`/`G` (top/bottom).

## Pinning an entry

Pinned entries are **always** included in the LLM context — auto-prune workers
and compaction skip them. Use pinning for durable facts: a plan, API contract,
naming decision, or correction you want enforced for the rest of the session.

1. Focus the chat history (normal mode) and select the entry with `j`/`k`
   (or a `]`/`[` jump).
2. Press `p` to pin it.

Pins apply to a whole **tool loop as a unit** — pinning a tool call also pins
its matching tool result, so the pair never desyncs in context.

### Choosing where a pin sits

Position matters: entries nearer the end of the context carry more recency
weight and pinning mid-history can shift how the model reads subsequent
messages. To reposition:

1. Focus the sidebar with `<c-l>`, then `J`/`K` until the **Pins** section is
   highlighted (or jump straight there and browse).
2. `j`/`k` to select the pin, then:
   - `t` — move to the **top** of context (strongest "always in view" signal).
   - `b` — move to the **bottom** of context (highest recency weight, closest
     to the newest messages).
   - `r` — move to a **relative** position (near where the entry sits in
     history).
   - `m` — **cycle** through positions.
3. `<enter>` (or `<esc>`/`<c-h>`) leaves the sidebar and returns to the chat,
   restoring your history position.

**Implications of each position:**
- *Top*: the instruction reads like a standing system rule; good for
  project-wide constraints. May be "far away" from the current turn.
- *Bottom*: maximum salience for the next reply; good for "do X now"
  directives. Gets pushed up as the conversation grows.
- *Relative*: keeps the instruction in its original conversational setting;
  good for comments that explain a specific block of history.

### Jumping among pins

`]p` / `[p` snap the selection to the next / previous pinned entry without
opening the sidebar.

## Excluding an entry

- `x` toggles the selected entry **out of** the LLM context ("ignore"). Great
  for "I sent a message but changed my mind" — no context poisoning.
- `r` resets the entry to default handling (auto-prune workers decide again).
- Excluded entries stay visible in the TUI (collapsed, dimmed by default);
  `h` toggles visibility of the collapsed blocks.
- Like pinning, `x` applies to a tool call + result as one unit.

## Isolate a single exchange: `gci`

Made a plan (or got a correction) and want to act on **only** that? Select the
entry and press `gci` (a sequence: `g`, `c`, `i`). jinn force-includes that
entry's tool loop and user-force-excludes every other non-pinned entry, while
pins remain untouched. The result is a fresh-feeling context that still
contains exactly the plan. Undo per-entry with `x`/`r`, or rebuild with a new
session (`n`).

## Background pruning and compaction

jinn prunes stale detail automatically so context stays small and prefix-cache
friendly; compaction (summarizing) exists only as a backstop and should almost
never fire during coding. Related tuning lives in `jinn.toml`:

- `[auto_prune.*]` — per-strategy rules (stale reads, repeated edits, old tool
  output, trivial assistant chatter, regex rules...), each with `enabled`,
  `min_age`, and per-strategy thresholds.
- `[auto_prune] accumulation_threshold_tokens` — prune context-mutations are
  batched until this token budget accumulates, protecting prefix-cache hits.
- `[compaction]` — `threshold` (usage fraction that triggers compaction),
  `reserve_tokens`, `fallback_context_window`, and an optional compaction
  `model`.

See `configuration.md` for how to edit these and the restart caveat.

## Related bindings

| Key | Action |
| --- | --- |
| `e` | Expand / collapse the selected tool entry |
| `a` | Toggle the audit popup for the selected entry |
| `y` | Yank the selected entry's raw content |
| `gcp` | Set the pruner accumulation threshold interactively |
