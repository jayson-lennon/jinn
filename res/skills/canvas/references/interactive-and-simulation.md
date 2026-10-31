# Interactive & simulation

Canvas artifacts can run code in the user's browser: sketches, physics,
parameter playgrounds, widget dashboards. Interactivity is expensive
attention-wise and bug-wise — this file is as much about *when not to*
as how.

## When interactive beats static

Default **static**. Go interactive only when the mechanism itself is the
message — i.e., the user's question is "what happens when X changes":

| Ask                                     | Build                                   | Static alternative that's often enough |
| --------------------------------------- | --------------------------------------- | -------------------------------------- |
| "Show me how gravity affects orbits"    | p5 simulation with a mass slider         | Three still frames at three masses     |
| "How does the filter respond to Q?"     | Slider + live response curve             | A small grid of curves                 |
| "Help me grok this algorithm"           | Step-through visualizer (next/prev over recorded states) | Worked-example page with numbered steps |
| "What if we double the traffic?"        | Queue simulation with a rate slider      | Two annotated scenarios                |
| "Let me poke at the data"               | Vue widget: filters + chart re-render    | Faceted small multiples                |
| "Explain X" (no parameter implied)      | **Stay static.** An explainer page       | —                                      |

The explorable-explanation standard, worth internalizing: the interactive
element is integrated with the explanation — the page *reads as prose*
and the reader nudges the examples in place. A widget parked in a sandbox
with no surrounding text is a toy, not an explanation. And animation is
a medium, not a garnish: text for abstract concepts, graphs for broad
relationships, animation for temporal ones — pick per content.

**p5 or three.js?** By what the scene needs, not by which is more fun:
- Toy 3D — a spinning shape, orbit lines, a 2D-projected bouncing ball —
  and all 2D animation stay in **p5** (its WEBGL mode covers primitives;
  no camera/lighting model needed).
- Anything needing **a movable camera, real lighting, or perspective
  geometry** — surfaces, fields, molecules, model files, gamedev/CG
  demos — goes to **three.js** (`references/3d.md`). If you find
  yourself hand-rolling a camera or fake shading in p5, that is the
  signal to move.

## p5.js — sketches & generative animation

```html
<script src="/vendor/p5/p5.min.js"></script>
<div id="sketch"></div>
<script>
  function setup() {
    createCanvas(640, 360).parent('sketch');
    noLoop();                    // static by default; loop() to animate
  }
  function draw() {
    background('var(--bg)');     // canvas.css custom props work via getComputedStyle if needed
    stroke('white');
    for (let i = 0; i < 100; i++) {
      point(random(width), random(height));
    }
  }
</script>
```

Use for: generative art, particle systems, wave/pendulum/orbit demos,
L-systems, cellular automata, "make it pretty" ambient animation.
**`noLoop()` for anything static** — an idle draw loop burns CPU in a
tab the user is half-watching. For heavy sketches, prefer
`frameRate(30)` and clear rectangles over full `background()` repaints.

Physics-free motion (waves, orbits, easing) is p5's sweet spot; for
*rigid-body* realism, drop to matter.js.

## matter.js — rigid-body physics

```html
<script src="/vendor/matter/matter.min.js"></script>
<script>
  const { Engine, Runner, Bodies, Composite } = Matter;
  const engine = Engine.create();
  Composite.add(engine.world, [
    Bodies.rectangle(320, 340, 640, 20, { isStatic: true }),  // floor
    Bodies.rectangle(320, 50, 30, 30),
    Bodies.circle(280, 30, 20),
  ]);
  const runner = Runner.create();
  Runner.run(runner, engine);
</script>
```

matter.js simulates; **you draw**. Render bodies yourself (canvas 2D or
SVG synced from `engine.world.bodies` each frame — a `requestAnimationFrame`
loop reading `body.position`/`body.angle`). Use for: collision demos,
stacking/toppling, pendulums-with-constraints, "why does the bridge
collapse". Coupling with p5 is the common pattern: matter updates
positions, p5 draws.

## Vue — stateful widgets & parameter playgrounds

```html
<script src="/vendor/vue/vue.global.prod.js"></script>
<div id="app">
  <label>mass: {{ mass }} kg</label>
  <input type="range" min="1" max="100" v-model.number="mass">
  <div id="plot"></div>
</div>
<script src="/vendor/plot/plot.umd.min.js"></script>
<script>
  Vue.createApp({
    data: () => ({ mass: 10 }),
    watch: {
      mass(m) { renderPlot(m); }
    },
    mounted() { renderPlot(this.mass); }
  }).mount('#app');

  function renderPlot(m) {
    const pts = d3.range(0, 10, 0.1).map(t => ({ t, x: m * t * t / 2 }));
    document.getElementById('plot').replaceChildren(
      Plot.plot({
        marks: [Plot.line(pts, { x: 't', y: 'x' })],
        y: { label: 'distance (m)' }, x: { label: 'time (s)' }
      })
    );
  }
</script>
```

(Uncomment the d3 tag too when using `d3.range`; Observable Plot alone
suffices if you build the array with a plain loop.)

The slider + watch + re-render loop is the workhorse: every "let me
tweak" ask. Keep charts *controlled* (re-render on change) rather than
reactive-graph-bound for Plot/Vega — replaceChildren is cheap and avoids
stale-widget bugs. Preact+htm is the same idea with `htmPreact.html`
tagged templates; pick Vue for template-in-string ergonomics, preact
when you want function components.

### Step-through algorithm visualizer

Record algorithm states into an array (snapshots per loop iteration),
then Next/Prev buttons + a slider index the array; render each snapshot
as bars/grid/nodes. This is the honest way to "animate" an algorithm:
deterministic, scrubbable, and the states are real outputs of the real
code you're teaching — not hand-faked frames.

## Wiring live re-render to canvas.css

Read the theme's custom properties once per render so charts follow
light/dark:

```js
const css = getComputedStyle(document.documentElement);
const fg = css.getPropertyValue('--fg').trim();
const accent = css.getPropertyValue('--accent').trim();
```

Pass into Chart.js colors, Plot's `color`/`stroke` options, SVG fills.
Re-read on `matchMedia('(prefers-color-scheme: dark)').addEventListener`
if you want live theme switches.

## Dashboard composition

Vue for layout + state, library-of-the-question for each panel:

- KPI cards (pure HTML), 2–4 charts (Chart.js/uPlot), one table.
- Filters in Vue state; each panel watches and re-renders.
- Keep per-panel rendering functions pure (data in, DOM out) so a filter
  change re-renders exactly one panel.

## Antipatterns

- **Interactive by reflex.** If no parameter is the point, animation and
  sliders are attention tax with no learning payoff — ship the static
  page (the explorable-explanation literature is explicit: widgets
  dumped in a sandbox "are not explanations").
- **Animation that animates nothing.** Spinners, pulsing headers,
  floating particles around a *static* chart: motion signals "this is
  changing"; when nothing changes it's noise.
- **The idle draw loop.** A p5 sketch without `noLoop()` (or a physics
  runner without a stop control) burns CPU forever in a background tab.
  Static image ⇒ no loop; live sim ⇒ visible pause button.
- **matter.js with no render.** It simulates invisibly; the user sees a
  blank page and assumes it's broken. Wire the draw side before
  celebrating the engine side.
- **Handlers that assume globals.** Multiple sketches/widgets on one
  page: p5's global mode collides — use *instance mode*
  (`new p5(s, container)`) per widget.
- **Vue app hydrating a chart the library already owns.** Don't bind
  Vue templates *inside* a Chart.js canvas or Plot SVG; Vue owns the
  chrome (sliders, layout), the library owns its render root.
- **Interaction without keyboard/pointer fallback.** Sliders are native
  elements and free; custom drag-only interactions need at least a
  range input twin, or the widget is mouse-only.
- **Recording "animation frames" by hand-faking positions.** For
  algorithm demos, snapshot *real* intermediate states (see step-through
  recipe); invented frames teach the wrong algorithm.
- **State lost on every live-reload.** Long-running parameter sessions
  fight the quarter-second hot reload; persist widget state in
  `location.hash` or `sessionStorage` so an edit doesn't reset the
  user's sliders.
- **Thirty-second "interactives".** If wiring the widget costs more
  than building three static variants, build the three statics — the
  user gets the same insight sooner.
