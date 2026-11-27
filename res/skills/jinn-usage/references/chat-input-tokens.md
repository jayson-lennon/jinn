# Chat Input Tokens

While typing a message (input mode: `i` from normal mode), three token kinds
do something special. Each opens an autocomplete popup anchored to the token
you're typing: `<tab>` confirms the highlighted suggestion, arrows move the
selection, typing narrows it.

## `#name`: prompt templates

`#` + a name expands to the full body of a **prompt template** at send time.
Templates are markdown files (with TOML frontmatter) loaded from
`~/.config/jinn/prompts/` and the project's `.agents/prompts/` directories;
when a name exists in both, the most-local file wins.

- `#plan` might expand into your standard planning instructions.
- Browse the available templates in the popup as you type after `#`.
- **Added a file? Refresh with `gcr` in normal mode**; it re-scans every
  project resource (prompt templates included), and the new template is
  picked up at next use.
- **Expansion runs in a first pass** before the message dispatches; `@path`
  attachment resolution is the second pass. By send time the `#token` has
  been replaced entirely, so the model reads the template body.

## `@path`: file attachments

`@` + a path attaches an **image** to the message. The popup lists matching
files (directories with trailing slashes) as you type.

- Paths resolve against the session cwd (`~`-style home paths work). The file
  must exist and be a readable image.
- **Attached images render green. Failed resolutions render red** (the file
  is missing or unreadable as an image), and the token stays literal text.
  The message still sends.
- The active model must support image input. On a text-only or unknown
  model, jinn blocks the attachment with an error entry.
- Images are the entire multimodal surface, and the flow is one-way: from
  your message to the model.
- **Display**: in graphics-capable terminals (kitty, ghostty, ...) the
  image also renders in the chat log below the message; `read_image`
  results render in place of their text summary. Without graphics
  support, images stay one-line text placeholders. Opt into block-glyph
  rendering on any terminal with `[graphics] protocol = "halfblocks"`
  in jinn.toml — it applies live and needs no detection.

## The web UI's composer: image attachments

The web UI's composer takes images three ways, all ingested when you send:

- **Paste** a screenshot (or any image) from the clipboard — anywhere on
  the page.
- **The `+` button** beside the composer opens the file picker (the
  photo library on phones). Several at once works.
- **A URL in the text**: any `https://…` link ending in
  `.png .jpg .jpeg .gif .webp` is fetched and attached at send. The
  link stays visible in the message. Other links are never fetched.

Pending images show as removable chips above the composer. A 5 MB
per-image cap applies; oversize, non-image, or failed-URL sends are
refused with a message naming the offender. Images render inline on the
sent message in the transcript, and the same vision gate applies: a
model not confirmed image-capable blocks the attachment (mark it via
`[[providers.model_info]]` `input_modalities` in providers.toml — the
web UI honors the marker even before the first model refresh).

The `#` token works in the web composer too: a `#` at a word
boundary — the start of the draft, or right after a space or newline —
opens a popup above the draft listing the session's templates as
`name - description`, narrowing as you type. `Tab` inserts the bare
`#name` (it expands at send, exactly as in the TUI), the arrow keys move
the highlight, `Esc` closes it, and a tap picks a row on a phone. Moving
the caret out of the token closes it. The `@path` and `/command` tokens
are TUI-only: the web composer sends that text verbatim, and attaches an
image through the `+` beside the draft instead of `@path`.

`Ctrl+Enter` and `Shift+Enter` send when the session is idle and queue
while a turn is running (`Enter` inserts a newline — the reverse of the
TUI's `<enter>` / `<s-enter>`). An `https://` image URL in the text is
fetched and attached too (`webui.md`).

## `/command`: command popup

`/` opens the command popup only as the buffer's very first character; same
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
  tool calls (reaching the model mid-turn) and lands in history as a
  normal user message. This is the only way to steer a live subagent: the
  end of a subagent turn disconnects it from the parent, so a queued
  message arrives after the subagent is gone. Correct the agent mid-turn
  and let the turn keep running. On an idle session, a Steer-mode
  submission behaves like a normal turn.
