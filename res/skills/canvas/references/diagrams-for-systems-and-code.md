# Diagrams for systems & code

The vendored Mermaid build (12.x) is the workhorse for this whole area: it
takes *text*, which an agent writes natively and edits surgically. This
file catalogs its verified types, the syntax that actually parses in this
binary, and when to leave Mermaid for D3 or an HTML page.

## Loading Mermaid — the form that works

The vendored bundle sets a **`mermaid` global via a plain script tag**.
It is *not* an ES module — `import('...mermaid.min.js')` resolves to
`undefined` and throws. Always:

```html
<script src="/vendor/mermaid/mermaid.min.js"></script>
<script>mermaid.initialize({ startOnLoad: true });</script>
<div class="mermaid">
flowchart TD
  A --> B
</div>
```

(KaTeX-style onerror rendering also works:
`mermaid.run({ querySelector: '.mermaid' })` after DOM edits.)

## Verified diagram types

All of the following parse and render in the vendored binary (verified
against this exact build; type names are what `mermaid.parse` reports):

| Declared as         | Renders as      | The ask it answers                              |
| ------------------- | --------------- | ----------------------------------------------- |
| `flowchart` / `flowchart-v2` | flowchart-v2 | decisions, request paths, cause→effect chains  |
| `sequenceDiagram`   | sequence        | request lifecycles, protocol handshakes         |
| `classDiagram`      | classDiagram    | OOP structure, interfaces, inheritance          |
| `stateDiagram-v2`   | stateDiagram    | lifecycles, UI/event state machines             |
| `erDiagram`         | er              | schemas, data models                            |
| `gantt`             | gantt           | schedules, release plans with dependencies      |
| `pie`               | pie             | tiny static proportions                         |
| `mindmap`           | mindmap         | radial brainstorms/outlines                     |
| `timeline`          | timeline        | chronologies grouped by era                     |
| `journey`           | journey         | experience stages scored by sentiment           |
| `kanban`            | kanban          | task boards (static snapshot)                   |
| `gitGraph`          | gitGraph        | branching/merge history                         |
| `xychart-beta`      | xychart         | quick bar/line inside a Mermaid-driven page     |
| `quadrantChart`     | quadrantChart   | 2×2 prioritization matrices                     |
| `requirementDiagram`| requirement     | requirement traceability                        |
| `C4Context`         | c4              | C4 system-context diagrams                      |
| `sankey-beta`       | sankey          | flow quantities between stages (CSV rows)       |
| `block-beta`        | block           | layered architecture boxes                      |
| `packet-beta`       | packet          | byte-field / packet layouts                     |
| `architecture-beta` | architecture    | service-topology diagrams                       |
| `radar-beta`        | radar           | multi-dimension comparison spokes               |
| `treemap-beta`      | treemap         | sized hierarchies                               |
| `info`              | info            | banner (rarely wanted)                          |

Also present and parsing: `treeView-beta` (indented tree),
`agentflow-beta` (AI-agent flows). **Not** dedicated types in this build —
shape them as flowcharts instead: venn, ishikawa/fishbone, wardley maps
(each verified to fall back to `flowchart-v2`).

## Type-selection guide

| The user asks for                          | Diagram                       | Notes                                          |
| ------------------------------------------ | ----------------------------- | ---------------------------------------------- |
| How the system fits the world (C4 level 1) | `C4Context`                   | Show people, the system, externals. Stop here for non-dev audiences. |
| The deployable pieces (C4 level 2)         | `block-beta` or flowchart     | Containers + technologies; the level most teams actually use. |
| How a request flows through services       | `sequenceDiagram`             | Time-ordered; use `alt`/`opt` for branches.    |
| The states one entity moves through        | `stateDiagram-v2`             | Lifecycles; start from `[*]`, end at `[*]`.    |
| The data model                             | `erDiagram`                   | Cardinality labels (`||--o{`) carry the point. |
| Who-inherits-what / interface shape        | `classDiagram`                | Show members only when they matter.            |
| What happens when the user clicks X        | `flowchart TD`                | Decision diamonds for branches.                |
| Diagnose "why is it slow/failing"          | `flowchart` fishbone layout   | Spine + category branches + causes.            |
| Branching strategy / this PR's story       | `gitGraph`                    | `branch`, `merge`, `checkout` — mirrors git.   |
| Plan with dates and dependencies           | `gantt`                       | `after` task ids encode dependencies.          |
| Bits on the wire                           | `packet-beta`                 | Bit ranges + field names.                      |
| Grammar of a DSL                           | `treeView-beta`/railroad-ish  | Full railroad exists but syntax is finicky — verify output. |
| Where money/tokens/traffic flows           | `sankey-beta`                 | CSV rows `source,target,value`.                |
| Service topology / mesh                    | `architecture-beta`           | Edge syntax below — it is version-specific.    |
| Prioritize a backlog                       | `quadrantChart`               | Label quadrants with actions.                  |
| Capability comparison across options       | `radar-beta`                  | Normalize axes to a common max.                |
| Spend/disk/org tree with sizes             | `treemap-beta`                | Nested `key: value` blocks.                    |

## Syntax that actually parses (version-specific, verified)

**architecture-beta edges** — the grammar is unforgiving; this exact shape
parses: `srcId:srcSide --> tgtSide:tgtId` (sides mandatory, `:` tight,
sides are L/R/T/B):

```mermaid
architecture-beta
  service api(cloud)[Api]
  service db(database)[Db]
  api:T --> B:db
  api:L --> R:db
```

Do not name services `left`/`right`/`top` — those strings collide with the
grammar's direction tokens (verified failure). Arrowheads (`-R->`) also
fail in this build; plain `-->` and `--` are the safe connectors.

**sequence** — `A->>B: msg` (solid), `A-->>B: dashed reply`, `autonumber`
for step numbers; participant aliases keep labels short.

**state** — `[*] --> Draft` to open; `Published --> [*]` to close; label
transitions with `: event`.

**er** — `USER ||--o{ POST : writes`; attribute blocks below the entity
carry field types.

**flowchart** — `A{decision?}` diamonds, `A -->|label| B` edge labels,
`subgraph` blocks for grouping; `flowchart LR` for left-right process
flows, `TD` for decision trees.

**mindmap** — indentation defines the tree; one root.

**timeline** — `era : event : event` lines under a period title.

**gitGraph** — `commit`, `branch name`, `checkout name`, `merge name`,
`tag "v1"`.

**gantt** — sections group; `task :done, id, date, N d` statuses;
`after id` dependencies.

**kanban** — `column[Title]` then `task[Task]@{ column: column }`.

**sankey-beta** — CSV rows after the declaration:
`Source,Target,42` (no quotes needed).

**packet** — `0-15: "field"` bit-range rows.

**radar-beta** — `axis a, b` then `curve name{a: 5, b: 3}` and a common
`max N`.

**treemap-beta** — nested indentation with `"label": value` leaves.

**C4Context** — `Person(id, "Name", "desc")`, `System(id, "Name", "desc")`,
`Rel(from, to, "label")`; `System_Ext` for external systems.

**quadrantChart** — axis ranges, four `quadrant-N` labels, then
`Point: [x, y]` with x/y in 0..1.

**xychart-beta** — `x-axis [labels]`, `y-axis "Name" min --> max`, then
`bar [...]` / `line [...]`.

## Mermaid alternatives (when Mermaid is the wrong tool)

- **Dependency graphs / import graphs** — Mermaid flowcharts render them
  only for a handful of modules. For a real codebase: compute the graph
  (read imports with your file tools; you already know how to grep),
  inline `nodes`/`links` as a JS const, and lay it out with
  `d3.forceSimulation` + drag/zoom. Color cycle edges red (madge's own
  convention: blue = has deps, green = leaf, red = cycle).
- **Big/derived treemaps** — `treemap-beta` for small trees; for
  size-computed hierarchies (file sizes, LOC), D3's `d3.hierarchy` +
  `d3.treemap()` on a computed tree. See
  `flows-hierarchies-networks.md`.
- **Flame graphs** — no diagram type. Build with D3 `partition()`:
  width = share of samples, depth = stack. (A *flame chart* — time on x —
  is a different visual; don't mislabel.)
- **Annotated code** — not a diagram job. See the recipe below.

## Code-page recipes

### Annotated code / explain this snippet

```html
<link rel="stylesheet" href="/vendor/highlightjs/github.min.css">
<script src="/vendor/highlightjs/highlight.min.js"></script>
<div class="annotated">
  <pre><code class="language-rust">fn quicksort(v: &mut [i32]) {
    if v.len() <= 1 { return; }
    let p = partition(v);
    let (a, b) = v.split_at_mut(p);
    quicksort(a);
    quicksort(b);
}</code></pre>
  <ol class="callouts">
    <li>Base case: length ≤ 1 is already sorted</li>
    <li>Partition reorders around a pivot in place</li>
    <li>Recurse on both halves — bounds known</li>
  </ol>
</div>
<script>hljs.highlightElement(document.querySelector('code'));</script>
```

The worked-example discipline: code on the left, numbered steps on the
right, one step per idea, and the final state (output/return value)
visible without scrolling.

### Side-by-side diff

Two `<pre>` columns in a flex row, line arrays computed in JS, added
lines tinted green, removed tinted red, common lines neutral; keep both
panels scroll-synchronized (`scrollHeight` match on scroll). Highlight
with hljs after diffing so the tokens match the language. For narrow
changed regions, word-level highlighting inside changed lines beats
whole-line tints.

### Package/module dependency report

A small dashboard: force graph (or treemap of directory sizes) + a table
of top fan-in/fan-out files + the cycle list. Generate the data with your
ordinary tools, inline it as a JS const, serve.

## Antipatterns

- **ES-module import of the vendored Mermaid.** It fails (`default` is
  `undefined`) — plain script tag only. This will bite anyone copying a
  generic Mermaid embedding tutorial.
- **Diagram sprawl.** One diagram, one question. The "smallest diagram
  that answers the reader's question" beats a full-detail dump; split
  per reader task (a validation flowchart + a token-exchange sequence,
  not one mega-diagram of everything).
- **C4 code-level diagrams by default.** Levels 1–2 (context, container)
  serve nearly every ask; component/code diagrams are noise unless the
  request is explicitly about internals.
- **Naming Mermaid nodes after grammar tokens.** `left`, `right`, `top`,
  `bottom` as architecture-beta service ids collide with the parser's
  direction tokens — pick descriptive ids (`api`, `db`).
- **Trusting the diagram because it rendered.** Mermaid renders syntax
  errors as error *boxes* that look like content from a distance. After
  any Mermaid edit, check the page for error boxes and read the message.
- **Sequence diagrams for everything.** If the ask has no meaningful
  time ordering, a flowchart or block diagram is clearer. Sequence is
  for *when*, not *what*.
- **Fake mermaid types.** `venn`, `ishikawa`, `wardley` are not diagram
  types in this build — they fall back to flowchart. Build those shapes
  explicitly as flowcharts (circles + labeled connectors for venn-likes,
  spine-and-branches for fishbone).
- **Class diagrams with every field and method.** Full member dumps
  render as walls of text in boxes; show members only when the ask is
  about the members.
- **gantt for anything that isn't time-and-dependency.** A task list is
  not a schedule; Mermaid kanban or a plain list serves planning asks
  without inventing fake dates.
- **Screenshots of code as images.** Always real text with hljs — text
  is searchable, readable at any zoom, and light/dark aware via canvas.css.
