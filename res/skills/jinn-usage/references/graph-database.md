# Graph + vector database (overgraph)

jinn bundles **overgraph**, an embedded graph database that speaks GQL and
supports semantic (vector) search over its nodes. It ships as a builtin MCP
server — configured out of the box, disabled per session until enabled. See
`mcp-servers.md` for the shipped `[mcp.overgraph]` block and how enabling
works. Once enabled, its tools are available as `mcp__overgraph__graph_<name>`:

| Tool                  | What it's for                                                                                                                                   |
| --------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------- |
| `graph_query`         | Run GQL: reads (`MATCH`/`RETURN`/`WHERE`), writes (`CREATE`, `MERGE`, `SET`, `DELETE`), schema DDL (`SHOW`, `ALTER`, `CHECK`, property indexes) |
| `graph_upsert_node`   | Create or update one node by `(label, key)`; `props` replace existing; `embed_text` makes the node findable by semantic search                  |
| `graph_upsert_edge`   | Create an edge between two existing nodes; a repeat call updates that edge's props and weight                                                   |
| `graph_vector_search` | Nearest-neighbor search by meaning over nodes carrying an embedding (`k` hits, default 10, max 100; optional `labels` filter)                   |
| `graph_ingest_file`   | Bulk-load a JSONL batch file of nodes and edges in one call — the move for any dataset bigger than a handful of records                         |
| `graph_docs`          | The bundled GQL reference, whole or by `section`; call it before writing queries you are unsure of                                              |

Data persists under a jinn-managed directory across sessions.

**Separate graphs.** The bundled entry is one shared database. Want a custom
one? Add a new `[mcp.<name>]` entry in `jinn.toml` — jinn can add it for you
via the configuration protocol (`configuration.md`) — that passes the `--db`
flag:

```toml
[mcp.project-graph]
command = "<jinn>"
args = ["mcp", "overgraph", "--db", "/home/me/graphs/project-x"]
```

…then enable that entry as its own MCP server. Each entry with its own
`--db` is a fully separate graph, so per-project and per-user graphs are
just more entries. Use absolute paths.

## Four ways to use it

### 1. Agent / user memory

Store entities as nodes — each entity type is a label, the unique name is
the key, atomic facts are properties — and connect them with typed edges in
active voice. Identity is `(label, key)`: re-upserting updates in place,
never duplicates. Memory edits are just re-upserts. A turn's worth of
updates goes in one batch file (see [Batch ingestion](#batch-ingestion)):

```jsonc
// memory.jsonl
{"node": {"label": "Person", "key": "jayson", "props": {"name": "Jayson", "role": "maintainer"}}}
{"node": {"label": "Project", "key": "jinn", "props": {"name": "jinn", "language": "Rust"}}}
{"node": {"label": "Preference", "key": "editor", "props": {"value": "vim bindings"}}}
{"edge": {"label": "MAINTAINS", "from": "Person:jayson", "to": "Project:jinn", "props": {"since": 2024}}}
{"edge": {"label": "PREFERS", "from": "Person:jayson", "to": "Preference:editor", "weight": 0.8}}
```

"Everything about X" is one traversal:

```gql
MATCH (p:Person {elementKey: 'jayson'})-[r]-(other)
RETURN type(r) AS relation, labels(other) AS other_label, elementKey(other) AS other_key, other
```

For "where did I put that", give the entity a searchable phrase via
`embed_text` (`"prefers vim keybindings in the editor"`) — then
`graph_vector_search` finds it by meaning rather than by exact key.

### 2. Document corpus Q&A

Model a corpus as `Doc` → `HAS_CHUNK` → `Chunk` nodes, with `MENTIONS` edges
to `Concept` nodes. The chunk text goes in `embed_text`; the edges carry the
structure the vectors can't see. bge-small-en-v1.5 truncates at 512 tokens,
so chunk to ~400 tokens (with a sentence or two of overlap), and prepend a
context line (doc title + section) so a bare chunk still embeds meaningfully:

```jsonc
// docs.jsonl
{"node": {"label": "Doc", "key": "design/overgraph", "props": {"title": "Overgraph design"}, "embed_text": "Overgraph design: embedded property graph engine with WAL, MVCC snapshot reads"}}
{"node": {"label": "Chunk", "key": "design/overgraph#wal", "props": {"order": 2, "text": "The WAL is replayed on open; group commit batches fsyncs"}, "embed_text": "Overgraph design, WAL section: WAL replay on open, group commit batching fsyncs"}}
{"edge": {"label": "HAS_CHUNK", "from": "Doc:design/overgraph", "to": "Chunk:design/overgraph#wal", "props": {"position": 2}}}
{"node": {"label": "Concept", "key": "wal", "embed_text": "write-ahead log durability and replay"}}
{"edge": {"label": "MENTIONS", "from": "Chunk:design/overgraph#wal", "to": "Concept:wal"}}
```

Retrieval is a two-call hybrid. Vector search finds the entry point;
traversal builds the answer:

```
1. graph_vector_search { "query": "how does the database recover after a crash?" }
   → hit: Chunk:design/overgraph#wal  (key + labels come back with the score)
2. graph_query:
   MATCH (c:Chunk {elementKey: 'design/overgraph#wal'})-[:MENTIONS]->(k:Concept)
   MATCH (k)<-[:MENTIONS]-(sibling:Chunk)
   RETURN sibling.text, elementKey(sibling) AS related
```

### 3. Temporal facts

Edges carry `valid_from`/`valid_to` epoch millis. A new fact is a new edge;
an old fact's edge gets a `valid_to` (also writable in GQL:
`SET validTo(r) = ...`). "What was true at time T" is then a query:

```jsonc
// roles.jsonl — jayson led the team until March, then Alice did
{"edge": {"label": "LED", "from": "Person:jayson", "to": "Team:platform", "valid_from": 1704067200000, "valid_to": 1709251200000}}
{"edge": {"label": "LED", "from": "Person:alice", "to": "Team:platform", "valid_from": 1709251200000}}
```

```gql
-- who led platform as of Feb 2024? (1707206400000)
MATCH (p:Person)-[r:LED]->(t:Team {elementKey: 'platform'})
WHERE validFrom(r) <= 1707206400000 AND coalesce(validTo(r), 9223372036854775807) > 1707206400000
RETURN elementKey(p), validFrom(r), validTo(r)
```

`validTo` defaults to "no expiry" — hence the `coalesce` against `i64::MAX`.

### 4. Hybrid retrieval

The general recipe behind pattern 2. **The vector layer finds entry points;
the graph layer builds the answer.** Vector search returns things that _mean
like_ the query. Traversal returns things _connected to_ those things. When
a question needs both — find by meaning, then use relationships — do the
two-step:

1. `graph_vector_search` → take `key` + `labels` from the hits.
2. `graph_query` with a labeled `MATCH` on that key → walk the edges that
   answer the actual question (siblings, owners, timelines).

## Batch ingestion

For any dataset bigger than a handful of records, don't call
`graph_upsert_node`/`graph_upsert_edge` per record. Write a conversion
script that emits a JSONL file, then call `graph_ingest_file` **once per
file** — pay for one call, not a thousand:

```jsonc
// graph_ingest_file { "path": "/home/me/notes.jsonl", "batch_size": 512 }
```

**The file contract.** Each line is a JSON object with exactly one of
`"node": {...}` or `"edge": {...}`. Node lines: `label`, `key`, optional
`props`/`weight`/`embed_text` (embedded locally, like `graph_upsert_node`).
Edge lines: `label`, `from`, `to` plus optional `props`/`weight`/`valid_from`/`valid_to`;
`from`/`to` are `Label:key` strings (split at the first colon, so keys may
contain colons — labels may not). The engine enforces the contract:

- **Line order is free.** All nodes are upserted before any edge endpoint
  resolves, so an edge may reference a node declared later in the file or
  already in the database.
- **Abort names the line.** An edge whose endpoint resolves to nothing
  aborts the whole file with zero writes, and the response names the missing
  endpoints with their line numbers. No placeholder nodes are created.
- **Re-runs are idempotent.** Upserts key on `(label, key)` and edges are
  unique per `(from, to, label)`, so a crashed or partial run is repaired by
  re-running the same file.
- **`batch_size`** caps each engine write batch (default 512, max 4096).
  Progress lines go to the server's stderr, visible in the picker's preview
  pane (`<c-t>` in the MCP server picker).

### Generating the file

Three shapes cover most sources. **CSV → JSONL** (bash + jq):

```bash
# people.csv: id,name,title,manager_id   (fields must not contain commas)
# Node line and edge line are emitted separately — a line carries one or the other.
tail -n +2 people.csv | jq -c -R -s '
  split("\n") | map(select(length > 0) | split(",")) | .[]
  | {node: {label: "Person", key: .[0], props: {name: .[1], title: .[2]}}}
  , (select(.[3] != "") | {edge: {label: "REPORTS_TO", from: ("Person:" + .[3]), to: ("Person:" + .[0])}})
' > people.jsonl
```

**Notes directory → JSONL** (one node per file, text as `embed_text`):

```bash
for f in notes/*.md; do
  key=$(basename "$f" .md)
  printf '{"node": {"label": "Note", "key": "%s", "embed_text": %s}}\n' \
    "$key" "$(jq -R -s . < "$f")"
done > notes.jsonl
```

**SQLite → JSONL** (Python — the right tool when the source is a query):

```python
import json, sqlite3
con = sqlite3.connect("tracker.db")
for ticket_id, title, status, assignee in con.execute(
        "SELECT id, title, status, assignee FROM tickets"):
    print(json.dumps({"node": {"label": "Ticket", "key": str(ticket_id),
        "props": {"title": title, "status": status},
        "embed_text": f"{title} ({status})"}}))
    if assignee:
        print(json.dumps({"edge": {"label": "ASSIGNED_TO",
            "from": f"Ticket:{ticket_id}", "to": f"Person:{assignee}"}}))
```

Then ingest:

```jsonc
// graph_ingest_file { "path": "/home/me/people.jsonl" }
```

When the abort names a line, fix that line in the file (or create the node
it points at) and re-run. The re-run is idempotent, so re-upserting the
already-written records is harmless.

## Pick the right tool

| The question looks like…                 | Reach for                                                                                                           |
| ---------------------------------------- | ------------------------------------------------------------------------------------------------------------------- |
| Exact key, label, or property filter     | `graph_query` alone — no vectors involved                                                                           |
| Find by meaning, structure irrelevant    | `graph_vector_search` alone                                                                                         |
| Find by meaning, then use relationships  | The two-step hybrid (default for corpus questions)                                                                  |
| Exact dates, names, code identifiers     | GQL property filters, **not** vectors — embeddings are unreliable on exact strings, and there is no full-text index |
| "How are X and Y connected?" (many hops) | `graph_query` with bounded paths / `shortestPath`                                                                   |

## Gotchas

- **A function call is engine metadata; a dot access is a user property.**
  Read the key with `elementKey(n)`, the weight with `weight(n)`/`weight(r)`,
  validity with `validFrom(r)`/`validTo(r)`. `n.key` and `n.weight` are
  plain user properties — null unless you set them yourself. The one legal
  dot-free-looking form is `elementKey` inside a `MATCH`/`MERGE`/`CREATE`
  pattern map (`{elementKey: 'ada'}`).
- **No unlabeled scans.** Reads run with `allow_full_scan: false`, so every
  `MATCH` needs a label — `MATCH (n)` is rejected. This is the most common
  first-query error.
- **`embed_text` truncates at 512 tokens.** Longer text loses its tail in
  the embedding, with no warning. Chunk before ingesting, not after.
- **Pass absolute paths.** For `--db` in a custom entry and for
  `graph_ingest_file`'s `path`: both resolve against the server process's
  working directory, not yours.
- **Embedding model lifecycle.** The first embed downloads ~35MB
  (bge-small-en-v1.5, 384-dim cosine); pre-fetch it headlessly with
  `jinn mcp overgraph --fetch-model` if desired. The dimension locks in at
  first open — switching embed models needs a fresh `--db`.
- **When unsure, call `graph_docs`** (optionally with `section`) before
  writing GQL you're not certain of — it serves the full reference from
  inside the chat.
