# Context Management

You control what enters the LLM's context window. Every chat entry (user
message, assistant reply, tool call, tool result) can be pinned, excluded,
or reset individually, and background workers prune the rest automatically.
All bindings below are normal mode unless stated.

## Select an entry

- `j` / `k` move the selection down / up the chat history.
- **Jumps land on kinds of entries**: `]u`/`[u` user messages, `]p`/`[p`
  pinned, `]c`/`[c` compaction summaries, `]s`/`[s` Sources annotations,
  `gg`/`G` top/bottom. Each `]` jumps forward and its `[` partner backward.
  Jumps clamp at the ends of history.

## Pin an entry

**A pinned entry is always in context.** Auto-prune workers and compaction
skip it. Pin durable facts: a plan, an API contract, a naming decision, a
correction you want enforced for the rest of the session.

1. Select the entry with `j`/`k` (or a `]`/`[` jump).
2. Press `p`. Pressing `p` on a pinned entry unpins it.

**A pin takes the whole tool loop.** Pinning a tool call pins its matching
result too, so the pair stays intact in context.

### Choose where the pin sits

Position matters. Entries nearer the end of context carry more weight in the
next reply, and a pin dropped mid-history can change how the model reads the
messages after it. To reposition:

1. Press `<c-l>` for the sidebar, then `J`/`K` until **Pins** is highlighted
   (or jump straight there and browse).
2. Select a pin with `j`/`k`, then:
   - `t`: **top** of context. Strongest "always in view" signal.
   - `b`: **bottom** of context. High recency weight, and it splices in
     before the trailing tool run / final turn, which keeps the loop whole.
   - `r`: **relative** position, near where the entry sits in history.
   - `m`: **cycle** through positions.
   - `u`: unpin the selected entry.
3. `<enter>` (or `<esc>`/`<c-h>`) leaves the sidebar and returns to the chat,
   restoring your history position.

**What each position buys:**
- *Top*: reads like a standing rule. Best for project-wide constraints; far
  from the current turn.
- *Bottom*: among the newest messages, spliced in ahead of the trailing
  tool run / final assistant turn, so the loop stays whole and the pin sits
  in the highest-weight material of the next reply. Best for "do X now".
- *Relative*: stays beside the history it explains. Best for comments on a
  specific block.

### Jump among pins

`]p` / `[p` snap the selection to the next / previous pinned entry, straight
from the chat view.

## Exclude an entry

- **`x` toggles an entry out of context and back in** (`ForcedExclude` ↔
  `ForcedInclude`), advancing the cursor each press. Use it on a message you
  want hidden from the model: the text stays visible in the TUI and stays
  out of context. Hold `x` and the captured state sweeps across successive
  entries (presses within 100ms), skipping pinned and collapsed entries.
- `r` resets the entry to default handling: the auto-prune workers decide
  again.
- **Excluded entries stay visible**, collapsed and dimmed by default; `h`
  toggles the collapsed blocks.
- **`x` takes the loop too**: a tool call and its result toggle as one unit,
  like pinning.

## Isolate a single exchange: `gci`

Act on only one entry (a plan, a correction): select it and press `gci`
(`g`, `c`, `i`). jinn force-includes that entry's tool loop and
force-excludes every other entry, with pins exempt. The context is the
isolated exchange plus your pins. Pressing `p` right after (`gcip`) also
pins the isolated entry. Undo per-entry with `x`/`r`, or start a new session
(`n`).

## Tune pruning and compaction

jinn prunes stale detail automatically, keeping context small and
prefix-cache friendly. Compaction (summarizing the history) is the
backstop; during coding, pruning keeps usage below the threshold, so
compaction rarely fires. The tuning lives in `jinn.toml`:

- `[context_curation.auto_prune.*]`: per-strategy rules (stale reads,
  repeated edits, old tool output, trivial assistant chatter, regex
  rules...), each with `enabled`, `min_age`, and per-strategy thresholds.
- `[context_curation.auto_prune] accumulation_threshold_tokens`: prune
  mutations batch until this token budget accumulates, protecting
  prefix-cache hits.
- `[context_curation.compaction]`: `threshold` (usage fraction that
  triggers compaction), `reserve_tokens`, `fallback_context_window`, and an
  optional compaction `model`.

See `configuration.md` for how to edit these; they are re-read at next
use, so an edit applies by the next compaction.

## Related bindings

| Key | Action |
| --- | --- |
| `e` | Expand / collapse the selected tool entry |
| `a` | Toggle the audit popup for the selected entry |
| `y` | Yank the selected entry's content (a tool call yanks its arguments; a tool result yanks its full untruncated content) |
| `gcp` | Set the pruner accumulation threshold interactively |

## Audit an entry (`a`)

`a` on a selected entry opens a read-only popup showing how that entry was
produced. When its model stamps resolve, a `Model:` line names the concrete
model that generated it. If that model differs from the model the session
requested at dispatch time (OpenRouter's `auto` routing strategy is the
usual cause), an indented `via <requested>` line renders beneath it. With
unresolved stamps (unpaired or unstamped), the popup omits the model line.

## Pins in the sidebar

The **Pins** section shows one row per standalone pinned entry, one row per
completed tool **result** of a pinned loop, and one row per tool call still
missing its result. Pin a saved plan or a skill load and the loop
contributes one row; pin state still applies to the whole loop.
