# Specialist & scientific

Domain-shaped visuals where a field has its own conventions: math pages,
function plots, physics and force diagrams, circuits, and maps. These
asks carry hidden requirements — the conventions ARE the content — so
this file is heavy on what each genre must get right.

## Math

### Formula / derivation pages (KaTeX)

```html
<link rel="stylesheet" href="/vendor/katex/katex.min.css">
<script src="/vendor/katex/katex.min.js"></script>
<div class="math-block"></div>
<script>
  katex.render(String.raw`c = \pm\sqrt{a^2 + b^2}`,
    document.querySelector('.math-block'),
    { throwOnError: false, displayMode: true });
</script>
```

- **Always include the CSS** — without it, math renders as garbled
  overlapping glyphs. Fonts are served alongside (`/vendor/katex/fonts/`).
- `throwOnError: false` renders the bad input visibly instead of
  throwing — keeps the page alive during live-reload edits. For
  authoring-phase confidence, set `throwOnError: true` temporarily and
  watch the console.
- `displayMode: true` for centered display math; inline math with
  `katex.render(tex, el)` (no displayMode) or `katex.renderToString`
  for mixing into HTML strings.
- Multi-step derivations: one `displayMode` block per line, aligned
  section-by-section, with the *justification* of each step as a short
  margin note (the worked-example principle applies to algebra too).
- Mix freely with everything else — KaTeX is independent of the chart
  libraries.

### Function / equation shape plots

Observable Plot (concise, d3-flavored):

```js
const pts = d3.range(-5, 5, 0.05).map(x => ({ x, y: Math.sin(x) / x }));
Plot.plot({
  marks: [
    Plot.line(pts, { x: 'x', y: 'y' }),
    Plot.ruleY([0]), Plot.ruleX([0]),       // axes through origin
  ],
  x: { label: 'x' }, y: { label: 'sin(x)/x' },
})
```

- Sample finely near singularities and asymptotes; a coarse grid turns
  tan/sec into fat lies. Break the line at discontinuities
  (`NaN`/`undefined` gaps).
- Vega-Lite when the plot should also carry data+statistical layers
  (regression over scatter); Plot for pure function shape.
- Annotate the interesting features (max, root, asymptote) with
  `Plot.text`/rules — a shape without labeled features is a texture.

## Physics & mechanics

### Free-body / force diagrams

The convention set that makes FBDs correct (learners are graded on
exactly these): the object as a simple shape, **arrows from the center
of mass**, each arrow labeled with the force name, arrow *length
proportional to magnitude*, and nothing else drawn (no velocity arrows
mixed into the same diagram — acceleration arrows get a different
style/dashed form when needed).

```html
<script src="/vendor/p5/p5.min.js"></script>
<div id="fbd"></div>
<script>
  function setup() { createCanvas(400, 300).parent('fbd'); noLoop(); }
  function draw() {
    background('white');
    translate(200, 150);
    // box (the body)
    rect(-30, -25, 60, 50);
    strokeWeight(3);
    const forces = [
      { name: 'N',   dx: 0,   dy: -80 },   // normal
      { name: 'mg',  dx: 0,   dy: 80  },   // weight
      { name: 'F',   dx: 60,  dy: 0   },   // applied
      { name: 'f',   dx: -25, dy: 0   },   // friction
    ];
    for (const f of forces) {
      line(0, 0, f.dx, f.dy);
      const [a, b] = [f.dx || 1, f.dy || 1];
      const ang = Math.atan2(b, a);
      // arrowhead
      line(f.dx, f.dy, f.dx - 8 * Math.cos(ang - 0.4), f.dy - 8 * Math.sin(ang - 0.4));
      line(f.dx, f.dy, f.dx - 8 * Math.cos(ang + 0.4), f.dy - 8 * Math.sin(ang + 0.4));
      noStroke();
      text(f.name, f.dx * 1.15 - 5, f.dy * 1.15 + 5);
      strokeWeight(3); stroke('black');
    }
  }
</script>
```

For a *simulation* of the resulting motion, animate with p5 and let
matter.js handle the dynamics — but the static labeled FBD stays the
first artifact (physics-education research consistently finds students
who draw correct FBDs solve the problems correctly; the diagram is the
teaching).

### Circuit schematics

Two views, each answering a different question: the **schematic**
(logic — symbols, no physical layout; this is the one to teach from) and
the **pictorial** (physical arrangement on a breadboard — the one a
novice builds from). For "explain this circuit", ship both side by side.
No schematic library is vendored: draw with inline SVG symbols (battery,
resistor, capacitor, ground are five-line shapes) — keep a small symbol
library as JS functions and compose. Conventions: current-flow arrows
labeled, node junctions as filled dots, values ON the symbols
(`10kΩ`), never a bare unlabeled box.

## Maps & spatial

**There is no map library in the vendored set** (no Leaflet, no
topojson; d3-geo primitives — `geoPath`, `geoMercator` — are present,
but boundary geometry must be inlined as GeoJSON, which is bulky).
Treat maps as a last resort:

| The ask                          | Better artifact (usually)                    |
| -------------------------------- | -------------------------------------------- |
| "Sales by country" (values)      | Sorted horizontal bar                        |
| "Which regions are above 10%" (rates) | Tile-grid map (CSS grid of equal cells) or bar |
| "Where does X happen geographically" (pattern IS the point) | Choropleth via d3-geo + inlined GeoJSON |
| "Trade/migration between places" | Sankey (drop the geography) or an adjacency matrix |
| Distance/adjacency puzzle        | Schematic diagram, not a projection          |

Choropleth rules *when* geography is genuinely the headline: normalize
to rates (raw counts on differently-sized areas mislead — big area
looks like big value), sequential color ramp, visible legend, and say
in a caption what the enumeration units are.

Tile-grid maps (equal squares per region, CSS grid, hex/rounded cells)
preserve "regional pattern" readability with none of the projection
problems — the usual right answer for canvas artifacts.

## Chemistry / molecules

**No SMILES renderer is vendored.** Options, honest about limits:

- Simple molecules: hand-drawn inline SVG (lines for bonds, element
  symbols for atoms) — fine for water, methane, benzene-style rings.
- From a SMILES string: state plainly that offline rendering isn't
  available; offer the hand-drawn SVG for the specific molecule instead.
- **3D ball-and-stick is available** via three.js from hard-coded
  coordinates (`references/3d.md`, molecules section) — the right
  shape when the question is 3D geometry (VSEPR, crystal lattices,
  chirality). It does not close the 2D gap: structural formulas with
  rings and double-bond placement stay hand-drawn SVG.

Benzene-style ring drawings: SVG polygon + alternating double bonds;
`KaTeX` for reaction equations alongside (`2H₂ + O₂ → 2H₂O`).

## Data-table-as-visual

Sometimes the honest answer to "show me X" is a **styled table**:
sortable columns, zebra rows, monospace numerals, right-aligned numbers,
highlighted row for the winner. For benchmarks, config comparisons, and
"list everything with columns", a table beats a chart on every axis
except instant-trend-reading. canvas.css styles tables adequately;
add `text-align: right` and `font-variant-numeric: tabular-nums` for
the numeric columns.

## Terminology notes (avoid confidently wrong labels)

- **Flame graph vs flame chart**: flame *graph* = x-axis is merged
  samples (alphabetical frames); flame *chart* = x-axis is time. Chrome
  DevTools draws flame *charts*.
- **Sankey vs alluvial**: sankey = stages/nodes; alluvial = same
  categories tracked across time points. Visuals look alike; claims
  differ.
- **Choropleth vs bubble map**: shaded areas vs sized circles at
  centroids. Counts → bubbles; rates → choropleth.
- **Histogram vs bar chart**: histogram bins a continuous variable
  (order matters, bars touch); bar chart compares categories (order
  free, gaps between bars).
- **Dendrogram vs tree**: dendrogram's leaves align at one depth and
  edge lengths encode merge distances (clustering); a tidy tree's edge
  lengths mean nothing.

## Antipatterns

- **KaTeX without its CSS.** The formulas render as mangled glyph
  piles; the missing `<link>` is invisible in the editor and obvious in
  the browser. Always the CSS, always `/vendor/katex/fonts/` intact.
- **Coarse sampling through singularities.** `tan(x)` on a 0.5 grid
  draws vertical-looking jumps that aren't the function. Fine sampling
  + gap-breaking at discontinuities or the plot asserts falsehoods.
- **Unlabeled forces in a free-body diagram.** An arrow without a name
  and correct proportional length fails the diagram's own grading
  rubric; FBD conventions aren't decoration, they're the assessment.
- **Mixing velocity and force arrows in one FBD.** Different quantities
  on one diagram teach exactly the wrong lesson; use a second panel
  (or dashed arrows) for kinematics.
- **Schematic when the user will build, pictorial when the user will
  debug.** The breadboard picture teaches placement but hides logic;
  the schematic teaches logic but hides placement — "explain this
  circuit" almost always deserves both.
- **Projection maps for non-geographic asks.** A choropleth of "users
  per country" invites area-based misreading; the bar or tile grid
  answers the actual question without cartographic baggage.
- **Raw-count choropleths.** Already stated, worth repeating as the
  single most common map sin: normalize or don't map.
- **Word clouds.** Area ≠ frequency perception, word order lost, and
  no vendored renderer anyway — a sorted bar of top-N frequencies
  carries the same data honestly.
- **Music notation by hand.** Sheet music needs real engraving
  (staff positioning, beams); hand-built SVG scores look plausible and
  read wrong. Offer tablature-style text or an audio synth sketch
  instead — and say the limitation out loud.
- **3D everything.** When the user says "make it 3D", ask what the
  third dimension *encodes*: if the answer is "depth for realism", a
  2D artifact with better encoding beats an unrotatable 3D one. When
  the third dimension is real information — z = f(x,y), geometry,
  camera/lighting concepts — three.js is now the tool
  (`references/3d.md`); the antipattern is the default-3D habit,
  not 3D itself.
