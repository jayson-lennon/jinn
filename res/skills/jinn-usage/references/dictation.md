# Dictation (local voice input)

Press `<M-i>` from any scope to toggle voice recording. Words you speak are
transcribed locally — the model runs entirely on this machine, no cloud path
— and append to the chat input of whichever session holds focus. Dictation
types into the input; it never sends.

Dictation is **off by default**. Until it is enabled, `<M-i>` only hints
"disabled" on the dashboard row and nothing downloads.

> `<M-i>` is the default binding. Press `?` (or `<f1>` in input mode) to open
> the which-key popup, which shows the bindings live in the current scope.

## First use

1. **Enable it.** Set `enabled = true` under `[dictation]` in
   `~/.config/jinn/jinn.toml` (see `configuration.md` for the edit
   protocol) — the reload is picked up and the backend builds the next
   time you arm `<M-i>`.
2. **Prefetch the model if you want.** Run `jinn fetch dictation-model`
   outside the TUI. Skipping it is fine — the first arm downloads the model
   itself, with progress on the dashboard row. The model lands in a
   jinn-managed cache directory, not `~/.cache/huggingface`.
3. **Press `<M-i>` to arm.** The first press runs the full chain: fetching →
   loading → the device opens (yellow meter dots) → recording.

## Recording

| Key       | Action                                     |
| --------- | ------------------------------------------ |
| `<M-i>`   | Arm when ready; disarm when recording      |
| `<enter>` | Submit whatever dictation typed (as usual) |

- **While recording**, the meter strip at the right edge of the chat area
  paints live cyan bars. It hides on full-width tabs; at idle it shows a
  dotted placeholder with an `i` hint at the bottom.
- **Each utterance commits as it closes** — after half a second of silence,
  by default — so text lands in the input while you keep talking. Utterances
  queue in the order spoken. A mid-sentence disarm force-closes the trailing
  fragment, so nothing is dropped.
- Text appends to the end of the focused input buffer, including
  mid-composition in input mode. Dictate, then edit or submit as usual.

## States

The dashboard row names the current phase and model (e.g. "ready
(parakeet-tdt-0.6b-v2)") and updates on every transition.

| Phase     | Meaning                                                     |
| --------- | ----------------------------------------------------------- |
| Offline   | Disabled (or not yet enabled) — `<M-i>` hints how to enable |
| Fetching  | Model downloading; percent shown on the row                 |
| Loading   | Model loading into memory                                   |
| Ready     | Waiting for `<M-i>` to arm                                  |
| Arming    | Device opening; yellow meter dots                           |
| Recording | Live; cyan meter bars; utterances commit as they close      |
| Error     | Terminal fault; the row names it                            |

A failed arm reverts to Ready and puts the reason in the row's note. A
`device` name that matches nothing lists the available inputs — a typo is
self-correcting from the dashboard row alone.

## Configuration (`jinn.toml` → `[dictation]`)

```toml
[dictation]
enabled = true
# device = "USB Microphone"     # omit for the system default input
model = "parakeet-tdt-0.6b-v2"  # the shipped model: English, int8 ONNX
noise_reduction = true          # denoise before transcription (recommended)
```

Device matching is **exact**, not fuzzy — the strings `jinn dictation
list-devices` prints are precisely what `device` accepts. Every other key
(`vad_threshold`, `min_silence_duration`, `min_speech_duration`,
`max_speech_duration`, `decoding_method`, `max_active_paths`,
`blank_penalty`, `num_threads`, `hotwords_file`, `hotwords_score`,
`rule_fsts`) defaults to the shipped behavior and is documented with
comments in the auto-created `jinn.toml`. Hotwords need
`decoding_method = "modified_beam_search"` (the default). `[dictation]`
edits apply at the next arm — the backend rebuilds from the reloaded
config then (see `configuration.md`).

## CLI utilities

| Command                       | What it does                                                                                                    |
| ----------------------------- | --------------------------------------------------------------------------------------------------------------- |
| `jinn dictation list-devices` | Print capture inputs one per line, marking the system default — these names are what `[dictation] device` takes |
| `jinn fetch dictation-model`  | Download the configured model into jinn's cache                                                                 |

## Platforms

Dictation runs natively on **Linux** (PipeWire) and **Windows** (WASAPI).
On other platforms (e.g. macOS) the toggle, meter, and lifecycle still run
but nothing transcribes: `list-devices` and `fetch dictation-model` both
print "dictation backend unavailable on this platform" and exit non-zero.
