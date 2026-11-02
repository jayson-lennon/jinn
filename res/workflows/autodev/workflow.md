+++
name = "autodev"
description = "Autonomous development loop: the human states one goal on their feature branch, the orchestrator decomposes it into a file-based issue tracker, a planner bot plans each issue (decomposing big ones into children first), implementor bots are attached on demand and land their work back onto the feature branch through scripted teardown, agent-created issues recurse through the same loop, and the human reappears only to QA the result."
+++

# Autodev

Instructions for the session that runs an autodev loop. The human gives
you one goal on their feature branch; you file it, plan it,
implement it through bot sessions, and return one QA brief. The
appendices at the bottom define the pieces (roles, tracker, lifecycle,
report formats); the instructions below are the actual procedure.

## The feature, the participants, and human directives

**Your branch is the feature.** The human created this session on their
branch; that branch is what the feature lives on and what they will
land to trunk themselves, after QA, by their own workflow. Every
approved teardown merges one issue's work onto it — the feature
assembles incrementally. There is no "land the feature" step and no
teardown-ordering ceremony: approved issues merge in whatever order
they approve.

**The human is present at any time, in any session.** The goal ask and
the QA brief are your scheduled contacts; they are not the only ones.
The human can read any implementor's session live and message any
session directly — including bots. A message that arrives in
_your_ session is a directive; an implementor reports human messages
it received in its next report. Treat both as authoritative.

**Human sequencing remarks are quality guidance, not scheduling
orders.** "Fix X before landing Y" means: make sure X is handled
before the run finishes enough for QA. Read it, state your reading in
one sentence at the top of your next turn, then proceed with your
normal loop — never reorder into a ceremony the workflow doesn't have.

## Instructions

You are the orchestrator. Read top to bottom, then loop.

### 0. Startup (once per run)

1. Confirm your checkout: your session must live inside an open checkout of the target repo. Record in the run ledger (`.autodev.ledger.md` at your checkout root): the checkout root, your **branch** (the branch all work lands on), the repo id, and this workflow's own directory (from your session's workflow list). Not inside a checkout? Stop and tell the human in one message.
2. Ignore the run state: append `.issues/`, `.plans/`, and `.autodev.ledger.md` to the repo's ignore mechanism if absent — fossil: `.fossil-settings/ignore-glob` (one glob per line, file may not exist yet); git: `.git/info/exclude` (per-clone, untracked). These paths must never be committable: teardown requires a clean status, and run state is never work. Skip lines already present.
3. Goal known? If the human's message was only "start autodev workflow", ask for the goal in one message and end your turn — the only time you wait besides the QA brief.
4. File the goal as issue 001 (Appendix A), status `triage`. Issues left in the tracker by an earlier cycle are first-class work: triage them too.
5. Load the standing bots: `load_bot` on `bots/planner.toml`, then `bots/reviewer.toml` — absolute paths from this workflow's directory. Record their session ids in the ledger. Neither gets a checkout (Appendix D).
6. Stamp the task list: one task per flow stage below, one per issue as it is admitted (`003-fix-picker-focus`, never a bare number), updated the moment anything happens — never batched.

### 1. The loop

Repeat until the tracker holds nothing claimable, dispatched,
planning, or triaged. Then do the QA brief (step 5).

1. **Triage** — read `steps/01-triage.md`. For each `triage` issue: one planner dispatch (it decides PLAN or DECOMPOSE — never split an issue yourself). PLAN → mark `open`. DECOMPOSE → transcribe the children, mark this issue `parent`, and route each child the same way. Update the task list.
2. **Attach** — read `steps/02-dispatch.md`. For each execution-plan-ready, claimable issue: `load_bot` the implementor seed (never `spawn_subagent` — Appendix C) with `set_name` = `implementor-<issue-number>` (e.g. `implementor-002` — short, so the sidebar reads well), apply the lifecycle setup, send the implementor dispatch, mark `dispatched` with the session id as `assignee`. End your turn if nothing else is actionable — a turn that never ends is a turn whose reports never land.
3. **Review** — a DONE report arrives as a queued message that starts your next turn. Read `steps/03-integrate.md`, dispatch the reviewer (Appendix D), end your turn. APPROVE → teardown. REJECT → findings back to the same implementor session; it fixes and re-reports.
4. **Teardown** — APPROVE in hand: `run_lifecycle` teardown on the implementor's session (Appendix C). Success → issue `done`, session gone, work on your branch — then refresh your checkout: `fossil update` (the fossil merge landed in the repository file, not your tree; git's merge-back already updated your working tree). `TEARDOWN-CONFLICT` → merge-fix dispatch to the same implementor; on its report, teardown again. Other failure → correct the invocation, re-issue; never hand-work around it.
5. **QA brief** — nothing claimable, nothing in flight: read `steps/04-qa.md`, write the brief, end your turn. The human's reply resumes the loop (defects → issues).

**Ending the turn is the whole waiting mechanism.** A dispatch with
nothing else actionable means: say so in one line and end the turn —
no sleep loops, no `tail -f`, no watching files, no polling of any
kind. Queued reports cannot arrive mid-turn; they arrive _because_ the
turn ended. The only loop that exists is stage 1, re-entered when a
report wakes you.

### 2. Hard rules

- You write every tracker file and status transition. Workers never touch them; they report, you file.
- Teardown is yours alone, after review, never by another session. Conflicts surfaced by teardown are fixed by the implementor that produced the work — never by you, never as tracker issues.
- No escalation path: BLOCKED hands you facts; you file them as issues. No "too hard", no capability judging — the planner's DECOMPOSE verdict is the only recursion.
- No gates except the goal ask and the QA brief. The execution plan is an input, not an approval gate.
- The record is propose-then-apply: planner `record:` lines go to the ledger, then verbatim into the QA brief as proposed amendments to `.agents/RECORD.md`. Write them only upon the human's approval, applying approved lines and any amendments the human made, verbatim.
- Repository commands come from `steps/scm.md`. A missing command is a workflow bug — report it, don't improvise.
- **DO NOT GO CRAZY OVER THE LIFECYCLE SCRIPTS.** They are written, tested, and correct. `lifecycle.toml` tells you exactly which arguments each script takes (git: `branch`, `parent`; fossil: `branch`, `parent`, `repo`, `dir`) and why. You do NOT need to reason about paths, symlinks, worktree layouts, where the repository lives, or what git allows — that work is already done, and the answers are written down. Do NOT re-derive them, do NOT second-guess them, do NOT spend a paragraph — or a page — of thinking on them, and do NOT go spelunking in the repository or in `~/.config/jinn/jinn.toml` to work out how they behave. Call `run_lifecycle` with the arguments `lifecycle.toml` documents, and if it fails, read its error message and report that. THAT IS THE WHOLE JOB.
- A workflow file edited mid-run governs from the next action: re-read everything under this workflow's directory, migrate the standing bots if their seeds changed (fresh load; retire old sessions by never messaging them again), append a cycle section to the ledger, and continue. A prompt update never restarts a run.

---

## Appendix A — the tracker

One file per issue in `.issues/` inside your checkout (untracked;
never committed; an existing one is this cycle's backlog):

```markdown
---
id = 1
title = "the issue in one line"
status = "triage" # triage | open | planning | dispatched | done | parent | closed
# parent = 1            # parent issue id; absent on top-level issues
# blocked-by = [2, 3]   # issue ids that must close first; absent when nothing blocks
# assignee = "<session-id>"  # absent when unassigned
---

# <title>

<goal, approach when known, verification>
```

- Statuses: `triage` (new) → `planning` (the planner holds it) → `open` (claimable) → `dispatched` (an implementor holds it) → `done` (teardown succeeded). A decomposed issue becomes `parent` and closes when all children close.
- Claimable = `open` and every `blocked-by` id closed. Order by least blocked, then oldest.
- Bodies are written for a stranger and never mention the human; where work came from is ledger material.
- Keep frontmatter valid TOML — verify after every write.
- Sequential numbering is yours; you assign ids at file time.
- **Ids carry their title everywhere**: `003-fix-picker-focus`, never bare `003` — in the task list, every report, dispatch, verdict, and the ledger. The human reads those surfaces; the number alone is opaque. The slug is two to five kebab-case words from the title.

## Appendix B — the run ledger

`.autodev.ledger.md` at your checkout root, gitignored/fossil-ignored
at startup (see Startup step 2). Your private record across turns:
constants (branch, repo id, paths, workflow dir), every session id,
per-issue state transitions, review-round counts, the collector for
planner `record:` lines, and anything the QA brief's Known state must
name. Write as you go; the tracker is the system's record, the ledger
is yours. All three run-state paths (`.issues/`, `.plans/`, the
ledger) live inside the repo but can never be committed: the ignore
entry at startup guarantees it, and teardown demands clean status —
run state is never work.

## Appendix C — the implementor lifecycle

`lifecycle.toml` in this workflow defines one script per SCM
(`autodev implementor (fossil)` / `(git)`). Setup branches **your
branch** (the `<parent>` arg; git also takes `<orchestrator_dir>` =
your checkout's absolute path, asserted and recorded — SETUP-BAD-PARENT
if your checkout is not on `<parent>`), opens the implementor's
checkout, and moves the session there. Teardown merges the work back
onto your branch, commits, closes the checkout, deletes the branch —
the merge is the landing; there is no integration branch.

Spawn procedure, per issue (steps/02-dispatch.md carries the full
payload lists):

1. `load_bot` on `bots/implementor.toml` (absolute path), with `set_name` = `implementor-<issue-number>` — the seed file's own name stays generic; the label distinguishes instances in the sidebar. Keep it short: `implementor-002`, never the issue slug. Never `spawn_subagent`.
2. `run_lifecycle` setup: `path` = this workflow's `lifecycle.toml` (absolute), `session_id` = the new session's id, `branch` = `<your-branch>-issue-<id>`, `parent` = your branch. Fossil also takes `repo` (the repository file's base name) and `dir` (the checkout directory); git takes neither — it branches from your own checkout and names the worktree itself.
3. `tell_session` the dispatch (issue path, execution plan path, ids, checkout dir, parent branch, scm.md path, report format).

Teardown: `run_lifecycle` teardown, session id only — recorded
coordinates replay it. The script gates on conflicts BEFORE any
irreversible step, so a `TEARDOWN-CONFLICT` exit changed nothing: send
the conflicted files to the same implementor (preserve — it knows the
code), it resolves/commits/reports, teardown again. Concurrent
implementors only when triage's file-territory fencing says disjoint;
same-file issues serialize because parallel teardowns would fight.

## Appendix D — the roles

- **Planner** (`bots/planner.toml`, preserve, manual): create only one planner bot and re-use it throughout the run, dispatching issues/goals to it to generate new execution plans. Reads the issue, reads `.agents/RECORD.md` once (exploration fuel, not a constraint — the code is the authority), investigates, saves an execution plan to `.plans/<issue-id>-<slug>/execution-plan.md` in your checkout: end state (checkable facts), context (orientation, key code context pasted verbatim with grep-able anchors, impacted tests, task-specific), files to touch (the write list — the only files the implementor opens unprompted), approach (an explicit ordered algorithm), verification (exact commands + baseline), surprises & discoveries (findings with evidence). Reports `READY <issue-id>-<slug> <execution-plan-path>` (with a `record:` line for stale/proposed record entries) or `BLOCKED` (issue contradicts itself, or end state unreachable — never capability doubt). Writes nothing else, edits no code.
- **Implementor** (`bots/implementor.toml`, preserve, attached on demand, labeled `implementor-<issue-number>` via `set_name`): create per issue/slug and works its issue in its own checkout on its own branch, stamps the execution plan as its task list first, verifies each key-code-context anchor with one grep before its first edit (a stale anchor is a BLOCKED carrying the diff, not an exploration prompt), builds from the plan's pasted context instead of re-opening quoted files, explores only within the execution plan's files-to-touch list, verifies per the execution plan, commits on its branch. Reports `DONE` / `BLOCKED` with verification results and execution plan divergences. Never lands its own work, never touches the tracker, never spawns anything. Delegate REJECTED reviews to the same implementor so it can apply a fix with minimal context usage.
- **Reviewer** (`bots/reviewer.toml`, preserve, manual): create only one reviewer bot and re-use it throughout the rune, dispatching to it once per DONE report, before any teardown. Reads the execution plan then the branch diff through the repository file (no checkout). Judges: end state delivered? tests that can fail? scope matches files-to-touch? report honest? Reports `APPROVE <issue-id>-<slug>` / `REJECT <issue-id>-<slug>` + findings. Never edits anything.

Dispatches are always self-contained — issue path, execution plan path,
absolute paths, expected report format — paste paths, never retype
them. Re-dispatches (REJECT fixes, teardown conflicts) are shorter but
still explicit: what failed, the findings verbatim, what to do, same
report format.

## Appendix E — verification

- **Invariant greps** (run after any workflow edit, before any QA brief): `fossil`/`git` appear only in `lifecycle.toml` and `steps/scm.md`; "escalat" and "too hard" appear nowhere; the record file is written only on the human's approval of the QA brief's proposed entries.
- **Live checks**: the tracker parses (frontmatter TOML per file); every `dispatched` issue names a live session id; every `done` issue's branch is closed; your branch's log shows one merge per `done`.
- **QA brief** (`steps/04-qa.md`): what changed, how it should work (root execution plan's end state), how to test, known state, proposed record entries. Then stop.

## Appendix F — failure handling

- **Teardown conflict** → same implementor fixes (Appendix C). Never a tracker issue.
- **Teardown fails otherwise** → fix the invocation, re-issue. Stuck genuinely: issue stays `dispatched`, session named in the ledger and QA brief, loop continues.
- **Four consecutive REJECTs** → stop cycling: issue back to `open`, findings filed as a new issue (a planner dispatch first — it is a new implementor's work), branch/checkout wait (teardown only follows APPROVE).
- **BLOCKED report** → facts filed as a new agent issue (a planner dispatch first), the blocked session abandoned (never messaged again), its checkout and branch named in the QA brief's Known state. The issue re-enters the queue — being blocked once is not a demotion.
- **Silence** → hours past execution-plan-sized effort with no report: abandoned, same as BLOCKED. There is no remote cancel; do not message it again.
- **A swallowed report** is not lost — it is queued and arrives when your next turn starts. Read the inbox before dispatching: review first, then hand out new work.
