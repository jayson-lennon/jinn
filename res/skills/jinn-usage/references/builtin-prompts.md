# Builtin Prompts

Type `#` in input mode and the prompt popup opens. Each builtin is a markdown
template jinn replaces with its full body at send time — the model sees the
body, never the `#token`. Whatever you type after the token lands after the
body: every template ends with a marker heading or closing structure
(`## SUBJECT`, `## TASK`, `## User task`, `</instructions>`), so your text
arrives where the template expects it.

For expansion mechanics and writing your own templates, see
`chat-input-tokens.md` and `extending-jinn.md`.

## The analysis prompts

`#diverge`, `#rank`, `#judge`, and `#falsify` share one design: the agent
does not answer the question itself in that turn. It writes a short brief,
spawns parallel subagents with the `spawn_subagent` tool, and reports their
answers. The subagents never see the conversation — that's what keeps the
answers independent. Nothing changes: these are read-and-report turns, and
each report leads with the answer in one sentence.

| Prompt     | Question it answers     | Subagents                             | Look at                                      |
| ---------- | ----------------------- | ------------------------------------- | -------------------------------------------- |
| `#diverge` | What else could we do?  | 2, each given a different constraint  | The new ideas                                |
| `#rank`    | Which option is best?   | 2, given the same brief               | Ties — pairs with no consistent winner       |
| `#judge`   | Is this good?           | 3, given the same brief               | Reviewer disagreement                        |
| `#falsify` | How does this fail?     | 3, given the same brief               | Failures nothing would catch                 |

Each ends with a "when this approach is the wrong one" section pointing to a
sibling — `#judge` hands you to `#falsify` when you want failure modes
rather than a quality ruling.

### `#diverge <the problem>`

The only analysis prompt that gives each subagent a different brief: two
constraints chosen to push in different directions ("solve it so the obvious
approach is the one that fails", "solve it at a different layer"). Each
subagent develops one approach and describes its tradeoffs — no option
lists.

Use it when the options on the table feel like variations of one idea. The
report names any new approach that turned out to be a restatement of an
existing one — and if they all do, the current options already cover the
space.

### `#rank <list of choices>`

Compares three or four options. Two is too few to compare, and the prompt
never adds options of its own. Both subagents run the full comparison
independently, and every pair is compared twice — once in each order.

- A pair whose winner changes with order is reported as a tie.
- A circular result (A beats B, B beats C, C beats A) is reported as having
  no winner.
- Both subagents landing on the same order is reported as a reliable
  ranking.

### `#judge <a thing>`

The agent first writes the rubric it will judge against: MUST include
(required properties), ACCEPTABLE alternative (correct approaches that
differ from the obvious one), MUST NOT (disqualifying problems). Three
subagents then rule PASS, PARTIAL, or FAIL against it, each quoting
evidence.

The report shows the verdict table and highlights any disagreement — all
three passing gets said outright. A subagent that errored counts as failed,
not skipped.

### `#falsify <a thing>`

Name the thing and what it is supposed to do. Name only the thing and the
agent infers the goal from the conversation. Three subagents each hunt for
ways it fails. Every finding carries a trigger (the input or state that sets
it off), an explanation of how the goal breaks, a severity, and a quoted
line of evidence — no evidence, no finding.

- Failures nothing would catch automatically are reported first.
- "What held up" lists the attacks that were tried and couldn't be made to
  work.
- No fixes proposed unless you ask.

## The generating prompts

### `#metaprompt <repeatable 1-shot task>`

Writes a new prompt template for a task you expect to repeat. Built for
small tasks that take 1–2 prompts, not full features: it analyzes the task,
designs a structured prompt, and outputs it — ending at the marker where
your task description belongs.

### `#generate-persona <description>`

Writes a complete persona file from a description — "ruthless article
editor", "pedantic code reviewer". The persona comes out in prose, not
bullets: the model copies a persona's formatting style in its responses.
Save it to `~/.config/jinn/personas/`, then select it with the persona
picker (`<leader>se`) or edit it with `c` from the sidebar's Persona
section.

## The research prompt

### `#research <topic>`

The longest builtin workflow, and the only one that stops for your approval
before doing the work:

1. **Orient.** 2–4 broad searches map the topic.
2. **Interrogate.** At most 2 rounds of questions, batched into one message.
   It infers what it can from your request and asks only what it cannot.
   Anything still unknown goes into the brief as a stated assumption.
3. **Brief.** It proposes the brief: objective, scope, source categories (it
   suggests which kinds of sources to use — papers, practitioner blogs,
   issue trackers, forums — and asks which to weight), and what the report
   will contain. Nothing starts until you approve it.
4. **Report.** Findings group by source category, each source classified at
   the point of use. Where categories disagree, the report shows the
   disagreement — it doesn't merge them into one conclusion.

It does not write your output artifact (the post, the paper) — it produces
the research data. Gaps count as findings: no good source exists for a
claim, and the report says so. Output is saved to `./research/` with
markdown footnote citations.

## The code workflow prompts

`#plan`, `#approve-plan`, `#goal`, and `#gap-analysis` implement the
plan→implement→verify workflows described in the README.

`#plan` and `#goal` both consult `.agents/RECORD.md` — the project's record
of current-state facts — before asking questions, and both produce a
**Decision Ledger**: one row per question the dialogue settled, with your
wording of the decision, the rejected alternatives, and whether the choice
was yours (`user`) or the agent's (`agent`). This table matters because both
prompts hand the implementer a document without the conversation — the
ledger is the only record of what you decided.

### `#plan <thing to make/do>`

A Socratic dialogue about how to build the thing. The agent asks numbered
questions — lettered options where real alternatives exist, a recommendation
while the question is open — and traces the code as it goes, so its
questions describe actual current behavior.

- No plan proposal while questions are still open.
- A settled question re-opens once, in one sentence naming the consequence.
- Say "just do it" (or raise the same closed question twice) and the
  dialogue ends — it proceeds with what was decided.
- Nothing is written to disk during the dialogue.

The output is a short High-Level Plan: Problem, Solution, Phases, Acceptance
Criteria, and a table of test cases. When it looks right, follow up with
`#approve-plan`.

### `#approve-plan`

Converts the approved plan into a detailed implementation specification,
saved to `.plans/<task>/plan.md` with the `save_plan` tool. The spec stands
alone: an implementer with none of your conversation can implement from it.
The Decision Ledger is copied in verbatim, then the spec is checked against
it — any decision in the spec the dialogue never settled is flagged or
removed. The todo list comes from the plan's phases, plus a final
Verification phase holding each acceptance criterion, and the ledger comes
back one last time for you to confirm each row.

### `#goal <thing to accomplish>`

The same dialogue process as `#plan`, about what to accomplish rather than
how. The output is a **Task Contract**: End State (checkable facts),
Invariants, Decision Rules for choices you delegate, Constraints, Traps,
Verification, and Acceptance Criteria. No phases, no steps, no code — the
agent decides the route itself.

Flow: question rounds → a short CONTRACT BRIEF (Problem, End State,
done-when) that you approve → the full contract. Approval discards the
conversation — the implementer receives only the contract. Make sure the End
State and Constraints sections say what you mean before you approve.
Afterward, a common pattern is to select the contract message, press `gcip`
to isolate-and-pin it, and tell the agent to begin.

### `#gap-analysis`

A read-only post-implementation check. It reads the plan/spec from
`.plans/`, then evaluates each Acceptance Criterion one by one — met / unmet
/ partial, with evidence and a recommendation for anything unmet. A plan
containing "Record Updates" also gets `.agents/RECORD.md` compared against
what was actually implemented, reporting entries that are missing, wrong, or
newly needed. It does not edit code.

## Choose between them

The README compares the coding workflows in a full table. The short version:

- `#goal` — you can describe the end state but not the route. Fewer question
  rounds, then the agent works autonomously inside the contract.
- `#plan` + `#approve-plan` — you know how it should be built. More question
  rounds, but the implementation follows your decisions. Implement with the
  `simple-task-loop` skill (verifies and commits each phase) or
  `micro-task-loop` (one commit at the end); name the skill in the same
  message.
- `#gap-analysis` — optional last step for either workflow: an
  acceptance-criteria audit report.

The analysis prompts work at any point: `#diverge` before planning, `#rank`
when choosing between approaches, `#judge` on a plan or a diff, `#falsify`
before merging.
