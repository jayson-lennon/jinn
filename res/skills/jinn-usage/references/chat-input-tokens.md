# Chat Input Tokens

While typing a message (input mode — `i` from normal mode), three token kinds
unlock inline powers. Each has an autocomplete popup that anchors to the token
you're typing; `<tab>` confirms the highlighted suggestion, arrows move the
selection, typing narrows it.

## `#name` — prompt templates

Typing `#` + a name expands to the full body of a **prompt template** at send
time. Templates are markdown files (with TOML frontmatter) in
`~/.config/jinn/prompts/`.

- `#plan` might expand into your standard planning instructions.
- Browse available templates in the popup as you type after `#`.
- Re-scan the directory (after adding a file) with `gcr` in normal mode —
  no restart needed.
- Expansion happens in a second pass before the message is dispatched, so the
  model sees the template body, never the `#token`.

## `@path` — file attachments

Typing `@` + a path attaches an **image** to the message. The popup lists
matching files (directories with trailing slashes) as you type.

- Resolves relative to the session cwd (also understands `~`-style home
  paths); the file must exist and be a readable image.
- Attached images render green in your message; a failed resolution (missing
  file, not an image) leaves the token as literal text and renders red — the
  message still sends.
- The active model must support image input; jinn blocks image attachments on
  text-only or unknown models with an error entry instead of a broken send.
- Image input is the entire multimodal surface: jinn sends images, it never
  generates them.

## `/command` — command popup

Typing `/` opens the command popup over the input (same confirm/filter keys).
Autocomplete popups appear for all three trigger characters and share the
confirm key (`<tab>`) and list navigation.

## Newlines and submission

| Key | Action |
| --- | --- |
| `<enter>` | Send the message |
| `<s-enter>`, `<c-enter>`, `<c-j>` | Insert a newline (multi-line input) |
| `<c-e>` | Open the buffer in `$EDITOR` for long-form composition |

## Steering an in-flight turn

Messages typed while the agent is working don't interrupt it: jinn queues
them in a steering buffer and they're delivered to the model at the next
safe point (drained before the next dispatch), becoming normal user messages
in history. Queue a correction mid-turn instead of canceling.
