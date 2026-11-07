# Bots

A bot is a **session that references a parent without inheriting its
conversation**. The parent is an ordinary jinn session. The bot is a second
session that watches it. What crosses over is the _environment_ — the
creating session's cwd, project, profile (model, persona, tool/skill filters,
reasoning effort), and enabled MCP servers. The bot starts with an empty
history and builds its own.

Subagents are different (see `sessions-and-subagents.md`). A
`spawn_subagent` subagent is a session you ask for once to run one
assignment — empty history and task list, but the same model, cwd,
persona, tools, skills, and MCP servers as the parent. A bot runs on a
condition, re-reads the parent each time, and reports back.

Reach for a bot when work should re-run whenever a session finishes a turn —
a reviewer, a test-watcher, an agent that prompts the parent to continue
when it has stalled. Two bot-only tools make the loop possible. `conclude`
records a verdict the user reads. `notify_parent` starts a turn in the
parent. Both are filtered out of every **non-bot** session's tool list — an
ordinary session is never offered them.

## Creating a bot

| Key          | Where                    | Action                                                       |
| ------------ | ------------------------ | ------------------------------------------------------------ |
| `b`          | Sessions sidebar section | New bot of the highlighted session, immediately active       |
| `B`          | Sessions sidebar section | New **standalone** bot (no parent) from the highlighted session's environment |
| `<leader>sb` | Normal mode              | Saved-bots picker — create one from `jinn.toml`              |

`b` puts the new bot in **prep mode**: nothing it holds can dispatch yet,
and anything you type is pinned into its context rather than sent. Compose
its standing instructions as pins, then turn prep mode off (see below) to
let it run.

`b` refuses when the highlighted session is busy, and works only from the
Sessions section.

`B` creates the same kind of session with **no parent**. The highlighted
session supplies only the environment (profile, working directory, MCP
enablement). The standalone bot's trigger is locked to `manual`, its
`notify_parent` tool is withheld — it keeps `conclude`, so its report
history still works — and its seed template starts empty. Use it when the
bot is a self-contained job rather than something that watches a
conversation.

In the saved-bots picker, `<enter>` attaches the entry's bot to the
active session (you stay where you are) and `<c-enter>` spawns it
standalone (no parent, same rules as `B`) and switches you to the new
bot's session. The entry's saved `prep_mode` is honored on either path.
The picker refuses an entry when the active session is itself a bot — a
bot of a bot is a lineage the trigger actor has no rules for.

## The properties popup

`P` opens the bot's properties over the highlighted session — bound
identically in the **Sessions** and the **Bots** sidebar sections; only
the highlighted row differs. The popup is a form of seven fields, in this
order:

| Field             | Values                       | What it governs                                                       |
| ----------------- | ---------------------------- | --------------------------------------------------------------------- |
| **Trigger**       | `manual`, `parent-completed` | When the bot re-runs on its own                                 |
| **Behavior**      | `reset`, `preserve`          | What a run _sees_ of the conversation (never whether it happens)      |
| **Prep mode**     | on / off                     | Whether the bot is still being composed — a gate, not a setting |
| **Tool set**      | `live`, `frozen`             | Whether new tools are admitted automatically or refused               |
| **Skill set**     | `live`, `frozen`             | Same, over skills                                                     |
| **Model**         | `inherit`, `fixed`           | Whether the model the session already holds belongs to this bot |
| **Seed template** | text                         | The instructions injected ahead of each run's prior report            |

`frozen` sets act as an allowlist, `live` sets act as a blocklist.

The **Model** row declares ownership, not model choice. Every bot holds a
concrete model — it needs one to run. This row only says whether that
model is the bot's own or the copy it inherited when it was created.
`inherit` (the default) writes no `model` key when the bot is saved.
`fixed` writes the model it holds, so recreating the bot from its entry
keeps it. Change the model itself from the status bar's model picker, not
here.

### Popup keys

| Key               | Action                                                                 |
| ----------------- | ---------------------------------------------------------------------- |
| `j` / `k`         | Move the form cursor between the seven fields                           |
| `h` / `l`         | Pick a choice on the focused field (walk the row, clamped at its ends) |
| `i`               | Edit the seed template — only from the seed-template field             |
| `?`               | Toggle the help overlay                                                |
| `<c-s>`           | Save this bot to `jinn.toml` (stays open)                        |
| `<enter>`         | Apply all fields and close                                             |
| `<esc>` / `<c-c>` | Restore the values the popup opened with, and close                    |

The properties scope is **navigation-only**: it captures no typed input. A
letter typed while the cursor is on a form field does nothing. Editing text
happens only in the seed-template editor. `<c-c>` is bound and behaves
identically to `<esc>`, though the popup's footer lists only `<esc>`.

### Seed-template editor keys

Opened with `i` from the seed-template field; it is its own text scope.

| Key                               | Action                                          |
| --------------------------------- | ----------------------------------------------- |
| any text / `<backspace>` / arrows | Edit the template                               |
| `<enter>`                         | Keep the edited template                        |
| `<esc>`                           | Restore the pre-editor template                 |
| `<c-c>`                           | Clear the template, or leave when already empty |

### Saving

`<c-s>` writes the bot as an `[[bot.entry]]` in `jinn.toml` — comments and
ordering on sibling entries are preserved. A save applies the popup's
pending values first, so what it writes is what the panel showed. The
session's **name** is the entry's key. Saving under a name that already
exists **replaces that entry in place** — a second `<c-s>` confirms the
overwrite. A bot with no name cannot be saved.

## What a run does

Every run — however it starts — builds a prompt from the **seed template**
and the bot's most recent `conclude` report, then dispatches it as a
fresh turn:

- **The `<prior_report>` placeholder becomes the last report's body.** On
  a first run it becomes the sentence
  "this is the first run, so there is no prior report".
- **The `<parent_id>` placeholder becomes the parent session's id.** That
  is how the bot reads the transcript it reports on. A standalone bot (no
  parent) gets "unavailable" there.
- **The seed renderer only substitutes** — it appends and removes nothing.
  An empty template is valid: the run dispatches whatever the composition
  produces (the message alone, for a message run).
- **The default seed template for a parent bot** is
  `"The parent session's id is <parent_id>. The previous run of this bot
reported: <prior_report>."` A standalone bot's default is empty.

### Trigger

- `manual` — runs only when you re-run it (`G` in the Sessions,
  Bots, or Library sidebar sections). Setting the trigger to `manual`
  just means it never fires on its own.
- `parent_completed` — runs after a **parent turn completes successfully**.
  An errored or cancelled parent turn does not fire it.

A trigger fires **at most once per completed parent turn**. Nothing caps
how often a bot runs overall, and **no guard stops a loop** — two bots
whose triggers wake each other's parents can keep each other running
forever. Keep mutually-triggering bots supervised: prep mode, a `manual`
trigger, or an attentive user is what stops a loop.

A "turn" is one complete tool loop — on a long implementation task with
thousands of tool calls, it ends when the LLM produces its final
assistant response.

### Behavior

The behavior decides what a run _sees_, not whether it runs:

- `reset` (default) — force-excludes every non-pinned entry, so the run
  sees only the pins. The conversation is **not deleted**: the human still
  sees the full transcript, and the excluded entries can be un-excluded
  from context.
- `preserve` — leaves the conversation as it stands. A prior report is
  still included in the seed.

Both behaviors dispatch a message. The difference is the history the run
sees.

## Prep mode

Prep mode (the `b` default) means the bot is still being composed.
While it is on:

- manual re-run is refused (`G` surfaces "Cannot re-run: bot is in prep
  mode" as a system line),
- the trigger does not fire,
- a typed submission is **pinned into context instead of sent**.

A hand-written `[[bot.entry]]` that omits `prep_mode` gets the same
default — it does not run until prep mode is explicitly turned off
(`prep_mode = false`). The rows above prep mode (trigger, behavior) are
shown dimmed while it is on — they do not apply.

## Bot-only tools

`conclude` and `notify_parent` exist only inside a bot. Every other
session's tool list omits both:

- `conclude` records a 1-line verdict in the bot's report log. It carries
  information from previous runs through a reset, and it shows in the
  sidebar for the user. Examples:
  - 8 of 10 tests passed
  - 2 of 3 judges agreed
  - Task list incomplete
  - Retried: 3 times
- `notify_parent` sends a message to the parent session.

## Messaging a bot (`tell_session`)

`tell_session` (available in every session) delivers a text message to any
session by id. On bots it is the dispatch mechanism:

- An **idle** bot starts a run immediately, seeded from its template
  plus the message.
- A **busy** bot queues the message and runs it when its current turn
  finishes. Nothing is cancelled.
- Any other session receives it as a new user turn.

## The Bots sidebar section

Attached bots are displayed in the sidebar for the active session. Open
the one under the cursor with `<enter>` or `i` (insert mode). Each shows
the most recent report, or "no reports yet" if none has been filed. `s` on
a bot in the sidebar shows its historical reports. `G` re-runs the
highlighted bot, cancelling its in-flight turn and the turns of its
bot/subagent descendants. `R` resets its context in place — every
non-pinned entry is force-excluded and nothing is dispatched.

An `⇉` icon next to an attend means it activates automatically on
assistant turn end. `⏸` means prep mode — the bot will not execute.

## The Library sidebar section

`L` in the Sessions section adds the highlighted session to the
**Library** — a flat sidebar section of saved sessions, listed whether
loaded or archived (archived members render dimmed; `<enter>` loads and
switches to one, unarchiving as a side effect). Members are hidden from
the Sessions list while they are in the Library. Removing one (`L` again,
from inside the Library) puts it back.

The same bot keys work over library members: `G` re-runs, `R` resets the
bot's context without dispatching, `P` opens a member bot's properties,
`y` yanks the member's session id, `i` switches and enters input mode, and
`c` resumes a loaded member. Only a bot **with a parent** is refused at
the `L` move — a shelved parent-completed bot would never fire, and parent
links cannot be changed. Plain sessions, forks, subagents, and standalone
bots can all enter.

## Saved bots (`[[bot.entry]]`)

A bot saved from the properties popup becomes one `[[bot.entry]]` block
in `jinn.toml`, restorable by name. The saved-bots picker (`<leader>sb`)
re-reads the document on every open, so a hand edit shows up at the next
picker open or bot creation (see `configuration.md`).

Creating from an entry reproduces its **configured** fields verbatim and
inherits the rest of the environment from the creating session — the same
environment a fresh `b` bot gets. The inheritance rule for every field:

> An **absent** field is inherited from the creating session; a **present**
> field overrides.

For every field, "present" means the literal key is there. An **empty
allow-list is meaningful** — `{ mode = "allow", names = [] }` means "this
bot may use no skills" and is different from omitting the field (inherit
the parent's). Do not strip empty arrays.

| Field              | Type                                                                     | When absent                                                      |
| ------------------ | ------------------------------------------------------------------------ | ---------------------------------------------------------------- |
| `name`             | string                                                                   | Required — the identity, and the key the save matches entries by |
| `behavior`         | `"reset"` \| `"preserve"`                                                | `reset`                                                          |
| `trigger`          | `"manual"` \| `"parent_completed"`                                       | `manual`                                                         |
| `prep_mode`        | bool                                                                     | `true` (still composing)                                         |
| `seed_template`    | string                                                                   | `"The parent session's id is <parent_id>. The previous run of this bot reported: <prior_report>."` |
| `model`            | `{ single = "..." }` or alloy                                            | Inherit the creating session's model — the panel's Model row reads `inherit` |
| `persona_name`     | string                                                                   | Inherit the creating session's persona                           |
| `tool_filter`      | `{ mode = "deny"\|"allow", names = [...] }`                              | Inherit the parent's tool filter                                 |
| `skill_filter`     | `{ mode = "deny"\|"allow", names = [...] }`                              | Inherit the parent's skill filter                                |
| `reasoning_effort` | `"max"`\|`"xhigh"`\|`"high"`\|`"medium"`\|`"low"`\|`"minimal"`\|`"none"` | Inherit the parent's effort                                      |
| `endpoint`         | `{ tag = "...", provider_name = "..." }`                                 | No OpenRouter endpoint pinned for this bot; never written by a save |
| `pins`             | `[{ role = "user"\|"assistant", text = "..." }]`                         | No standing instructions                                         |

Notes on the less obvious fields:

- `pins` are the bot's standing instructions, saved in history order.
  With `reset`, the pinned sequence is what the run sees as prior
  context. A pin **requires** a `role` — a pin without one fails to
  parse, so an agent result is never restored as a user instruction.
  Only `user` and `assistant` entries are saved; a pinned tool result
  is not.
- `endpoint` records the OpenRouter routing slug a run was pinned to.
  Routing itself is resolved per model from `[[endpoint_defaults]]` in
  `providers.toml` at dispatch time, not from the entry (see
  `models-and-providers.md`). Saving never writes this field — it exists
  as documentation in hand-written entries.
- Model selection matters for bots. If you are making one on behalf of
  the user, ask: specific model, or inherit the session model?
- `model` corresponds to the panel's **Model** row. A `fixed` bot writes
  this key, an `inherit`ing bot writes nothing, and an entry that carries
  the key recreates as `fixed`. A hand-written entry without a `model`
  key survives being saved from the popup unchanged. Inheriting has one
  consequence — the bot keeps whatever model it was created with and does
  not re-read the creating session's model on a later run.

### A complete example

The shipped default `jinn.toml` includes a worked entry you can copy. It
ships as `auto-nudge` and might not be present if the user deleted it.

```toml
# ###########################################################################
[bot]

# > "Nudges" a session to complete it's work if it stops mid-way. Relies
# > on the agent having a defined list of work and that it reports how far along
# > it is. This is NOT for evaluating the quality nor checking if the work was
# > actually implemented.
[[bot.entry]]
behavior = "reset"
name = "auto-nudge"
pins = [{ role = "user", text = """
You are in charge of keeping another agent on track. Please read the most recent message from the parent using the `session_fetch` tool and determine if it stopped prematurely. If it stopped prematurely, use the `notify_parent` tool to tell the agent to continue. The "continue" explanation should be brief but firm like "Continue with the <task>".

Examples of agents that stopped too early:
> I am on phase 3 of 5 <explanation of why it stopped>.

Examples of agents that stopped correctly:
> I've finished all phases of the plan.
> I had to stop because there is a problem with the current plan and I cannot continue because <reasons>.

It's important that you only tell the agent to continue if it stopped for something it _should not_ have stopped for. If it got blocked by something, then stopping is OK.

Regardless of the outcome, always use the `conclude` tool to record your conclusion:
- If you had to use `notify_parent` -> the "conclude" should be "Nudged"
- If the agent finished -> the "conclude" should be "Task complete"

You **do not** need to evaluate whether the agent actually did the work. Your job is simply to keep the agent moving if it says that it stopped before finishing. You don't need to check the work at all.""" }]
prep_mode = false
reasoning_effort = "high"
seed_template = "The parent session's id is <parent_id>. The previous run of this bot reported: <prior_report>."
trigger = "parent_completed"
tool_filter = { mode = "allow", names = ["conclude", "notify_parent", "session_fetch", "session_search"] }
skill_filter = { mode = "allow", names = [] }
```

What the example demonstrates, field by field:

- `trigger = "parent_completed"` + `behavior = "reset"` — it re-reads the
  parent after every successful turn, seeing only its own pins. The
  `session_fetch` tool in `tool_filter` is what reads the parent
  transcript.
- `prep_mode = false` — a hand-written entry must state this, or the bot
  will not run.
- `pins` — a single `user` pin holding the whole standing instruction.
  With `reset`, this pin is the only prior context the run sees.
- `tool_filter` / `skill_filter` are both `allow` and both narrow. The
  bot may use exactly the two tools it exists to call plus the two it
  needs to read the parent, and **no skills** (`names = []` is the
  meaningful empty allow-list — "may use no skills", not an absent
  filter).
- `reasoning_effort = "high"` — the `model` key is absent. A bot usually
  runs on a cheaper model than the parent's, and this one takes whatever
  the user had selected.

The `tool_filter` allow-list names `conclude` and `notify_parent`
explicitly. Both tools are always *registered* for a bot, but the filter
applies to the assembled prompt's tool list like any other tool — an
allow filter that omits a name withholds it. Keep both in the list.

The entry also has no `model` field. The bot takes whatever model the
user has selected when it is created, and its Model row reads `inherit` —
so saving it back adds no `model` key.

## Picker and sidebar keys

Report-history picker (opened with `s` in the **Bots** section; see
`keybindings.md` for the section table). It is **read-only** — `<enter>` is
unbound.

| Key                                 | Action                                |
| ----------------------------------- | ------------------------------------- |
| `<esc>`                             | Close the report history              |
| `<c-c>`                             | Clear the filter, or close when empty |
| `<up>` / `<down>`                   | Move selection                        |
| `<pgup>` / `<pgdn>`                 | Page the list                         |
| any letter / `<backspace>` / arrows | Edit the filter                       |

Saved-bots picker (`<leader>sb` in Normal; `<enter>` creates the
highlighted bot on the active session):

| Key                                 | Action                                        |
| ----------------------------------- | --------------------------------------------- |
| `<enter>`                           | Attach this bot to the active session   |
| `<c-a>`                             | Attach and keep the picker open         |
| `<c-enter>`                         | Spawn this bot standalone, switch to it, and close |
| `<esc>`                             | Close without attaching                       |
| `<c-c>`                             | Clear the filter, or close when already empty |
| `<up>`/`<down>`/`<pgup>`/`<pgdn>`   | Move / page the list                          |
| any letter / `<backspace>` / arrows | Edit the filter                               |

## Cancellation

Cancelling a session recursively cancels its bot and subagent
descendants. The cascade **stops at forks** — a fork is an independent
thread, so a cancel does not reach a fork's own descendants.

The double-`Esc` prompt appears whenever the session, or anything a cancel
would reach beneath it, is still running — including when the session's
own turn has already finished and a bot is what remains. Confirming over
such a session stops the descendants below it and leaves the session
itself untouched.

## See also

- `keybindings.md` — the Bots sidebar section's key table
- `configuration.md` — `tool_filter` / `skill_filter` modes and the
  `[[bot.entry]]` snippet
- `context-management.md` — what pinning and forced-exclusion mean for a
  `reset` run
- `models-and-providers.md` — endpoint pinning and reasoning effort
