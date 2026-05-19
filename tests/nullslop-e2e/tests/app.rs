//! Cucumber `World` wrapping a full application with production actor wiring.
//!
//! The [`AppWorld`] creates a complete application using the same
//! `actor_wiring::create_core_with_actor_host` function that production uses,
//! but with fake services so no real backends are hit. All 16 actors spawn,
//! init sequences run, and the system-ready signal fires.
//!
//! This is the standard e2e world for all future feature files.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use cucumber::World;
use nullslop::actor_wiring;
use nullslop_domain::ApiKeys;
use nullslop_domain::ApiKeysService;
use nullslop_domain::AppMsg;
use nullslop_domain::AppState;
use nullslop_domain::AppUiRegistry;
use nullslop_domain::CancelStream;
use nullslop_domain::ChatEntry;
use nullslop_domain::ChatEntryKind;
use nullslop_domain::ConfigStorageService;
use nullslop_domain::EnqueueUserMessage;
use nullslop_domain::Event;
use nullslop_domain::FakeLlmServiceFactory;
use nullslop_domain::InMemoryConfigStorage;
use nullslop_domain::InMemoryUserPreferencesStorage;
use nullslop_domain::LlmMessage;
use nullslop_domain::LlmServiceFactoryService;
use nullslop_domain::PinChatEntry;
use nullslop_domain::PinPosition;
use nullslop_domain::PromptStrategyId;
use nullslop_domain::ProviderRegistry;
use nullslop_domain::ProviderRegistryService;
use nullslop_domain::ProvidersConfig;
use nullslop_domain::SessionId;
use nullslop_domain::StateReadGuard;
use nullslop_domain::StreamCompleted;
use nullslop_domain::StreamCompletedReason;
use nullslop_domain::StreamToken;
use nullslop_domain::SwitchPromptStrategy;
use nullslop_domain::ToolDefinition;
use nullslop_domain::ToolResult;
use nullslop_domain::UnpinChatEntry;
use nullslop_domain::UserPreferencesStorageService;
use nullslop_tui::AppStatus;
use nullslop_tui::MsgHandler;
use nullslop_tui::Scope;
use nullslop_tui::TuiApp;
use nullslop_tui::app::WhichKeyInstance;
use nullslop_tui::config::TuiConfig;
use nullslop_tui::selection::SelectionState;
use nullslop_tui::suspend::Suspend;

/// Cucumber world wrapping a full application with production actor wiring.
///
/// Created fresh for each scenario. Provides the full actor system
/// (all 16 actors) backed by fake services.
#[derive(World)]
#[world(init = Self::new_app_world)]
pub struct AppWorld {
    /// The full TUI application under test (no terminal backend connected).
    pub app: TuiApp,
    /// Tokio runtime handle.
    #[allow(dead_code)]
    handle: tokio::runtime::Handle,
    /// Temp directory holding all test filesystem paths. Cleaned up on drop.
    #[allow(dead_code)]
    temp_dir: tempfile::TempDir,
    /// The fake LLM factory — kept so tests can inspect received calls.
    fake_factory: Arc<FakeLlmServiceFactory>,
    /// Pre-reload CWD captured during "saved and reloaded" step.
    cwd_before_reload: Option<std::path::PathBuf>,
}

impl std::fmt::Debug for AppWorld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppWorld")
            .field("state", &self.app.core.state)
            .finish_non_exhaustive()
    }
}

impl AppWorld {
    /// Creates a new world with the full production actor wiring and fake services.
    fn new_app_world() -> Self {
        let temp_dir = tempfile::TempDir::new().expect("test temp dir");
        let fake_factory = Arc::new(FakeLlmServiceFactory::new(vec![]));
        let (app, handle) = Self::new_app_world_impl(temp_dir.path(), fake_factory.clone());
        Self {
            app,
            handle,
            temp_dir,
            fake_factory,
            cwd_before_reload: None,
        }
    }

    /// Creates a `TuiApp` and tokio runtime handle backed by fake services at the given path.
    ///
    /// Shared between initial world creation and app restart. The caller
    /// supplies the fake factory so call history can be inspected across
    /// restarts.
    fn new_app_world_impl(
        temp_path: &Path,
        fake_factory: Arc<FakeLlmServiceFactory>,
    ) -> (TuiApp, tokio::runtime::Handle) {
        // Run setup on a separate thread to avoid
        // "Cannot block the current thread from within a runtime".
        // Only the core/services/actor_host cross the thread boundary
        // (TuiApp is !Send due to trait objects).
        let (handle_tx, handle_rx) = std::sync::mpsc::channel();
        let temp_dir_path = temp_path.to_path_buf();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("test runtime");
            let handle = rt.handle().clone();

            // Build fake services — same pattern as production App::dispatch
            // but with all fake implementations.
            let paths = nullslop_domain::AppPaths::new_in(&temp_dir_path);
            let config_storage = ConfigStorageService::new(Arc::new(InMemoryConfigStorage::new()));
            let resolved_api_keys = ApiKeysService::new(ApiKeys::new());
            let empty_config = ProvidersConfig {
                providers: vec![],
                aliases: vec![],
                default_provider: None,
            };
            let provider_registry = ProviderRegistryService::new(
                ProviderRegistry::from_config(empty_config).expect("empty config is valid"),
            );
            let llm_service = LlmServiceFactoryService::new(fake_factory);
            let user_preferences_storage =
                UserPreferencesStorageService::new(Arc::new(InMemoryUserPreferencesStorage::new()));
            let session_store = nullslop_domain::SessionStoreService::new(Arc::new(
                nullslop_domain::SqliteSessionStore::new_in(&paths.sessions_dir()).expect("store"),
            ));

            // Call production wiring — spawns all 16 actors.
            let (core, services, actor_host) = actor_wiring::create_core_with_actor_host(
                &handle,
                llm_service,
                provider_registry,
                resolved_api_keys,
                config_storage,
                session_store,
                user_preferences_storage,
            );

            // Intentionally leaked: each AppWorld restart gets a completely fresh tokio runtime.
            // Leaking avoids subtle issues where a reused runtime retains closed channels,
            // stale actor state, or dangling task handles from the previous app instance.
            // Test processes are short-lived, so the memory cost is negligible.
            let _ = Box::leak(Box::new(rt));

            handle_tx
                .send((handle, core, services, actor_host))
                .expect("send results");
        });

        let (handle, core, services, actor_host) = handle_rx.recv().expect("receive setup results");

        // Build TuiApp following the production App::dispatch pattern.
        let mut ui_registry = AppUiRegistry::new();
        nullslop_domain::register_all_ui_elements(&mut ui_registry);

        let app = TuiApp {
            core,
            services,
            actor_host,
            ui_registry,
            events: MsgHandler::new(),
            which_key: WhichKeyInstance::new(nullslop_tui::keymap::init(), Scope::Normal),
            suspend: Suspend::new(),
            event_thread: None,
            status: AppStatus::Starting,
            selection: SelectionState::Idle,
            selectable_rects: Default::default(),
            pending_clipboard: false,
            config: TuiConfig::default(),
            sidebar: {
                let mut s = nullslop_domain::feat::ui::sidebar::Sidebar::new();
                nullslop_domain::feat::ui::sidebar::register_sections(&mut s);
                s
            },
        };

        (app, handle)
    }

    /// Polls `AppState` at 10ms intervals until `predicate` returns `true`
    /// or the 5-second timeout expires.
    ///
    /// Use in `When` steps that trigger async actor work, so `Then` steps
    /// can assert synchronously.
    pub async fn wait_until(&self, predicate: impl Fn(&AppState) -> bool) {
        let state = self.app.core.state.clone();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if predicate(&state.read()) {
                break;
            }
            if tokio::time::Instant::now() > deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Sends a keystroke to the app.
    pub fn press_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        let event = crossterm::event::Event::Key(KeyEvent::new(code, modifiers));
        self.app.handle_msg(nullslop_tui::msg::Msg::Input(event));
    }

    /// Routes an intent through the app.
    pub fn route_intent(&mut self, intent: nullslop_domain::Intent) {
        self.app.route_intent(intent);
    }

    /// Submits a command to the core's message channel.
    #[allow(dead_code)]
    pub fn submit_command(&self, cmd: nullslop_domain::Command) {
        self.app.core.submit_command(cmd);
    }

    /// Submits an event to the core's message channel.
    fn submit_event(&self, event: Event) {
        let _ = self.app.core.sender().send(AppMsg::Event {
            event,
            source: None,
        });
    }

    /// Returns a read guard to the application state.
    pub fn state(&self) -> StateReadGuard<'_> {
        self.app.core.state.read()
    }

    /// Returns the active session ID.
    fn active_session_id(&self) -> SessionId {
        self.state().session.active_session_id().clone()
    }

    /// Returns a copy of all messages received by the fake LLM factory.
    pub fn received_llm_calls(&self) -> Vec<Vec<LlmMessage>> {
        self.fake_factory.received_calls()
    }

    /// Runs graceful coordinated shutdown of the actor system.
    ///
    /// Spawns a dedicated thread to avoid "Cannot block the current thread
    /// from within a runtime" panics from coordinated_shutdown's blocking_recv.
    #[allow(dead_code)]
    pub fn graceful_shutdown(&mut self) {
        let actor_host = self.app.actor_host.clone();
        let state = self.app.core.state.clone();
        let handle = self.handle.clone();

        let (tx, rx) = std::sync::mpsc::channel::<()>();
        std::thread::spawn(move || {
            nullslop_domain::coordinated_shutdown(
                actor_host.backend(),
                &state,
                &handle,
                nullslop_domain::SHUTDOWN_TIMEOUT,
            );
            let _ = tx.send(());
        });
        rx.recv().expect("shutdown thread completed");
    }
}

// ---------------------------------------------------------------------------
// Step definitions
// ---------------------------------------------------------------------------

/// Parses a human-readable key name into a [`KeyCode`].
fn parse_key_code(name: &str) -> KeyCode {
    // Match special key names case-insensitively.
    match name.to_lowercase().as_str() {
        "enter" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "backspace" => KeyCode::Backspace,
        "tab" => KeyCode::Tab,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "delete" => KeyCode::Delete,
        "space" => KeyCode::Char(' '),
        _ => {
            // Single characters preserve case (G vs g are different keys).
            if name.len() == 1 {
                KeyCode::Char(name.chars().next().expect("single char"))
            } else {
                panic!("unknown key: {name}")
            }
        }
    }
}

/// Parses a human-readable modifier name into [`KeyModifiers`].
fn parse_modifier(name: &str) -> KeyModifiers {
    match name.to_lowercase().as_str() {
        "shift" => KeyModifiers::SHIFT,
        "ctrl" | "control" => KeyModifiers::CONTROL,
        "alt" => KeyModifiers::ALT,
        _ => panic!("unknown modifier: {name}"),
    }
}

/// Parses a human-readable mode name into [`nullslop_domain::Mode`].
fn parse_mode(name: &str) -> nullslop_domain::Mode {
    match name.to_lowercase().as_str() {
        "normal" => nullslop_domain::Mode::Normal,
        "input" => nullslop_domain::Mode::Input,
        "picker" => nullslop_domain::Mode::Picker,
        _ => panic!("unknown mode: {name}"),
    }
}

/// Parses a stream-completed reason from a human-readable word.
fn parse_stream_reason(name: &str) -> StreamCompletedReason {
    match name.to_lowercase().as_str() {
        "finished" => StreamCompletedReason::Finished,
        "canceled" | "cancelled" => StreamCompletedReason::Canceled,
        "tooluse" | "tool_use" | "tool use" => StreamCompletedReason::ToolUse,
        _ => panic!("unknown stream reason: {name}"),
    }
}

/// Parses a pin position from a human-readable word.
fn parse_pin_position(name: &str) -> PinPosition {
    match name.to_uppercase().as_str() {
        "TOP" => PinPosition::Top,
        "BOTTOM" => PinPosition::Bottom,
        "RELATIVE" => PinPosition::Relative,
        _ => panic!("unknown pin position: {name}"),
    }
}

// --- Given steps ---

/// World is already initialised with a fresh AppWorld.
#[cucumber::given(expr = "a fresh app")]
fn given_a_fresh_app(_world: &mut AppWorld) {}

/// Sets the app's mode by pushing the appropriate scope onto the scope stack.
#[cucumber::given(expr = "the app is in {word} mode")]
fn given_app_in_mode(world: &mut AppWorld, mode: String) {
    let scope = match parse_mode(&mode) {
        nullslop_domain::Mode::Normal => Scope::Normal,
        nullslop_domain::Mode::Input => {
            let mut state = world.app.core.state.write();
            state
                .frontend
                .scope_stack
                .push(nullslop_domain::common::app_state::FocusScope::Input);
            drop(state);
            Scope::Input
        }
        nullslop_domain::Mode::Picker => Scope::Picker,
    };
    world.app.which_key.set_scope(scope);
}

/// Pre-fills the active chat input buffer with the given text.
#[cucumber::given(expr = "the input buffer contains {string}")]
fn given_input_buffer_contains(world: &mut AppWorld, text: String) {
    world
        .app
        .core
        .state
        .write()
        .active_chat_input_mut()
        .replace_all(text.to_owned());
}

/// Sets the active provider to a dummy value so message submission works.
#[cucumber::given(expr = "the active provider is set")]
fn given_active_provider_set(world: &mut AppWorld) {
    world
        .app
        .core
        .state
        .write()
        .active_session_mut()
        .set_model("test".to_owned());
}

/// Pre-populates the active session with user and assistant messages.
#[cucumber::given(expr = "the active session has {int} user messages and {int} assistant messages")]
fn given_session_has_messages(world: &mut AppWorld, user_count: u64, assistant_count: u64) {
    let mut state = world.app.core.state.write();
    for i in 0..user_count {
        state
            .active_session_mut()
            .push_entry(ChatEntry::user(format!("user message {i}")));
    }
    for i in 0..assistant_count {
        state
            .active_session_mut()
            .push_entry(ChatEntry::assistant(format!("assistant message {i}")));
    }
}

/// Sets the active session to streaming state.
#[cucumber::given(expr = "the active session is streaming")]
fn given_session_is_streaming(world: &mut AppWorld) {
    world
        .app
        .core
        .state
        .write()
        .active_session_mut()
        .begin_streaming();
}

/// Sets the active session to sending state.
#[cucumber::given(expr = "the active session is sending")]
fn given_session_is_sending(world: &mut AppWorld) {
    world
        .app
        .core
        .state
        .write()
        .active_session_mut()
        .begin_sending();
}

/// Sets the active session's prompt strategy.
#[cucumber::given(expr = "the active session strategy is {word}")]
fn given_session_strategy(world: &mut AppWorld, strategy: String) {
    let strategy_id = PromptStrategyId::new(&strategy);
    world
        .app
        .core
        .state
        .write()
        .active_session_mut()
        .switch_strategy(strategy_id);
}

/// Adds a system entry to the active session's history.
#[cucumber::given(expr = "the active session has a system entry with text {string}")]
fn given_system_entry(world: &mut AppWorld, text: String) {
    world
        .app
        .core
        .state
        .write()
        .active_session_mut()
        .push_entry(ChatEntry::system(&text));
}

/// Injects a prompt template into the app state's template store.
#[cucumber::given(expr = "a prompt template {string} with body {string}")]
fn given_prompt_template(world: &mut AppWorld, name: String, body: String) {
    let mut state = world.app.core.state.write();
    let mut templates = state.context.prompt_templates.templates().to_vec();
    templates.push(nullslop_domain::PromptTemplate {
        name,
        description: String::new(),
        body,
    });
    state.context.prompt_templates = nullslop_domain::PromptTemplateStore::from_vec(templates);
}

/// Pins the last entry in the active session with the given position.
#[cucumber::given(expr = "the active session has a pinned {word} entry with text {string}")]
fn given_pinned_entry(world: &mut AppWorld, position: String, text: String) {
    let pin_pos = parse_pin_position(&position);
    let mut state = world.app.core.state.write();
    let index = state
        .active_session_mut()
        .push_entry(ChatEntry::system(&text));
    let entry_id = state.active_session().history()[index].id.clone();
    state.active_session_mut().pin_entry(&entry_id, pin_pos);
}

// --- When steps ---

/// Simulates the user pressing a single key (no modifiers).
#[cucumber::when(expr = "the user presses {word}")]
fn when_user_presses_key(world: &mut AppWorld, key: String) {
    let code = parse_key_code(&key);
    world.press_key(code, KeyModifiers::NONE);
}

/// Simulates the user pressing a key with a modifier.
#[cucumber::when(expr = "the user presses {word} with {word}")]
fn when_user_presses_key_with_mod(world: &mut AppWorld, key: String, modifier: String) {
    let code = parse_key_code(&key);
    let mods = parse_modifier(&modifier);
    world.press_key(code, mods);
}

/// Routes a ToggleWhichKey command directly.
#[cucumber::when(expr = "the app routes the ToggleWhichKey command")]
fn when_routes_toggle_which_key(world: &mut AppWorld) {
    world.route_intent(nullslop_domain::Intent::ToggleWhichkey);
}

/// Runs a headless script through the keymap pipeline.
#[cucumber::when(expr = "I run the headless script {string}")]
fn when_run_headless_script(world: &mut AppWorld, script: String) {
    run_headless_script(world, &script);
}

/// Restarts the app against the same temp directory.
///
/// Shuts down the old actor system (closing SQLite connections), then spins
/// up a fresh one. The temp directory is preserved so session persistence
/// can be verified.
///
/// Runs shutdown on a background thread to avoid "Cannot block the current
/// thread from within a runtime" panics from `coordinated_shutdown`'s
/// `blocking_recv` call.
#[cucumber::when(expr = "we restart the app")]
fn when_restart_app(world: &mut AppWorld) {
    let temp_dir_path = world.temp_dir.path().to_path_buf();
    let fake_factory = world.fake_factory.clone();

    // Extract handles needed for shutdown (all Send).
    let actor_host = world.app.actor_host.clone();
    let state = world.app.core.state.clone();
    let old_handle = world.handle.clone();

    // Run shutdown + actor wiring on a dedicated thread to escape the
    // cucumber tokio runtime context (coordinated_shutdown uses blocking_recv).
    // Only the Send-safe parts cross the thread boundary.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // Shut down the old actor system.
        nullslop_domain::coordinated_shutdown(
            actor_host.backend(),
            &state,
            &old_handle,
            nullslop_domain::SHUTDOWN_TIMEOUT,
        );

        // Create a new runtime and actor system on this thread.
        let rt = tokio::runtime::Runtime::new().expect("test runtime");
        let handle = rt.handle().clone();

        let paths = nullslop_domain::AppPaths::new_in(&temp_dir_path);
        let config_storage = ConfigStorageService::new(Arc::new(InMemoryConfigStorage::new()));
        let resolved_api_keys = ApiKeysService::new(ApiKeys::new());
        let empty_config = ProvidersConfig {
            providers: vec![],
            aliases: vec![],
            default_provider: None,
        };
        let provider_registry = ProviderRegistryService::new(
            ProviderRegistry::from_config(empty_config).expect("empty config is valid"),
        );
        let llm_service = LlmServiceFactoryService::new(fake_factory);
        let user_preferences_storage =
            UserPreferencesStorageService::new(Arc::new(InMemoryUserPreferencesStorage::new()));
        let session_store = nullslop_domain::SessionStoreService::new(Arc::new(
            nullslop_domain::SqliteSessionStore::new_in(&paths.sessions_dir()).expect("store"),
        ));

        let (core, services, actor_host) = actor_wiring::create_core_with_actor_host(
            &handle,
            llm_service,
            provider_registry,
            resolved_api_keys,
            config_storage,
            session_store,
            user_preferences_storage,
        );

        // Intentionally leaked: each AppWorld restart gets a completely fresh tokio runtime.
        let _ = Box::leak(Box::new(rt));

        tx.send((handle, core, services, actor_host))
            .expect("send restart results");
    });

    let (handle, core, services, actor_host) = rx.recv().expect("receive restart results");

    // Build TuiApp on the calling thread (TuiApp is !Send).
    let mut ui_registry = AppUiRegistry::new();
    nullslop_domain::register_all_ui_elements(&mut ui_registry);

    let app = TuiApp {
        core,
        services,
        actor_host,
        ui_registry,
        events: MsgHandler::new(),
        which_key: WhichKeyInstance::new(nullslop_tui::keymap::init(), Scope::Normal),
        suspend: Suspend::new(),
        event_thread: None,
        status: AppStatus::Starting,
        selection: SelectionState::Idle,
        selectable_rects: Default::default(),
        pending_clipboard: false,
        config: TuiConfig::default(),
        sidebar: {
            let mut s = nullslop_domain::feat::ui::sidebar::Sidebar::new();
            nullslop_domain::feat::ui::sidebar::register_sections(&mut s);
            s
        },
    };

    world.app = app;
    world.handle = handle;
}

/// Submits an EnqueueUserMessage command and waits for the entry to appear.
#[cucumber::when(expr = "the app submits an EnqueueUserMessage with text {string}")]
async fn when_enqueue_user_message(world: &mut AppWorld, text: String) {
    let session_id = world.active_session_id();
    world.submit_command(nullslop_domain::Command::EnqueueUserMessage(
        EnqueueUserMessage {
            session_id,
            entry: ChatEntry::user(&text),
        },
    ));
    world
        .wait_until(|s| {
            s.active_session().history().iter().any(|e| {
                matches!(&e.kind, ChatEntryKind::User { display, .. } if display.contains(&text))
            })
        })
        .await;
}

/// Submits a StreamToken event and waits for it to appear in history.
#[cucumber::when(expr = "the app submits a StreamToken with text {string}")]
async fn when_submit_stream_token(world: &mut AppWorld, text: String) {
    let session_id = world.active_session_id();
    world.submit_event(Event::StreamToken(StreamToken {
        session_id,
        index: 0,
        token: text,
        is_thinking: false,
    }));
    // Give the actor system time to process.
    tokio::time::sleep(Duration::from_millis(50)).await;
}

/// Submits a StreamCompleted event with the given reason.
#[cucumber::when(expr = "the app submits a StreamCompleted with {word} reason")]
async fn when_submit_stream_completed(world: &mut AppWorld, reason: String) {
    let session_id = world.active_session_id();
    let parsed_reason = parse_stream_reason(&reason);
    world.submit_event(Event::StreamCompleted(StreamCompleted {
        session_id,
        reason: parsed_reason,
        assistant_content: None,
        tool_calls: None,
        cost: None,
    }));
    // Give the actor system time to process.
    tokio::time::sleep(Duration::from_millis(50)).await;
}

/// Submits a CancelStream command.
#[cucumber::when(expr = "the app submits a CancelStream command")]
async fn when_submit_cancel_stream(world: &mut AppWorld) {
    let session_id = world.active_session_id();
    world.submit_command(nullslop_domain::Command::CancelStream(CancelStream {
        session_id,
    }));
    world
        .wait_until(|s| s.active_session().phase() != nullslop_domain::SessionPhase::Streaming)
        .await;
}

/// Submits a SwitchPromptStrategy command.
#[cucumber::when(expr = "the app submits a SwitchPromptStrategy with {word}")]
async fn when_submit_switch_strategy(world: &mut AppWorld, strategy: String) {
    let session_id = world.active_session_id();
    let strategy_id = PromptStrategyId::new(&strategy);
    world.submit_command(nullslop_domain::Command::SwitchPromptStrategy(
        SwitchPromptStrategy {
            session_id,
            strategy_id,
        },
    ));
    // Give the actor system time to process.
    tokio::time::sleep(Duration::from_millis(50)).await;
}

/// Submits a ToolsRegistered event with a single tool definition.
#[cucumber::when(expr = "the app submits a ToolsRegistered event with tool {string}")]
async fn when_submit_tools_registered(world: &mut AppWorld, tool_name: String) {
    world.submit_event(Event::ToolsRegistered(nullslop_domain::ToolsRegistered {
        provider: "test".to_owned(),
        definitions: vec![ToolDefinition {
            name: tool_name,
            description: String::new(),
            parameters: serde_json::json!({"type": "object"}),
            prompt_snippet: None,
            prompt_guidelines: vec![],
        }],
    }));
    // Give the actor system time to process.
    tokio::time::sleep(Duration::from_millis(50)).await;
}

/// Submits a ToolExecutionCompleted event.
#[cucumber::when(expr = "the app submits a ToolExecutionCompleted event")]
async fn when_submit_tool_execution_completed(world: &mut AppWorld) {
    let session_id = world.active_session_id();
    world.submit_event(Event::ToolExecutionCompleted(
        nullslop_domain::ToolExecutionCompleted {
            session_id,
            result: ToolResult {
                tool_call_id: "call_1".to_owned(),
                name: "test_tool".to_owned(),
                content: "ok".to_owned(),
                success: true,
                full_content: None,
                truncation: None,
            },
        },
    ));
    // Give the actor system time to process.
    tokio::time::sleep(Duration::from_millis(50)).await;
}

/// Pins the last entry in the active session with the given position.
#[cucumber::when(expr = "the app submits a PinChatEntry for the last entry with position {word}")]
async fn when_submit_pin_entry(world: &mut AppWorld, position: String) {
    let session_id = world.active_session_id();
    let pin_pos = parse_pin_position(&position);
    let state = world.state();
    let history = state.active_session().history();
    let last_id = history.last().expect("history has entries").id.clone();
    drop(state);
    world.submit_command(nullslop_domain::Command::PinChatEntry(PinChatEntry {
        session_id,
        entry_id: last_id,
        position: pin_pos,
    }));
    // Give the actor system time to process.
    tokio::time::sleep(Duration::from_millis(50)).await;
}

/// Unpins the last entry in the active session.
#[cucumber::when(expr = "the app submits an UnpinChatEntry for the last entry")]
async fn when_submit_unpin_entry(world: &mut AppWorld) {
    let session_id = world.active_session_id();
    let state = world.state();
    let history = state.active_session().history();
    let last_id = history.last().expect("history has entries").id.clone();
    drop(state);
    world.submit_command(nullslop_domain::Command::UnpinChatEntry(UnpinChatEntry {
        session_id,
        entry_id: last_id,
    }));
    // Give the actor system time to process.
    tokio::time::sleep(Duration::from_millis(50)).await;
}

/// Shared implementation for running a headless script.
fn run_headless_script(world: &mut AppWorld, content: &str) {
    let leader = nullslop_domain::KeyEvent {
        key: nullslop_domain::Key::Char('\\'),
        modifiers: nullslop_domain::Modifiers::none(),
    };
    let lines: Vec<Vec<nullslop_domain::KeyEvent>> = content
        .lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| ratatui_which_key::parse_key_sequence(line, &leader))
        .collect();

    for keys in lines {
        for key in keys {
            let state_read = world.app.core.state.read();
            let scope =
                nullslop_tui::app::scope_for_focus(state_read.frontend.scope_stack.current());
            drop(state_read);
            world.app.which_key.set_scope(scope);
            if let Some(intent) = world.app.which_key.handle_key(key) {
                world.route_intent(intent);
            }
        }
    }
}

// --- Then steps ---

/// Asserts the application's current mode matches the expected value.
#[cucumber::then(expr = "the mode should be {word}")]
fn then_mode_should_be(world: &mut AppWorld, mode: String) {
    let expected = parse_mode(&mode);
    let actual = world
        .app
        .core
        .state
        .read()
        .frontend
        .scope_stack
        .current()
        .mode();
    assert_eq!(
        actual, expected,
        "expected mode {expected:?}, got {actual:?}"
    );
}

/// Asserts the application has requested to quit.
#[cucumber::then(expr = "the app should quit")]
fn then_app_should_quit(world: &mut AppWorld) {
    let should_quit = world.app.core.state.read().frontend.should_quit;
    assert!(
        should_quit,
        "expected app to quit, but should_quit is false"
    );
}

/// Asserts the application has NOT requested to quit.
#[cucumber::then(expr = "the app should not quit")]
fn then_app_should_not_quit(world: &mut AppWorld) {
    let should_quit = world.app.core.state.read().frontend.should_quit;
    assert!(
        !should_quit,
        "expected app to not quit, but should_quit is true"
    );
}

/// Asserts the active chat input buffer is empty.
#[cucumber::then(expr = "the input buffer should be empty")]
fn then_input_buffer_empty(world: &mut AppWorld) {
    let text = world
        .app
        .core
        .state
        .read()
        .active_chat_input()
        .text()
        .to_owned();
    assert!(
        text.is_empty(),
        "expected empty input buffer, got: {text:?}"
    );
}

/// Asserts the active chat input buffer matches the expected text.
#[cucumber::then(expr = "the input buffer should be {string}")]
fn then_input_buffer_should_be(world: &mut AppWorld, expected: String) {
    let actual = world
        .app
        .core
        .state
        .read()
        .active_chat_input()
        .text()
        .to_owned();
    let expected = expected.replace("\\n", "\n").replace("\\t", "\t");
    assert_eq!(actual, expected, "input buffer mismatch");
}

/// Asserts the active session's chat history contains the expected number of entries.
/// Waits up to 5 seconds for the count to match.
#[cucumber::then(expr = "the chat history should contain {int} entry")]
async fn then_chat_history_count(world: &mut AppWorld, count: u64) {
    let expected = count as usize;
    world
        .wait_until(|state| state.active_session().history().len() >= expected)
        .await;
    let actual = world.state().active_session().history().len();
    assert_eq!(
        actual, expected,
        "expected {count} history entries, got {actual}"
    );
}

/// Asserts the active session's chat history contains at least the expected number of entries.
/// Waits up to 5 seconds for the count to match.
#[cucumber::then(expr = "the chat history should contain at least {int} entry")]
async fn then_chat_history_at_least_count(world: &mut AppWorld, count: u64) {
    let expected = count as usize;
    world
        .wait_until(|state| state.active_session().history().len() >= expected)
        .await;
    let actual = world.state().active_session().history().len();
    assert!(
        actual >= expected,
        "expected at least {count} history entries, got {actual}"
    );
}

/// Asserts the last chat entry's text contains the expected string.
#[cucumber::then(expr = "the last chat entry should contain {string}")]
async fn then_last_entry_contains_text(world: &mut AppWorld, expected: String) {
    world
        .wait_until(|state| {
            state
                .active_session()
                .history()
                .last()
                .is_some_and(|e| e.text().contains(&expected))
        })
        .await;
    let state = world.state();
    let last = state
        .active_session()
        .history()
        .last()
        .expect("at least one entry");
    let text = last.text();
    assert!(
        text.contains(&expected),
        "expected last entry to contain '{expected}', got '{text}'"
    );
}

/// Asserts the which-key popup is active.
#[cucumber::then(expr = "which-key should be active")]
fn then_which_key_active(world: &mut AppWorld) {
    assert!(
        world.app.which_key.active,
        "expected which-key to be active"
    );
}

/// Asserts the which-key popup is inactive.
#[cucumber::then(expr = "which-key should be inactive")]
fn then_which_key_inactive(world: &mut AppWorld) {
    assert!(
        !world.app.which_key.active,
        "expected which-key to be inactive"
    );
}

/// Asserts the active session's state matches the expected value.
#[cucumber::then(expr = "the session should be {word}")]
fn then_session_state(world: &mut AppWorld, state_name: String) {
    let expected: nullslop_domain::SessionPhase = state_name.parse().expect("valid session phase");
    let state = world.state();
    let session = state.active_session();
    assert_eq!(
        session.phase(),
        expected,
        "expected {expected:?}, got {:?}",
        session.phase()
    );
}

/// Asserts the active session's message queue has the expected count.
#[cucumber::then(expr = "the session queue should have {int} message(s)")]
fn then_session_queue_count(world: &mut AppWorld, count: u64) {
    let actual = world.state().active_session().queue_len();
    assert_eq!(
        actual, count as usize,
        "expected {count} queued messages, got {actual}"
    );
}

/// Asserts the active session's token ledger has the expected number of records.
#[cucumber::then(expr = "the token ledger should have {int} record(s)")]
fn then_token_ledger_count(world: &mut AppWorld, count: u64) {
    let actual = world.state().active_session().token_ledger().len();
    assert_eq!(
        actual, count as usize,
        "expected {count} token records, got {actual}"
    );
}

/// Asserts the last token record has nonzero tokens_received.
#[cucumber::then(expr = "the last token record should have nonzero tokens_received")]
fn then_last_token_record_nonzero_received(world: &mut AppWorld) {
    let state = world.state();
    let ledger = state.active_session().token_ledger();
    let last = ledger.last().expect("token ledger has records");
    assert!(
        last.tokens_received > 0,
        "expected nonzero tokens_received, got {}",
        last.tokens_received
    );
}

/// Asserts the last token record has zero tokens_received.
#[cucumber::then(expr = "the last token record should have zero tokens_received")]
fn then_last_token_record_zero_received(world: &mut AppWorld) {
    let state = world.state();
    let ledger = state.active_session().token_ledger();
    let last = ledger.last().expect("token ledger has records");
    assert_eq!(
        last.tokens_received, 0,
        "expected zero tokens_received, got {}",
        last.tokens_received
    );
}

/// Asserts the active session's context size is cached.
#[cucumber::then(expr = "the context size should be cached")]
fn then_context_size_cached(world: &mut AppWorld) {
    let context_size = world.state().active_session().context_size();
    assert!(context_size.is_some(), "expected context size to be cached");
}

/// Asserts the active session's context size is not cached.
#[cucumber::then(expr = "the context size should not be cached")]
fn then_context_size_not_cached(world: &mut AppWorld) {
    let context_size = world.state().active_session().context_size();
    assert!(
        context_size.is_none(),
        "expected context size to not be cached, got {:?}",
        context_size
    );
}

/// Asserts the fake LLM received the expected number of calls.
#[cucumber::then(expr = "the fake LLM should have received {int} call(s)")]
fn then_llm_call_count(world: &mut AppWorld, count: u64) {
    let calls = world.received_llm_calls();
    assert_eq!(
        calls.len(),
        count as usize,
        "expected {count} LLM calls, got {}",
        calls.len()
    );
}

/// Asserts the fake LLM call at the given index contains the expected number of messages.
#[cucumber::then(expr = "the fake LLM call {int} should contain {int} messages")]
fn then_llm_call_message_count(world: &mut AppWorld, call_index: u64, message_count: u64) {
    let calls = world.received_llm_calls();
    let call = calls.get(call_index as usize).unwrap_or_else(|| {
        panic!(
            "LLM call index {call_index} out of bounds ({} calls)",
            calls.len()
        )
    });
    assert_eq!(
        call.len(),
        message_count as usize,
        "expected {message_count} messages in LLM call {call_index}, got {}",
        call.len()
    );
}

/// Asserts the active session's prompt strategy matches the expected value.
#[cucumber::then(expr = "the session strategy should be {word}")]
fn then_session_strategy(world: &mut AppWorld, strategy: String) {
    let expected = PromptStrategyId::new(&strategy);
    let actual = world.state().active_session().active_strategy().clone();
    assert_eq!(
        actual, expected,
        "expected strategy {expected}, got {actual}"
    );
}

/// Asserts the tool definitions contain a tool with the given name.
#[cucumber::then(expr = "the tool definitions should contain {string}")]
fn then_tool_definitions_contain(world: &mut AppWorld, tool_name: String) {
    let state = world.state();
    let contains = state.context.tool_definitions.contains_key(&tool_name);
    assert!(
        contains,
        "expected tool definitions to contain '{tool_name}'"
    );
}

/// Asserts the tool definitions do not contain a tool with the given name.
#[cucumber::then(expr = "the tool definitions should not contain {string}")]
fn then_tool_definitions_not_contain(world: &mut AppWorld, tool_name: String) {
    let state = world.state();
    let contains = state.context.tool_definitions.contains_key(&tool_name);
    assert!(
        !contains,
        "expected tool definitions to not contain '{tool_name}'"
    );
}

/// Asserts the prompt template store contains a template with the given name.
#[cucumber::then(expr = "the prompt template store should contain {string}")]
fn then_prompt_template_store_contains(world: &mut AppWorld, name: String) {
    let state = world.state();
    let found = state
        .context
        .prompt_templates
        .templates()
        .iter()
        .any(|t| t.name == name);
    assert!(found, "expected prompt template store to contain '{name}'");
}

/// Asserts the active session's title matches the expected value.
#[cucumber::then(expr = "the session title should be {string}")]
fn then_session_title(world: &mut AppWorld, expected: String) {
    let title = world
        .state()
        .active_session()
        .title()
        .map(String::from)
        .unwrap_or_default();
    assert_eq!(title, expected, "session title mismatch");
}

/// Asserts the last history entry is of the given kind and has the expected text.
#[cucumber::then(expr = "the last history entry should be a {word} entry with text {string}")]
fn then_last_history_entry_kind_text(world: &mut AppWorld, kind: String, text: String) {
    let state = world.state();
    let history = state.active_session().history();
    let last = history.last().expect("expected at least one history entry");
    match kind.to_lowercase().as_str() {
        "user" => match &last.kind {
            ChatEntryKind::User { display, .. } => {
                assert_eq!(display, &text, "user entry text mismatch");
            }
            other => panic!("expected User entry, got {other:?}"),
        },
        "assistant" => match &last.kind {
            ChatEntryKind::Assistant(content) => {
                assert_eq!(content, &text, "assistant entry text mismatch");
            }
            other => panic!("expected Assistant entry, got {other:?}"),
        },
        "system" => match &last.kind {
            ChatEntryKind::System(content) => {
                assert_eq!(content, &text, "system entry text mismatch");
            }
            other => panic!("expected System entry, got {other:?}"),
        },
        "error" => match &last.kind {
            ChatEntryKind::Error(content) => {
                assert_eq!(content, &text, "error entry text mismatch");
            }
            other => panic!("expected Error entry, got {other:?}"),
        },
        _ => panic!("unknown entry kind: {kind}"),
    }
}

/// Asserts the history contains an error entry with text containing the given string.
#[cucumber::then(expr = "the history should contain an error entry with text {string}")]
fn then_history_contains_error(world: &mut AppWorld, text: String) {
    let state = world.state();
    let history = state.active_session().history();
    let found = history.iter().any(|e| match &e.kind {
        ChatEntryKind::Error(content) => content.contains(&text),
        _ => false,
    });
    assert!(
        found,
        "expected an error entry containing '{text}' in history"
    );
}

/// Asserts the prompt template store does not contain a template with the given name.
#[cucumber::then(expr = "the prompt template store should not contain {string}")]
fn then_prompt_template_store_not_contains(world: &mut AppWorld, name: String) {
    let state = world.state();
    let found = state
        .context
        .prompt_templates
        .templates()
        .iter()
        .any(|t| t.name == name);
    assert!(
        !found,
        "expected prompt template store to not contain '{name}'"
    );
}

/// Asserts the active session has pinned entries.
#[cucumber::then(expr = "the session has pinned entries")]
fn then_session_has_pinned_entries(world: &mut AppWorld) {
    let state = world.state();
    let pinned = state.active_session().pinned_entries();
    assert!(!pinned.is_empty(), "expected pinned entries");
}

/// Asserts the active session has no pinned entries.
#[cucumber::then(expr = "the session has no pinned entries")]
fn then_session_has_no_pinned_entries(world: &mut AppWorld) {
    let state = world.state();
    let pinned = state.active_session().pinned_entries();
    assert!(
        pinned.is_empty(),
        "expected no pinned entries, got {}",
        pinned.len()
    );
}

// --- Chat scroll step definitions ---

/// Asserts the cursor is on the last entry in the chat history.
/// Waits up to 5 seconds for the history to have at least one entry.
#[cucumber::then(expr = "the cursor should be on the last entry")]
async fn then_cursor_on_last_entry(world: &mut AppWorld) {
    world
        .wait_until(|state| {
            let history_len = state.active_session().history().len();
            history_len > 0
                && state.active_session().selected_entry_index() == Some(history_len - 1)
        })
        .await;
    let state = world.state();
    let history_len = state.active_session().history().len();
    let cursor = state.active_session().selected_entry_index();
    assert_eq!(
        cursor,
        Some(history_len - 1),
        "expected cursor on last entry ({})",
        history_len - 1
    );
}

/// Asserts the cursor is on a specific entry by index.
#[cucumber::then(expr = "the cursor should be on entry {int}")]
fn then_cursor_on_entry(world: &mut AppWorld, index: u64) {
    let state = world.state();
    let cursor = state.active_session().selected_entry_index();
    assert_eq!(
        cursor,
        Some(index as usize),
        "expected cursor on entry {index}, got {:?}",
        cursor
    );
}

/// Asserts the scroll is at the bottom (auto-scroll position).
#[cucumber::then(expr = "the scroll should be at the bottom")]
fn then_scroll_at_bottom(world: &mut AppWorld) {
    let state = world.state();
    assert!(
        state.active_session().is_at_bottom(),
        "expected scroll at bottom"
    );
}

// --- Prompt template expansion step definitions ---

/// Asserts the last User entry has the expected display and expanded text.
#[cucumber::then(expr = "the last user entry has display {string} and expanded {string}")]
fn then_last_user_entry_display_expanded(world: &mut AppWorld, display: String, expanded: String) {
    let state = world.state();
    let history = state.active_session().history();
    let user_entries: Vec<_> = history
        .iter()
        .rev()
        .filter(|e| matches!(&e.kind, ChatEntryKind::User { .. }))
        .collect();
    let last = user_entries
        .first()
        .expect("expected at least one user entry");
    match &last.kind {
        ChatEntryKind::User {
            display: actual_display,
            expanded: actual_expanded,
        } => {
            assert_eq!(actual_display, &display, "display text mismatch");
            assert_eq!(actual_expanded, &expanded, "expanded text mismatch");
        }
        _ => panic!("expected User entry, got {:?}", last.kind),
    }
}

// --- Session CWD step definitions ---

/// Asserts the active session's CWD is not empty.
#[cucumber::then(expr = "the session CWD should not be empty")]
fn then_session_cwd_not_empty(world: &mut AppWorld) {
    let state = world.state();
    let cwd = state.active_session().cwd();
    assert!(
        !cwd.as_os_str().is_empty(),
        "expected non-empty CWD, got: {:?}",
        cwd
    );
}

/// Saves the active session, captures its CWD, then triggers a reload.
#[cucumber::when(expr = "the session is saved and reloaded")]
async fn when_session_saved_and_reloaded(world: &mut AppWorld) {
    // Wait for any pending actor work to complete (e.g., session save after
    // message enqueue). The session actor saves asynchronously, so we wait
    // for the history to stabilize.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Capture CWD before reload.
    let (session_id, cwd_before) = {
        let state = world.state();
        let session = state.active_session();
        (session.session_id().clone(), session.cwd().to_owned())
    };

    // Trigger reload by sending SessionLoadRequested.
    world.submit_command(nullslop_domain::Command::SessionLoadRequested(
        nullslop_domain::SessionLoadRequested {
            session_id: session_id.clone(),
        },
    ));

    // Wait for the load to complete (is_loading transitions to false).
    world.wait_until(|state| !state.session.is_loading()).await;

    // Wait a bit more for the async cwd check to complete.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Store the pre-reload CWD for comparison.
    world.cwd_before_reload = Some(cwd_before);
}

/// Asserts the session CWD is the same as before the reload.
#[cucumber::then(expr = "the session CWD should be preserved")]
fn then_session_cwd_preserved(world: &mut AppWorld) {
    let state = world.state();
    let cwd_after = state.active_session().cwd();
    let cwd_before = world
        .cwd_before_reload
        .as_ref()
        .expect("no pre-reload CWD stored");
    assert_eq!(cwd_after, cwd_before, "CWD not preserved across reload");
}

/// Sets the active session's CWD to a non-existent path.
#[cucumber::given(expr = "the session CWD is set to a non-existent path")]
fn given_session_cwd_nonexistent(world: &mut AppWorld) {
    world
        .app
        .core
        .state
        .write()
        .active_session_mut()
        .set_cwd(std::path::PathBuf::from("/nonexistent/test/path/xyz"));
}

/// Asserts a warning about the missing CWD appears in chat history.
#[cucumber::then(expr = "a warning about the missing CWD should appear")]
async fn then_warning_about_missing_cwd(world: &mut AppWorld) {
    world
        .wait_until(|state| {
            state
                .active_session()
                .history()
                .iter()
                .any(|e| {
                    matches!(&e.kind, nullslop_domain::ChatEntryKind::System(t) if t.contains("Warning: working directory"))
                })
        })
        .await;
    let state = world.state();
    let found = state
        .active_session()
        .history()
        .iter()
        .any(|e| {
            matches!(&e.kind, nullslop_domain::ChatEntryKind::System(t) if t.contains("Warning: working directory"))
        });
    assert!(
        found,
        "expected a warning about missing CWD in chat history"
    );
}

/// Asserts the session CWD has fallen back to the global default.
#[cucumber::then(expr = "the session CWD should fall back to the global CWD")]
fn then_session_cwd_fallback(world: &mut AppWorld) {
    let state = world.state();
    let actual = state.active_session().cwd();
    let expected = state.session.default_cwd();
    assert_eq!(actual, expected, "expected CWD to fall back to global CWD");
}
