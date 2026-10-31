---
name: canvas
description: Produce visual artifacts the user can see in their browser — charts, diagrams, math, simulations, dashboards, generative sketches. Write one self-contained HTML file with the ordinary file tools, serve it with the `canvas` tool, then edit it (live reload is automatic). Covers which vendored library fits which visual, with request-to-technique mapping tables. Use when the user asks for a chart, plot, diagram, visualization, dashboard, infographic, timeline, mind map, study guide, math rendering, animation, physics demo, or any "show me" or "help me understand" output; also when a task says to use the canvas.
---

# Canvas — visible artifacts for the user

You can produce visuals the user opens in their browser. The whole workflow
is file-based: write one HTML file, serve it, edit it. No Python, no npm, no
environment sidequests — every library is already inside the jinn binary.

## The workflow

1. **Create the artifact** as an HTML file at any writable path (use the
   session's project directory so it sits beside the code it describes).
   Start from the skeleton in `references/artifact-template.md` — copy it
   into the file with the ordinary file tools. The skeleton is a ready
   HTML page with every library available as a commented script tag.
2. **Uncomment the script tag(s)** for the library the visualization needs
   (mapping tables below). Write the visualization code in a `<script>`
   block at the end of `<body>`.
3. **Serve it**: `canvas` tool call, passing the artifact path (a
   relative path resolves against the session's working directory). The
   result text contains the URL — **put that URL in your reply** so the
   user can click it. jinn does not open a browser for the user.
4. **Edit freely**. Every save to the artifact (or any file in its
   directory) hot-reloads the user's browser tab within a quarter second.
   Do not call the tool again after edits — the running server picks
   changes up by itself.
5. **Switch artifacts** with another `canvas` tool call naming the other
   path: the same server re-targets in place at the same URL and
   connected browsers follow.

## Rules

- **One artifact = one self-contained HTML file.** Styles and data may be
  sibling files in the same directory (a sibling change also triggers
  reload); never reference anything under a path the server does not serve.
- **All library paths are `/vendor/...`** — see the template's commented
  tags for the exact paths. They work offline; never write a remote
  resource URL (an `http` or `https` address in a `src`/`href`/`fetch`
  position) into artifacts. XML namespace strings inside SVG markup — the
  `xmlns` attribute naming the w3 dot org — are fine: they are
  identifiers, not fetches.
- **Never fetch data from the network inside the artifact.** Inline the
  data as a JS const, or write it to a sibling JSON file and fetch it
  relatively (`fetch('./data.json')`).
- The server **never edits your file** — it injects a hot-reload script into
  the HTTP response only. What you write is exactly what's on disk.
- The server binds local addresses only. Sharing the URL beyond the user's
  machine is not possible by design.
- **After writing a Mermaid diagram, check the served page for Mermaid error
  boxes.** A syntax error renders as a visible error box, not a blank page —
  read the error text and fix the diagram source. The type references list
  syntax shapes that are verified against this binary (architecture edge
  syntax and diagram-type names differ between Mermaid versions).

## Picking the library

| The user wants                                        | Use                                      |
| ----------------------------------------------------- | ---------------------------------------- |
| Bar/line/pie/radar/scatter charts                     | **Chart.js**                             |
| Dense time series, thousands of points, zoomable      | **uPlot**                                |
| Rich interactive viz from JSON (Vega-Lite grammar)    | **Vega-Lite** (via vega-embed)           |
| Layered composition of scales/axes/marks, d3-flavored | **Observable Plot** (+ D3 when needed)   |
| Flowcharts, sequence diagrams, gantt, mindmaps        | **Mermaid** (text → diagram)             |
| Math typesetting (LaTeX)                              | **KaTeX**                                |
| Generative/creative sketches, p5-style animation      | **p5.js**                                |
| Reactive UI with state (small dashboard widgets)      | **Vue** (global build) or **preact+htm** |
| Physics simulation (gravity, collisions)              | **matter.js**                            |
| True 3D scenes — math surfaces, gamedev, molecules, model files | **three.js**                        |
| Highlighted code snippets in the artifact             | **highlight.js**                         |

Default to Chart.js for a plain "chart of this data" ask; reach further
down the table only when the ask names the richer shapes.

Every library below is embedded in the jinn binary and served at `/vendor/`
while the canvas server is running. Versions are pinned in the canvas
slice's manifest; the paths are stable across upgrades.

| Path                                                      | Library                | Strengths                                                                               | Watch out for                                                                                        |
| --------------------------------------------------------- | ---------------------- | --------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------- |
| `/vendor/chartjs/chart.umd.js`                            | Chart.js               | The 80% chart: bar, line, pie, doughnut, radar, scatter. Tiny API, fast to write.       | Antialiasing-free pixel access and huge datasets are not its game.                                   |
| `/vendor/uplot/uPlot.iife.min.js` + `uPlot.min.css`       | uPlot                  | Time-series at scale: 100k+ points, synchronized cursors, zoom. Needs the CSS link too. | API is terse; you size the canvas yourself. Global is `uPlot`.                                       |
| `/vendor/vega/vega.min.js`                                | Vega                   | Full visualization grammar, lowest level.                                               | Prefer vega-embed + Vega-Lite unless you need Vega's expressiveness.                                 |
| `/vendor/vega/vega-lite.min.js`                           | Vega-Lite              | Declarative JSON chart grammar: encode data → marks → channels.                         | Spec, not code: large specs get unwieldy. Its global is `vegaLite` (not `vl`) — vega-embed calls it for you. |
| `/vendor/vega/vega-embed.min.js`                          | vega-embed             | One call (`vegaEmbed('#el', spec)`) to render a Vega-Lite spec with interactions.       | Loads the other two vega files itself — include all three tags.                                      |
| `/vendor/plot/plot.umd.min.js`                            | Observable Plot        | Concise d3-flavored plots: `Plot.dot(data, {x:, y:}).plot()`.                           | Pair with D3 for axes outside the default frame.                                                     |
| `/vendor/d3/d3.min.js`                                    | D3                     | Scales, shapes, axes, selections, force and hierarchy layouts — the toolbox.            | Full D3 is a commitment; Plot or Chart.js is usually enough.                                         |
| `/vendor/mermaid/mermaid.min.js`                          | Mermaid                | Text → diagram: ~25 types incl. flowchart, sequence, class, state, ER, gantt, git graph, mindmap, timeline, C4, quadrant, sankey, treemap, packet, radar. | Load with a **plain script tag** — the vendored build sets a `mermaid` global and is **not** an ES module (`import` fails). Syntax errors render as error boxes. |
| `/vendor/katex/katex.min.js` + `katex.min.css` + `fonts/` | KaTeX                  | LaTeX math typesetting, fast, offline (fonts served alongside the CSS).                 | Include the CSS; use `katex.render(tex, el, { throwOnError: false })`.                               |
| `/vendor/p5/p5.min.js`                                    | p5.js                  | Creative coding: setup/draw loop, canvas drawing, generative art.                       | Runs an animation loop — stop it (`noLoop()`) for static images.                                     |
| `/vendor/vue/vue.global.prod.js`                          | Vue 3                  | Reactive state for widget-y dashboards without a build step.                            | Global build: use `Vue.createApp`, templates in-string.                                              |
| `/vendor/preact/standalone.umd.js`                        | Preact + htm           | Same idea as Vue with a JSX-less `html\`\`` tagged templates.                           | One global: `htmPreact` (contains `html`, `render`, hooks). Smaller ecosystem than Vue.              |
| `/vendor/highlightjs/highlight.min.js` + `github.min.css` | highlight.js           | Syntax-highlight code snippets inside the artifact.                                     | Common languages only (core build — rust/python/typescript and friends are present); call `hljs.highlightElement`. |
| `/vendor/matter/matter.min.js`                            | matter.js              | 2D rigid-body physics: gravity, collisions, constraints.                                | Ties to your own canvas/render loop; it simulates, you draw.                                         |
| `/vendor/three/three.module.min.js` + `addons/`           | three.js               | Real 3D: math surfaces & vector fields, gamedev scenes, molecules, STL/OBJ/glTF models. | ES module, not a script tag — needs an **import map** plus `<script type="module">` (see the template's commented block and `references/3d.md`). OrbitControls lives at `/vendor/three/addons/`. |
| `/vendor/canvas.css`                                      | jinn canvas stylesheet | Light/dark via `prefers-color-scheme`, spacing, system fonts. Always safe to link.      | Pure CSS custom properties; does not style charts.                                                   |

## The mapping: what to build for the request

Route by what the user *asked for*, not by which library you know best.
Full per-area guidance, snippets, and antipatterns live in the reference
files listed after the tables.

### Learning, study & explanation

| The user asks for                                   | Represent it as                                   | Libraries |
| --------------------------------------------------- | ------------------------------------------------- | --------- |
| An overview / one-page explainer of a topic         | Informational infographic page (header, sections, one visual each) | HTML + canvas.css |
| "Compare A and B" (options, libraries, plans)       | Comparison page: side-by-side columns with shared row axes | HTML + canvas.css |
| A key term / vocabulary                             | Frayer grid (definition / characteristics / examples / non-examples) | HTML + canvas.css |
| How concepts relate                                 | Concept map — nodes + labeled arrows               | Mermaid flowchart |
| Brainstorm / outline / organize thoughts            | Mind map (radial hierarchy)                        | Mermaid mindmap |
| Why something happened / root cause                 | Fishbone-style cause map                           | Mermaid flowchart |
| History, milestones, version timeline               | Timeline (dated spine, grouped eras)               | Mermaid timeline |
| Quiz me / flashcards for X                          | Flip cards, click-to-reveal Q → A                  | HTML + CSS + a little JS |
| Teach me this algorithm / worked example            | Numbered walkthrough: code + annotation + intermediate states | highlight.js + HTML |
| Summarize a paper/lecture visually                  | Sketchnote-style page (icons, arrows, short text)  | HTML + inline SVG |
| Study sheet / cheat sheet                           | Card grid: sections → cards, 2–3 columns, code highlighted | HTML + highlight.js |

### Systems, code & process

| The user asks for                                   | Represent it as                                   | Libraries |
| --------------------------------------------------- | ------------------------------------------------- | --------- |
| System architecture / how this app fits together    | C4-style context or container diagram              | Mermaid C4Context or block |
| Request lifecycle / how services talk               | Sequence diagram                                   | Mermaid sequence |
| State machine / lifecycle of an entity              | State diagram                                      | Mermaid state |
| Database schema / data model                        | ER diagram                                         | Mermaid ER |
| Class relationships / OOP design                    | Class diagram                                      | Mermaid class |
| Decision flow / what happens when                   | Flowchart with decision diamonds                   | Mermaid flowchart |
| Branching strategy / merge history                  | Git graph                                          | Mermaid gitGraph |
| Module dependencies / import graph                  | Compute the graph, draw it as a force layout       | D3 force |
| Where code is big / coverage by file                | Treemap (area = size, color = rating)              | Mermaid treemap or D3 hierarchy |
| Where time goes / profile breakdown                 | Flame-graph-style icicle (width = share of samples) | D3 hierarchy (partition) |
| Annotated code / explain this snippet               | Code block with margin callouts                    | highlight.js + HTML |
| Diff two versions visually                          | Side-by-side pre blocks, additions/deletions tinted | highlight.js + HTML |
| Wire format / packet / bit-field layout             | Packet diagram                                     | Mermaid packet |
| Grammar / syntax of a DSL                           | Railroad-style diagram                             | Mermaid railroad |
| Plan / schedule with dependencies                   | Gantt chart                                        | Mermaid gantt |
| Sprint progress / work remaining                    | Burndown line; cumulative-flow stacked area        | Chart.js |

### Data questions

| The user asks for                                   | Represent it as                                   | Libraries |
| --------------------------------------------------- | ------------------------------------------------- | --------- |
| "Chart this data" (default)                         | Bar or line chart                                  | Chart.js |
| Compare values across categories                    | Grouped bar (horizontal when labels are long)      | Chart.js |
| Trend over time                                     | Line / area; small multiples for many series       | Chart.js; Vega-Lite for facets |
| Huge time series (100k+ points, dense, zoom)        | Dense line with synchronized cursors               | uPlot |
| Distribution / spread / outliers                    | Histogram, boxplot, error bars                     | Vega-Lite |
| Correlation of two variables                        | Scatter + trend line (loess/regression built in)   | Vega-Lite |
| Composition / "what fraction"                       | Pie (≤5 slices) or stacked bar; treemap beyond that | Chart.js; Mermaid treemap |
| Ranking / leaderboard / benchmark results           | Sorted horizontal bar; bump chart for rank-over-time | Chart.js |
| Budget vs actual / vs target                        | Deviation bars or bullet-style rows                | Chart.js + rule annotations |
| "How the total moved" (bridge from X to Y)          | Waterfall (floating stacked bars)                  | Chart.js or Vega-Lite |
| Flow between stages / where traffic goes            | Sankey                                             | Mermaid sankey |
| Hierarchy with sizes (spend, disk usage, org)       | Treemap / sunburst / icicle                        | Mermaid treemap; D3 for control |
| "N percent of the whole" (one number)               | Waffle grid (10×10) or big-number KPI card         | HTML + canvas.css |
| Progress toward a goal                              | Gauge / bullet row / progress bar                  | HTML + canvas.css |
| Uncertainty / confidence intervals                  | Error bars / bands                                 | Vega-Lite |
| Heatmap (two dimensions × intensity)                | Rect heatmap; calendar heatmap for daily activity  | Vega-Lite or D3 |
| Prioritize / 2×2 matrix                             | Quadrant chart                                     | Mermaid quadrant |
| Strategy / evolution map                            | Wardley-style map                                  | Mermaid flowchart styled as stages |
| Cost / token usage over time                        | Stacked time series + summary table                | uPlot or Vega-Lite |

### Specialist, science & play

| The user asks for                                   | Represent it as                                   | Libraries |
| --------------------------------------------------- | ------------------------------------------------- | --------- |
| Math formula / derivation                           | Typeset LaTeX                                      | KaTeX |
| Shape of a function / equation                      | Function plot                                      | Observable Plot or Vega-Lite |
| Surface z = f(x, y) / multivariable calculus        | 3D surface mesh (code geometry, no assets)         | three.js |
| Vector fields, gradient/loss-surface descent        | 3D field of arrows; sphere walk on the surface     | three.js |
| Multivariable data / embedding clusters             | 3D scatter (or 2D first — see the 3D bar below)    | three.js |
| Crystal lattices / simple molecules (3D)            | Ball-and-stick from hard-coded coordinates         | three.js |
| "View / inspect this STL/OBJ/glTF model file"       | Load into a lit scene with an orbiting camera      | three.js + loader addon |
| Gamedev / computer-graphics concept (camera, lighting, PBR, shadows) | Small 3D scene demonstrating exactly that concept | three.js |
| "Let me tweak the parameters"                       | Interactive playground: sliders + live re-render   | Vue + Plot, or p5 |
| Gravity / pendulum / waves / orbits                 | Animated simulation                                | p5 (2D) / three.js (3D) |
| Collisions / rigid bodies                           | Physics simulation                                 | matter.js (+ your own draw loop) |
| Force diagram / free-body problem                   | SVG arrows sized and labeled per force             | Inline SVG; p5 to animate |
| Circuit / schematic explanation                     | Schematic symbols + (when useful) a pictorial view | Inline SVG |
| "Make it move" on a static diagram                  | Rebuild as an interactive simulation               | p5 or Vue |
| Generative art / ambient animation                  | Generative sketch                                  | p5 |

Default to the **simplest genre that answers the question** — the smallest
diagram or plainest chart that carries the point beats an elaborate one,
and a static page beats an interactive one unless a parameter is the point.

## References

Read the one that matches the request; each carries detailed mappings,
verified snippets, and its own antipatterns section (paths relative to this
skill's base directory):

| File                                        | Covers                                                                    |
| ------------------------------------------- | ------------------------------------------------------------------------- |
| `references/artifact-template.md`           | The single-file HTML skeleton with every library tag ready to uncomment   |
| `references/charts-and-data.md`             | Chart selection by intent, statistical marks, business chart genres (waterfall, Pareto, bump, waffle, quadrant), dashboards |
| `references/diagrams-for-systems-and-code.md` | The verified Mermaid type catalog and syntax notes, C4/UML selection, dependency graphs, annotated code and diff pages |
| `references/learning-and-study.md`          | Infographic page layouts, graphic organizers (Frayer, T-chart, concept maps), timelines, flashcards, cheat sheets |
| `references/flows-hierarchies-networks.md`  | Sankey/alluvial, treemap/sunburst/icicle, dendrograms, force-directed graphs, heatmaps |
| `references/interactive-and-simulation.md`  | Explorable-explanation discipline, p5 sketches, matter.js, Vue widgets, when interactive beats static |
| `references/specialist-and-scientific.md`   | Math pages, function plots, physics/force diagrams, schematics, maps, and the honest gaps |
| `references/3d.md`                          | Real 3D with three.js: the import-map loading pattern, surfaces/vector fields, molecules, model viewers, gamedev/CG scenes, and when 2D beats 3D |

### Combining libraries

- Vega-Lite: include all three vega tags and call `vegaEmbed('#el', spec)`.
- Observable Plot + D3: Plot renders into a div; D3 selects inside it for
  annotations.
- KaTeX + anything: independent; math typesetting layers onto any page.
- Mermaid: **plain script tag** (`<script src="/vendor/mermaid/mermaid.min.js"></script>`),
  independent of the UMD tags. Do not copy the template's commented
  module-import line — with this build it throws.
- three.js: needs the **import map before the module script** (see the
  template's commented block). Mixes freely with the UMD libraries —
  a KaTeX label layer over a 3D scene works; so does a Vue panel
  driving a three.js scene's uniforms.

### Data in

Inline small datasets as JS consts. For bigger data or data shared across
artifacts, write a sibling file (`data.json`) and `fetch('./data.json')` —
the server serves the artifact's directory watch scope over the same
origin, and a sibling change triggers reload like an artifact change does.

### Styling

Link `/vendor/canvas.css` (the template does). Use its custom properties
(`--bg`, `--fg`, `--accent`, `--space-*`, font stack) so artifacts match
the user's light/dark preference without a theme system.
