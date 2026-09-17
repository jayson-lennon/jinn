---
name: simple-task-loop
description: Structured phased implementation workflow for multi-phase coding tasks. Use when the user wants to execute a phased plan. Agents MUST continue through all phases autonomously without stopping between phases.
---

# Simple Task Loop

A disciplined workflow for implementing multi-phase coding tasks. The task list tracks progress in real time.

**Project commands are referenced by role** (`check`, `test`, `lint`, `format`, `commit`, `sync-trunk`, `vcs`). The project's `AGENTS.md` resolves each role to its actual command (e.g. for jinn: `test` → `just test`, `commit` → `fossil commit`, `sync-trunk` → `fossil merge trunk`). When this skill says "run the project's `test` command," look up `test` in `AGENTS.md` and run that. If the project does not define a role, skip it.

## Constraints

1.  **Stay on your branch.** Never commit to the project's main line (e.g. `trunk`/`main`/`master`). Your environment is on the correct branch.
2.  **Sync before next phase.** Run the project's `sync-trunk` command before moving to the next phase.
3.  **No reverse merge.** Never merge your branch onto the main line.
4.  **Continuous execution.** Proceed from one phase to the next without stopping. Only stop when all phases are complete or an unrecoverable error blocks progress.
5.  **Stay in `.plans/<task>/`.** All execution plans go here. Do not create new directories.
6.  **Never rewrite the spec.** The spec (`plan.md`) is annotate only (strikethrough, divergence notes). The task list tracks status, not checkboxes in the spec.
7.  **Todo list management.** Update the todo list as you work on the overall task. This helps to keep track of what still needs to be done.

---

## Concepts

**Task list** — Live progress tracker, managed via `todo_*` tool calls. Update immediately when state changes.

**Spec** — The file `plan.md`. Annotate only.

**Task list ≠ acceptance criteria.** The task list tracks _what's done_. Acceptance criteria verify _correctness_. They are separate.

**NEXT block** — Every `todo_*` tool call returns a `→ NEXT` block at the top of its result. It names the next task you should work on (or tells you the phase is complete / all phases are complete).

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

**Repeat the following cycle until all tasks are complete.** Work per-phase, top to bottom.

1.  **Check status.** Check the todo list. If all items in all phases are complete → leave the loop and do **Final verification**. Otherwise, the NEXT block at the top of the result names the next task to work on. Begin there.

2.  Gather information needed for the phase you are working on.

3.  **Implement the current task only.** This is a sub-loop, run once per task:

    a. **Restate** the current task in one sentence before touching the keyboard.

    b. **Do the work**. Run the build/check command after each logical group of changes. A "logical group" might be a single task, an entire phase, or multiple phases depending on the nature of the work. Run build/check regularly so that you know your edits landed properly and that you get expected results (ie: you rename something and expect to get compiler errors indicating what needs to be updated; use this information to help guide the process. You won't see those errors unless you _actually run the commands_).

    c. **Update the todo list.**

    d. **Repeat from (a)** with the task named by the NEXT block, until the NEXT block says the phase is complete.

    If the build fails: fix, re-run, continue. Some tasks are large and will inherently fail to build/check between major phases; this is OK and sometimes expected. HOWEVER, once you reach a point where the code is in a state where it _should_ build/check, then make sure that the build/check no longer fails.

    If you discover that a task is actually larger than expected, update the todo list immediately — then resume the sub-loop at step (a) with whichever task is now next per the NEXT block. Do this so that you don't lose track of what needs to be done.

    If you discover that a task cannot be implemented _as planned_ and there is no obvious solution that **aligns with the user request**, then STOP and explain the details to the user and ask how to proceed.

4.  **Verify.** At the end of the phase, run the full test suite. In most cases: all tests must pass and you must fix failed tests before proceeding. Some changes will require a broken build between phases, such as large refactors; this is an exception to the rule. If a build will be broken between phases, make an attempt to re-order the tasks such that you actually _can_ verify the build/tests between phases.

5.  **Commit and cleanup.**

    Refer to the commands table in AGENTS.md for how to properly commit code.

    ```
    <commit command> "<TASK> Phase N: <one sentence description>"   # the project's `commit` command
    <sync command>                                           # the project's `sync-trunk` command (resolve conflicts, re-test, commit)
    ```

    Never merge your branch onto the project's main line.

    _NEVER commit broken code_. If the code is broken intentionally between phases (like during a large refactor), DO NOT COMMIT. Wait until the next phase where the code builds properly before committing.

6.  **Go to step 1.**

## When you are done

If the project defines `lint` and `format` commands, run them and fix every error and warning, then `commit` the result. For example:

```
<lint command>                           # the project's `lint` command
<format command>                         # the project's `format` command
<commit command> "<TASK>: Lints fixed"   # the project's `commit` command
```

_All reported errors and warnings must be fixed_, **even if they are from other files that you didn't touch**. **DO NOT SIGNAL COMPLETION UNLESS THERE ARE ZERO WARNINGS**. If the project defines no `lint`/`format` commands, skip this step.
