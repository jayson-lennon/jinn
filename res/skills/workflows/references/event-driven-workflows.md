# Event-driven workflows — sessions that react, not steps that follow

The second jinn workflow architecture. Where a sequential workflow is a
pipeline of one-shot subagents, an event-driven workflow is a **topology
of sessions** that exchange messages and react to completion events: an
orchestrator session, specialist bots, and workers attached on
demand. Use it when the flow needs concurrency, recursion, or
long-lived workers; use the sequential architecture otherwise.

**`res/workflows/autodev/` ships as the worked example** — an autonomous
development loop with a planner, a reviewer, and implementors attached
per issue. This file states the general rules; that directory shows all
of them in working code. Read both before authoring your first
event-driven workflow.

## The topology

Every event-driven workflow has the same shape:

```
                 ┌── tell_session ──▶ planner (1)
   orchestrator ─┼── tell_session ──▶ reviewer (1)
                 └── tell_session ──▶ workers (attached on demand)
        ▲                     │
        └──── notify_parent ──┘        (all reports ride one channel)
```

- **One orchestrator** — the human-facing session that started the workflow. It holds the task list, the ledger, and every decision. It is the only session that talks to every other session.
- **Bots** — child sessions, each from one TOML seed file. Specialists load once each (planner, reviewer, any other role whose procedure is too big to be dispatch data); workers attach on demand, one session per unit of work, from one shared seed.
- **Hub-and-spoke messaging.** Every arrow goes through the orchestrator. Bots learn only their parent's id (the harness appends it to every dispatch); sibling ids never enter their context, and dispatch payloads carry file paths, not session ids. Bots cannot create bots. If two bots seem to need to talk, the workflow is missing an orchestrator step — route it through the hub.

The fanout bound is structural: only the orchestrator attaches, only from
the workflow's fixed set of seed files, and concurrency is bounded by
the work actually claimable — an idle pre-attached pool is waste, and an
orchestrator attaching sessions with no dispatch behind them is broken.
A bot that spawns anything is impossible.

## The wake model — how work actually moves

Sessions cannot start themselves. Everything moves through two
mechanisms, and both are turn-boundary:

1. **`tell_session`** (orchestrator → bot): an idle bot starts a run immediately, seeded from its template plus the message; a busy one queues the message and runs it when its current turn ends. Nothing cancels; nothing races. One queued message is the most a worker should ever hold — the tracker/state files hold the real queue.
2. **`notify_parent`** (bot → orchestrator): lands as a user message that starts the parent's next turn. If the parent is mid-turn, the report **queues until that turn ends**.

The consequence that shapes every event-driven orchestrator: **end your
turn.** After dispatching, or whenever nothing is left to do this
instant, the orchestrator ends its turn — that is what lets the next
report land. A turn held open with sleeps is a turn whose queued
reports never arrive; a turn that never ends is a wedged run. The
orchestrator's turn ends are not pauses to be optimized away; they are
the delivery mechanism. "End your turn" is literal: announce it and
stop calling tools. Sleep loops, `tail -f`, file-watching, and polling
are all turn-holding in disguise — the orchestrator that "waits" with
tools is the orchestrator whose reports never arrive.

A scan timer is a stall detector, not a waiting mechanism, and most
workflows should not have one: a run whose workers always report needs
no scan at all. If a workflow does want one (workers known to die
silently), it is plain shell inside the turn — sleep, rescan state
files, act, end turn — bounded, and named in the instructions as the
stall detector it is. Events do the actual waking.

## Bot seed files

One file per role, TOML, living beside the workflow (loaded by absolute
path via `load_attendant`; the same shape as an `[[bot.entry]]`
block in `jinn.toml`, but workflow-scoped — it cannot be saved to user
config). Load one file **K times to get K workers of that role**; a
pool is one seed loaded repeatedly, not K files.

```toml
[[bot.entry]]
name = "implementor"
behavior = "reset"          # reset | preserve — context across dispatches
trigger = "manual"          # ALWAYS manual for dispatch-driven roles
prep_mode = false
seed_template = "Your last run reported: <prior_report>"
pins = [{ role = "user", text = """ ...standing instructions... """ }]
```

Fields that matter:

- **`trigger`** — `"manual"` for anything the orchestrator dispatches. A completion trigger fires on every parent turn end, which is the opposite of dispatch-driven work: a manual-trigger bot runs only when messaged, so idle workers cost nothing.
- **`behavior`** — `"reset"` wipes context at each dispatch: the run sees its pins plus the one dispatch, so dispatches must be self-contained. Right for fungible workers. `"preserve"` keeps the conversation, so the session's exploration (a repo map, a codebase orientation) survives across dispatches. Right for specialists whose value compounds — and it comes with a hygiene rule: state in the pin what persists (geography, maps) and what must not (verdicts, judgments about past work), or the session starts assuming from its own history.
- **`seed_template`** — the standing text prepended to every dispatch. The `<prior_report>` token is the **only** way a `conclude` report re-enters context. Put the token in a template only when the bot's conclusions arrive in an order that makes the previous one relevant (one work unit per run: a worker). Leave it out otherwise (a reviewer judging many issues in arbitrary order) — a template without the token never receives reports. A template with no token still needs its own text: it is what the bot reads first on every dispatch.
- **`pins`** — the bot's standing instructions: role, what it must never do, how it reports. Written for a stranger; every dispatch assumes the pins are all the context there is (reset) or the durable part of it (preserve).

A seed's pins should always carry: the role in one sentence, the hard
boundaries (never touch the tracker / never write product code / never
create sessions), the procedure or artifact it produces, and the exact
report format — including that it reports via `notify_parent` and never
lets information travel any other way.

## Lifecycle scripts: setup and teardown are the whole contract

`run_lifecycle` setup is **one per session, ever**; teardown is the
playback of what setup recorded. Together they are the workflow's
checkout machine — use them fully or not at all:

- **Spawning a worker = `load_attendant` the seed → lifecycle setup → dispatch.** The bot call creates the session and stamps it; the setup is issued **by the orchestrator, targeting the new session's id**, immediately after — the worker wakes up already sitting in its own checkout or worktree. There is no standing pool to pre-open seats for: on-demand spawn per unit of work keeps every seat owned by a live session, and teardown retires it.
- **Landing = lifecycle teardown.** The script's teardown command does the merge-back, the commit, the checkout removal, and the branch retirement, replaying setup's recorded coordinates (session id only — the args are remembered). Because teardown is the cleanup, cleanup cannot be skipped, forgotten, or aimed at the wrong run's seat: a session that never existed holds no coordinates, so there is nothing to mis-teardown.
- **Order teardown's steps so conflicts fail before anything irreversible.** Both `fossil merge` and `git merge` exit 0 on conflict — gate on markers/unmerged paths and exit nonzero _before_ the branch close/delete. Everything before that point is retryable (a re-run's merges are no-ops); a closed branch cannot accept the retry. A conflicted teardown should leave the worker session alive: it knows the code, so the fix dispatch goes to it, and teardown is re-issued after its report.
- **Never nest `bash -c` in a lifecycle command.** The runner already executes every command through `bash -c`; a wrapper spawns a second shell for nothing, and its single quotes clash with the shell-quoted values the runner substitutes for placeholders.
- **Script variants are workflow-local.** The defaults in `default_jinn.toml` branch and land on main/trunk — the human's own checkout flow. A workflow that needs a different parent ships its own script with the parent as a third parameter (autodev's branch the feature branch and merge back onto it).
- Specialists that need no filesystem seat (readers who work through paths, or through a repository file with read-only SCM commands) get no setup — saving the budget is the point of giving them no checkout.

The lifecycle TOML and one crib-sheet step file are the only places a
workflow should name SCM commands; every other file stays SCM-blind so
porting to another SCM touches two files.

## State files

Event-driven workflows accumulate state that outlives any turn. Keep it
in files, and name each one's owner and purpose in the workflow:

- **The ledger** (orchestrator's private record, beside its checkout): the run's constants, paths, session ids, decisions, and things to carry into the QA brief. The orchestrator's memory across turns.
- **The tracker** (the system's shared record): the work items themselves — one file each, frontmatter for state, body for the work. The tracker is the queue, the dependency graph, and the run's history. Declare its location, its frontmatter schema, and **its single writer** — the orchestrator writes transitions; workers never touch it (they report through `notify_parent` and the orchestrator files the consequences).
- **Artifacts** (contracts, plans, briefs): written by whoever owns them, passed by path. `save_plan` for anything that must survive compaction.

Single-writer is what makes file-based coordination safe in a
multi-session run. Every state file gets exactly one owner; everyone
else reads or requests.

## The orchestrator instructions

A sequential workflow's `workflow.md` is a flow list; an event-driven
one is instructions. It must carry, in order:

1. **Frontmatter** — same as always (`name`, `description`; discovery reads nothing else).
2. **Constants** — the declared numbers and names: branch-naming conventions, tracker and artifact paths, stall thresholds, retry bounds. Every number a session could otherwise improvise.
3. **Startup** — the ordered setup: probe the seat, record the ledger, ensure run state is ignored (never committable), file the goal, load the standing bots, stamp the task list.
4. **The loop** — numbered stages naming their step files, with the turn-end rule stated explicitly: end the turn after dispatching and whenever nothing is left to do; reports wake you.
5. **Amendment** — how the run behaves when the human edits the workflow mid-run: re-read everything, migrate the standing bots (load new seeds, retire old sessions by never messaging them again), let in-flight workers finish under the old seed, append a cycle section to the ledger, and continue — a prompt update is never a restart.
6. **Boundaries** — who writes what, what no session may do, what gates exist (and the statement that none exist beyond the declared human contacts).

## Steps, reports, and routing

- Step files are per-stage procedures the instructions name — written for the orchestrator executing them, not for a stranger subagent (the bots get their procedures through their seed pins and their dispatches).
- Every dispatch states the **report format** the orchestrator will parse (autodev: `DONE <issue-id>-<slug> <branch>` / `BLOCKED <issue-id>-<slug>`; `READY <issue-id>-<slug> <contract-path>`; `APPROVE|REJECT <issue-id>-<slug>`). Short, greppable, first-line-parseable. **Work-item ids carry a title slug everywhere a human might read** — task lists, reports, verdicts, ledgers — never a bare number: `003` alone is opaque to the person watching the run, and every surface is a surface they can watch.
- Every non-success report routes somewhere explicit: a BLOCKED report files its facts as new work items; a REJECT re-dispatches the same worker with the findings verbatim; a stall re-queues the work. Nothing ever just disappears, and the orchestrator never does the worker's job itself to unblock a stage.
- **No escalation paths.** If the flow must assume bots can do what is asked, say so in the pins and the instructions: a dead end is reported as facts (BLOCKED), not as a request for a human. The orchestrator files the facts and re-sequences.

## The human's real presence, gates, and baselines

- **Scheduled contacts are not the only contacts.** A design that says "the human appears only at the goal ask and the QA brief" describes the schedule, not reality: the human can open any bot session, read its work live, and message it directly. Write the seed pins to match — a direct human message is authoritative input, followed and then disclosed in the next report ("the human told me directly"), never silently folded into the plan. An orchestrator that receives a surprising claim ("the human approved this") checks the channel before doubting the reporter: the human _can_ have said it.
- **Human sequencing remarks are quality guidance, not scheduling orders.** "Fix X before landing Y" gets a one-sentence reading at the top of the next turn, then the normal loop proceeds — never a re-ordering into a ceremony the workflow doesn't have.
- **State the feature-model sentence.** The orchestrator should know in one line what "the feature" physically is (autodev: the human's own branch, assembled incrementally by teardowns; no landing step). Without it, an orchestrator invents big-bang landing ceremonies and ordering constraints from ambiguous phrasing.
- **Gates are measured against a recorded baseline.** Any "green before you commit" rule needs the starting state recorded: run the gate on the parent once during planning, write pre-existing failures into the contract, and measure the worker against that. Without a baseline the worker faces an impossible dilemma — commit on red (a rule violation) or stall forever on failures it didn't cause — and it will invent a justification, which reads as dishonesty.
- **Verification verdicts belong to the verifier.** An orchestrator with doubts about a worker's report attaches them to the review dispatch, attributed and verbatim, and never pre-empts the verdict itself (no self-REJECT, no self-APPROVE).

## Failure and stall handling

The harness gives an event-driven workflow no remote cancel, no
attention signal, and no watchdog for wedged turns. Design for it:

- **Stalls**: a worker silent far past its expected effort is abandoned — its work item returns to the queue for re-triage, and the orphan is named in the next human report. The orchestrator never messages a suspected-dead worker again this run; its session just goes idle forever.
- **Non-convergence**: a work item bounced back and forth past a declared bound (autodev: four consecutive REJECTs) leaves the cycle — back to the queue with the findings filed as a new item, so the tracker re-triages it instead of the same two sessions arguing forever.
- **Queued-message hazards**: a message queued behind a cancelled turn can land as an editable draft in the target session's input box. Design dispatch payloads so a stale draft is safe (self-contained, dated by content, idempotent to re-run), and name the hazard in the orchestrator's amendment section.
- **A failing run is data.** Session transcripts, the tracker, and the ledger are readable after the fact (jinn's own session-search tools or the session store). Do not clean up a failed run before reading it.

## Anti-patterns

- **Action verbs without a mechanism.** "Create a session" with no tool named sends the executor hunting; it improvises `spawn_subagent` and silently discards the bot seed. Every prescribed action names its exact tool call.
- **Bot-to-bot messaging.** Sibling ids never enter context; routes go through the orchestrator. A workflow that wants workers to coordinate directly is missing an orchestrator stage.
- **The held turn.** Sleeping in a loop to "wait" for reports. Turn-end is the delivery mechanism; a turn that never ends never receives them.
- **A pool of duplicate files.** Workers are one seed; on-demand attaches load the same file per session. N copies of one TOML drift.
- **The ad-hoc cleanup carve-out.** Lifecycle scripts used for setup but teardown-as-checklist — "trash-move it with this shell recipe." Halfway usage means cleanup correctness rides on the orchestrator's diligence instead of recorded coordinates; use the scripts fully or not at all.
- **Branch/seat names a second run would collide with.** Fixed names (`autodev-main`, a shared trash dir) make two concurrent runs merge into each other silently or purge each other's seats. Name per-run artifacts after the run — or better, derive them from the session's own branch so runs on different branches can never share a seat.
- **The startup purge.** A run that deletes "stale" state at startup cannot tell stale from active. Cleanup belongs to teardown of the run that created the debris — never to the next run's morning.
- **The preserve everything seed.** Preserve without a geography-not-verdict hygiene rule turns a specialist into a grudges-by-email machine: it starts judging from its history instead of the work in front of it.
- **Completion triggers on dispatch-driven bots.** The bot fires on every orchestrator turn end and invents work. Manual trigger, always, for anything the orchestrator dispatches.
- **Reset where re-dispatch loops exist.** A worker that will receive fix-rounds (review rejects, teardown conflicts) must be preserve — a reset seed re-explores its own code on every round.
- **The improvise number.** A retry count, or timeout that lives in the orchestrator's judgment instead of the instructions' constants. Every number is data.
- **Tracker writes from workers.** Single-writer is what makes the file-based queue safe. A worker that edits its own issue file is a race the tracker cannot survive.
- **"The human only sees X."** Any pin or instruction asserting the human cannot reach a session. They can reach every session. Writing the fiction in forces orchestrators to read truthful reports as fabrications and workers to hide real contact. Say "no scheduled contact" and require disclosure instead.
- **Bare-number work-item ids.** `003` in a task list, report, or verdict is opaque to the human watching the run. `003-fix-picker` everywhere ids travel.
- **Gate rules with no baseline.** "All green before commit" with no recorded starting state punishes workers for pre-existing failures and teaches them to invent justifications. Record the parent's gate state in the plan; measure against it.
- **The silent second queue.** Holding undelivered work in session inboxes instead of state files. The tracker holds the queue; inboxes hold at most one message.
- **Bounded fanout as a suggestion.** "A few workers, as needed" is unbounded. K is a constant; recursion is structural (only the orchestrator attaches, from a fixed file set).
