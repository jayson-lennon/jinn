# Charts & data — picking and building them

Route by the *question the visual answers*, then pick the simplest library
that renders it. This file covers every chart genre a data request can name;
`flows-hierarchies-networks.md` covers sankey/treemap/network genres.

## The intent skeleton

Nearly every data question is one of these intents (the shared core of the
FT Visual Vocabulary, From Data to Viz, and the classic chart choosers):
**comparison, distribution, correlation, trend-over-time, magnitude,
ranking, part-to-whole, deviation, flow, spatial**. Name the intent first —
the genre follows from it.

| Intent           | Ask sounds like                          | Genre                        | Library |
| ---------------- | ---------------------------------------- | ---------------------------- | ------- |
| Comparison       | "compare X across categories"            | grouped/stacked bar          | Chart.js |
| Distribution     | "spread, outliers, typical value"        | histogram, boxplot, violin   | Vega-Lite |
| Correlation      | "does X relate to Y"                     | scatter (+ trend line)       | Vega-Lite |
| Trend over time  | "how it changed"                         | line / area / small multiples | Chart.js; Vega-Lite facets |
| Magnitude        | "how big is each"                        | bar                          | Chart.js |
| Ranking          | "who is first / fastest / cheapest"      | sorted bar; bump over time   | Chart.js |
| Part-to-whole    | "what fraction"                          | pie (≤5), stacked bar, treemap | Chart.js; Mermaid treemap |
| Deviation        | "vs target, vs baseline, +/-"            | deviation bar, bullet row    | Chart.js |
| Flow             | "where does it go"                       | sankey                       | Mermaid sankey |
| Spatial          | "where in the world"                     | see `specialist-and-scientific.md` (maps gap) | — |

## Library selection within a data request

- **Chart.js** — the default. All standard controllers verified in the
  vendored build (bar, line, pie, doughnut, radar, scatter, mixed).
- **uPlot** — when the series is *big*: 10k+ points, high-frequency data,
  multiple synchronized series, or when you want a built-in zoom/cursor.
  You size the canvas yourself (`new uPlot(opts, data, container)`).
- **Vega-Lite** — when the chart is *statistical* (distributions, uncertainty,
  fitted trends, faceting) or you'd rather declare JSON than write canvas
  code. Include all three vega tags; call `vegaEmbed('#el', spec)`. Specs
  are data — you can also write them to a sibling `.json` file and fetch.
- **Observable Plot** — d3-flavored one-liners with good defaults
  (`Plot.dot(data, {x: "a", y: "b"}).plot()`), great when Chart.js feels
  limiting but full Vega-Lite feels heavy.
- **D3** — only for shapes the above can't express (custom geometry,
  force/hierarchy layouts). See `flows-hierarchies-networks.md`.
- **Mermaid** `xychart` — a quick bar/line when the page is already
  Mermaid-driven and the data is tiny and static.

## Genre recipes

Each recipe is a shape, not a full file. Wrap in the standard template,
uncomment the relevant script tag, and follow the antipatterns below.

### Line / area (trend over time)

```html
<script src="/vendor/chartjs/chart.umd.js"></script>
<canvas id="c" width="640" height="360"></canvas>
<script>
  new Chart(document.getElementById('c'), {
    type: 'line',
    data: {
      labels: ['Mon', 'Tue', 'Wed', 'Thu', 'Fri'],
      datasets: [{ label: 'commits', data: [12, 19, 7, 15, 22], tension: 0.3 }]
    },
    options: { scales: { y: { beginAtZero: true } } }
  });
</script>
```

### Grouped bar (comparison)

Same skeleton with `type: 'bar'`; multiple datasets for groups; set
`indexAxis: 'y'` for the horizontal form when category labels are long —
the standard fix for unreadable rotated labels.

### Scatter + trend (correlation)

Vega-Lite layers a point mark over a fitted trend without manual math:

```html
<div id="viz"></div>
<script src="/vendor/vega/vega.min.js"></script>
<script src="/vendor/vega/vega-lite.min.js"></script>
<script src="/vendor/vega/vega-embed.min.js"></script>
<script>
  vegaEmbed('#viz', {
    data: { values: [{x: 1, y: 2.1}, {x: 2, y: 3.9}, {x: 3, y: 6.2}] },
    layer: [
      { mark: 'point',
        encoding: { x: {field: 'x', type: 'quantitative'},
                    y: {field: 'y', type: 'quantitative'} } },
      { mark: 'line',
        transform: [{regression: 'y', on: 'x'}],
        encoding: { x: {field: 'x', type: 'quantitative'},
                    y: {field: 'y', type: 'quantitative'} } }
    ]
  });
</script>
```

Swap the regression transform for `{loess: 'y', on: 'x'}` when the
relationship isn't linear.

### Histogram / boxplot / error bars (distribution)

Vega-Lite composite marks do the statistics for you:

- Histogram: bar mark with `encoding.x.bin: true`.
- Boxplot: `{ mark: 'boxplot', encoding: { x: {...}, y: {field: 'v'} } }`
  — whiskers, quartiles, outliers computed from raw values.
- Error bars / bands: `mark: 'errorbar'` / `'errorband'` with
  `extent: 'ci' | 'stderr' | 'stdev' | 'iqr'`.

When to prefer which: boxplot for comparing groups; histogram for the
shape of one variable; strip/rug dots under a boxplot when group sizes
differ wildly (a boxplot of 3 points lies).

### Deviation (vs target / vs baseline)

Subtract the reference in JS, color by sign, draw a zero rule:

```js
const rows = [
  { area: 'auth',   delta: 220 },
  { area: 'search', delta: -80 },
];
const colors = rows.map(r => (r.delta >= 0 ? 'var(--accent)' : 'crimson'));
// Chart.js bar with backgroundColor: colors; add a flat dataset of zeros
// rendered as a line for the baseline.
```

### Waterfall (bridge from X to Y)

Floating stacked bars: one transparent "base" series + positive series +
negative series, all stacked; Chart.js renders it with `stacked: true`
and a zero-colored base dataset. Reserve it for the ask where the *steps
between two totals* are the point (budget bridges, variance walkthroughs,
score breakdowns).

### Pareto (vital few)

Sorted descending bars + cumulative-% line on a second axis. Use for
root-cause and "where does the pain concentrate" asks. The cumulative
axis must be labeled — its 0–100% scale reads as the value scale otherwise.

### Bump (rank over time)

Line chart where y = rank (1 at top: invert the y scale), x = time, one
line per entity, labeled at each end. Use only when *position* is the
story; switch to a line chart when absolute values matter.

### Waffle / big-number / gauge (one proportion)

- Waffle: a 10×10 CSS grid, filled cells = percentage; clearer than a pie
  for a single percentage, charming in infographics.
- Big number: a KPI card (huge figure + small context line).
- Gauge/bullet: progress bar or doughnut against a target line — for
  "how close to done".

### Quadrant / 2×2 (prioritize)

Mermaid's verified quadrant chart:

```html
<pre class="mermaid">
quadrantChart
  title Rollout decisions
  x-axis Low effort --> High effort
  y-axis Low value --> High value
  quadrant-1 Do now
  quadrant-2 Plan
  quadrant-3 Skip
  quadrant-4 Quick wins
  Auth refactor: [0.8, 0.9]
  Dark mode: [0.2, 0.5]
</pre>
```

Every quadrant chart is an *opinion* — state the axes' meaning in the
quadrant labels, not just in prose.

### Small multiples (many series)

Vega-Lite's `facet` channel: one row per series, shared scales — the
right answer to "chart these 12 metrics" where one chart would be
spaghetti. Chart.js has no native facet; hand-building 12 canvases is
legal but tedious.

## Dashboards & engineering data

- **Burndown/burnup** — line chart of remaining (or completed) work vs an
  ideal straight line; Chart.js.
- **Cumulative flow** — stacked area of work per state over time; wide
  horizontal bands are the bottleneck. Chart.js stacked area.
- **Cost/token dashboards** — stacked time series per model + a summary
  table; uPlot or Vega-Lite, table in plain HTML.
- **Benchmark results** — sorted horizontal bar with values labeled at
  bar ends; annotate the control/baseline bar. For benches across many
  dimensions, a table beats a chart.
- **KPI cards** — big number + delta + sparkline (tiny inline chart);
  combine with charts into an infographic layout (see
  `learning-and-study.md` for page structure).

## Antipatterns

- **Pie with more than five slices, or slices that don't sum to 100%.**
  Beyond that, bars or a treemap carry the same data legibly; a pie of
  multi-answer survey data (sums > 100%) is simply wrong.
- **Dual-axis charts without labeling the second axis.** The Pareto's
  cumulative-% line and the waterfall's totals get misread as the value
  scale — label the right axis explicitly.
- **Waterfall with tiny deltas against huge totals.** The steps vanish;
  zoom the axis or drop the baseline.
- **Bump chart presented as values.** It shows rank only. If the user
  asked "how much", give a line chart.
- **3D charts and exploded pies.** Perspective distorts area — the exact
  channel the chart encodes. No vendored library defaults to them; keep
  it that way.
- **Truncated bar axes.** Bars encode by length from zero; a non-zero
  baseline turns a 5% gap into a 2× visual gap. Line charts may truncate;
  bars may not.
- **Rainbow/traffic-light color scales on continuous data.** Hue is
  categorical; continuous magnitudes want sequential lightness ramps
  (and the mid-range stops lying about where "middle" is).
- **A chart when a sentence would do.** One number, one comparison, no
  distribution: a KPI card or bolded figure communicates faster than any
  plot. Canvas artifacts make this cheap — don't pay the chart tax.
- **uPlot for a 12-point bar chart.** Its terse API buys nothing at small
  scale; Chart.js's defaults are better there. Reach for uPlot when the
  point count justifies it.
- **Vega-Lite spec sprawl.** If the spec exceeds ~80 lines or three layers,
  stop and reconsider: either the chart is the wrong genre or the data
  needs pre-shaping in JS before the spec sees it.
- **Feeding raw uncleaned data into any chart.** Sort categories, drop
  nulls, compute derived fields in JS first — charts don't fix data, they
  frame it.
