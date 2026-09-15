# Style Guide

This document defines the _coding conventions_, _patterns_, and _architecture_ for the `jinn` codebase.

- IGNORE ALL CODE IN `vendor/` UNLESS IT'S SPECIFICALLY RELATED TO THE TASK.
- NEVER RUN `cargo test -p <package>`. ALWAYS USE `just test` to test the code!
- NEVER run `cargo test` directly — not even `--workspace`. The suite is slow; run it ONCE per check via `just test`, which tees full output to `target/test-output.log` and prints a summary.
- NEVER pipe `cargo test` through `grep`/`awk`/`head` filters, and never invoke it twice in one command (e.g. once for a tally, once for failure names). The summary and `just test-failures` already provide this — re-deriving it re-runs the whole suite.
- NEVER use `--no-run` to "pre-compile tests" before a test run. `just test` compiles anyway; if you only want a compile check, use `just check`.

### Test Discipline

The full workspace suite takes minutes. Treat suite executions as expensive:

1. Run `just test` ONCE. It runs the suite with `--no-fail-fast`, captures everything to `target/test-output.log`, and prints a passed/failed summary plus failing test names.
2. Get failure details from the capture with `just test-failures` — instant, no cargo.
3. Iterate cheaply while fixing: `just test-one <test_name_filter>` runs only matching tests across the workspace.
4. Before committing, confirm with one final `just test`.

## 1. Overview

This style guide ensures consistent, maintainable Rust code across the codebase. It covers error handling, trait-based design, testing patterns, documentation standards, and module organization. Following these patterns enables dependency injection for testability and clear separation of concerns.

## 2. Core Patterns

### Error Handling

Use `wherror::Error` with `error_stack::Report` for all fallible operations.

**Colocate errors with their related types.** Never create standalone `error.rs` or `errors.rs` files. Error types belong in the same module as the trait, struct, or function that produces them. For example, `ActorHostError` lives in `actor_host.rs` alongside the `ActorHost` trait, not in a separate `error.rs`.

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

### Validator Pattern

Each `Intent` variant has a dedicated validator function. Validators are plain functions — no registries or trait objects. Fallible validators return `Result<(), SpecificError>` with a custom error enum per intent.

```rust
// Validator pattern — co-located per feature, e.g. jinn-domain/src/feat/chat_input/validator.rs
pub fn validate_submit_message(state: &AppState) -> Result<(), SubmitMessageError> {
    if state.active_chat_input().is_empty() {
        return Err(SubmitMessageError::EmptyBuffer);
    }
    Ok(())
}
```

**Validator rules:**

- All validators take `&AppState` as input
- Each fallible intent has a custom error enum describing why it cannot proceed
- On validation failure, the `IntentHandler` match arm does nothing (no-op)

### Trait Usage

Every external dependency or service must have a trait abstraction.

**Colocate traits with their related types.** Never create standalone `traits.rs` files. Traits belong in the same module as the types that implement them or the domain they define. For example, `MessageSink` lives in `message_sink.rs`, not in a separate `traits.rs`.

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
pub struct ActorHostService {
    #[debug("ActorHost<{}>", self.backend.name())]
    host: Arc<dyn ActorHost>,
}

impl ActorHostService {
    pub fn new(host: Arc<dyn ActorHost>) -> Self {
        Self { host }
    }
}
```

**Key trait design rules:**

- Use `#[async_trait]` for async methods
- Include a `name(&self) -> &'static str` method for debugging on service traits
- Service structs wrap `Arc<dyn Trait>` for shared ownership

### Module System

Use the new Rust module system throughout:

- **Top-level feature directories** use `mod.rs` (e.g., `feat/chat_input/mod.rs`). This is the only exception.
- **All other modules** use `foo.rs` alongside `foo/` directory — never `mod.rs` inside a non-feature directory.
- The `feat/` directory itself has `feat.rs` at the `src/` level, not `feat/mod.rs`.

### Actor Naming

Actors are domain logic that spans the entire application, so they have specific naming conventions for discoverability:

- **Actor-only features** are named with an `_actor` suffix.
- **Within domain features**, each actor lives in its own `*_actor.rs` file.
- **One actor per file.** Never combine multiple actors in a single file.
- **Spawn functions live with their actor.** Each `*_actor.rs` file contains both the actor struct/impl and the `spawn_*()` function that creates it. Feature `mod.rs` files do not contain spawn functions.

### Dependency Injection

**Services container (in `jinn-domain/src/common/services.rs`):**

```rust
#[derive(Debug, Clone)]
pub struct Services {
    // See jinn-domain/src/common/services.rs for the current fields.
    // Services are added as the domain grows — the exact set of fields
    // changes over time. The pattern is what matters, not the specific list.
}
```

Created once at startup and shared throughout the application. `Services` is the DI container for the actor system — the frontend and intent handler don't need it because they work with `AppState` directly.

All services within the `Services` struct must either:

- Be cheap to clone
- Use the "service wrapper" pattern detailed above.

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

User-editable TOML files (`providers.toml`, `jinn.toml`) must be written via the
`DocumentPatcher` in `crates/jinn-domain/src/common/toml_patch.rs`, **never** via
`toml::to_string_pretty` directly. The plain serializer wipes every comment,
blank line, and field-ordering choice on every save.

Pattern: behind the `ConfigStorage` / `UserPreferencesStorage` traits, the
`Filesystem*::save` impl reads the on-disk document, applies the new struct as
a patch, and writes it back. `InMemory*` test impls stay simple.

Why: the trait is the mutation boundary; patching preserves user comments,
ordering, and unknown keys (forward-compat for newer jinn versions) for free.

Adding a new scalar or sub-table field to `ProvidersConfig` / `UserPreferences`
requires **zero** storage-layer changes — `Serialize` produces the new key and
the patcher writes it through. Adding a new array-of-tables requires one
`DocumentPatcher::register_array_key` call so the patcher can match entries
by their key field (`name`, `pattern`, etc.).

Read-only TOML files (themes, prompt frontmatter) are unaffected.

### Plugin API Policy

When asked to create a new plugin, attempt to expand the plugin API surface to
broaden plugin capabilities generally. Avoid introducing host capabilities that
exist solely to satisfy a single plugin.

## 3. Architecture

### Data Flow

```
  Keyboard / Mouse / Script
         │
         ▼
  Keymap (produces Intent)
         │
         ▼
  IntentHandler (sync, single match block)
    ├── validator  → passes or rejects
    ├── mutate AppState directly (scroll, cursor, mode, etc.)
    └── return IntentResult { commands }
         │
         ▼
  AppCore.sender  →  async forwarding task  →  ActorHost
                                                    │
                                          ┌──────���──┴─────────┐
                                          │                   │
                                   Domain actors          other actors
                                          │                   ���
                                          ▼                   ▼
                                    write AppState        Commands/Events
                                    (shared RwLock)             │
                                          │                    │
                                          └─────────┬──────────┘
                                                    ▼
                                            TUI renderer reads AppState
```

Unidirectional: frontend → actor system. No feedback loop.
The shared AppState is the feedback — domain actors write their fields,
the renderer reads it on the next tick.

### Command/Event System

**Intents are for user input only.** The `IntentHandler` validates, mutates `AppState` directly, and returns commands. It never accesses external services and never emits events.

**All domain logic goes through commands.** When something needs to happen — send a message, switch provider, run a tool — the IntentHandler returns a `Command`. Commands are routed by the actor host to exactly one subscribed actor.

**Actors handle all async operations.** They communicate through the actor host's pub/sub routing. Events are broadcast to all subscribers; commands route to exactly one. Actors may emit events or commands back onto the bus in response.

Each `AppState` field/sub-struct is written by **at most one actor** — its owner. "At most one" is an upper bound on co-writers, not a requirement that a dedicated writing actor exist. Writing state is ordinary inline work for whatever actor already owns that field's domain (it may also persist to disk, subscribe to the bus, forward to a channel, run business logic). Do not create an actor in order to write.

The `IntentHandler` is **not an actor** and is exempt from this rule. It is the synchronous frontend mutator (user input → `AppState`). It may write any field, in frontend or elsewhere; it is never counted as a writer. An actor owning a field the `IntentHandler` also writes is not a conflict (e.g. optimistic IntentHandler write + authoritative actor write is fine).

Ownership is per-field, not per-top-level-struct: a domain actor writing `frontend.pins` it owns is correct; the `IntentHandler` writing it too is also correct (exempt); a _second actor_ writing it is the red flag. A cross-boundary write is mutating a field you don't own, regardless of which top-level struct it lives under.

**Anti-pattern — the "sync sibling."** Do not split one domain boundary across two actors where one persists/forwards and a second "sync" actor subscribes to the first's event just to write `AppState`. If you keep an actor "pure" (no `State`) and spawn a sibling to do the write, the sibling is the bug — give the first actor a `State` clone and write inline. One boundary = one actor.

## 4. Tests

Important:

- Tests should only verify _observable behavior_
- Testing internal details is an _anti-pattern_.
- Prefer testing observable behavior ONLY. If observable behavior cannot be tested, then an abstraction needs to be created. Ask the user how to proceed in this case.

### One Test, One Behavior

**Every test must assert exactly one semantic concept.** A test should answer a single question about the system. When it fails, the test name alone must tell you _what_ broke.

This means each test has exactly **one** `// When` and **one** `// Then` block. A `// Then` may be followed by `// And` lines, but only those lines elaborate on the same observable behavior — never when they describe a different behavior.

**What counts as "one concept":**

- Checking multiple fields of the _same result_ — fine. All confirms "the result is correct."
- Checking that a serialization roundtrip preserved all fields — fine. All confirms "the roundtrip worked."
- Checking that reset cleared all fields — fine. All confirms "everything was reset."
- Checking that every item in a filtered list matches the filter — fine. All confirms "the filter worked."

**What counts as separate concepts (split into separate tests):**

- A handler that updates state **and** emits a command → two tests. State change and command emission are separate observable behaviors.
- Processing a second input after a first → two tests. Each input triggers its own behavior.
- Rendering multiple entry types from one widget → one test per entry type. Each entry type is a separate rendering behavior.
- A multi-step lifecycle (start, complete, finalize, advance) → one test per step. Each step is a separate state transition.

**Anti-patterns to avoid:**

```rust
// ❌ BAD — two When/Then blocks in one test
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
#[test]
fn first_stream_token_creates_assistant_entry() {
    // ...setup...
    // When processing StreamToken("Hello").
    // Then the session has an Assistant entry with "Hello".
}

#[test]
fn subsequent_stream_token_appends_to_existing_entry() {
    // ...setup with one token already processed...
    // When processing another StreamToken(" world").
    // Then the text is "Hello world".
}
```

```rust
// ❌ BAD — checking state change AND command emission in one test
#[test]
fn submit_message_clears_input_and_enqueues() {
    // ...setup...
    // When submitting a message.
    // Then the input buffer is cleared.
    // And EnqueueUserMessage was returned.
}
```

```rust
// ✅ GOOD — split into separate tests
#[test]
fn submit_message_clears_input_buffer() {
    // ...setup...
    // When handling Intent::SubmitMessage.
    // Then the input buffer is empty.
}

#[test]
fn submit_message_returns_enqueue_command() {
    // ...setup...
    // When handling Intent::SubmitMessage.
    // Then the result contains EnqueueUserMessage.
}
```

```rust
// ❌ BAD — checking multiple entry type renders in one test
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
#[test]
fn render_system_entry_is_dark_gray() {
    // Given a ChatLogElement with a system entry.
    // When rendering.
    // Then the system entry line has dark gray foreground.
}

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
fn pop_returns_none_when_stack_empty() {
    // Given an empty stack.
    let mut stack = Stack::default();

    // When popping from the stack.
    let item = stack.pop();

    // Then we get nothing back.
    assert!(item.is_none());
}
```

**Example — testing the intent handler:**

```rust
#[test]
fn quit_sets_should_quit_in_state() {
    // Given default app state.
    let mut state = AppState::default();

    // When handling Intent::Quit.
    let result = IntentHandler::handle(&Intent::Quit, &mut state);

    // Then should_quit is set to true.
    assert!(state.should_quit);
    // And no commands are emitted.
    assert!(result.commands.is_empty());
}
```

**Example — testing a validator:**

```rust
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

**Example — testing a domain actor:**

```rust
#[test]
fn stream_token_appends_to_assistant_entry() {
    // Given a projector with an active session.
    let state = State::new(AppState::default());
    let sink = RecordingSink::new();
    let session_actor = SessionPersistenceActor::activate(&mut ctx);

    // When handling StreamToken("Hello").
    session_actor.handle_stream_token(&StreamToken { /* ... */ }, &sink);

    // Then the session has an Assistant entry with "Hello".
    let s = state.read();
    assert_eq!(s.active_session().last_entry_text(), "Hello");
}
```

### Parameterized Tests with rstest

If a test has many inputs, prefer parametrizing with `rstest`:

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
#[tokio::test]
async fn actor_host_loads_manifest() {
    // Given an in-memory actor host.
    let host = InMemoryActorHost::new();

    // When loading actors.
    let result = host.discover().await;

    // Then discovery succeeds.
    assert!(result.is_ok());
}
```

### Test Utilities

**Shared test helpers:**

- `RecordingSink` (in `jinn-domain/src/common/actor.rs`) — records messages emitted by actors during tests.
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

### Type Documentation

```rust
/// The user's in-progress message being composed in the input box.
#[derive(Debug)]
pub struct ChatInputBoxState {
    /// The text the user has typed so far.
    input_buffer: String,
}
```

## 6. Modification Guide

When implementing features, locate each concern by convention rather than hardcoded paths — the exact crate layout may shift as the domain grows. Use `grep`/`rg` to find the current location if unsure.

1. **Add Intent variant** — find the `Intent` enum and add a variant.
2. **Add validator** — co-locate a `validator.rs` in the relevant feature directory. Infallible intents don't need a validator. Fallible ones return `Result<(), SpecificError>`.
3. **Add handler match arm** — find the `IntentHandler`: call validator (if any), mutate `AppState`, return commands.
4. **Add keymap binding** — bind key to `Intent` variant in the appropriate `Scope`.
5. **Add Command/Event if needed** — define domain structs alongside the relevant `Command` or `Event` enum. Forgetting the enum variant is the most common oversight — the struct alone is not enough.
6. **Add domain actor logic if needed** — find the appropriate actor within the relevant feature directory and subscribe to the new command/event.
7. **Add UI element if needed** — add a new module under the UI feature directory and register it.
8. **Write tests** — Use Given/When/Then structure: test validator in isolation, test intent handler for state changes and commands, test domain actor for event→state mapping.
9. **Add documentation** — Module docs, type docs, error docs. Describe behavior and purpose, not technical implementation.

## 7. Tooling

Read the `justfile` to determine what additional tooling is related to this project. Prioritize running commands from the `justfile` instead of manual invocation.

### Project Commands

Skills refer to commands by **role**; the table below resolves each role to this project's actual command.

| Role         | Command                   | Description                                                                                            |
| ------------ | ------------------------- | ------------------------------------------------------------------------------------------------------ |
| `vcs`        | Fossil                    | This project uses Fossil for version control (`fossil status`, `fossil diff`, `fossil timeline`, ...). |
| `check`      | `just check`              | `cargo check --workspace` — fast compilation without codegen.                                          |
| `test`       | `just test`               | `cargo test --workspace` + e2e tests — **all tests must pass before committing**.                      |
| `lint`       | `just lint`               | Lint checks.                                                                                           |
| `format`     | `just fmt-fix`            | Apply formatting fixes.                                                                                |
| `commit`     | `just commit '<message>'` | Commit changes (uses `--dotfiles` so `.agents/` is included).                                          |
| `sync-trunk` | `fossil merge trunk`      | Sync latest changes with your branch (resolve conflicts, re-test, commit).                             |

### Plan Directory

Task plans live in `.plans/<task>/` where `<task>` is a slugified task name. Each task directory contains:

- `plan.md` — the specification (source of truth for what to implement)
- `phase-N.md` — execution plans and phase reviews for each phase

The task list (managed via `todo_*` tools) tracks progress. The spec is an immutable reference — agents annotate it with divergence notes but never rewrite it.

## 8. Misc

- NEVER manually split a string using `.chars` or by indexing. Use the `unicode-segmentation` crate.
- No trivial setters for struct methods. Prefer meaningful semantic actions. It's an anti-pattern to directly inspect and manipulate state.
- Environment variables should only be accessed at program initialization and then saved into a struct as needed. Environment variables are a global namespace and should be avoided outside of program startup.
- Use `where` clause for all generics.
- Prefer `match` over `if` where appropriate.
- DO NOT USE CODE COMMENTS TO WRITE ABOUT "SPEC DIVERGENCES" OR "DIVERGENCES". Code comments in the codebase is not the place to discuss planning information. PLANS ARE NOT PERSISTED.
