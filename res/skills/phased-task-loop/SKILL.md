---
name: phased-task-loop
description: Structured phased implementation workflow for multi-phase coding tasks. Use when the user wants to execute a phased plan with execution plans, acceptance criteria verification, phase reviews, and spec reconciliation. Agents MUST continue through all phases autonomously without stopping between phases.
---

# Phased Task Loop

A disciplined workflow for implementing multi-phase coding tasks. The task list tracks progress in real time.

**Project commands are referenced by role** (`check`, `test`, `lint`, `format`, `commit`, `sync-trunk`, `vcs`). The project's `AGENTS.md` resolves each role to its actual command (e.g. for jinn: `test` → `just test`, `commit` → `fossil commit`, `sync-trunk` → `fossil merge trunk`). When this skill says "run the project's `test` command," look up `test` in `AGENTS.md` and run that. If the project does not define a role, skip it.

---

## Constraints

1.  **Branch only.** Stay on your branch. Never commit to the project's main line.
2.  **Sync before next phase.** Run the project's `sync-trunk` command before moving to the next phase.
3.  **No reverse merge.** Never merge your branch onto the project's main line.
4.  **Continuous execution.** Proceed from one phase to the next without stopping. Only stop when all phases are complete or an unrecoverable error blocks progress.
5.  **Stay in `.plans/<task>/`.** All execution plans go here. Do not create new directories.
6.  **Never rewrite the spec.** The spec (`plan.md`) is annotate only (strikethrough, divergence notes). The task list tracks status, not checkboxes in the spec.
7.  **Todo list management.** Update the todo list as you work on the overall task. This helps to keep track of what still needs to be done.

---

## Concepts

**Task list** — Live progress tracker, managed via `todo_*` tool calls. Update immediately when state changes.

**Spec** — The file `plan.md`. Annotate only.

**Execution plan** — The file `phase-N.md`. Contains file-by-file implementation details and a verification checklist (acceptance criteria) at the bottom using `[ ]`/`[x]`.

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

1.  **Check status.** Check the todo list. If all tasks in all phases are complete → **done, stop.** Otherwise, the NEXT block at the top of the result names the next task to work on. Begin there.

2.  **Create an execution plan.** Save to `.plans/<task>/phase-N.md` using the `save_plan` tool. Required sections:
    - **Problem** — What and why.
    - **What Moves / What Stays** — Scope boundary.
    - **File Changes** — Numbered list. For each: created/modified/deleted, before/after code snippets, source→destination for moves.
    - **Implementation Order** — Sequence of file changes.
    - **Acceptance Criteria** — `[ ]` checklist. Each item independently verifiable (file exists, test passes, import resolves).

    Do not copy-paste the spec. The execution plan says _how_, file by file.

    Before implementing, check: follows conventions? No circular deps? All consumers updated? Tests preserved?

3.  **Implement the current task only.** This is a sub-loop, run once per task:

    a. **Restate** the current task in one sentence before touching the keyboard.

    b. **Do the work** for that one task. Run the build command after each logical group of changes.

    c. **Update the todo list.**

    d. **Repeat from (a)** with the task named by the NEXT block, until the NEXT block says the phase is complete.

    If the build fails: fix, re-run, continue.

    If you discover that a task is actually larger than expected, update the todo list immediately — then resume the sub-loop at step (a) with whichever task is now next per the NEXT block. Do this so that you don't lose track of what needs to be done.

    If you discover that a task cannot be implemented _as planned_ and there is no obvious solution that **aligns with the user request**, then STOP and explain the details to the user and ask how to proceed.

4.  **Verify.** At the end of the phase, run the full test suite. All tests must pass. Then re-read the execution plan — for every `[ ]` acceptance criterion, verify it's met and change to `[x]`. Fix any gaps before proceeding.

5.  **Commit and cleanup.**

    Refer to the commands table in AGENTS.md for how to properly commit code.

    ```
    <commit command> "<TASK> Phase N: <one sentence description>"   # the project's `commit` command
    <sync command>                                           # the project's `sync-trunk` command (resolve conflicts, re-test, commit)
    ```

    Never merge your branch onto the project's main line.

    _NEVER commit broken code_.

6.  **Review.** _Append_ to the _execution plan_ file: **Changes** (what and why), **Divergence** (what didn't go to plan, or "None"), **Verification** (how verified), **Risks** (concerns or follow-up).

7.  **Annotate the spec.** Add strikethrough / divergence notes only. Never rewrite. This **must** be done at the end of each phase. The purpose is so that the next plan that gets generated from the spec has up-to-date information.

8.  **Go to step 1.**

## When you are done

If the project defines `lint` and `format` commands, run them and fix every error and warning, then `commit` the result. For example:

```
<lint command>                           # the project's `lint` command
<format command>                         # the project's `format` command
<commit command> "<TASK>: Lints fixed"   # the project's `commit` command
```

_All reported errors and warnings must be fixed_, **even if they are from other files that you didn't touch**. **DO NOT SIGNAL COMPLETION UNLESS THERE ARE ZERO WARNINGS**. If the project defines no `lint`/`format` commands, skip this step.
