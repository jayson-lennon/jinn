# Chat Input Tokens

While typing a message (input mode — `i` from normal mode), three token kinds
do something special. Each opens an autocomplete popup anchored to the token
you're typing: `<tab>` confirms the highlighted suggestion, arrows move the
selection, typing narrows it.

## `#name` — prompt templates

`#` + a name expands to the full body of a **prompt template** at send time.
Templates are markdown files (with TOML frontmatter) loaded from
`~/.config/jinn/prompts/` and the project's `.agents/prompts/` directories —
when a name exists in both, the most-local file wins.

- `#plan` might expand into your standard planning instructions.
- Browse the available templates in the popup as you type after `#`.
- **Added a file? Re-scan with `gcr` in normal mode** — it's re-read at
  next use.
- **Expansion runs in a first pass** before the message dispatches; `@path`
  attachment resolution is the second pass. By send time the `#token` has
  been replaced entirely, so the model reads the template body.

## `@path` — file attachments

`@` + a path attaches an **image** to the message. The popup lists matching
files (directories with trailing slashes) as you type.

- Paths resolve against the session cwd (`~`-style home paths work). The file
  must exist and be a readable image.
- **Attached images render green. Failed resolutions render red** — the file
  is missing or unreadable as an image — and the token stays literal text.
  The message still sends.
- The active model must support image input. On a text-only or unknown
  model, jinn blocks the attachment with an error entry.
- Images are the entire multimodal surface, and the flow is one-way: from
  your message to the model.

## `/command` — command popup

`/` opens the command popup only as the buffer's very first character — same
confirm/filter keys. At any other position in the text the slash is literal.
All three trigger characters share the confirm key (`<tab>`) and list
navigation.

## Newlines and submission

| Key | Action |
| --- | --- |
| `<enter>` | Send the message |
| `<s-enter>`, `<c-enter>` | Insert a newline (multi-line input) |
| `<c-e>` | Open the buffer in `$EDITOR` for long-form composition |

## Steer an in-flight turn

Input mode has two behaviors, toggled with `<M-q>`:

- **Steer** (the default): a message typed while the agent is working enters
  the steering buffer, which is flushed as one concatenated message between
  tool calls — reaching the model mid-turn — and lands in history as a
  normal user message. This is the only way to steer a live subagent: the
  end of a subagent turn disconnects it from the parent, so a queued
  message arrives after the subagent is gone. Correct the agent mid-turn
  and let the turn keep running. On an idle session, a Steer-mode
  submission behaves like a normal turn.
