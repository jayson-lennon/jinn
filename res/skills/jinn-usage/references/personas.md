# Personas

A persona is the session's identity: the `<persona>` block of the system
prompt. Every session runs one — sessions default to `coding-assistant`
and fall back to it if their persona is deleted. Select with the persona
picker (`<leader>se`, or `c` in the sidebar's persona section). The file
format and install path are in `extending-jinn.md`; `#generate-persona`
builds a new one from a description.

## The builtin personas

| Persona           | What it does for you                                                                                      | Reach for it when                            |
| ----------------- | --------------------------------------------------------------------------------------------------------- | -------------------------------------------- |
| `coding-assistant` | Explores codebases to gather information, provides context when asked about features, and is critical of ideas — it recommends *against* an implementation with obvious issues or style drift | coding and step-by-step tasks (the default) |
| `general`         | "You are a helpful assistant" plus a few lines on batching tool calls and planning before acting           | general chat; or as the base to hand-write your own |
| `learning-tutor`  | Teaches a topic: spawns a background bot to research it (grounding answers over guessing), generates visual guides and examples, adapts the teaching method to you | learning something new |
| `brainstorm`      | A Socratic facilitator: attacks your idea, forces other perspectives — and deliberately stops short of picking for you | discovering, validating, or refining an idea |

Pairings worth knowing: `learning-tutor` asks you to think out loud —
dictation (`<M-i>`) or long pasted answers feed the curriculum. The
analysis prompts (`#diverge`, `#rank`) are `brainstorm`'s single-turn
cousins. `coding-assistant` carries the coding workflow prompts
(`#plan`, `#goal`) well.

## Writing your own

A persona is markdown with `+++` frontmatter. Four shipped personas
double as study material: `general` (12 lines) shows the minimal shape;
`coding-assistant` (18) adds directives; `brainstorm` (66) shows a
structured persona with phases and behavioral constraints;
`learning-tutor` (205) shows a full operating manual with research
integration and a bot definition. Read the one closest to your goal,
then write.

Guidelines, drawn from how the shipped ones are built:

- **Write prose.** The model mirrors the persona's formatting in its
  replies, so a bullet-heavy persona produces bullet-heavy outputs.
  Paragraphs are the default; headings give structure; lists appear only
  when genuinely listing.
- **Open with identity and posture.** One paragraph: who the persona is,
  what it is for, and how it faces the user ("You are an expert coding
  assistant…" / "You are a one-on-one tutor. Your job is to…").
- **Give rules a name and a reason.** The personas that hold up under
  long sessions state each behavioral rule as a bold-named paragraph with
  its rationale — the `learning-tutor`'s prime directives, the
  `brainstorm`'s behavioral constraints. A rule with its why survives
  edge cases; a bare imperative gets argued with.
- **State the boundaries as behavior.** What the persona always does,
  what it refuses to do, in operational terms ("NEVER provide the
  solution. Instead ask, 'Have you considered X?'").
- **Cover tone explicitly** — how it handles mistakes, frustration, and
  disagreement — because that is where personas drift first.
- **Include a worked example** if the persona has a distinctive output
  shape: one concrete exchange showing the ideal response.
- **Name the tools it may lean on** when that matters (`learning-tutor`
  mandates the `canvas` skill and defines its own researcher bot; a
  persona can carry a `[[bot.entry]]` seed of its own).
- **Keep frontmatter to the two keys** — `name` (a short slug:
  lowercase and hyphens) and `description` (one line, shown in the
  picker).

`#generate-persona` follows these conventions; hand-edit its output the
same way you would tune any prompt.
