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

## Fork patch 7 — CalculateNativeWinOcclusion disabled (input pacing on native Wayland)

**Symptom this fixes:** `Input.dispatchMouseEvent` acks take exactly ~5.0s when the
automation window is not the active window (and ack in ~0.04s when it is). A single
humanized click (9-26 bezier points, each a full CDP round-trip) then takes 45-130s,
blowing the daemon's 10s per-command budget on every mouse command — while `evaluate`
stays instant (it does not wait on the compositor frame pipeline).

**Root cause:** on native Wayland (`--ozone-platform` unset → native backend), Chromium's
`CalculateNativeWinOcclusion` feature marks the window occluded/backgrounded from the
compositor's state and applies the browser's background frame throttle: one frame
wakeup per 5 seconds (`kBackgroundFrameInterval`). Input event dispatch acks are gated
on that frame pipeline. Upstream's default args include
`--disable-backgrounding-occluded-windows` and `--disable-renderer-backgrounding`, but
those flags cover the *renderer* throttles, not the native-window occlusion *tracker*
that decides the browser process itself is backgrounded on Wayland.

**Patch:** `DEFAULT_ARGS` now passes
`--disable-features=TranslateUI,CalculateNativeWinOcclusion`.
`--disable-backgrounding-occluded-windows` (already present) keeps renderer-side
behavior; the feature disable stops the Wayland occlusion tracker from ever marking
the automation window occluded, so input acks ride the normal frame pipeline.

**Stealth assessment:** occlusion reporting is a window-manager-side signal, not
page-observable; a real user on a multi-window desktop commonly has backgrounded
windows, and this flag family is already part of the standard automation/UserAgent
field-tooling arg vocabulary (`disable-backgrounding-occluded-windows` ships in
upstream defaults). Risk accepted.

**Pinned by:** `fork_invariants::launch_args_exclude_automation_tells` asserts the
`disable-features` arg contains `CalculateNativeWinOcclusion`.

## Fork patch 8 — OOPIF (cross-origin iframe) target support

**What this enables:** snapshot/evaluate/click inside out-of-process iframes (OOPIF) —
consent banners (Sourcepoint/OneTrust), embedded payment widgets, ad frames. Upstream
chaser-oxide only ever drives page targets; everything inside a cross-origin frame was
invisible, and top-frame trusted mouse dispatch does not route into an OOPIF's process
(proven at CDP level: the event dies at the `<iframe>` element in the parent).

**Four source patches, all in `src/handler/`:**

1. `mod.rs` `on_attached_to_target` — create the targets-map entry from the attach
   event's `TargetInfo` when it is missing. This build's event path delivers
   `Target.targetCreated` for OOPIFs *after* auto-attach (or not at all), so the map
   otherwise has no entry when the session arrives and `get_page` on the child 404s
   forever.
2. `mod.rs` `on_target_created` — do not clobber an attach-bootstrapped target.
   Upstream inserted unconditionally, which dropped the already-registered session and
   pushed a duplicate id into `target_ids`; the poll loop then removed/polled the same
   target several times per pass (verified: duplicated poll-loop entries, orphaned
   `PageHandle` → every command on the parent page failing "receiver is gone"). A
   duplicate creation now refreshes the stored `TargetInfo` only (`Target::set_info`).
3. `mod.rs` `on_detached_from_target` — clear the target's session only when it is the
   detaching one. Auto-attach re-attaches on every OOPIF navigation; a stale detach for
   the old session must not wipe a freshly attached session.
4. `target.rs` `poll` — a target created from its own `attachedToTarget` event already
   has a session (flatten auto-attach); skip the `AttachToTarget` init stage (which
   would mint a second session and orphan the first) and go straight to frame init.
   Also: iframe (`type: "iframe"`) targets are polled like pages so their init chain
   runs at all.

`frame.rs` already carried the earlier fork patch bootstrapping an OOPIF main frame
from a frame-tree root that carries a `parentId` (the embedding page's frame).

**Stealth posture:** OOPIF targets ride the same frame-init chain (no `Runtime.enable`,
no init-path `Network.enable`); evals go through `evaluate_stealth` in the isolated
world; input through `click_human` trusted dispatch on the iframe's own session.
The `wait_for_debugger_on_start` auto-attach pause is released by the existing
`RunIfWaitingForDebugger` handling in `target.rs::on_event`.

**jinn-side wiring** (`crates/jinn-browser/src/backend.rs`): snapshot enumerates
iframe targets, matches them to top-level iframe placeholders by scheme+host+path
(query ignored, src-equal groups in order), walks the interactive elements inside each
matched target, and mints `g<k>:e<n>` refs; `click` dispatches trusted input on the
iframe's own session at iframe-local coordinates; `evaluate` accepts `frame: "g<k>"`.
Every use re-resolves the target live (no caching across navigations); a dead or
re-created frame surfaces as a stale ref with re-snapshot guidance.
