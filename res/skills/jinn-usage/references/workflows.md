# Workflows

A workflow is a repeatable task execution context stored under
`~/.config/jinn/workflows/`. It can be anything from a step-by-step
checklist the session follows, to orchestrated concurrent work where
multiple sessions talk to each other. Each step in a workflow is a
markdown file, editable to tune exactly what that step does.

## Running one

- **Workflow picker:** `<leader>sw` — `<enter>` loads the workflow into
  the current session; `<c-enter>` starts a new session for it.
- **Or just ask:** "run the autodev workflow and <what you want>" in a
  single message — no need to open the picker first.

A loaded workflow drives the session: the session's task list tracks the
steps, and the session walks them, spawning bot sessions where the
workflow says to.

## Building one

Ask jinn: "I need a code review workflow", "create a workflow that burns
captions into my videos" — it walks you through the creation process and
saves the workflow to `~/.config/jinn/workflows/<name>/`. A workflow is:

| File             | Role                                                                     |
| ---------------- | ------------------------------------------------------------------------ |
| `workflow.md`    | The top-level procedure the running session follows (with `+++` frontmatter naming it) |
| `steps/*.md`     | One markdown file per step — edit these to tune what each step does      |
| `bots/*.toml`    | Bot definitions the workflow attaches (see `bots.md`)                    |
| `lifecycle.toml` | Lifecycle scripts the workflow uses for branch/worktree isolation        |

Rescan with `<c-r>` after editing files by hand — workflows are re-read at
next use (see `configuration.md` for the reload categories).

## The bundled `autodev` workflow

`autodev` is jinn's built-in orchestration loop for agentic coding: you
state **one goal** on your feature branch and the orchestrator does the
rest — decomposes the goal into a file-based issue tracker, a planner bot
plans each issue (splitting big ones into children), implementor bots are
attached on demand and land their work onto your branch through scripted
teardown, and agent-created issues recurse through the same loop. The
human reappears only to QA the result, then lands the branch.

Run it via the picker or a single message ("run the autodev workflow and
add dark mode to the settings panel"). See `builtin-prompts.md` for how
it compares against the `#goal` and `#plan` single-session workflows.
