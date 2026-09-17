//! Feature-registered keybind routing — the slice keybind manifest.
//!
//! [`KeyRoutes`] dissolves the central-handler coupling: each slice
//! registers *route rows* — "when this key fires in this scope, produce
//! this outcome" — and composition generates the keymap bindings from
//! the registered rows. Rows carry no central intent: a row either
//! resolves through the route table itself (a [`RouteOutcome::Action`],
//! looked up by dynamic intent) or names a static intent by [`RouteId`]
//! for composition to bind directly ([`RouteOutcome::StaticIntent`]).
//! The intent vocabulary therefore lives in exactly one place — the
//! composition-side `RouteId` map — and slices never edit central
//! enums.
//!
//! Rows also declare *where* their key binds: a slice's own dynamic
//! scope ([`BindSite::OwnScope`]), every composition scope
//! ([`BindSite::GlobalToggle`] — e.g. the key that opens the slice), or
//! named static scopes only ([`BindSite::StaticScopes`] — a key that
//! belongs to a composition context, not to the slice's scope).
//! Composition's generator walks the rows; nothing else does.
//!
//! Alongside the rows, a slice may register one *input hook* per scope
//! ([`InputHook`]): a synchronous interceptor consulted before the
//! handler's built-in arms while that scope is active. This is the
//! sanctioned carve-out for per-keystroke input surfaces — the hook
//! writes the slice's own state synchronously, exactly as a built-in
//! input popup does. Hooks match the [`EditIntent`] vocabulary (the
//! editing intents a text surface can serve); the kernel translates its
//! richer intent enum down to it, keeping this crate free of the
//! kernel's intent types.
//!
//! Actions return a [`RouteResult`]: erased publish closures plus an
//! optional scope transition. The closures carry real bus publications
//! (this crate depends on kameo for the publish shape); only the
//! kernel's *intent enum* stays out of reach. State access goes
//! through the [`SliceActionState`] trait — the kernel's application
//! state is its sole implementor, so a slice crate declares the
//! surface it needs instead of depending on the kernel.
//!
//! The table is small and scanned linearly; rows attach at slice
//! activation (startup wiring) before the keymap is generated.

use std::sync::Arc;

use kameo::prelude::ActorRef;
use kameo_actors::message_bus::MessageBus;

use crate::key::KeyEvent;
use crate::slice_scope::SliceScopeId;

/// A closure that publishes a typed message to the kernel's bus.
///
/// The kernel's drain task calls each closure with the bus ref; the
/// closure spawns the publish so the synchronous intent handler never
/// awaits.
pub type PublishClosure = Box<dyn FnOnce(&ActorRef<MessageBus>) + Send + 'static>;

/// A message that may travel the kernel's kameo message bus.
///
/// Marker trait owned here (the slice vocabulary crate) so slice
/// crates publish without depending on the kernel. The kernel's bus
/// implements `Publish<M>` for every `M: BusMessage`.
pub trait BusMessage: Clone + Send + 'static {}

/// Composition-side identifier for a route's intent resolution.
///
/// Rows never name kernel intent variants directly — they carry a
/// [`RouteId`], and composition's generator maps ids to intents in one
/// table. A `RouteId` unknown to that map is a wiring bug that
/// surfaces as an unbound key at startup, not a compile error; the
/// mapping test pins every registered id against it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RouteId(&'static str);

impl RouteId {
    /// Mints a route id from its canonical dotted name.
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        Self(name)
    }

    /// The canonical name, e.g. `dashboard:nav-down`.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        self.0
    }
}

/// Where a row's keybinding materializes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindSite {
    /// Bind in the slice's own dynamic scope only.
    OwnScope,
    /// Bind in every composition (static) scope — and in other slices'
    /// dynamic scopes — so a slice's entry-point key works everywhere.
    /// Within the slice's own scope the row is skipped, letting the
    /// slice's own binding (e.g. a close key) win.
    GlobalToggle,
    /// Bind in the named composition (static) scopes only — for slices
    /// whose key belongs to a static context (e.g. normal-mode command
    /// prefixes) rather than to the slice's dynamic scope or everywhere.
    ///
    /// Names are `Scope` display forms (e.g. `"Normal"`); unknown names
    /// warn and skip at generation time. The owning slice's dynamic
    /// scope is never included — a row's scope is where its dynamic
    /// intent resolves, not where it must be displayable.
    StaticScopes(&'static [&'static str]),
}

/// What a row's keypress produces.
#[derive(Debug, Clone)]
pub enum RouteOutcome {
    /// Bind the key to a static intent, resolved by composition from
    /// the [`RouteId`]. The route table is not consulted at keypress
    /// time — the intent flows through the handler's built-in arms.
    /// Used for a slice's shared-chrome keys (`q` → quit).
    StaticIntent(RouteId),
    /// A slice-specific action: the key binds to a dynamic intent and
    /// the handler dispatches through this row's `run`.
    Action {
        /// The action name — the route-table key within the slice.
        action: &'static str,
        /// Human-readable label for which-key popups.
        display: &'static str,
        /// Produces the outcome when the dynamic intent fires.
        run: ActionFn,
    },
}

/// The handler context a row action runs in.
///
/// Actions that touch app state write through `state` — the same
/// guard the intent handler already holds, so an action never mints a
/// second write capability and never takes a second lock. Actions that
/// resolve slice cells take `slices`; cell handles captured at attach
/// time remain the preferred form (the ctx is for state a cell cannot
/// carry).
///
/// `key_bytes` carries the dispatching intent's byte payload to
/// key-hook actions (the terminal capture's PTY encoding); it is empty
/// for actions dispatched from explicit key bindings.
///
/// `state` is the minimal [`SliceActionState`] surface, not the
/// kernel's full application state: a slice crate must never depend on
/// the kernel, so it declares only the state it reads.
pub struct ActionCtx<'a> {
    /// Mutable application state surface, borrowed from the intent handler.
    pub state: &'a mut dyn SliceActionState,
    /// The slice registry, borrowed from the intent handler.
    pub slices: &'a crate::slices::Slices,
    /// The dispatching dynamic intent's byte payload, if any.
    pub key_bytes: Vec<u8>,
}

impl std::fmt::Debug for ActionCtx<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActionCtx")
            .field("state", &"dyn SliceActionState")
            .field("slices", &self.slices)
            .field("key_bytes", &self.key_bytes.len())
            .finish()
    }
}

/// The minimal state surface a slice action may read or write.
///
/// Implemented by the kernel's application state. Growing this trait
/// is the sanctioned way to hand a slice more state — the slice keeps
/// declaring the surface it needs, and the kernel keeps being the only
/// production implementor.
pub trait SliceActionState {
    /// The active session's title, if it has one yet.
    fn active_session_title(&self) -> Option<String>;
    /// The active session's identifier.
    fn active_session_id(&self) -> jinn_core_types::SessionId;
    /// Pushes an error line into the active session's chat history.
    fn push_session_error(&mut self, message: &str);
    /// The active session's working directory (the cwd popup's seeding base).
    fn active_session_cwd(&self) -> std::path::PathBuf;
    /// Mints the publish closure for the kernel's `SetSessionCwd` command.
    ///
    /// The kernel impl wraps its own command type; the cwd slice stays
    /// kernel-free and only ever holds the opaque closure.
    fn publish_session_cwd(
        &self,
        session_id: jinn_core_types::SessionId,
        cwd: std::path::PathBuf,
    ) -> PublishClosure;

    /// Downcast hook: slices that must drive concrete kernel behavior
    /// (e.g. the sidebar's session activation, which both mutates state
    /// and returns commands) request the kernel's application state by
    /// type. Returns `None` when this implementor is not that state.
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        None
    }
}

/// A user-initiated action belonging to a dynamically-registered slice.
///
/// Carries its identity as data instead of an enum variant, so slices
/// (built-in or guest) never edit central intent enums. `action` is the
/// route-table lookup key (scoped by `slice`); `display` is the
/// human-readable label for which-key popups. `bytes` is an optional
/// payload for actions that forward a byte stream (e.g. a key hook
/// wrapping a terminal's PTY encoding) — empty when the action needs
/// none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DynamicIntent {
    /// The slice this intent belongs to.
    pub slice: SliceScopeId,
    /// The action name within the slice (route-table key).
    pub action: String,
    /// Human-readable label for key UI (which-key popup).
    pub display: String,
    /// Optional byte payload carried to the dispatched action.
    pub bytes: Vec<u8>,
}

impl DynamicIntent {
    /// Builds a dynamic intent. Kept total and infallible so slices can
    /// mint intents as `const`-adjacent data.
    #[must_use]
    pub fn new(slice: SliceScopeId, action: &str, display: &str) -> Self {
        Self {
            slice,
            action: action.to_owned(),
            display: display.to_owned(),
            bytes: Vec::new(),
        }
    }

    /// Builds a dynamic intent carrying a byte payload — the key-hook
    /// path, where the encoded key travels with the intent to the
    /// action that publishes it.
    #[must_use]
    pub fn with_bytes(slice: SliceScopeId, action: &str, display: &str, bytes: Vec<u8>) -> Self {
        Self {
            slice,
            action: action.to_owned(),
            display: display.to_owned(),
            bytes,
        }
    }
}

impl std::fmt::Display for DynamicIntent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display)
    }
}

/// The editing intents a slice input hook can serve.
///
/// The kernel's intent enum is richer, but a text input surface can
/// only mean these seven things. Hooks match this vocabulary; the
/// handler translates from its own enum before consulting the hook.
/// This keeps `jinn-slices` free of the kernel's intent enum while
/// preserving the hooks' synchronous write carve-out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditIntent {
    /// Insert a character at the cursor.
    InsertChar(char),
    /// Delete the grapheme before the cursor.
    DeleteBackward,
    /// Delete the grapheme after the cursor.
    DeleteForward,
    /// Move the cursor one character left.
    CursorLeft,
    /// Move the cursor one character right.
    CursorRight,
    /// Move the cursor to the start of the input.
    CursorHome,
    /// Move the cursor to the end of the input.
    CursorEnd,
    /// Insert pasted text at the cursor.
    Paste(String),
}

/// The outcome a route action produces: messages to publish, plus an
/// optional scope transition.
///
/// Carries typed message closures to be dispatched to the actor system
/// via the kernel's message bus, plus an optional scope signal. The
/// scope signal is applied by the handler (an exempt scope-stack
/// writer) *before* the messages publish, so a slice that opens itself
/// pushes its scope before any bus message a subscriber could observe.
pub struct RouteResult {
    /// Typed message closures to publish to the kameo bus.
    pub messages: Vec<PublishClosure>,
    /// Type names of messages, for test inspection.
    pub message_names: Vec<&'static str>,
    /// Scope transition to apply before publishing, if any.
    pub scope_signal: Option<ScopeSignal>,
}

impl std::fmt::Debug for RouteResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RouteResult")
            .field("messages", &self.messages.len())
            .field("message_names", &self.message_names)
            .field("scope_signal", &self.scope_signal)
            .finish()
    }
}

/// A scope-stack transition requested by a route action.
///
/// Slices declare their transitions as data; the composition-side
/// handler applies them. Ownership stays single-writer: only the
/// handler mutates the scope stack, and it does so only on these signals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeSignal {
    /// Push `scope` onto the stack (entering the slice's overlay/tab).
    Push(SliceScopeId),
    /// Pop `scope` if it is the current top scope (leaving the slice).
    PopIf(SliceScopeId),
}

impl RouteResult {
    /// An empty result with no messages.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            messages: Vec::new(),
            message_names: Vec::new(),
            scope_signal: None,
        }
    }

    /// A result with a single typed message to publish to the bus.
    ///
    /// The message is wrapped in a closure that spawns
    /// `bus.tell(Publish(msg))` when the kernel's drain task processes
    /// it — the same publish shape the kernel's bridge uses.
    #[must_use]
    pub fn new_message<M>(msg: M) -> Self
    where
        M: Clone + Send + 'static,
    {
        let mut result = Self::empty();
        result.push_message(msg);
        result
    }

    /// Requests a scope transition, applied by the handler before the
    /// messages publish.
    #[must_use]
    pub fn with_scope_signal(mut self, signal: ScopeSignal) -> Self {
        self.scope_signal = Some(signal);
        self
    }

    /// Append a typed message and return self for chaining.
    #[must_use]
    pub fn with_message<M: Clone + Send + 'static>(mut self, msg: M) -> Self {
        self.push_message(msg);
        self
    }

    /// Append multiple messages of one type at the same time.
    #[must_use]
    pub fn with_messages<I, M>(mut self, msgs: I) -> Self
    where
        M: Clone + Send + 'static,
        I: IntoIterator<Item = M>,
    {
        for msg in msgs {
            self.push_message(msg);
        }
        self
    }

    /// Merge another result's messages into this one.
    #[must_use]
    pub fn merge(mut self, other: RouteResult) -> Self {
        self.messages.extend(other.messages);
        self.message_names.extend(other.message_names);
        self
    }

    /// Wraps the message into a publish closure and records its type.
    fn push_message<M>(&mut self, msg: M)
    where
        M: Clone + Send + 'static,
    {
        self.messages
            .push(Box::new(move |bus: &ActorRef<MessageBus>| {
                let bus = bus.clone();
                tokio::spawn(async move {
                    let _ = bus.tell(kameo_actors::message_bus::Publish(msg)).await;
                });
            }));
        self.message_names.push(std::any::type_name::<M>());
    }
}

/// A row action: produces the route result (messages + optional scope
/// signal) when its dynamic intent fires.
///
/// A closure, not a bare `fn` pointer: actions may capture the slice's
/// cell handle (e.g. submit reads and clears the input buffer). The
/// captured handle is the one registered at slice activation — closure
/// capture does not mint a second write capability. State outside the
/// slice's cells is reached through [`ActionCtx`], lent by the handler
/// at dispatch time.
#[derive(Clone)]
pub struct ActionFn(Arc<dyn Fn(ActionCtx<'_>) -> RouteResult + Send + Sync>);

impl ActionFn {
    /// Wraps a closure or function into a row action.
    #[must_use]
    pub fn new<F>(f: F) -> Self
    where
        F: Fn(ActionCtx<'_>) -> RouteResult + Send + Sync + 'static,
    {
        Self(Arc::new(f))
    }

    /// Runs the action with the handler's context.
    #[must_use]
    pub fn run(&self, ctx: ActionCtx<'_>) -> RouteResult {
        (self.0)(ctx)
    }
}

impl std::fmt::Debug for ActionFn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ActionFn(..)")
    }
}

/// A slice-registered keybind row — one entry of the slice manifest.
#[derive(Debug, Clone)]
pub struct RouteRow {
    /// Composition-facing id (static resolution + diagnostics).
    pub route_id: RouteId,
    /// The dynamic scope this row's key lives in.
    pub scope: SliceScopeId,
    /// The key, in keymap display form (e.g. `<esc>`, `j`).
    pub key: &'static str,
    /// Keymap category hint: `general`, `navigation`, or `input`.
    pub category: &'static str,
    /// Where the binding materializes.
    pub site: BindSite,
    /// Display name of the owning slice, for diagnostics.
    pub feature: &'static str,
    /// What the keypress produces.
    pub outcome: RouteOutcome,
}

impl RouteRow {
    /// The which-key label this row's key shows.
    ///
    /// Static intents are labeled by composition (the bound intent's
    /// own `Display`); dynamic actions carry their label here.
    #[must_use]
    pub fn display(&self) -> &'static str {
        match &self.outcome {
            RouteOutcome::StaticIntent(_) => "",
            RouteOutcome::Action { display, .. } => display,
        }
    }
}

/// A synchronous per-scope input interceptor.
///
/// Consulted by the intent handler while the hook's scope is the active
/// focus: editing intents (typing, cursor moves) are routed here so the
/// slice's input surface captures keystrokes without hard-coded handler
/// arms. Returning `None` lets the intent fall through to the built-in
/// arms (quit and other app-level intents keep working).
///
/// The hook matches the [`EditIntent`] vocabulary; the kernel's intent
/// enum is translated down to it before the hook is consulted.
pub type InputHook = Arc<dyn Fn(&EditIntent) -> Option<RouteResult> + Send + Sync>;

/// A synchronous per-scope interceptor serving raw key events.
///
/// Composition binds the hook as its scope's catch-all: it is consulted
/// only when no explicit binding matched, so the slice's own rows and
/// composition's global toggles keep priority. Returning `Some` yields
/// a dynamic intent dispatched through the route table (e.g. the term
/// overlay's capture hook wraps the key's terminal bytes for the PTY);
/// returning `None` drops the key.
pub type KeyHook = Arc<dyn Fn(&KeyEvent) -> Option<DynamicIntent> + Send + Sync>;

/// Registry of slice keybind routes and hooks.
///
/// Rows attach at slice activation (startup wiring), so the table is
/// interior-mutable behind a lock — the same shape as
/// [`Slices`](crate::slices::Slices). Lookup is infallible: an unbound
/// dynamic intent yields `None` and the handler treats it as inert.
#[derive(Clone, Debug, Default)]
pub struct KeyRoutes {
    rows: row_store::Rows,
    input_hooks: row_store::HookStore<InputHook>,
    key_hooks: row_store::HookStore<KeyHook>,
}

impl KeyRoutes {
    /// Creates an empty route table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Attaches a built-in row.
    pub fn attach(&self, row: RouteRow) {
        self.rows.push(row);
    }

    /// Registers the synchronous input hook for a slice's scope.
    pub fn register_input_hook(&self, scope: &SliceScopeId, hook: InputHook) {
        self.input_hooks.insert(scope.clone(), hook);
    }

    /// Returns the input hook registered for `scope`, if any.
    #[must_use]
    pub fn input_hook(&self, scope: &SliceScopeId) -> Option<InputHook> {
        self.input_hooks.get(scope)
    }

    /// Registers the raw-key hook for a slice's scope.
    pub fn register_key_hook(&self, scope: &SliceScopeId, hook: KeyHook) {
        self.key_hooks.insert(scope.clone(), hook);
    }

    /// Returns the raw-key hook registered for `scope`, if any.
    #[must_use]
    pub fn key_hook(&self, scope: &SliceScopeId) -> Option<KeyHook> {
        self.key_hooks.get(scope)
    }

    /// Dispatches a dynamic intent through its registered row.
    ///
    /// Matches by `(slice, action)` — the dynamic intent's identity.
    /// `None` means no row serves this intent: the handler treats the
    /// intent as inert.
    pub fn action_for(&self, intent: &DynamicIntent, ctx: ActionCtx<'_>) -> Option<RouteResult> {
        let run = {
            let rows = self.rows.rows();
            rows.into_iter().find_map(|row| match &row.outcome {
                RouteOutcome::Action {
                    action: row_action,
                    run,
                    ..
                } if *row_action == intent.action && row.scope == intent.slice => Some(run.clone()),
                _ => None,
            })
        };
        run.map(|run| run.run(ctx))
    }

    /// Returns all attached rows in attach order.
    #[must_use]
    pub fn rows(&self) -> Vec<RouteRow> {
        self.rows.rows()
    }

    /// Returns the scope ids of all registered input hooks.
    #[must_use]
    pub fn input_hook_scopes(&self) -> Vec<SliceScopeId> {
        self.input_hooks.keys()
    }

    /// Returns the scope ids of all registered raw-key hooks.
    #[must_use]
    pub fn key_hook_scopes(&self) -> Vec<SliceScopeId> {
        self.key_hooks.keys()
    }
}

/// Append-only row/hook store shared by all clones of the table.
mod row_store {
    use super::RouteRow;
    use crate::SliceScopeId;
    use parking_lot::RwLock;
    use std::collections::HashMap;
    use std::sync::Arc;

    #[derive(Debug, Default)]
    pub struct Rows {
        inner: Arc<RwLock<Vec<RouteRow>>>,
    }

    impl Clone for Rows {
        fn clone(&self) -> Self {
            Self {
                inner: Arc::clone(&self.inner),
            }
        }
    }

    impl Rows {
        pub fn push(&self, row: RouteRow) {
            self.inner.write().push(row);
        }

        /// Snapshot of all rows; the guard is released before return.
        pub fn rows(&self) -> Vec<RouteRow> {
            self.inner.read().clone()
        }
    }

    /// Scope-keyed hook store shared by all clones.
    ///
    /// Keyed on the [`SliceScopeId`] itself, never a string form: a
    /// roundtrip through `FromStr` reconstructs ids with
    /// `captures_input: true`, so navigation scopes would silently miss
    /// their own lookups.
    pub struct HookStore<H> {
        inner: Arc<RwLock<HashMap<SliceScopeId, DebugEntry<H>>>>,
    }

    /// A hook wrapped for `Debug` (closures are not `Debug`).
    struct DebugEntry<H>(H);

    impl<H> std::fmt::Debug for DebugEntry<H> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("hook(..)")
        }
    }

    impl<H> Default for HookStore<H> {
        fn default() -> Self {
            Self {
                inner: Arc::default(),
            }
        }
    }

    impl<H> std::fmt::Debug for HookStore<H> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("HookStore")
                .field("scopes", &self.keys())
                .finish()
        }
    }

    impl<H> Clone for HookStore<H> {
        fn clone(&self) -> Self {
            Self {
                inner: Arc::clone(&self.inner),
            }
        }
    }

    impl<H> HookStore<H> {
        pub fn insert(&self, scope: SliceScopeId, hook: H) {
            self.inner.write().insert(scope, DebugEntry(hook));
        }

        pub fn get(&self, scope: &SliceScopeId) -> Option<H>
        where
            H: Clone,
        {
            self.inner.read().get(scope).map(|entry| entry.0.clone())
        }

        pub fn keys(&self) -> Vec<SliceScopeId> {
            self.inner.read().keys().cloned().collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ActionCtx;
    use super::ActionFn;
    use super::BindSite;
    use super::EditIntent;
    use super::KeyRoutes;
    use super::PublishClosure;
    use super::RouteId;
    use super::RouteOutcome;
    use super::RouteResult;
    use super::RouteRow;
    use super::SliceActionState;
    use crate::SliceScopeId;
    use crate::slices::Slices;

    fn scope() -> SliceScopeId {
        SliceScopeId::new("test-slice", "main")
    }

    fn row(action: &'static str, key: &'static str) -> RouteRow {
        RouteRow {
            route_id: RouteId::new("test-slice:action"),
            scope: scope(),
            key,
            category: "general",
            site: BindSite::OwnScope,
            feature: "test-slice",
            outcome: RouteOutcome::Action {
                action,
                display: "test action",
                run: ActionFn::new(|_ctx| RouteResult::empty()),
            },
        }
    }

    fn dynamic_intent(action: &str) -> crate::DynamicIntent {
        crate::DynamicIntent::new(scope(), action, "test action")
    }

    fn ctx<'a>(state: &'a mut dyn SliceActionState, slices: &'a Slices) -> ActionCtx<'a> {
        ActionCtx {
            state,
            slices,
            key_bytes: Vec::new(),
        }
    }

    /// Minimal state double for action-context tests.
    #[derive(Debug, Default)]
    struct TestState {
        errors: Vec<String>,
    }

    impl SliceActionState for TestState {
        fn active_session_title(&self) -> Option<String> {
            Some("test".to_owned())
        }

        fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
            Some(self)
        }

        fn active_session_id(&self) -> jinn_core_types::SessionId {
            jinn_core_types::SessionId::new()
        }

        fn push_session_error(&mut self, message: &str) {
            self.errors.push(message.to_owned());
        }

        fn active_session_cwd(&self) -> std::path::PathBuf {
            std::path::PathBuf::from("/test/cwd")
        }

        fn publish_session_cwd(
            &self,
            _session_id: jinn_core_types::SessionId,
            _cwd: std::path::PathBuf,
        ) -> PublishClosure {
            Box::new(|_bus| {})
        }
    }

    #[derive(Debug, Default)]
    struct OtherState;

    impl SliceActionState for OtherState {
        fn active_session_title(&self) -> Option<String> {
            None
        }

        fn active_session_id(&self) -> jinn_core_types::SessionId {
            jinn_core_types::SessionId::new()
        }

        fn push_session_error(&mut self, _message: &str) {}

        fn active_session_cwd(&self) -> std::path::PathBuf {
            std::path::PathBuf::new()
        }

        fn publish_session_cwd(
            &self,
            _session_id: jinn_core_types::SessionId,
            _cwd: std::path::PathBuf,
        ) -> PublishClosure {
            Box::new(|_bus| {})
        }
    }

    #[rstest::rstest]
    #[test]
    fn as_any_mut_downcasts_only_when_implemented() {
        // Given a TestState behind the trait.
        let mut test = TestState::default();
        let test: &mut dyn SliceActionState = &mut test;

        // When downcasting to the concrete type.
        let down = test
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<TestState>());

        // Then the concrete state resolves.
        assert!(down.is_some());

        // Given a state without the hook.
        let mut other = OtherState;
        let other: &mut dyn SliceActionState = &mut other;

        // Then the downcast is None.
        assert!(other.as_any_mut().is_none());
    }

    #[rstest::rstest]
    #[test]
    fn dynamic_intent_with_registered_row_dispatches_action() {
        // Given a table with an action row attached.
        let routes = KeyRoutes::new();
        routes.attach(row("poke", "<enter>"));

        // When dispatching a dynamic intent carrying the row's action.
        let mut state = TestState::default();
        let slices = Slices::new();
        let result = routes.action_for(&dynamic_intent("poke"), ctx(&mut state, &slices));

        // Then the row's action ran (empty result, no error).
        assert!(result.is_some());
    }

    #[rstest::rstest]
    #[test]
    fn dynamic_intent_without_row_is_inert() {
        // Given a table with no matching row.
        let routes = KeyRoutes::new();

        // When dispatching an unregistered dynamic intent.
        let mut state = TestState::default();
        let slices = Slices::new();
        let result = routes.action_for(&dynamic_intent("missing"), ctx(&mut state, &slices));

        // Then nothing resolves — the handler will treat it as inert.
        assert!(result.is_none());
    }

    #[rstest::rstest]
    #[test]
    fn input_hook_intercepts_edit_intents_for_its_scope() {
        // Given a table with a hook registered for the scope.
        let routes = KeyRoutes::new();
        routes.register_input_hook(
            &scope(),
            std::sync::Arc::new(|intent: &EditIntent| {
                matches!(intent, EditIntent::DeleteBackward).then(RouteResult::empty)
            }),
        );

        // When looking up the hook.
        #[expect(
            clippy::expect_used,
            reason = "test helper: the routes under test register their hook"
        )]
        let hook = routes
            .input_hook(&scope())
            .expect("hook registered in this test's routes");

        // Then the hook serves the editing intent and declines others.
        assert!(hook(&EditIntent::DeleteBackward).is_some());
        assert!(hook(&EditIntent::InsertChar('x')).is_none());
    }

    #[rstest::rstest]
    #[test]
    fn input_hook_scopes_enumerates_registered_scopes() {
        // Given a table with one input hook registered.
        let routes = KeyRoutes::new();
        routes.register_input_hook(&scope(), std::sync::Arc::new(|_: &EditIntent| None));

        // When enumerating input hook scopes.
        let scopes = routes.input_hook_scopes();

        // Then the registered scope is listed.
        assert_eq!(scopes, vec![scope()]);
    }

    #[rstest::rstest]
    #[test]
    fn key_hook_resolves_on_navigation_scope() {
        // Given a navigation (non-input-capturing) scope and a key hook
        // registered for it — the term overlay's capture shape.
        let navigation = SliceScopeId::navigation("term", "control");
        let routes = KeyRoutes::new();
        let hook: super::KeyHook = {
            let hook_scope = navigation.clone();
            std::sync::Arc::new(move |event: &crate::key::KeyEvent| {
                Some(crate::DynamicIntent::new(
                    hook_scope.clone(),
                    "send-key",
                    "send key",
                ))
                .filter(|_| event.key == crate::key::Key::Char('x'))
            })
        };
        routes.register_key_hook(&navigation, hook);

        // When looking the hook up by the same id (no string roundtrip).
        let hook = routes.key_hook(&navigation);

        // Then the hook resolves for the navigation scope — a FromStr
        // roundtrip would have reconstructed a captures_input id and
        // missed this lookup.
        let hook = hook.expect("key hook registered for navigation scope");
        // And the hook serves the key it was registered to serve.
        let event = crate::key::KeyEvent {
            key: crate::key::Key::Char('x'),
            modifiers: crate::key::Modifiers::none(),
        };
        let served = hook(&event).expect("hook serves the registered key");
        assert_eq!(served.slice, navigation);
        assert_eq!(served.action, "send-key");
        // And it declines other keys.
        let other = crate::key::KeyEvent {
            key: crate::key::Key::Char('y'),
            modifiers: crate::key::Modifiers::none(),
        };
        assert!(hook(&other).is_none());
    }

    #[rstest::rstest]
    #[test]
    fn key_hook_scopes_enumerates_registered_scopes() {
        // Given a table with one key hook registered.
        let routes = KeyRoutes::new();
        routes.register_key_hook(
            &scope(),
            std::sync::Arc::new(|_: &crate::key::KeyEvent| None),
        );

        // When enumerating key hook scopes.
        let scopes = routes.key_hook_scopes();

        // Then the registered scope is listed.
        assert_eq!(scopes, vec![scope()]);
    }

    #[rstest::rstest]
    #[test]
    fn new_message_records_type_name_and_closure() {
        // Given an empty route result.
        // When building it from one typed message.
        let result = RouteResult::new_message("hello".to_owned());

        // Then the message name is recorded for inspection.
        assert_eq!(result.message_names.len(), 1);
        // And the publish closure is present.
        assert_eq!(result.messages.len(), 1);
        // And no scope transition was requested.
        assert!(result.scope_signal.is_none());
    }
}
