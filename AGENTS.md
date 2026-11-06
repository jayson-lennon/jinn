# Style Guide

This document defines the _coding conventions_, _patterns_, and _architecture_ for the `jinn` codebase.

- IGNORE ALL CODE IN `vendor/` UNLESS IT'S SPECIFICALLY RELATED TO THE TASK.
- ALWAYS USE `just` RECIPES TO BUILD, TEST, LINT, AND COMMIT. Every command you are told to run is a recipe in the `justfile`; do not invoke a compiler or test runner directly.
- NEVER run the test suite directly, not even across the whole workspace. The suite is slow; run it ONCE per check via `just test`, which tees full output to `target/test-output.log` and prints a summary.
- NEVER pipe the test run through `grep`/`awk`/`head` filters, and never invoke it twice in one command (e.g. once for a tally, once for failure names). The summary and `just test-failures` already provide this — re-deriving it re-runs the whole suite.
- NEVER use a "pre-compile tests" flag before a test run. `just test` compiles anyway; if you only want a compile check, use `just check`.

### Test Discipline

The full workspace suite takes minutes. Treat suite executions as expensive:

1. Run `just test` ONCE. It runs the suite with fail-fast disabled, captures everything to `target/test-output.log`, and prints a passed/failed summary plus failing test names.
2. Get failure details from the capture with `just test-failures` — instant, no rebuild. Or grep the log manually as needed.
3. Iterate cheaply while fixing: `just test-one <test_name_filter>` runs only matching tests across the workspace.
4. Before committing, confirm with one final `just test`.

## 1. Overview

This style guide ensures consistent, maintainable Rust code across the codebase. It covers the slice architecture, error handling, trait-based design, testing patterns, documentation standards, and module organization.

## 2. Architecture: The Slice System

`jinn` is assembled out of **slices**. A slice is one self-contained domain feature: it owns its state, its logic, its view, and its keybinds, and it contributes those to the application at startup through a single `activate` function. The kernel never names a slice; a slice never reaches into the kernel's enums.

Four pieces make this work, and every convention below follows from them.

### 2.1 Implementation crate + `-msg` contract crate

Slice crates live under `crates/slices/`. Most come in a pair:

- `crates/slices/jinn-foo/` — the **implementation**: the actor, the view, the routes.
- `crates/slices/jinn-foo-msg/` — the **contract**: the cell payload, the slot key, the scope id, and the command/event structs that cross the slice boundary.

The dependency arrow points one way: **the implementation crate depends on its `-msg` crate; a `-msg` crate never depends on an implementation crate.** A `-msg` crate holds pure data and pure functions only — no actors, no traits with behavior, no `activate`.

This split exists so the shared cell catalog can name a slice's payload without pulling in that slice's actor and renderer. It is why every cell payload type lives beside its slot key in the `-msg` crate, and why `jinn-slices` (below) can be consumed by the kernel while slices depend on the kernel.

Exceptions exist and are legitimate. A slice with **no cell, no routes, and no view** needs no contract crate — the watchdogs are the worked example: two actors, config read at activation, no shared state. A slice whose cell is read by four different crate families puts the payload in `jinn-slices` instead. Follow the pattern, not the directory name.

### 2.2 `jinn-slices` — shared vocabulary

`crates/jinn-slices` sits **below** the kernel and holds the types every slice uses, as opposed to the types one slice owns. It never depends on `jinn-kernel`, which is what lets a slice import it freely.

It defines the cell primitives (`TypedCell`, `SlotKey`, `SlotTaken`), the dynamic identity types (`SliceScopeId`, `DynamicIntent`, `RouteId`), the activation surface (`SliceHost`), the routing table (`KeyRoutes`, `RouteRow`, `RouteOutcome`, `ActionFn`, the three hook aliases), the view primitives (`SliceView`, `Viewport`, `Region`, `RenderFacts`), and the bus wrapper (`BusService`, `BusMessage`, `PublishSink`).

There is **no registry of slice ids**. Spelling a new `SliceScopeId` is exactly the act of creating a slice.

### 2.3 `activate()` at composition

Composition owns the boot list: `src/bootstrap/slices.rs` holds `activate_all`, which is called once from `src/actor_wiring.rs`. Every `pub fn activate*` in a slice crate is called from there and nowhere else in production.

There is no trait and no macro for `activate` — each slice declares the signature it needs. The convention that fits the tree:

- **Take `host: &mut SliceHost<'_, jinn_slices::RenderFacts>` first** when the slice touches cells, routes, overlays, or views. This is the majority case.
- Add `&State` and/or `Services` after it when the slice reads shared state or configuration.
- A slice that only spawns actors may take `&ActorSystem` and its deps instead, with no host at all.
- A slice that can fail to activate returns `Result`; composition propagates with `?`.

`SliceHost` is the complete contribution surface. Its verbs: `register_cell`, `spawn_service`, `attach_rows`, `register_tab_scope`, `register_overlay`, `register_overlay_slot`, `register_overlay_selectable`, `set_flag`, plus accessors `system()`, `slices()`, `key_routes()`, `viewport()`. Anything else a slice needs (`register_render_slot`, `register_scope_hint`, `push_pre_render_hook`) goes through `host.slices()`.

**Activation order is behavioural, not cosmetic.** The boot list is written in blocks: the cell catalog, then producers, then independents, then system-level actors. Inline comments at each call name the constraint that forces the order — a subscription that must exist before an actor spawns, a cell that must be minted before a consumer resolves it. Read them before moving a line.

### 2.4 The cell catalog

A cell is a slice's private storage: a `TypedCell<T>` under a `SlotKey`. The owning slice gets the write handle; everyone else resolves read-only.

**Every cell in the workspace is registered in one function**: `jinn_cell_catalog::register_all_cells`, in `crates/jinn-cell-catalog/src/lib.rs`. It runs before any slice activates, and every test harness calls it when it needs a seeded registry. A slice's `activate` **resolves** its cell; it never registers it.

Adding a cell means two edits: one `register!` entry in the catalog, and bumping `EXPECTED_CELL_COUNT` alongside it. The count assertion is a tripwire that catches both a stale count and a payload registered under the wrong type.

### 2.5 Route rows and key dispatch

Keybinds are **data, not enum variants**. A slice registers `RouteRow`s at activation; composition turns them into keymap bindings after every slice has activated.

A `RouteRow` says: _in this scope, this key, produce this outcome._ The outcome is one of:

- `RouteOutcome::Action` — a closure `ActionFn` that runs in the handler, resolved by a linear scan over `(slice, action)`.
- `RouteOutcome::StaticIntent` — a shared-chrome key (quit, which-key, tab switch) that composition binds to a real `KernelIntent` via the `static_intent` table in `crates/jinn-tui/src/keymap_gen.rs`. A route id missing from that table logs a warning and leaves the key unbound; it is a wiring bug, not a compile error.

The resulting intent is `KernelIntent::Dynamic(DynamicIntent)`, where `DynamicIntent` carries the slice, the action name, a display label, and an optional byte payload. **A slice's identity is data, so no slice ever edits a central enum.** That is the whole point of the layer.

`BindSite` decides where a row binds: its own dynamic scope, every scope (`GlobalToggle`), or named static scopes.

A slice may also register three per-scope hooks: `InputHook` (editing intents while the scope is focused — the synchronous typing carve-out), `KeyHook` (catch-all for unbound keys, e.g. a terminal capturing raw bytes), and `ScopeEnterHook` (fires once when a transition lands on the scope).

Slices can also declare a `scope_signal` on a `RouteResult` to request a focus-stack push or conditional pop. **The `IntentHandler` is the only writer of the scope stack**; a slice declares the transition as data and the handler applies it before any message publishes.

### 2.6 Data flow

```
  Keyboard / Mouse / Script
         │
         ▼
  Keymap  ────────────────────────────────┐
  (built-ins + rows generated from KeyRoutes) │
         │                                    │
         ▼                                    │
  IntentHandler::handle  (sync)                │
    1. route rows (DynamicIntent) ─────────────┘  (early return on a hit)
    2. slice input hooks                       (early return on a hit)
    3. built-in match arms → per-feature handle_*
    4. apply scope signal, return IntentResult { messages, scope_signal }
         │
         ▼
  Bridge (kanal channel, sync send)  →  async drain task
         │
         ▼
  BusService → trouper ActorSystem (schema broadcast)
         │
         ├──▶ actors that declared .handles::<M>()
         └──▶ TUI renderer, reading AppState and cell readers
```

Unidirectional: frontend → actors. The shared `AppState` and the cell registry are the feedback — actors and handlers write them, the renderer reads on the next tick.

### 2.7 Actors and the bus

There is no central actor host. The bus is a **schema broadcast**: a published message is addressed by its schema and delivered to the actors that declared `.handles::<M>()` at spawn, and to no one else. Publishing is not a route table — but the _transport_ a message rides depends on the path it takes and on the schema's declared kind, so read the rule below before giving a schema more than one declarant.

**One declarant per schema — the rule that keeps the two publish paths agreeing.** A schema's _declared kind_ decides its transport on the erased path (see `Bridge` below): an **Event** broadcasts to every declarant, a **Command** routes to **one** handler — and the route table rotates between handlers when a second actor declares the same schema. So a message declared by two actors is delivered to one of them, alternating, while the same message published directly reaches both. Keep one declarant per schema, and keep an imperative message a **Command** and a past-tense fact an **Event**: re-declaring a Command as an Event to obtain broadcast semantics makes the kind lie about what the message is. When one press must drive two actors, chain them — the first actor emits the fact, the second subscribes to it.

An actor is a struct ending in `Actor` that implements three things:

- `ServiceActor` (from `trouper`) — with `init` returning an error, because actors always spawn via `init_with`. That `Err` is a hard invariant, not a stub.
- `MsgHandler<M>` for each message it consumes, one impl per message, each delegating to a handler function.
- `BusPublish` (jinn's own trait, in `crates/jinn-kernel/src/common/actor_deps.rs`) — gives the actor the bus.

**The flush gate.** A spawn builder's `.handles::<M>()` declares subscriptions; its `.emits::<M>()` declares what the actor is allowed to send. A type that is neither handled nor emitted is **dropped silently at runtime**. When an outbound message vanishes, the fix is almost always a missing `.emits::<M>()`.

`Bridge` (`crates/jinn-kernel/src/common/bridge.rs`) is how a synchronous caller reaches the bus: the handler produces erased `PublishClosure`s, the bridge takes them over a synchronous channel, and an async task drains them through the same `BusService` every actor uses. The closure rides the same recording mode as a direct publish, and the same transport **when the schema has one declarant** — but a closure's publish is _erased_, so the system's transport comes from the schema's declared kind: an Event broadcasts, a Command routes to one handler. A direct `publish` always broadcasts. The two paths therefore disagree for a Command with more than one declarant, which is why the rule above keeps one declarant per schema.

### 2.8 State ownership

`AppState` (`crates/jinn-app-state/src/app_state.rs`) is deliberately small: `session`, `frontend`, and `task_spawns`. Anything a slice needs a typed handle to lives in a cell in the `Slices` registry, not as an `AppState` field.

Each field is written by **at most one owner** — a slice's actor for slice state, the `IntentHandler` for frontend state. "At most one" is an upper bound, not a requirement that a dedicated writer exist. Writing state is ordinary inline work for whatever already owns that domain; do not create an actor in order to write.

Ownership is per-field, not per-struct: a slice actor writing the cell it owns is correct, and the `IntentHandler` writing the same field is also correct (it is the exempt synchronous frontend mutator). A **second** actor writing a field another actor owns is the red flag.

**Anti-pattern — the "sync sibling."** Do not split one domain boundary across two actors where one persists and a second subscribes to the first's event purely to write state. If an actor has no `State`, and you spawned a sibling to do the write, the sibling is the bug. One boundary, one actor.

## 3. Core Patterns

### Error Handling

Use `wherror::Error` with `error_stack::Report` for all fallible operations.

**Colocate errors with their related types.** Never create standalone `error.rs` or `errors.rs` files. An error type belongs in the same module as the trait, struct, or function that produces it.

**Error type:**

```rust
use wherror::Error;

#[derive(Debug, Error)]
#[error(debug)]
pub struct ExternalEditorError;
```

**Result with error context:**

```rust
use error_stack::{Report, ResultExt};

pub fn load() -> Result<Config, Report<ConfigError>> {
    let content = std::fs::read_to_string(&path)
        .change_context(ConfigError)
        .attach("failed to read config file")?;
    Ok(config)
}
```

**Document errors in functions:**

```rust
/// # Errors
///
/// Returns an error if the terminal setup fails.
pub fn run(tick_rate: Duration) -> Result<(), Report<TuiRunError>>
```

### Validators

An action with preconditions has a dedicated validator co-located with its feature. Validators are plain functions — no registries, no trait objects.

```rust
// Co-located per feature, e.g. crates/slices/jinn-chat-input/src/validator.rs
pub fn validate_submit_message(state: &AppState) -> Result<(), SubmitMessageError> {
    if state.active_chat_input().is_empty() {
        return Err(SubmitMessageError::EmptyBuffer);
    }
    Ok(())
}
```

**Validator rules:**

- Validators take `&AppState` as their first parameter.
- Each fallible action has a custom error enum naming why it cannot proceed.
- An infallible action gets no validator; if you write one anyway, it returns `()` and says so in its doc.
- On validation failure the call site does nothing — no partial mutation, no error surfaced to the user.

A validator is not a formality. Most user actions are now **route actions, not `KernelIntent` variants**, and the route dispatch path has no validation stage. A precondition on a route action is enforced inside the `ActionFn` closure, where it can return an empty result. Reach for a validator when the action is a `KernelIntent` arm.

### Trait Usage

Every external dependency or service must have a trait abstraction.

**Colocate traits with their related types.** Never create standalone `traits.rs` files. A trait belongs in the same module as the types that implement it or the domain it defines.

**Service trait pattern:**

```rust
use wherror::Error;

#[derive(Debug, Error)]
#[error(debug)]
pub struct FooBackendError;

pub trait FooBackend {
    fn fetch_all(&self) -> Result<Vec<Foo>, Report<FooBackendError>>;
}
```

**Service wrapper pattern:**

```rust
use std::sync::Arc;
use derive_more::Debug;

#[derive(Debug, Clone)]
pub struct BusService {
    #[debug("BusService<{}>", self.name())]
    inner: Arc<dyn BusBackend>,
}

impl BusService {
    pub fn new(inner: Arc<dyn BusBackend>) -> Self {
        Self { inner }
    }
}
```

**Key trait design rules:**

- Use `#[async_trait]` for async methods.
- Include a `name(&self) -> &'static str` method for debugging on service traits.
- Service structs wrap `Arc<dyn Trait>` for shared ownership.

### Module System

There is no rule about _where_ `mod.rs` goes, because the tree does not follow one. It uses three shapes, and all three are correct:

- **Flat siblings** — `handler.rs`, `validator.rs`, `intent.rs` in one directory. The common case.
- **`foo.rs` beside `foo/`** — the parent module's own code in `foo.rs`, its children under `foo/`. Used wherever a parent has real content of its own, such as `crates/jinn-kernel/src/feat.rs` beside `feat/`, and `crates/slices/jinn-session-store/src/session_store_actor.rs` beside `session_store_actor/`.
- **`mod.rs`** — a directory of pure grouping, where the module doc and any shared constants belong. A `mod.rs` directory that also holds logic should have been a `foo.rs` + `foo/` pair.

**Pick the shape that matches what the parent module actually is**, and do not convert one shape to another as a matter of tidiness. A rule that fits every `mod.rs` in the tree is a rule about grouping, not about `mod.rs` placement.

### Actor Naming

Actors are domain logic that spans a whole slice, so they have conventions for discoverability:

- **One actor per file**, named `*_actor.rs`.
- **One `spawn` per actor**, living in that same file, next to the actor it builds. Composition calls `Actor::spawn(system, deps)`.
- **Deps are a separate struct** named `<Actor>ActorDeps`, holding the `State` and cell handles the actor needs. A `CellHandle` the actor must write goes in `Deps`, not in a global.
- **One `MsgHandler` impl per handled message**, each a one-line delegation to a function in the actor's `handlers.rs`.

### Dependency Injection

**`Services`** (`crates/jinn-kernel/src/common/services.rs`) is the DI container. It is built once at startup and cloned cheaply; every clone shares the same bus, cell registry, route table, and actor system.

All services within `Services` must either:

- Be cheap to clone.
- Use the "service wrapper" pattern above.

Read the file for the current field list — it is short and the set changes as the domain grows. The pattern matters, not the inventory.

### Block Scoping

When a value requires multiple setup steps or intermediate bindings, wrap the sequence in a block expression so the final binding is immutable and temporaries don't leak into the surrounding scope. This reduces the number of variables floating around a function and makes the code easier to extract into a function later.

**Create-then-configure:**

```rust
// ❌ BAD — mutable binding lives past setup
let mut services = ServiceBuilder::new();
services.register(auth_backend);
services.register(cache_backend);
services.register(storage_backend);
```

```rust
// ✅ GOOD — setup is scoped, final binding is immutable
let services = {
    let mut builder = ServiceBuilder::new();
    builder.register(auth_backend);
    builder.register(cache_backend);
    builder.register(storage_backend);
    builder.build()
};
```

**Intermediate values:**

```rust
// ❌ BAD — a and b remain in scope after c is computed
let a = 1;
let b = 2;
let c = a + b;
```

```rust
// ✅ GOOD — a and b are scoped to the block
let c = {
    let a = 1;
    let b = 2;
    a + b
};
```

### TOML Persistence (Comment-Preserving)

User-editable TOML files (`providers.toml`, `jinn.toml`) must be written through the `DocumentPatcher` in `crates/jinn-common/src/toml_patch.rs`, **never** by serializing the struct to a string directly. The plain serializer wipes every comment, blank line, and field-ordering choice on every save.

Pattern: behind a storage trait, the filesystem `save` impl reads the on-disk document, applies the new struct as a patch, and writes it back. The three user-editable files have three storage traits — `ConfigStorage` (`providers.toml`), `ConfigDocumentStorage` (`jinn.toml`), `AppStateStorage` (`state.toml`) — and the in-memory test impls behind them stay simple.

Why: the trait is the mutation boundary; patching preserves user comments, ordering, and unknown keys (forward-compat for newer jinn versions) for free.

Adding a new scalar or sub-table field to `ProvidersConfig` requires **zero** storage-layer changes — `Serialize` produces the new key and the patcher writes it through. Adding a new array-of-tables requires one `DocumentPatcher::register_array_key` call so the patcher can match entries by their key field.

Read-only TOML files (themes, prompt frontmatter) are unaffected.

## 4. Tests

Important:

- Tests should only verify _observable behavior_.
- Testing internal details is an _anti-pattern_.
- Prefer testing observable behavior ONLY. If observable behavior cannot be tested, then an abstraction needs to be created. Ask the user how to proceed in this case.

### The `rstest` Attribute Is Mandatory

**Every test stacks `#[rstest::rstest]` above its test attribute.** A bare `#[test]` or `#[tokio::test]` escapes the per-test timeout, and `just lint-testattr` fails the build on it.

```rust
#[rstest::rstest]
#[test]
fn pop_returns_none_when_stack_empty() { /* ... */ }

#[rstest::rstest]
#[tokio::test]
async fn publish_routes_to_declared_handler() { /* ... */ }
```

`RSTEST_TIMEOUT` is exported by the justfile and mirrored in the workspace's build configuration, so the timeout applies whether or not the run goes through `just`.

### One Test, One Behavior

**Every test must assert exactly one semantic concept.** A test should answer a single question about the system. When it fails, the test name alone must tell you _what_ broke.

This means each test has exactly **one** `// When` and **one** `// Then` block. A `// Then` may be followed by `// And` lines, but only those lines elaborate on the same observable behavior — never when they describe a different behavior.

**What counts as "one concept":**

- Checking multiple fields of the _same result_ — fine. All confirms "the result is correct."
- Checking that a serialization roundtrip preserved all fields — fine. All confirms "the roundtrip worked."
- Checking that reset cleared all fields — fine. All confirms "everything was reset."
- Checking that every item in a filtered list matches the filter — fine. All confirms "the filter worked."

**What counts as separate concepts (split into separate tests):**

- An action that updates state **and** publishes a message → two tests. State change and message emission are separate observable behaviors.
- Processing a second input after a first → two tests. Each input triggers its own behavior.
- Rendering multiple entry types from one widget → one test per entry type. Each entry type is a separate rendering behavior.
- A multi-step lifecycle (start, complete, finalize, advance) → one test per step. Each step is a separate state transition.

**Anti-patterns to avoid:**

```rust
// ❌ BAD — two When/Then blocks in one test
#[rstest::rstest]
#[test]
fn stream_token_appends_to_assistant_entry() {
    // ...setup...
    // When processing the first token.
    // Then the entry has "Hello".
    // When processing a second token.
    // Then the text is "Hello world".
}
```

```rust
// ✅ GOOD — split into two tests
#[rstest::rstest]
#[test]
fn first_stream_token_creates_assistant_entry() {
    // ...setup...
    // When processing StreamToken("Hello").
    // Then the session has an Assistant entry with "Hello".
}

#[rstest::rstest]
#[test]
fn subsequent_stream_token_appends_to_existing_entry() {
    // ...setup with one token already processed...
    // When processing another StreamToken(" world").
    // Then the text is "Hello world".
}
```

```rust
// ❌ BAD — checking state change AND message emission in one test
#[rstest::rstest]
#[test]
fn submit_message_clears_input_and_publishes() {
    // ...setup...
    // When submitting a message.
    // Then the input buffer is cleared.
    // And EnqueueUserMessage was published.
}
```

```rust
// ✅ GOOD — split into separate tests
#[rstest::rstest]
#[test]
fn submit_message_clears_input_buffer() {
    // ...setup...
    // When handling the submit action.
    // Then the input buffer is empty.
}

#[rstest::rstest]
#[test]
fn submit_message_publishes_enqueue_command() {
    // ...setup...
    // When handling the submit action.
    // Then the result names EnqueueUserMessage.
}
```

```rust
// ❌ BAD — checking multiple entry type renders in one test
#[rstest::rstest]
#[test]
fn render_mixed_entries() {
    // Given system, user, actor, and assistant entries.
    // When rendering.
    // Then line 6 is system (dark gray).
    // And line 7 is user (">" prefix, bold).
    // And line 8 is actor (yellow).
    // And line 9 is assistant (cyan).
}
```

```rust
// ✅ GOOD — one test per entry type
#[rstest::rstest]
#[test]
fn render_system_entry_is_dark_gray() {
    // Given a ChatLogElement with a system entry.
    // When rendering.
    // Then the system entry line has dark gray foreground.
}

#[rstest::rstest]
#[test]
fn render_user_entry_has_prefix() {
    // Given a ChatLogElement with a user entry.
    // When rendering.
    // Then the user entry line starts with ">".
}
```

**Duplicated test setup is acceptable.** Do not combine tests to avoid setup duplication.

### BDD-Style Tests (Given/When/Then)

Structure tests with clear Given/When/Then sections, and name the test so it can be read as a standalone program behavior in the test report:

```rust
#[rstest::rstest]
#[test]
fn pop_returns_none_when_stack_empty() {
    // Given an empty stack.
    let mut stack = Stack::default();

    // When popping from the stack.
    let item = stack.pop();

    // Then we get nothing back.
    assert!(item.is_none());
}
```

**Example — testing a validator:**

```rust
#[rstest::rstest]
#[test]
fn submit_message_rejected_when_buffer_empty() {
    // Given an empty input buffer.
    let state = AppState::default();

    // When validating submit message.
    let result = validate_submit_message(&state);

    // Then validation fails with EmptyBuffer.
    assert!(matches!(result, Err(SubmitMessageError::EmptyBuffer)));
}
```

**Example — testing a route action:**

```rust
#[rstest::rstest]
#[test]
fn bound_key_dispatches_through_the_slice_route_row() {
    // Given a registry with the slice's route row attached.
    let (slices, routes) = slices_with_sidebar_rows();
    let mut state = AppState::default();

    // When handling the key the row binds.
    let result = IntentHandler::handle(
        &KernelIntent::Dynamic(DynamicIntent::new(sidebar_scope(), "select", "select")),
        &mut state,
        &slices,
        &routes,
        &ConfigLayer::empty(),
    );

    // Then the row's action ran, not a built-in arm.
    assert!(result.scope_signal.is_some());
}
```

**Example — testing a domain actor:**

```rust
#[rstest::rstest]
#[test]
fn stream_token_appends_to_assistant_entry() {
    // Given a projector with an active session.
    let state = State::new(AppState::default());
    let sink = RecordingSink::new();
    let session_actor = SessionPersistenceActor::spawn(system, deps);

    // When handling StreamToken("Hello").
    session_actor.handle_stream_token(&StreamToken { /* ... */ }, &sink);

    // Then the session has an Assistant entry with "Hello".
    let s = state.read();
    assert_eq!(s.active_session().last_entry_text(), "Hello");
}
```

### Parameterized Tests with rstest

If a test has many inputs, prefer parametrizing:

```rust
#[rstest::rstest]
#[case(Key::Tab, "Tab")]
#[case(Key::Enter, "Enter")]
fn key_display(#[case] key: Key, #[case] expected: &str) {
    // Given / When / Then inline for simple cases
    assert_eq!(key.display(), expected);
}
```

For edge cases that don't easily fit into "expected", prefer a BDD-styled test instead.

Use rstest when you find yourself writing the same assertion logic against different inputs. Do _not_ use rstest to combine different behaviors into one test — each `#[case]` must test the same property.

### Async Tests

```rust
#[rstest::rstest]
#[tokio::test]
async fn publish_reaches_a_declared_handler() {
    // Given an actor spawned with .handles::<StreamToken>().
    let system = ActorSystem::new();

    // When publishing a StreamToken.
    let result = system.publish(StreamToken { /* ... */ });

    // Then publication succeeds.
    assert!(result.is_ok());
}
```

### Test Utilities

- **Recording buses** — `BusService::new_recording()` returns the service plus a `BusAudit` of everything published through it. This is how you assert that an action emitted a message, and it exercises the same publish path production does. There is no shared test-local sink to hunt for; build whatever the test needs.
- **Empty registries** — handler tests that don't exercise slices build a bare `Slices::new()` and `KeyRoutes::new()`. Slice tests that do build a registry with the catalog's `register_all_cells` or just the one cell they need.
- Create domain-specific test builders as needed within each feature's test module.
- Use ratatui's `TestBackend` directly for render tests.

## 5. Documentation

### Module-Level Documentation

Module level documentation should explain its purpose and high-level behaviors. Only explain technical details as necessary to make the high-level documentation understandable.

```rust
//! Chat input box — where the user composes and sends messages.
//!
//! This component manages the text input experience end to end: handling keystrokes,
//! displaying the in-progress message, and switching between browsing and typing modes.
```

A slice's `lib.rs` doc is the place a reader looks first to learn what the slice contributes and what it does not. Name the actors, the cell, and the route rows.

### Type Documentation

```rust
/// The user's in-progress message being composed in the input box.
#[derive(Debug)]
pub struct ChatInputBoxState {
    /// The text the user has typed so far.
    input_buffer: String,
}
```

### jinn-usage Skill Staleness Check

Before merging, check `res/skills/jinn-usage` for staleness **only if the
change resulted in a new or changed or removed user-facing behavior** — a
stale keybind or UI description in the skill misleads every future session,
which costs the user more than an honest "I don't know". Update `SKILL.md`
and/or the affected reference when a keybind, screen surface, config
section, resource family, or workflow the skill documents changed. Rust-internal
refactors with no user-visible surface skip the check.

## 6. Modification Guide

When implementing features, locate each concern by convention rather than hardcoded paths — the crate layout shifts as the domain grows. Use `grep` to find the current location if unsure.

**Adding a new slice:**

1. **Create the pair** — `crates/slices/jinn-foo-msg/` first, with the cell payload, its `SlotKey`, and any command/event structs. Then `crates/slices/jinn-foo/`, depending on it.
2. **Register the cell** — one `register!` entry in `crates/jinn-cell-catalog/src/lib.rs`, and bump `EXPECTED_CELL_COUNT`.
3. **Write `activate`** — taking `&mut SliceHost<'_, jinn_slices::RenderFacts>` first, resolving the cell, attaching route rows, registering render regions.
4. **Add the call to the boot list** in `src/bootstrap/slices.rs`, in the block whose constraints it satisfies, with a comment saying what it provides and what consumes it.
5. **Add the actor** if the slice has async logic — `*_actor.rs`, a `spawn`, `<Actor>ActorDeps`, and one `MsgHandler` impl per message.

**Adding a keybind to an existing slice:**

1. **Attach a `RouteRow`** in the slice's route module. If the action belongs to the slice, use `RouteOutcome::Action` with an `ActionFn`. Only reach for `RouteOutcome::StaticIntent` for a shared-chrome key, and only after adding its route id to `static_intent` in `crates/jinn-tui/src/keymap_gen.rs`.

**Adding a `KernelIntent` variant** (rare — prefer a route action):

1. **Add the variant** to the `KernelIntent` enum in `crates/jinn-kernel/src/protocol/intent.rs`. It is a bus message, so it needs the derives that makes.
2. **Add a validator** if the action has preconditions, co-located `validator.rs` in the relevant feature directory.
3. **Add a handler arm** in `crates/jinn-kernel/src/feat/intent/handler.rs` — a thin delegation to a `handle_*` function in the owning feature, not inlined mutation.
4. **Add a keymap binding** in `crates/jinn-tui/src/keymap.rs` under the right `Scope` and `KeyCategory`.

**Adding a message type that crosses a slice boundary:**

1. **Define the struct** in the owning slice's `-msg` crate, deriving the message schema the sibling structs use plus `BusMessage`.
2. **Add the enum variant or struct** to whatever the boundary names — a `-msg` crate has no central enum, so the definition _is_ the registration.
3. **Declare it** on the sending side: for an actor, add `.emits::<M>()` to its spawn builder, or it will be dropped at runtime. For a route action, the `PublishClosure` carries it.

**In every case:** write tests with Given/When/Then structure, stacking `#[rstest::rstest]`; add module docs, type docs, and error docs describing behavior and purpose, not implementation.

## 7. Tooling

Read the `justfile` to see what else is available. Prioritize running recipes from the `justfile` instead of manual invocation.

### Project Commands

Skills refer to commands by **role**; the table below resolves each role to this project's actual command.

| Role            | Command                   | Description                                                                                                               |
| --------------- | ------------------------- | ------------------------------------------------------------------------------------------------------------------------- |
| `vcs`           | `git`                     | This project uses git for version control .                                                                               |
| `check`         | `just check`              | Compile-check the workspace without codegen. Fast; use it instead of a speculative test run.                              |
| `test`          | `just test`               | Run the workspace suite, capture to `target/test-output.log`, print a summary. **All tests must pass before committing.** |
| `test-failures` | `just test-failures`      | Show failing test names from the last run. Reads the captured log; re-runs nothing.                                       |
| `test-one`      | `just test-one <filter>`  | Run only tests matching a name filter, across the workspace. The cheap fix loop.                                          |
| `lint`          | `just lint`               | Compile check, clippy, format check, and the test-attribute guard.                                                        |
| `test-attr`     | `just lint-testattr`      | Fail on a bare test attribute missing its `rstest` companion.                                                             |
| `format`        | `just fmt-fix`            | Apply formatting fixes.                                                                                                   |
| `commit`        | `just commit '<message>'` | Commit changes (uses `--dotfiles` so `.agents/` is included).                                                             |
| `sync-trunk`    | `git merge main`          | Sync latest changes onto your branch — resolve conflicts, re-test, commit.                                                |

## 8. Misc

- NEVER manually split a string using `.chars` or by indexing. Use the `unicode-segmentation` crate.
- No trivial setters for struct methods. Prefer meaningful semantic actions. It's an anti-pattern to directly inspect and manipulate state.
- Environment variables should only be accessed at program initialization and then saved into a struct as needed. Environment variables are a global namespace and should be avoided outside of program startup.
- Use `where` clause for all generics.
- Prefer `match` over `if` where appropriate.
- DO NOT USE CODE COMMENTS TO WRITE ABOUT "SPEC DIVERGENCES" OR "DIVERGENCES". Code comments in the codebase is not the place to discuss planning information. PLANS ARE NOT PERSISTED.
