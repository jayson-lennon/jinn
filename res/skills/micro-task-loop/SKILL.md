---
name: micro-task-loop
description: Straight-through phased execution for simple, low-risk plans with few moving parts. Runs every phase back to back with no per-phase verification, builds, or commits — unless an existing plan or task list names its own check/test/commit cadence, which is then followed as written; tests, lints, and a single commit happen once at the end. Agents MUST continue through all phases autonomously without stopping between phases. Only load if explicitly asked.
---

# Micro Task Loop

A fast path for implementing simple phased plans. Trades per-phase safety checkpoints for wall-clock time: no builds, tests, or commits until every phase is done, then one verification pass and one commit. The task list tracks progress in real time.

This deferred cadence is the **default**, not an override. When a plan or task list already exists and names its own checks, tests, or commits, those run where the plan puts them (see **Cadence**).

**Project commands are referenced by role** (`check`, `test`, `lint`, `format`, `commit`, `sync-trunk`, `vcs`). The project's `AGENTS.md` resolves each role to its actual command (e.g. for jinn: `test` → `just test`, `commit` → `just commit '<message>'`, `sync-trunk` → `git rebase main`). When this skill says "run the project's `test` command," look up `test` in `AGENTS.md` and run that. If the project does not define a role, skip it.

## Constraints

1.  **Stay on your branch.** Never commit to the project's main line (e.g. `trunk`/`main`/`master`). Your environment is on the correct branch.
2.  **No reverse merge.** Never merge your branch onto the main line.
3.  **Continuous execution.** Proceed from one phase to the next without stopping. Only stop when all phases are complete or an unrecoverable error blocks progress. Running checks, tests, or commits that the plan or task list names (see **Cadence**) is part of executing the plan, not stopping.
4.  **Stay in `.plans/<task>/`.** All execution plans go here. Do not create new directories.
5.  **Never rewrite the spec.** The spec (`plan.md`) is annotate only (strikethrough, divergence notes). The task list tracks status, not checkboxes in the spec.
6.  **Todo list management.** Update the todo list as you work on the overall task. This helps to keep track of what still needs to be done.

---

## Concepts

**Task list** — Live progress tracker, managed via `todo_*` tool calls. Update immediately when state changes.

**Spec** — The file `plan.md`. Annotate only.

**Task list ≠ acceptance criteria.** The task list tracks _what's done_. Acceptance criteria verify _correctness_. They are separate.

**NEXT block** — Every `todo_*` tool call returns a `→ NEXT` block at the top of its result. It names the next task you should work on (or tells you the phase is complete / all phases are complete).

---

## Cadence

The check/test/commit cadence is resolved once, when the loop starts:

**Planned cadence.** If a plan (`plan.md`) or a task list already exists and names checks, tests, or commits — a task that says "run `check`", a phase ending in a verify/commit step, or spec text that says when to verify — follow **that** cadence as written. Those items are executed like any other task, in the order the plan gives. Do not defer them to the end, and do not add verification the plan does not name.

**Default cadence.** If there is no plan and no task list — or they exist but are silent about checks, tests, and commits — run straight-through: no builds, tests, or commits until every phase is done, then one verification pass and one commit (Final verification, below).

Either way, Final verification still runs once at the end; under the planned cadence its commit step only fires if changes are left uncommitted.

A planned cadence is not an exception to Constraint 3: running the checks the plan names *is* the continuous execution of that plan. Stop only when all phases are complete or an unrecoverable error blocks progress — never to ask whether to run a planned check.

---

## Todo List Discipline

Update the todo list **at the moment a decision is made**, never retroactively:

- Task's work is done → update the todo list.
- Discovered unplanned work → update the todo list.
- Task no longer needed → update the todo list.
- Task belongs in a different phase → update the todo list.

**Do not batch.** A pattern of "do five things, then update the todo list is a failure mode this skill exists to prevent. One part done → update the todo list.

---

## The Loop

**Repeat the following cycle until all phases are complete.** Work per-phase, top to bottom.

1.  **Check status.** Check the todo list. If all items in all phases are complete → leave the loop and do **Final verification**. Otherwise, the NEXT block at the top of the result names the next task to work on. Begin there.

2.  Gather information needed for the task you are working on.

3.  **Implement the current task only.** This is a sub-loop, run once per task:

    a. **Restate** the current task in one sentence before touching the keyboard.

    b. **Do the work**. Make the edits the task calls for. Under the default cadence, do not run builds, checks, or tests — verification is deferred to the end (see Constraint 3). Under the planned cadence, run exactly what the plan or task list names, at the point it names it — and nothing beyond it.

    c. **Update the todo list.**

    d. **Repeat from (a)** with the task named by the NEXT block, until the NEXT block says the phase is complete.

    If you discover that a task is actually larger than expected, update the todo list immediately — then resume the sub-loop at step (a) with whichever task is now next per the NEXT block. Do this so that you don't lose track of what needs to be done.

    If you discover that a task cannot be implemented _as planned_ and there is no obvious solution that **aligns with the user request**, then STOP and explain the details to the user and ask how to proceed.

4.  **Go to step 1.**

---

## Final verification

All phases are done and the task list is clean. Now verify everything in one pass, in this order. Do not stop between these steps unless an unrecoverable error blocks progress.

1.  **Test.** Run the project's `test` command. All tests must pass; fix failures before continuing. Tests exercise the code you wrote across all phases at once, so failures can point at work from an earlier phase — fix forward.

2.  **Lint and format.** If the project defines `lint` and `format` commands, run them and fix every error and warning, then commit the result. For example:

    ```
    <lint command>                           # the project's `lint` command
    <format command>                         # the project's `format` command
    ```

    _All reported errors and warnings must be fixed_, **even if they are from other files that you didn't touch**. **DO NOT SIGNAL COMPLETION UNLESS THERE ARE ZERO WARNINGS**. If the project defines no `lint`/`format` commands, skip this step.

3.  **Commit once.** Refer to the commands table in AGENTS.md for how to properly commit code. Under the planned cadence, earlier phases may have committed already per the plan; commit here only if changes are still uncommitted.

    ```
    <commit command> "<TASK>: <one sentence description>"
    ```

    **NEVER commit broken code.** The commit happens only after tests and lints pass. If an unrecoverable failure blocks verification, stop and report instead of committing.

4.  **Sync.** Run the project's `sync-trunk` command once, after the commit. Run it
    unconditionally — do not defer, skip, or ask about syncing because other sessions
    are working in the repo; concurrent work is normal and is the reason the sync
    exists. Conflicts, if any, are yours to resolve here (resolve, re-test, commit
    the resolution), never the human's.

    Never merge your branch onto the project's main line.
