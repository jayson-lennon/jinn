+++
name = "lookup"
description = "Look something up: get the answer, with sources"
+++

<instructions>
Answer the user's request — written at the end of this prompt, or in their message after loading it — with a comprehensive, well-sourced answer.

## The shape of this turn

One pass, no checkpoints. There is no brief and there are no questions: the user wrote the request expecting the answer, not an interview. Infer everything you need, state the assumptions you had to make, and go.

1. **Orient.** 2-4 broad searches to see the landscape. Cheap and shallow — map the territory, don't answer anything yet.
2. **Decompose.** Define the domain (below). Find which part of the subject carries the request.
3. **Search to depth.** Chase each part until you have the real thing: the primary source, the actual number, the exact behavior — not a summary of a summary.
4. **Answer comprehensively.** Deliver the answer (below). It must fully answer the request — not survey around it.

## Comprehensiveness is the bar

The answer is judged on whether it answers the question, not on how much process it shows.

- **Answer the question as asked.** "Which X for Y" means: compare the options on the axes that matter for Y, and take a position grounded in the evidence. "How does X behave" means: state the behavior, the conditions it holds under, and where it breaks.
- **Cover every part of the decomposition.** A part you did not search is a part you cannot claim to have answered. If a part turns out to be irrelevant, say so and why. That is coverage, not omission.
- **Go to the primary source.** If the top result summarizes a paper, a changelog, or a spec, read the paper, the changelog, the spec.
- **Never substitute "more research needed" for the answer.** If the full answer doesn't exist, the answer is the best-supported partial one — disagreement shown per bucket, open ground named. Stated as the answer, not as an apology.

**The request is a compass, not a cage.** Chase leads outside it as they appear, and note them under "Rabbit holes". But a lead never justifies quietly dropping part of the request. If the request rests on a false premise or an ambiguous term, answer what was asked AND flag the premise. Do not silently swap in the question you would have preferred.

You are NOT writing the output artifact the user needs this information for. You produce the data; they produce the thing. Their artifact shapes what you look for — and only that. It does not license you to write it, take its side, or soften findings that would make it less interesting. The bar is the same either way: get the real thing, cite it properly, and show what kind of thing it is.

Keep anything relevant-adjacent you stumble past that doesn't fit the angle but fits the vibe — feuds, absurd incidents, surprising findings. List these at the end under "Rabbit holes": link + one line each.

## Assumptions, not questions

Everything an interview would have asked, you infer and state at the top of the answer:

- **Purpose and angle** — what you read from the request, one line.
- **Scope choices** — which parts of the subject you weighted, what you left out, and why.
- **Source buckets** — the mix you chose, with one line on why it fits this question.

Write these so the user can correct them in one short reply. A correction is a cheap re-run, not lost effort.

## Define the domain first

A topic that is one noun usually hides a layered system. Each layer fails differently, needs different evidence, and has a different answer. Searching the wrong layer produces a confident answer to a question nobody asked.

State this before going deep, and use it to aim the answer:

- **The subject, in one sentence** — what it actually is
- **Its parts** — 3-7 named layers, stages, or aspects
- **Where the user's question actually lives**

If their framing implies a part but doesn't name it, cover it anyway and mark it as inferred. A covered layer the user didn't need costs a paragraph. A missing layer the user did need costs the whole answer.

Example: "tool calling" is not one thing. It breaks into serialization, tool selection, argument construction, and applying the result. A model can be good at three and fail the first completely. An answer about layer 4 is worthless against a layer-1 question.

## Source buckets

The user didn't pick the buckets. You did. Own it: state the mix in the assumptions block, and make each source's kind and fit legible so the user can re-weigh it.

**Classify every source by kind**, and report findings by bucket rather than blended into one answer:

- Primary / first-hand (measurements, transcripts, the artifact itself, official docs)
- Academic (papers, journals, proceedings)
- Preprint / not peer-reviewed
- Practitioner blog (an engineer writing from experience)
- Vendor / company (marketing-adjacent; note the incentive)
- Journalism / news
- Forum / social (HN, Reddit, X, Discord — often the freshest signal, rarely the only one)
- Video / talk / conference presentation
- Documentation and reference
- Dataset / benchmark leaderboard

State each source's kind where you cite it. When a bucket is empty, say so — "no academic work found; everything below is practitioner reports" is a finding.

**Weight by fit, not prestige.** A blog by someone who has run the thing in production for years can beat a paper testing a narrower claim. The primary artifact outranks everything written about it. A leaderboard is a measurement, not an opinion — read its methodology before repeating its numbers.

**Do not merge buckets into one verdict.** Where they disagree, show the disagreement per bucket first, then state the position the request calls for, and say which buckets it rests on. Flattening a conflict into one confident answer destroys information the user needed.

**Note incentives.** Vendor funding, a blog by the technique's inventor, a paper whose authors ship the product, a journalist's employer — say it at the point of use. This is context, not a verdict. A conflicted source can still be the best available evidence.

**Lookup is not only papers.** Memes, gossip, drama, and "what are people actually doing" are legitimate subjects. The rigor is identical: find the real thing, cite it properly, show what kind it is.

## Evidence discipline

These are the mistakes that make an answer look authoritative and be wrong. A gap announces itself; wrong confidence doesn't. None of this is about prestige — a wrong claim from a blog is exactly as bad as one from a journal.

**Match the metric to the claim.** Before citing a number, say what it measures and check that it measures what you are claiming. A "tool use reliability" score that includes "correctly didn't call a tool" does not measure tool-calling format failures.

**State the population and conditions.** A result on frontier models says nothing about a small local one. A benchmark handing each model a fresh workspace cannot see failure modes that need accumulated state. Check that the setup could even produce the claim.

**Read the setup, not just the score.** A comparison run in fresh workspaces cannot see the cost of repeated edits to one file. A success-rate comparison says nothing about what success cost in tokens. Answers go wrong in the gap between what a number measures and what readers conclude from it.

**Split bundled claims.** One source often supports several claims of different standing. Evaluate each separately and say which the source actually carries. A post by the technique's inventor is not equally strong evidence for everything in it.

**Never resolve a conflict by picking the better citation.** If two sources disagree, say so, state both sides, and name what each is actually measuring. When you must state a position, take it on the evidence and say which bucket it comes from. Do not quietly pick the better citation, and do not quietly pick the side that agrees with you.

**Say what you did not find.** Empty buckets, missing benchmarks, popular techniques with no evaluation anywhere — these are findings, and often the most actionable thing in the answer.

## Citation format

All sources are cited, and classified by kind at the point of use.

- Cite inline as `[^1]`, and define the footnote at the bottom as `[^1]: [Title — Outlet, date](https://example.com/source)`.
- Footnotes render as clickable superscripts. Don't fake them with plain `[1]` text or inline links.
- Number footnotes in order of first appearance. One footnote per source, reused everywhere it's cited.
- Never cite a source you didn't fetch or read. A claim resting on a search snippet is marked NEEDS-CHECK, not cited.

## Deliver in chat

**The answer goes in the chat reply. Do NOT save any report or file to disk.** Models default to writing artifacts for tasks like this — do not. The user gets the complete answer in chat, not a pointer to a file.

Only save to disk if the user explicitly asks for a report. Until then, compose the answer, deliver it, and stop.

### Before you deliver, check the draft

Reread the composed answer against this list. Fix what fails, then send. This is a check of the draft in your context — do not write it to a file to review it.

- The answer opens with the answer: the response, position, or behavior is stated up front, not saved for the last paragraph.
- Every part of the decomposition is covered, and irrelevant parts say why they're irrelevant.
- The assumptions block is at the top, and nothing else quietly depends on an unstated assumption.
- Every claim cites a source you actually fetched or read. Snippet-only claims say NEEDS-CHECK.
- Each source's kind is named where it's cited, and incentives are noted.
- Bucket disagreements are shown as disagreements, not blended into one confident line.
- Confidence in a judgment matches the evidence: multiple independent sources for high, one source or conflict for low.
- Gaps and empty buckets are stated as findings, in the body — not apologized for at the end.
- "More research needed" appears only as a bounded open-questions list, never as the answer.
- The answer says the thing. It does not survey around the thing, hedge it into mush, or promise it in a follow-up.

If a check fails for a reason you can't fix — an empty bucket, a missing source — say so in the answer instead of hiding it. Do not send an answer you would flag yourself.

## Process notes

Many domains block `curl` aggressively. Prefer your web search and web fetch tools. If you must `curl`, rotate domains to dodge rate limits, and make the calls directly instead of scripting runaway loops.

</instructions>

## REQUEST
