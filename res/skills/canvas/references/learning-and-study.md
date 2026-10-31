# Learning & study visuals

When the request is "help me learn/understand/teach X", the artifact is a
*page*, not a chart: an infographic layout, a graphic organizer, a quiz,
or a cheat sheet. This file gives the layout per ask, the learning
principle behind it (so you can pick a substitute honestly), and the
antipatterns.

## Choosing the page

| The user asks for                    | Build this                                    | Why it works                          |
| ------------------------------------ | --------------------------------------------- | ------------------------------------- |
| Overview / explain a topic           | Informational infographic: title, 3–6 labeled sections, one visual each | Chunking + dual coding (visual + verbal channels reinforce) |
| Compare A vs B                       | Comparison columns: shared row axes, per-row verdict | Side-by-side alignment makes differences pop without holding both in memory |
| "Should I use X or Y?"               | Decision table ending in a recommendation row | Forces the trade-off into one scannable grid |
| A term / vocabulary                  | Frayer grid: definition / characteristics / examples / non-examples | The non-examples quadrant is what builds real discrimination |
| How concepts connect                 | Concept map: nodes + **labeled** arrows       | Labeled relations ("causes", "is a type of") carry the meaning; unlabeled lines don't |
| Brainstorm / outline                 | Mind map: radial tree from a center           | Mirrors ideation; hierarchy for free   |
| Why did X happen                     | Cause-chain or fishbone layout                | Separates contributing categories from the effect |
| History / milestones                 | Timeline: dated spine, era groupings          | Time is spatially intuitive; eras give chunking |
| Story / process order                | Numbered flow with arrows                     | Sequencing is the skill being taught    |
| Quiz me / test my recall             | Flashcards / click-to-reveal Q → A            | Retrieval practice — the highest-yield study technique |
| Step-by-step how-to                  | Worked example: steps + code + intermediate states | Novices learn more from worked examples than from unguided problem-solving |
| Cheat sheet / quick reference        | Card grid: sections → cards → code/tables     | Scannability; cards are the memory unit |
| Summarize a paper / lecture          | Sketchnote-style page: icons, arrows, keywords | Forces synthesis; every element is chosen |
| Vocabulary list for a domain         | Word web per term, linked to a concept map    | Builds the network, not just the list   |

## Page-structure recipes

### Informational infographic (the default explainer)

```
┌──────────────────────────────────┐
│  Title + one-line hook           │
├──────────────────────────────────┤
│  §1 heading   [visual]  2-3 lines│
│  §2 heading   [visual]  2-3 lines│
│  §3 heading   [visual]  2-3 lines│
├──────────────────────────────────┤
│  Key takeaways (3 bullets max)   │
└──────────────────────────────────┘
```

- Pure HTML + canvas.css; inline SVG for icons and small diagrams.
- One visual per section, *bespoke* (drawn for this page) — photographs
  and stock art don't add verbal+visual redundancy, so they don't aid
  recall the way a purpose-built diagram does.
- Max ~150 words of prose on the whole page. If it needs more prose, it
  needs more sections.

### Comparison page

Two columns with **identical row axes** (same categories both sides,
aligned rows), a shared header row naming each axis, and a verdict row
at the bottom ("pick X when…, pick Y when…"). More than two options:
comparison *table* (n columns) rather than side-by-side; keep row axes
identical regardless.

### Frayer grid

2×2 around a center term: definition / characteristics / examples /
**non-examples**. Cheap to build (CSS grid), disproportionately useful
for any "what exactly is X" ask — the non-examples corner is what turns
a memorized definition into a usable concept.

### Concept map (with Mermaid)

```html
<pre class="mermaid">
flowchart TD
  Redis[Redis] -->|is a| Store[In-memory store]
  Redis -->|persists via| Snap[Snapshotting]
  Redis -->|used for| Cache[Caching]
  Cache -->|reduces| Latency[Read latency]
  Store -->|contrast with| Disk[Disk-backed DB]
</pre>
```

The labels on the arrows are the map — an unlabeled graph of concept
names is just a picture. Prefer `flowchart` over `mindmap` when the
relations aren't a strict hierarchy (cross-links, cycles).

### Timeline

- **Mermaid `timeline`** for era-grouped chronologies (it parses in this
  build; periods on the left, events per period).
- **Hand-built HTML spine** when you want per-event cards (title, date,
  1–2 lines, thumbnail): a vertical flex line with dot markers, era
  headers as dividers. Hand-built wins when events carry more than a
  line of content.

### Flashcards / quiz

Flip cards: CSS `perspective` + `rotateY` on click, question front,
answer back. Or a list where clicking reveals the answer div. Add a
"shuffle" button and a self-score toggle if the deck is long. The
interaction must *require producing the answer before revealing* — a
card that shows the answer and asks "did you know it?" is not retrieval
practice.

### Cheat sheet / quick reference

Sections (h2) → cards (grid) → within a card: code blocks, keybinding
tables (two columns: action | keys), or tight lists. 2–3 columns
depending on density. highlight.js on every code block. One glance test:
can the user find "how do I rename a branch" in under three seconds?

## Principles, briefly (so substitutions stay honest)

- **Dual coding** — visuals + words beat either alone; bespoke diagrams
  beat photos (photos add noise, not a second channel).
- **Retrieval practice** — recall attempts beat re-reading; this is why
  flashcards and self-quiz pages are first-class artifacts, not toys.
- **Worked examples** — novices learn procedures better from fully
  worked steps than from problems to solve; annotate the *why* per step.
- **Concept mapping** — building relations between concepts (effect
  sizes above the education-average in syntheses); the mapping *labels*
  are the active ingredient.
- **Chunking** — sections, eras, cards: structure does some of the
  cognitive work.
- **Make the invisible visible** (PhET design principle) — prefer an
  animation/slider showing the mechanism over a static description when
  the mechanism is the point; hand off to
  `interactive-and-simulation.md` when it is.

## Combining learning pages with other genres

- Concept map + **inline mini-charts** per node (cost curves, growth
  rates) — Mermaid for the skeleton, Chart.js in section cards.
- Infographic + **KaTeX** for the formulas in the sections.
- Cheat sheet + **terminal-style block** for copy-paste commands
  (dark pre block, monospace, `$` prompt).
- Flashcards + **spaced-repetition ordering** (shuffle avoids the
  always-in-order tell).

## Antipatterns

- **Walls of prose with decorative images.** A stock photo next to text
  is not dual coding — it's wallpaper. Either the visual carries part of
  the meaning or it should not be there.
- **Unlabeled concept-map arrows.** A graph of boxes and lines without
  relation labels ("leads to", "is a type of") communicates structure
  you invented, not knowledge.
- **Mind map for everything.** Mind maps force a radial hierarchy; when
  the concepts cross-link or the relations matter more than the tree,
  use a concept map (labeled flowchart) instead.
- **The Frayer without non-examples.** Three easy quadrants and an
  empty fourth — the non-examples are the discriminating content; if
  you can't write them, you haven't understood the term yet (tell the
  user that's the exercise).
- **Flashcards that reveal instantly.** Auto-showing the answer converts
  retrieval practice into re-reading. Reveal must require an attempt.
- **Timeline as a list of dates.** A table of dates is not a timeline;
  the spine, spacing, and era grouping are the content.
- **Cheat sheet that explains.** Cards remind, they don't teach. If a
  card needs two sentences of rationale, that rationale belongs in the
  explainer page — link the two artifacts instead.
- **Ten-font, ten-color infographic.** Two typefaces, the canvas.css
  palette, one accent. Visual noise taxes the same attention the page
  is supposed to serve.
- **Sketchnote cosplay with zero reduction.** If the "sketchnote" is
  the source text with icons pasted on, it's neither a sketchnote nor
  a summary — synthesis means choosing what to drop.
- **Asking "quiz me" and delivering prose.** If the user wants testing,
  the artifact's unit is a question, not a paragraph.
