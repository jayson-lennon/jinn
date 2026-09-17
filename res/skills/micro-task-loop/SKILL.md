---
name: micro-task-loop
description: Straight-through phased execution for simple, low-risk plans with few moving parts. Runs every phase back to back with no per-phase verification, builds, or commits; tests, lints, and a single commit happen once at the end. Agents MUST continue through all phases autonomously without stopping between phases.
---

# Micro Task Loop

A fast path for implementing simple phased plans. Trades per-phase safety checkpoints for wall-clock time: no builds, tests, or commits until every phase is done, then one verification pass and one commit. The task list tracks progress in real time.

**Project commands are referenced by role** (`check`, `test`, `lint`, `format`, `commit`, `sync-trunk`, `vcs`). The project's `AGENTS.md` resolves each role to its actual command (e.g. for jinn: `test` → `just test`, `commit` → `just commit '<message>'`, `sync-trunk` → `fossil merge trunk`). When this skill says "run the project's `test` command," look up `test` in `AGENTS.md` and run that. If the project does not define a role, skip it.

## Constraints

1.  **Stay on your branch.** Never commit to the project's main line (e.g. `trunk`/`main`/`master`). Your environment is on the correct branch.
2.  **No reverse merge.** Never merge your branch onto the main line.
3.  **No builds or checks mid-loop.** Do not run the project's `check`/`test` commands during phases — not per-package, not per-module, not "just this once." Compile guidance comes only after the final phase. This is the entire point of this skill: avoid rebuild-thrash while stepping through phases of a straightforward plan.
4.  **Continuous execution.** Proceed from one phase to the next without stopping. Only stop when all phases are complete or an unrecoverable error blocks progress.
5.  **Stay in `.plans/<task>/`.** All execution plans go here. Do not create new directories.
6.  **Never rewrite the spec.** The spec (`plan.md`) is annotate only (strikethrough, divergence notes). The task list tracks status, not checkboxes in the spec.
7.  **One task per turn.** Each assistant turn advances exactly one task and ends by flipping that task's status via `todo_set_phase`. If a task grew beyond a single turn of work, you went too deep — split it via `todo_set_phase` (rewrite the current phase with the new sub-task added) and pick up the new sub-task next turn.

---

## Concepts

**Task list** — Live progress tracker, managed via `todo_*` tool calls. Update immediately when state changes: `todo_set_phase` rewrites one phase (including its tasks' statuses), `todo_set_list` replaces the whole list.

**Spec** — The file `plan.md`. Annotate only.

**Task list ≠ acceptance criteria.** The task list tracks _what's done_. Acceptance criteria verify _correctness_. They are separate.

**NEXT block** — Every `todo_*` tool call returns a `→ NEXT` block at the top of its result. It names the next task you should work on (or tells you the phase is complete / all phases are complete). Read it. Obey it. The block exists because the model otherwise drifts.

**Phase-aware list** — Every `todo_*` tool result includes the full task list, with phases you are not currently working on prefixed `(Blocked by previous phase)`. This is a cue, not a hard lock — you cannot jump to a blocked phase until the current one finishes.

---

## Task List Discipline

Update the task list **at the moment a decision is made**, never retroactively:

- Task's work is done → `todo_set_phase` flipping that task's status to `completed` immediately (during implementation, not batched at phase end)
- Discovered unplanned work → `todo_set_phase` right away (rewrite the current phase with the extra task added; use `todo_set_list` only if a whole new phase is needed)
- Task no longer needed → `todo_set_phase` with that task declared as `"status": "cancelled"` (renders as "CANCELLED: \<description\>")
- Task belongs in a different phase → declare it `cancelled` in its current phase via `todo_set_phase`, and include it (pending) in the target phase via a second `todo_set_phase` call

**Do not batch.** A pattern of "do five things, then flip five statuses in one giant `todo_set_phase`" is the failure mode this skill exists to prevent. One task done → one status flip → next task.

---

## The Loop

**Repeat the following cycle until all phases are complete.** Work per-phase, top to bottom.

1.  **Check status.** Call `todo_get_list`. If all tasks in all phases are complete → leave the loop and do **Final verification**. Otherwise, the NEXT block at the top of the result names the next task to work on. Begin there.

2.  Gather information needed for the task you are working on.

3.  **Implement the current task only.** This is a sub-loop, run once per task:

    a. **Restate** the current task in one sentence before touching the keyboard.

    b. **Do the work**. Make the edits the task calls for. Do not run builds, checks, or tests — verification is deferred to the end by design (see Constraint 3).

    c. **Complete the task.** Call `todo_set_phase` for the phase you are working on,
    resending the entire phase with the finished task declared as
    `{"description": "...", "status": "completed"}` (unchanged tasks stay in the
    payload as-is). Marking a task complete and the task's own verification are
    one action.

    d. **Read the NEXT block** returned by `todo_set_phase`. It points at the next task (or says "phase complete — proceed to verify", or "all phases complete — stop").

    e. **Repeat from (a)** with the task named by the NEXT block, until the NEXT block says the phase is complete.

    If you discover that a task is actually larger than expected, call `todo_set_phase` (or `todo_set_list` for a new phase) immediately — then resume the sub-loop at step (a) with whichever task is now next per the NEXT block. Do this so that you don't lose track of what needs to be done.

    If you discover that a task cannot be implemented _as planned_ and there is no obvious solution that **aligns with the user request**, then STOP and explain the details to the user and ask how to proceed.

3.5. **Audit.** When the NEXT block says the phase is complete, call `todo_get_list` and confirm every task in the phase is `[✓]`. If any task is not `[✓]`, **STOP and audit.** You have drifted. Pick the next pending task in this phase and return to step 3. Do not run tests. Do not commit. The list must be clean before you proceed to the next phase.

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

3.  **Commit once.** Refer to the commands table in AGENTS.md for how to properly commit code.

    ```
    <commit command> "<TASK>: <one sentence description>"
    ```

    **NEVER commit broken code.** The commit happens only after tests and lints pass. If an unrecoverable failure blocks verification, stop and report instead of committing.

4.  **Sync.** Run the project's `sync-trunk` command once (resolve conflicts, re-test, commit).

    Never merge your branch onto the project's main line.
