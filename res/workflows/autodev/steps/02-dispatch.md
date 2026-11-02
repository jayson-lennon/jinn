# Dispatch — attach an implementor for execution-plan-ready work.

## Execution plan first, always

_Every_ issue flows through the planner before any implementor session
exists for it:

1. **Plan dispatch.** `tell_session` to the planner: the issue file's absolute path, your checkout path (where it saves the execution plan), the issue id. Mark the issue `status: planning` while it runs.
2. **Spawn.** When the planner reports READY, the issue is claimable: mark it `open`, then attach an implementor per the procedure below. **Every freshly-attached implementor must have something the planner generated behind it** — the execution plan is what makes the run bounded. The two legal flows without a new planner round are: a REJECT sending work back to its existing implementor (the review is the execution plan), and a teardown conflict re-dispatch (same execution plan, same worker). Any _new_ issue — reviewer finding, implementor proposal, defect — that gets its own implementor goes through the planner first, every time, however small. The implementor enforces this gate from its side: an implementor dispatched with no plan reports `BLOCKED no-execution-plan`, and that BLOCKED is your signal to run the planner round you skipped.

Triage of a decomposable issue is the same dispatch (01-triage.md):
DECOMPOSE returns children; you transcribe them into tracker files and
dispatch each child back to the planner in plan mode. Never split an
issue yourself — the planner is the only session that has read the
code.

## Find claimable work

Scan the tracker. An issue is claimable when its status is `open` and
every issue named in its `blocked-by` is closed. Order claimable issues
by dependency (least blocked first), then by number (oldest first).
Concurrent implementors are fine when the issues are file-disjoint —
each execution plan fences its territory (names the files it owns), so
disjointness is already on disk. DECOMPOSE children carry their
boundaries in the verdict, which become their issue bodies. When two claimable issues
touch the same files, serialize them: the second waits for the first's
teardown, because parallel teardowns onto the same parent would fight.

## Attaching an implementor

One session per issue. The procedure, in order:

1. Attach the bot: `load_bot` on this workflow's `bots/implementor.toml` (absolute path) with the new session as its parent, passing `set_name` = `implementor-<issue-number>` (e.g. `implementor-002` — the issue number only, no slug; the sidebar is the audience). That call creates the session, stamps the implementor seed, and labels the instance — the seed is where its discipline lives (report formats, task-list mandate, baseline rules, record proposals); the label is how the human tells concurrent workers apart. **Never `spawn_subagent`**: a `spawn_subagent` subagent is a one-shot with no seed, and every session this workflow creates is a bot, without exception.
2. `run_lifecycle` setup `autodev implementor (fossil)` — or the git variant if the target repo is git — with:
   **DO NOT ANALYZE THESE SCRIPTS.** They are written, tested, and correct. Take the arguments `lifecycle.toml` documents and call it. No reasoning about paths, worktree layouts, or what git allows is needed or wanted. If the call fails, read its error message and report that.
   - `path` = this workflow's `lifecycle.toml` (absolute),
   - `session_id` = the new session's id,
   - `branch` = the issue's branch: `<your-branch>-issue-<id>` (derived from your branch — never a fixed prefix. Branches named after the workflow collide across runs and with a run's own leftovers; a branch named after its parent cannot collide),
   - `parent` = your branch (the one recorded in the ledger at startup). The runner moves the session into its checkout.
   - fossil only: `repo` = the repository file's base name (`jinn` for `jinn.fossil`), `dir` = a fresh checkout directory beside your own, named for the issue: `<your-root>-issue-<id>`.
   - git: no `repo` and no `dir` — the script names and creates the worktree itself. Args are `branch`, `parent`, and `orchestrator_dir` = your own checkout's absolute path (setup asserts your checkout has `<parent>` — the orchestrator's branch — checked out, SETUP-BAD-PARENT otherwise, and records the path; the teardown's merge-back runs there).
3. Send the dispatch via `tell_session`.

Paste every path; never retype one — long paths type badly under
pressure and a fat-fingered path is an attach failure (run 1
fat-fingered a bot file name once).

## Implementor dispatch payload

The implementor's reset is gone — it is preserve now — but keep the
dispatch self-contained anyway: a dispatch must be actionable on its
own, even though the session remembers the previous one. For a fresh
attach:

- the issue file's absolute path,
- the execution plan file's absolute path (`.plans/<issue>/execution-plan.md` in your checkout),
- the issue's id and its branch name,
- the implementor's checkout directory (absolute path — the lifecycle setup printed it; it is also the session's working directory),
- your branch name (the parent its teardown will merge into),
- the absolute path to this workflow's `steps/scm.md`,
- the target repo's AGENTS.md or conventions file, if it has one,
- the required report format (DONE/BLOCKED — restate it; do not assume).

For a re-dispatch to the same session (REJECT findings, teardown
conflict fixes): shorter, but still explicit — what failed and why,
the findings or conflict files verbatim, what to do, and the same
report format.

Mark the issue file the moment the attach is live: `assignee` = the
session id, `status: dispatched`. You are the only writer of these
fields.

## Planner dispatch payload

- the issue file's absolute path,
- the issue's id,
- your checkout's absolute path (the execution plan is saved under it, `.plans/<issue-id>-<slug>/execution-plan.md`),
- the absolute paths of `steps/scm.md` and the target repo's conventions file,
- the reminder that its repo map persists (`preserve`) — refresh only what the dispatch says changed.

## Reviewer dispatch payload

Sent after every DONE report, before any teardown (03-integrate.md's
review gate). The reviewer has no checkout — it reads the branch
through the repository file:

- the issue's id and its branch name,
- the execution plan file's absolute path,
- the absolute path of `steps/scm.md` (its review section tells it how to read a branch without a checkout),
- the repository file's absolute path (recorded in your ledger at startup),
- the target repo's conventions file, if it has one,
- the report's execution plan divergences line, if any (the reviewer judges whether the divergence was honest).
