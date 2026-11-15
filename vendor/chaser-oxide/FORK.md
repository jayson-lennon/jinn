# chaser-oxide fork notes

Vendored from the crates.io tarball `chaser-oxide 0.2.4` (April 2026,
MIT OR Apache-2.0, author ccheshirecat). The tarball is the source of
truth: the upstream GitHub repository moved and returns API 404s, so no
git history or upstream patch stream is tracked. Treat this directory as
the fork; changes ride in-tree and are documented here.

Upstream is itself a stealth fork of chromiumoxide: frame init sends
only `Page.enable` + `GetFrameTree` + `SetLifecycleEventsEnabled` (no
`Runtime.enable` — kills the rebrowser C1/C2 CDP-detection signals), JS
evaluation runs through `Page.createIsolatedWorld` (`ChaserPage::
evaluate_stealth`; never `raw_page().evaluate`, which triggers
`Runtime.enable`), and the utility world is renamed `util`.

## jinn fork patches (on top of 0.2.4)

1. **No `Network.enable` at target init (C3 leak).**
   `src/handler/target.rs`: the `TargetInit::InitializingNetwork` stage
   is skipped — frame init falls through to `InitializingPage`. The
   `NetworkManager` stays wired for event processing and explicit
   interception; no enable command goes out on the init path. Consequence:
   `Page::wait_for_navigation_response` (which reads Network-request
   bookkeeping) returns a None-filled request; navigation waiting in jinn
   uses lifecycle events (`SetLifecycleEventsEnabled` is already on).
   The `TargetInit::InitializingNetwork` variant itself stays (dead, matched
   as unreachable) so the enum's shape is stable.

2. **Curated launch args (F1/V1 tells).**
   `src/browser/config.rs`: `DEFAULT_ARGS` drops
   `--disable-ipc-flooding-protection` (F1: observable flooded-IPC
   `postMessage` timing) and
   `--disable-component-extensions-with-background-pages` (V1: observable
   extension-background-page absence). 19 args → 17.

3. **Explicit `pointerType` on every mouse dispatch.**
   `src/handler/page.rs` (`move_mouse`, `click_with`),
   `src/layout.rs` (`From<Point>`), `src/chaser.rs` (`scroll_human`):
   every `Input.dispatchMouseEvent` sets
   `pointerType: "mouse"` explicitly instead of relying on the CDP
   default, so a target scoring `force`/`pointerType` sees a fully
   specified trusted event.

4. **`BrowserConfig::build_args` extracted from `launch`.**
   `src/browser/config.rs`: the argv rendering is a public method
   `launch` calls; `tests/fork_invariants.rs` asserts the flag list
   without spawning a browser.

5. **Viewport emulation disabled at the jinn call site (not a source
   patch).** Upstream's `BrowserConfig::default` pins
   `viewport: Some(Viewport::default())` — a hardcoded 800x600 — which
   fires `Emulation.setDeviceMetricsOverride` on every target init and
   shrinks the page renderer to an 800x600 box inside the real window
   (observed live: `outerWidth=3840` but `innerWidth=800, innerHeight=
   600`). The jinn daemon passes `.viewport(None)`, which takes the
   `None => TargetInit::Initialized` path in `target.rs` — no emulation
   override is ever sent and the page fills the window. An 800x600
   viewport on a large display is itself a bot signal, so this is both
   a correctness and a stealth fix. Pinned by
   `the_upstream_default_viewport_is_the_800x600_pin` in
   `tests/fork_invariants.rs`, so an upstream default change surfaces.

6. **`evaluate_stealth` surfaces script exceptions (E1 leak).**
   `src/chaser.rs`: upstream read only `res.result.result.value`, so a
   script that threw — a `SyntaxError` from `return …` outside a
   function, a thrown error, a thrown selector error — surfaced as a
   silent `None`, indistinguishable from a legitimate null result.
   Recorded failure: an agent burned six `evaluate` calls on silent
   nulls before discovering the expression form. Now the
   `exceptionDetails` payload is checked and its description (falling
   back through value → class name → CDP text) is returned as an
   `Err("script exception: …")`.

## Tests

`tests/fork_invariants.rs` (registered as the `fork_invariants` test
target, no browser needed):

- `init_sends_no_runtime_or_network_enable` — the frame-init chain
  contains neither forbidden method.
- `launch_args_exclude_automation_tells` — no `--enable-automation`, no
  F1/V1 flags, and `--disable-blink-features=AutomationControlled`
  present (the `hidden: true` default).
- `the_upstream_default_viewport_is_the_800x600_pin` — the upstream
  default viewport the jinn daemon's `viewport(None)` works around; the
  assertion documents why the daemon disables emulation.

The upstream live-browser tests (`tests/lib.rs` target) are untouched
and are not run by jinn's suite; the jinn workspace's own test discipline
never launches a real Chrome.

## Launch requirements (jinn side)

Real branded Google Chrome preferred (codec gating), headed — never
`--headless` (S1–S7 signals, focus texture). The jinn daemon supplies
`user_data_dir` and never uses `new_headless_mode`. Known residual
leaks accepted at fork time: mouse presses carry no `force` (fine unless
a target scores pressure); the inherited chromiumoxide layer is
hang-prone, which is why jinn wraps it behind its own liveness/recycle
supervision rather than trusting it.
