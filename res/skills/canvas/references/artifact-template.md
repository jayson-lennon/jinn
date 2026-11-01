# Artifact template

The single-file HTML artifact skeleton. Create a file with this content
(uncommenting the script tag(s) for the libraries you need), then serve
it with the `canvas` tool. Every `/vendor/...` path below is served by
the canvas server itself, so the artifact works offline.

```html
<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>canvas</title>

  <!-- jinn canvas stylesheet: light/dark aware, no dependencies. -->
  <link rel="stylesheet" href="/vendor/canvas.css">

  <!-- The hot-reload script and the interaction tracker are injected by
       the canvas server into the served response — never add either by
       hand. To report clicks back to the agent, mark elements with
       data-metric="name" (and data-metric-* props), or call
       canvas.track(name, props) in your script. -->

  <!-- Uncomment ONE charting/diagram library (or two that layer well).
       Every path below is served by the canvas server — the artifact
       works offline and from any directory. -->

  <!-- Declarative charts, small: -->
  <!-- <script src="/vendor/chartjs/chart.umd.js"></script> -->

  <!-- Time-series / dense numeric line charts, fastest at scale: -->
  <!-- <link rel="stylesheet" href="/vendor/uplot/uPlot.min.css"> -->
  <!-- <script src="/vendor/uplot/uPlot.iife.min.js"></script> -->

  <!-- Declarative (JSON-to-vega-lite) visualization grammar: -->
  <!-- <script src="/vendor/vega/vega.min.js"></script> -->
  <!-- <script src="/vendor/vega/vega-lite.min.js"></script> -->
  <!-- <script src="/vendor/vega/vega-embed.min.js"></script> -->

  <!-- Grammatical layering of scales/shapes/axes (imperative): -->
  <!-- <script src="/vendor/plot/plot.umd.min.js"></script> -->
  <!-- <script src="/vendor/d3/d3.min.js"></script> -->

  <!-- Diagrams from text (flowcharts, sequence, gantt, mindmaps).
       Plain script tag — this build sets a `mermaid` global and is NOT
       an ES module (import resolves to undefined): -->
  <!-- <script src="/vendor/mermaid/mermaid.min.js"></script> -->

  <!-- Math typesetting: -->
  <!-- <link rel="stylesheet" href="/vendor/katex/katex.min.css"> -->
  <!-- <script src="/vendor/katex/katex.min.js"></script> -->

  <!-- Creative coding / generative sketches: -->
  <!-- <script src="/vendor/p5/p5.min.js"></script> -->

  <!-- Reactive UI without a build step: -->
  <!-- <script src="/vendor/vue/vue.global.prod.js"></script> -->
  <!-- <script src="/vendor/preact/standalone.umd.js"></script> -->

  <!-- Code snippet highlighting: -->
  <!-- <link rel="stylesheet" href="/vendor/highlightjs/github.min.css"> -->
  <!-- <script src="/vendor/highlightjs/highlight.min.js"></script> -->

  <!-- 2D rigid-body physics: -->
  <!-- <script src="/vendor/matter/matter.min.js"></script> -->

  <!-- Real 3D scenes (math surfaces, gamedev, molecules, models):
       ES module + import map — uncomment BOTH blocks. Addons are separate
       files that import the bare `three` specifier, so the map must come
       before any module script:
       <script type="importmap">
       {
         "imports": { "three": "/vendor/three/three.module.min.js" },
         "scopes": { "/vendor/three/": { "three": "/vendor/three/three.module.min.js" } }
       }
       </script>
       Core + orbiting camera (OrbitControls needs the map):
       <script type="module">
         import * as THREE from 'three';
         import { OrbitControls } from '/vendor/three/addons/controls/OrbitControls.js';
         window.THREE = THREE; window.OrbitControls = OrbitControls;
       </script>
       -->
</head>
<body>
  <main>
    <h1>canvas</h1>
    <div id="canvas-root"><!-- your visualization mounts here --></div>
  </main>
</body>
</html>
```
