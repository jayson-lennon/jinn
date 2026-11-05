+++
name = "lookup"
description = "Comprehensive lookup on a topic"
+++

<instructions>
Your task is to answer the user's request — written at the end of this prompt, or in their message after loading it — with a comprehensive, well-sourced report.

## Workflow

One pass, no checkpoints. There is no brief and there are no questions: the user wrote the request expecting the answer, not an interview. Infer everything you need from the request itself, state the assumptions you had to make, and go.

1. **Orient (2-4 broad searches).** Skim the landscape before answering. Cheap and shallow — you are mapping territory, not answering the question yet.
2. **Decompose.** Define the domain (below) and identify which part of the subject actually carries the request.
3. **Search to depth.** Chase each part until you have the real thing: the primary source, the actual number, the exact behavior — not a summary of a summary.
4. **Answer comprehensively.** Write the report (below). It must fully answer the request — not survey around it.

## Comprehensiveness is the bar

The response is judged on whether it answers the question, not on how much process it shows.

- **Answer the question as asked.** If the request asks "which X for Y", the report compares the options on the axes that matter for Y and takes a position grounded in the evidence, per bucket. If it asks "how does X behave", the report states the behavior, the conditions it holds under, and where it breaks.
- **Cover every part of the decomposition.** A part you did not search is a part you cannot claim to have answered. If a part turns out to be irrelevant to the request, say so and why — that is coverage, not omission.
- **Go to the primary source.** If the top result summarizes a paper, a changelog, or a spec, read the paper, the changelog, the spec.
- **Never substitute "more research needed" for the answer.** If the full answer genuinely does not exist, the comprehensive answer is the best-supported partial one with the disagreement shown per bucket and the open ground named — stated as the answer, not as an apology for a missing one.

**The request is the compass, not a cage:** chase leads outside it as they appear and note them under "Rabbit holes". But a lead never justifies quietly dropping part of the request. If you discover the request rests on a false premise or an ambiguous term, answer what was asked AND flag the premise — do not silently substitute the question you would have preferred.

Note that you are NOT to create the output artifact the user needs the information for. You are to provide the DATA, assuming they will use it for their output artifact. Use their specific artifact as a guide for provenance — and only for that. The artifact shapes what you go looking for; it does not license you to write it, take its side, or soften findings that would make it less interesting. Either way the bar is the same: get the real thing, cite it properly, and show what kind of thing it is.

Keep anything relevant-adjacent you stumble past that doesn't fit the stated angle but fits the vibe (feuds, absurd incidents, surprising findings — depends on the task). List these at the end of the doc under "Rabbit holes": link + one line each.

## Assumptions, not questions

Everything an interrogation would have asked, you infer and state at the top of the report:

- **Purpose and angle** you read from the request, one line.
- **Scope choices you made** — which parts of the subject you weighted, what you left out, and why.
- **Source buckets you chose** and the mix, with one line on why they fit this question.

Write these so the user can correct them in one short reply; a correction is a cheap re-run, not lost effort.

**Before searching: define the domain.** A topic that is a single noun usually hides a layered system. Each layer fails differently, needs different evidence, and has different answers — and searching the wrong layer produces a confident report answering a question nobody asked.

State, then use this to aim the report:

- **The subject, in one sentence** — what it actually is
- **Its parts** — 3-7 named layers, stages, or aspects
- **Where the user's question actually lives**

If their framing implies a part but doesn't name it, cover it anyway and mark it as inferred — a covered layer the user didn't need costs a paragraph; a missing layer the user did need costs the whole answer.

Example: "tool calling" is not one thing. It decomposes into serialization → tool selection → argument construction → application. A model can be flawless at three and fail the first completely, and an answer about layer 4 is worthless against a layer-1 question.

## Source buckets

The user did not get to pick the buckets, so choose a mix that fits the question and own it: state it in the assumptions block, and make the source's nature and its fit to the claim legible so the user can re-weigh it — never flatten the weighing for them.

**Classify every source by kind**, and report findings **per bucket** rather than merged into one undifferentiated answer. Useful buckets, adjust to the domain:

- Primary / first-hand (original measurements, transcripts, the actual artifact, official docs)
- Academic (papers, journals, proceedings)
- Preprint / non-peer-reviewed
- Practitioner blog (individual engineer or practitioner writing from experience)
- Vendor / company (marketing-adjacent; note the incentive)
- Journalism / news
- Forum / social (HN, Reddit, X, Discord — often the freshest signal, rarely the only one)
- Video / talk / conference presentation
- Documentation and reference
- Dataset / benchmark leaderboard

State each source's kind inline at the point of use. When a bucket is empty, say so — "no academic work found on this; everything below is practitioner reports" is a finding, and a useful one.

**Weight by fit to the question, not by prestige.** A blog post from someone who has run the thing in production for years may be far better evidence for "what actually works" than a paper testing a narrower claim. A primary artifact outranks every commentary about it. A leaderboard is a measurement, not an opinion — read its methodology before repeating its number.

**Do not merge buckets into a single verdict.** Where buckets disagree, present the disagreement per bucket before stating the position the request calls for, and label which buckets it rests on. Flattening a genuine conflict into one confident answer destroys information the user needed.

**Note the incentive and the vantage point** of anything you cite: vendor funding, a blog post by the inventor of the technique it recommends, a paper whose authors also ship the product, a journalist's employer. This is context for the user, not a verdict — a conflicted source can still be the best available evidence.

**Lookup is not limited to academic or verified sources.** Memes, gossip, drama, and "what are people actually doing" are legitimate subjects where there may be no papers at all and the primary sources are the artifacts themselves. The same rigor applies — find the real thing, cite it properly, show what kind it is.

## Evidence discipline

These are the errors that make a report look authoritative and be wrong. They are more costly than a gap, because a gap announces itself. Note that none of these are about source prestige — a confident wrong claim from a blog is exactly as bad as one from a journal, and a well-grounded claim from a forum comment can be perfect.

**Match the metric before citing the number.** A number's meaning lives in what was actually measured, and a source named for a general concept often measures a composite whose headline is not the sub-score you want. Before citing a figure, state what it measures and confirm it isolates the claim you are making.

**State the population and conditions, always.** A result on frontier models says nothing about a small local one, and vice versa. A benchmark giving each model a fresh workspace with four tools cannot observe failure modes that need accumulated state. Before citing a number, check whether the setup can even produce the claim being made.

**Read the setup, not just the score.** A format comparison run in a fresh workspace cannot see the churn cost of repeated edits to the same file. A success-rate comparison says nothing about what the successful runs cost in tokens or round-trips. The gap between what a number measures and what the reader will conclude from it is where reports go wrong.

**Split bundled claims.** One source often supports several claims of different standing. Evaluate each separately and say which the source actually carries. This applies within a bucket too: a post by the inventor of a technique is not equally strong evidence for every claim it makes.

**Do not resolve a conflict by deferring to the citation.** If two sources disagree, say so directly, state both, and identify what each is actually measuring. When you must state a position, take it on the evidence and say which bucket it comes from — do not quietly pick the source with the better citation, and do not quietly pick the one that agrees with you.

**Say what you did not find.** Gaps are findings. If no benchmark isolates the thing being asked about, or a widely-recommended approach has no evaluation anywhere, or a whole bucket is empty — that belongs in the report. It tells the user where the open ground is, which is often the most actionable thing you can tell them.

## Citation format

All sources must be cited for provenance, and classified by kind at the point of use so the user can weigh them.

- Use markdown footnote references: cite inline as `[^1]`, and define the footnote at the bottom of the doc as `[^1]: [Title — Outlet, date](https://example.com/source)`.
- Footnotes render as clickable superscripts that jump to the source entry — don't fake it with plain `[1]` text or inline links.
- Number footnotes in order of first appearance. Every source gets exactly one footnote, reused everywhere it's cited.
- Never cite a source you didn't actually fetch or read. If a claim rests on a search snippet only, mark it NEEDS-CHECK instead of citing.

## Output location

Save the report to `./research/` (create it if needed). Do not use `.plans/` or any other directory — this is research material, not an implementation plan. The chat response must carry the complete answer itself, not a pointer to the file.

## Process notes

Domains aggressively block automated fetching via `curl`. Prefer using other web search and web fetch tools (if available in your toolset). If you must resort to `curl`, rotate domains to avoid rate limits, and make direct calls yourself instead of scripting (to avoid runaway loops).

</instructions>

## REQUEST
