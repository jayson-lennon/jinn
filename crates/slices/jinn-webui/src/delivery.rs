//! Delivery — the composition sites for a web action.
//!
//! Validates the request against live session state and publishes the
//! **existing** schemas. The rules mirror the remote slice's delivery
//! exactly — one state write guard covers the phase check, the
//! interaction mark, and the entry build; the guard drops before the
//! publish awaits, so the bus is never reached while state is locked.
//!
//! - send → [`EnqueueUserMessage`] (a fully built `ChatEntry::user`;
//!   text delivered verbatim, no attribution, no transformation).
//! - cancel → [`CancelTurn`] (the event schema — two actors declare
//!   it, so one press does the whole cancel).
//! - model → [`ProviderSwitch`] with a `Single` selection (alloys are
//!   out of scope for the web).
//! - switch → [`SessionLoadRequested`] with `content_width: None` —
//!   the Discord precedent: the web renderer consumes no TUI layout
//!   measurement. The web UI and the TUI share one active session. The
//!   handler then waits (bounded) for the store actor to apply the
//!   activation, so the response page renders the *new* active mark.
//! - rename → [`PersistSession`] after the TUI rename confirm's three
//!   mutations (`set_title`, `touch`, `mark_interacted`) under one
//!   guard — the web adds only an entry point to the one rename
//!   semantic the TUI path already defines.

use jinn_chat_input_msg::EnqueueUserMessage;
use jinn_core_types::ModelSelection;
use jinn_core_types::SessionId;
use jinn_inference_msg::CancelTurn;
use jinn_kernel::State;
use jinn_preferences_config::protocol::app_state_command::AppStateUpdate;
use jinn_preferences_config::protocol::app_state_command::UpdateAppState;
use jinn_provider_selection_msg::ProviderSwitch;
use jinn_provider_selection_msg::RefreshModels;
use jinn_session_msg::MarkSessionInteracted;
use jinn_session_store_msg::PersistSession;
use jinn_session_store_msg::SessionLoadRequested;
use jinn_slices::bus::BusService;
use wherror::Error;

/// Why a web action was refused.
#[derive(Debug, Error)]
#[error(debug)]
pub enum ActionError {
    /// A form field naming a session id was not a UUID.
    BadSessionId,
    /// No live session carries the requested id.
    UnknownSession(SessionId),
    /// The target session is a bot run; external delivery does not
    /// fold text into bot templates.
    BotTarget(SessionId),
    /// The request carried no message text.
    EmptyText,
    /// The request carried a title that is empty after trimming.
    EmptyTitle,
    /// The requested model id names no available picker row (unknown,
    /// unknown provider, or an unavailable provider).
    UnknownModel(String),
    /// The requested persona name names no row the persona modal
    /// renders (the rendered rows are the accepted set, the
    /// model-switch precedent).
    UnknownPersona(String),
    /// The requested server name names no configured `[mcp.<name>]`
    /// server (the rendered rows are the accepted set, the same
    /// precedent).
    UnknownMcpServer(String),
    /// The requested skill name names no skill in the session's
    /// discovered set (the rendered rows are the accepted set, the
    /// same precedent).
    UnknownSkill(String),
    /// The request named no configured `[[project.entry]]` (or the
    /// request carried none at all).
    UnknownProject(String),
    /// A configured project path is neither `~`-prefixed nor absolute
    /// — config corruption the form cannot act on.
    BadProjectPath(String),
    /// The lifecycle value is neither blank nor a configured
    /// `[[session_lifecycle.script]]` name.
    UnknownLifecycle(String),
    /// The args text parsed to fewer values than the lifecycle's
    /// setup template declares.
    MissingArgs {
        /// The lifecycle whose minimum was missed.
        lifecycle: String,
        /// How many args the request carried.
        have: usize,
        /// How many the setup template requires.
        want: usize,
    },
    /// The creation function returned without a `SessionCreated`
    /// closure — an invariant of the shared path; refusing (not
    /// half-answering) is the safe answer.
    CreationMissingEvent,
}

impl ActionError {
    /// The HTTP status this refusal maps to.
    #[must_use]
    pub const fn status(&self) -> u16 {
        match self {
            // Malformed request bodies are client errors: 400.
            Self::BadSessionId
            | Self::EmptyText
            | Self::EmptyTitle
            | Self::BotTarget(_)
            | Self::UnknownModel(_)
            | Self::UnknownPersona(_)
            | Self::UnknownMcpServer(_)
            | Self::UnknownSkill(_)
            | Self::UnknownProject(_)
            | Self::BadProjectPath(_)
            | Self::UnknownLifecycle(_)
            | Self::MissingArgs { .. } => 400,
            // A well-formed request naming a session that does not
            // exist: 404.
            Self::UnknownSession(_) => 404,
            // The shared creation path always names its created
            // session; a miss is a server-side invariant break.
            Self::CreationMissingEvent => 500,
        }
    }

    /// The error sentence the response body carries.
    #[must_use]
    pub fn sentence(&self) -> String {
        match self {
            Self::BadSessionId => "session_id is not a valid session id".to_owned(),
            Self::EmptyText => "text must not be empty".to_owned(),
            Self::EmptyTitle => "title must not be empty".to_owned(),
            Self::BotTarget(id) => {
                format!("session {id} is a bot run; external delivery does not target bot sessions")
            }
            Self::UnknownSession(id) => format!("no live session with id {id}"),
            Self::UnknownModel(id) => format!("model {id} is not in the available model rows"),
            Self::UnknownPersona(name) => {
                format!("persona {name:?} is not in the rendered persona rows")
            }
            Self::UnknownMcpServer(name) => {
                format!("server {name:?} is not a configured [mcp.<name>] server")
            }
            Self::UnknownSkill(name) => {
                format!("skill {name:?} is not in the session's discovered skills")
            }
            Self::UnknownProject(project) => {
                format!("project {project:?} is not a configured [[project.entry]]")
            }
            Self::BadProjectPath(path) => {
                format!("configured project path {path:?} is neither ~-prefixed nor absolute")
            }
            Self::UnknownLifecycle(name) => {
                format!("lifecycle {name:?} is not a configured [[session_lifecycle.script]]")
            }
            Self::MissingArgs {
                lifecycle,
                have,
                want,
            } => format!("lifecycle {lifecycle:?} takes {want} argument(s), got {have}"),
            Self::CreationMissingEvent => {
                "session creation completed without naming the created session".to_owned()
            }
        }
    }
}

/// Handles one `POST /web/send`.
///
/// The validation and the entry build happen under one state write
/// guard; the guard drops before the publish awaits. The entry is the
/// same construction the keyboard path produces: `ChatEntry::user`
/// with the text verbatim as both display and expanded.
///
/// # Errors
///
/// Returns [`ActionError`] for every refusal; publication happens only
/// on `Ok`.
pub async fn handle_send(
    state: &State,
    bus: &BusService,
    session_id: &str,
    text: &str,
) -> Result<(), ActionError> {
    let message = {
        let session_id = SessionId::try_from_string(session_id).ok_or(ActionError::BadSessionId)?;
        if text.is_empty() {
            return Err(ActionError::EmptyText);
        }

        let mut guard = state.write();
        let Some(session) = guard.session.get_mut(&session_id) else {
            return Err(ActionError::UnknownSession(session_id));
        };

        if session.is_bot() {
            return Err(ActionError::BotTarget(session_id));
        }

        // A delivered message marks the target interacted — without
        // the mark `is_persistable()` gates every save and the turn
        // would turn but never reach disk (the tell_session
        // invariant).
        session.mark_interacted();

        EnqueueUserMessage {
            session_id,
            entry: jinn_core_types::ChatEntry::user(text.to_owned()),
        }
    };
    bus.publish(message).await;
    Ok(())
}

/// Handles one `POST /web/cancel`.
///
/// `CancelTurn` is an Event (two actors declare it — the inference
/// actor stops the stream, the session actor settles the phase), so
/// this publishes the fact and both halves of the cancel happen on the
/// same press. No live-session check: a cancel racing a load lands on
/// the session id, which is the safer target than dropping the press.
///
/// # Errors
///
/// Returns [`ActionError::BadSessionId`] for a malformed id; the
/// publish always happens on `Ok`.
pub async fn handle_cancel(bus: &BusService, session_id: &str) -> Result<(), ActionError> {
    let session_id = SessionId::try_from_string(session_id).ok_or(ActionError::BadSessionId)?;
    bus.publish(CancelTurn {
        session_id,
        // A user-confirmed cancel drains the buffered work into the
        // input box as editable draft text — the ESC-ESC semantics the
        // keyboard path applies, and the only publisher that sets it.
        drain_buffers_to_input: true,
    })
    .await;
    Ok(())
}

/// Handles one `POST /web/model`.
///
/// The model id must name an **available row the picker renders** —
/// the same shared builder (`load_provider_entries` over the registry,
/// api keys, and the provider cell's cache) that produces the modal's
/// row list. Validating against the raw cache instead would (a)
/// compare bare cache ids against the rows' full provider ids —
/// `gpt-4o` vs `openai/gpt-4o` — refusing every rendered row, and (b)
/// lock the picker on a fresh boot where discovery has not listed
/// anything yet. The rendered rows are the single source of truth: the
/// switch publishes `ProviderSwitch::Single` and the provider actor
/// validates its side again.
///
/// All reads happen under short-lived guards inside one block; every
/// guard is dropped before the publish awaits, per the guard
/// discipline.
///
/// # Errors
///
/// Returns [`ActionError`] for a malformed id, an unknown session, or
/// a model id outside the available rows.
pub async fn handle_model(
    state: &State,
    provider_cell: &jinn_slices::TypedCell<jinn_provider_selection_msg::ProviderCell>,
    provider_registry: &jinn_provider_config::ProviderRegistryService,
    api_keys: &jinn_provider_config::ApiKeysService,
    bus: &BusService,
    session_id: &str,
    model: &str,
) -> Result<(), ActionError> {
    let session_id = SessionId::try_from_string(session_id).ok_or(ActionError::BadSessionId)?;

    // The availability check: the picker's rendered rows are the
    // offered set, so they are the accepted set too. Rows carry full
    // provider ids (`openai/gpt-4o`), which is the shape the switch
    // publishes.
    let known = {
        let theme = crate::regions::model_picker::picker_theme(state);
        let registry = provider_registry.read();
        let api_keys = api_keys.read();
        let cache = provider_cell.read().model_cache.clone();
        crate::regions::model_picker::available_model_ids(
            &registry,
            &api_keys,
            cache.as_ref(),
            &theme,
        )
        .iter()
        .any(|id| id == model)
    };
    if !known {
        return Err(ActionError::UnknownModel(model.to_owned()));
    }

    {
        let guard = state.read();
        if !guard.session.contains(&session_id) {
            return Err(ActionError::UnknownSession(session_id));
        }
    }

    // Apply the switch to the session state synchronously, BEFORE the
    // publish: the POST's response fragment re-renders the titlebar from
    // session state, and the bus is async — a response rendered between
    // the publish and the actor's application of the command would name
    // the PREVIOUS model (the stale-label bug: the button caught up only
    // when a later interaction re-read state). The write is the same one
    // the command's declarant applies (`ProviderActor`'s handler is this
    // exact `set_model`), so the command's arrival re-applies it
    // idempotently and no other reader can tell the difference — the
    // command remains the writer of record for every OTHER subscriber,
    // and this handler's own response is rendered from state that
    // already holds the pick.
    state.with_session(|view| {
        view.session
            .map()
            .get_or_create(&session_id)
            .set_model(ModelSelection::Single(model.to_owned()));
    });

    bus.publish(ProviderSwitch {
        session_id,
        provider_id: ModelSelection::Single(model.to_owned()),
    })
    .await;
    Ok(())
}

/// Handles one `POST /web/persona` — the persona modal's row click.
///
/// The web mirror of the TUI persona picker's confirm: validates the
/// name against the rows the modal renders (the personas cell is the
/// offered set, so it is the accepted set — the model-switch
/// precedent), switches the viewed session's persona **synchronously
/// before** the publish (the POST's response fragment re-renders from
/// session state, and the bus is async — the stale-fragment pattern
/// `handle_model` documents), then publishes the same message set the
/// TUI confirm publishes: [`UpdateAppState`] with
/// `AppStateUpdate::SetPersona` (the startup-session seed) and
/// [`MarkSessionInteracted`].
///
/// Returns whether the chosen persona carries no `<compaction>`
/// handoff block — the caller renders the transcript fragment into
/// the response when it does, so the loud error entry (the same text
/// the TUI pushes, from `jinn_persona_msg`) is visible without a
/// reload. The fan-out handles no chat-entry event, so the POST
/// response is the only correction path.
///
/// # Errors
///
/// Returns [`ActionError`] for a malformed id, an unknown session, or
/// a persona name outside the rendered rows.
pub async fn handle_persona(
    state: &State,
    personas_cell: &jinn_slices::TypedCell<jinn_persona_msg::Personas>,
    bus: &BusService,
    session_id: &str,
    persona: &str,
) -> Result<bool, ActionError> {
    let session_id = SessionId::try_from_string(session_id).ok_or(ActionError::BadSessionId)?;

    // The offered set is the accepted set: the modal's rows are built
    // from this cell, so a name outside it was never rendered.
    let chosen = personas_cell
        .read()
        .entries
        .iter()
        .find(|entry| entry.name == persona)
        .cloned()
        .ok_or_else(|| ActionError::UnknownPersona(persona.to_owned()))?;

    {
        let guard = state.read();
        if !guard.session.contains(&session_id) {
            return Err(ActionError::UnknownSession(session_id));
        }
    }

    // Apply the switch to the session state synchronously, BEFORE the
    // publish — the same ordering discipline `handle_model` documents.
    let blockless = chosen.handoff_prompt.is_empty();
    state.with_session(|view| {
        view.session
            .map()
            .get_or_create(&session_id)
            .set_persona_name(chosen.name.clone());
    });

    if blockless {
        // The block-less persona is a loud error, not a silent
        // downgrade: the entry names the persona and states the
        // fallback — byte-identical to the TUI's push (the load-path
        // dedupe compares this exact text).
        state.with_session(|view| {
            view.session.map().get_or_create(&session_id).push_entry(
                jinn_core_types::ChatEntry::error(
                    jinn_persona_msg::missing_compaction_block_message(&chosen.name),
                ),
            );
        });
    }

    bus.publish(UpdateAppState {
        updates: vec![AppStateUpdate::SetPersona(Some(chosen.name.clone()))],
    })
    .await;
    bus.publish(MarkSessionInteracted { session_id }).await;
    Ok(blockless)
}

/// Handles one `POST /web/mcp` — the MCP modal's row click.
///
/// The web mirror of the TUI inspector's toggle: validates the
/// session id and the server name against the `[mcp.<name>]` config
/// section, XORs the server into/out of the session's enabled set
/// (the desired state is derived server-side from the set the server
/// actually holds — the wire never carries a desired-state field),
/// writes the session state synchronously BEFORE the publish (the
/// `handle_model` ordering: the POST's response fragment renders from
/// state that already holds the toggled set), and publishes the
/// existing [`jinn_mcp_msg::McpEnablementChanged`] carrying the **full
/// desired set** — its documented contract, never a delta. No
/// `PersistSession` rides along: the web adds no persistence behavior
/// beyond what the TUI confirm performs, and the TUI toggle performs
/// none.
///
/// # Errors
///
/// Returns [`ActionError`] when the session id is malformed, no live
/// session carries it, or the server name names nothing in the
/// `[mcp.<name>]` section (the rendered rows are the accepted set).
pub async fn handle_mcp_toggle(
    state: &State,
    config: &jinn_slices::ConfigLayer,
    bus: &BusService,
    session_id: &str,
    server: &str,
) -> Result<(), ActionError> {
    let session_id = SessionId::try_from_string(session_id).ok_or(ActionError::BadSessionId)?;

    // The configured set is the accepted set: the modal's rows are
    // built from this section, so a name outside it was never
    // rendered. (The config snapshot may lag a jinn.toml reload — the
    // accepted staleness the session-new form shares.)
    let configured = config
        .get::<jinn_preferences_config::schemas::mcp::McpServersConfig>()
        .is_ok_and(|servers| servers.0.contains_key(server));
    if !configured {
        return Err(ActionError::UnknownMcpServer(server.to_owned()));
    }

    {
        let guard = state.read();
        if !guard.session.contains(&session_id) {
            return Err(ActionError::UnknownSession(session_id));
        }
    }

    // Derive the full desired set and write it synchronously, BEFORE
    // the publish — the stale-fragment rule. The cell handle rides the
    // signature so the webui's cell wiring stays honest (the modal
    // renders from it); the toggle itself rides session state, the
    // enablement surface's one writer of record.
    let desired = state.with_session(|view| {
        let session = view.session.map().get_or_create(&session_id);
        let mut enabled = session.enabled_mcp_servers().clone();
        if enabled.remove(server) {
            session.set_enabled_mcp_servers(enabled.clone());
        } else {
            enabled.insert(server.to_owned());
            session.set_enabled_mcp_servers(enabled.clone());
        }
        enabled
    });

    bus.publish(jinn_mcp_msg::McpEnablementChanged {
        session_id,
        enabled: desired,
    })
    .await;
    Ok(())
}

/// Handles one `POST /web/models/refresh` — the modal's refresh button.
///
/// Publishes the existing [`RefreshModels`] command (one declarant:
/// `DiscoverActor`, the TUI's own discovery pipeline; the web adds no
/// second handling of the schema). It validates nothing
/// session-specific, blocks on nothing: discovery is async, and the
/// rows arrive when the provider cache lands and the fan-out pushes
/// the `models` event. No transcript note is pushed from the web path
/// — the summary entry the provider actor's own handler emits is
/// shared-pipeline behavior and applies to this trigger too.
///
/// # Errors
///
/// Returns [`ActionError`] when the session id is malformed.
pub async fn handle_refresh_models(bus: &BusService, session_id: &str) -> Result<(), ActionError> {
    // The id is validated (a malformed id is a 400) but not otherwise
    // used: `RefreshModels` is a unit command — `DiscoverActor` stamps
    // the active session when it emits the refresh event, and the
    // `models` push is addressed per live session by the fan-out.
    SessionId::try_from_string(session_id).ok_or(ActionError::BadSessionId)?;
    bus.publish(RefreshModels).await;
    Ok(())
}

/// Handles one `POST /web/session-new` — create a session through the
/// shared creation path.
///
/// Validates against the config layer (project must name a configured
/// `[[project.entry]]`; lifecycle must be blank or a configured
/// `[[session_lifecycle.script]]`; args must meet the lifecycle's
/// parameter minimum), then — under ONE state write guard — stashes
/// the `PendingSessionCreation` (the Discord precedent: the stash and
/// the call are one synchronous unit, so the web's stash cannot race
/// an abandoned TUI picker's) and calls
/// `jinn_kernel::session_lifecycle::intent::handle_session_lifecycle_setup`,
/// the exact function the TUI pickers and Discord `/new` end in. The
/// returned publish closures ride [`jinn_slices::bus::Bridge`], the
/// same drain the TUI's IntentHandler uses — no new schema, no
/// typed publish.
///
/// The session is created active instance-wide; the POST's response
/// is the new session's whole page (server-side; the client merely
/// swaps it).
///
/// # Errors
///
/// Returns [`ActionError`] for every refusal: no configured projects,
/// an unknown project, an unknown lifecycle, or args short of the
/// lifecycle's parameter minimum — all 400s. Publication happens only
/// on `Ok`, after the guard drops. Synchronous: the bridge drain is a
/// channel send, and nothing here awaits.
pub fn handle_create(
    state: &State,
    bus: &BusService,
    config: &jinn_slices::ConfigLayer,
    form: &jinn_webui_msg::CreateSessionForm,
) -> Result<jinn_core_types::SessionId, ActionError> {
    use jinn_kernel::Bridge;
    use jinn_kernel::session_lifecycle::intent::handle_session_lifecycle_setup;
    use jinn_preferences_config::schemas::{LifecycleCommand, ProjectConfig, SessionLifecycle};

    let args_text = form.args.as_deref().unwrap_or_default();
    let arg_values = jinn_session_lifecycle_msg::command_template::parse_quoted_args(args_text);

    let project_dir = {
        // The project list and the lifecycle list are read before the
        // guard; both are config data, not session state.
        let projects: Vec<ProjectConfig> = config.get_list().unwrap_or_default();
        let chosen = projects
            .iter()
            .find(|entry| entry.path.to_string_lossy() == form.project)
            .ok_or_else(|| ActionError::UnknownProject(form.project.clone()))?;

        // ~ expansion — the Discord handler's exact form (no `dirs`
        // edge; the home resolution rides the chat-input msg crate's
        // helper).
        let expanded = match chosen.path.strip_prefix("~") {
            Ok(rest) => jinn_chat_input_msg::home_dir()
                .join(rest)
                .map_err(|_path_error| {
                    ActionError::BadProjectPath(chosen.path.to_string_lossy().into_owned())
                })?,
            Err(_) => marked_path::MarkedPathBuf::<marked_path::Absolute>::new(chosen.path.clone())
                .map_err(|_path_error| {
                    ActionError::BadProjectPath(chosen.path.to_string_lossy().into_owned())
                })?,
        };

        // The lifecycle must be blank or exactly a configured name —
        // `find_lifecycle` silently falls back to blank for an
        // unknown name, so the POST validates it here or a bogus name
        // would stamp a session that runs no script.
        let lifecycles: Vec<SessionLifecycle> = config.get_list().unwrap_or_default();
        if !form.lifecycle.is_empty() && !lifecycles.iter().any(|l| l.name == form.lifecycle) {
            return Err(ActionError::UnknownLifecycle(form.lifecycle.clone()));
        }

        // The arg minimum is the setup template's parameter count —
        // the TUI arg popup's rule. `$@` splat templates add none.
        let param_count = lifecycles
            .iter()
            .find(|l| l.name == form.lifecycle)
            .and_then(|l| l.setup.as_ref())
            .and_then(|setup| match setup {
                LifecycleCommand::Shell(raw) => Some(
                    jinn_session_lifecycle_msg::command_template::CommandTemplate::parse(raw)
                        .param_count(),
                ),
                LifecycleCommand::Builtin(_) => None,
            })
            .unwrap_or(0);
        if arg_values.len() < param_count {
            return Err(ActionError::MissingArgs {
                lifecycle: form.lifecycle.clone(),
                have: arg_values.len(),
                want: param_count,
            });
        }

        expanded
    };

    // One synchronous unit: stash the project, run the shared
    // creation, collect the closures — the guard drops when the block
    // ends, before any await (guard discipline, and the reason the
    // stash cannot go stale).
    let closures = {
        let mut guard = state.write();
        guard.frontend.pending_creation =
            Some(jinn_kernel::state::frontend_state::PendingSessionCreation {
                project_dir: project_dir.clone(),
                starting_cwd: project_dir,
            });
        let result =
            handle_session_lifecycle_setup(&mut guard, &form.lifecycle, &arg_values, None, config);
        result.messages
    };

    // The created session is the one the shared path made active
    // instance-wide — the same id the closures' `SessionCreated`
    // carries, read from state (the POST's response page is rendered
    // from this same active session).
    let created = state.read().session.active_session_id().clone();

    let bridge = Bridge::new(bus.clone());
    for closure in closures {
        // A closed bridge is the process shutting down; the TUI's own
        // drain ignores the error the same way.
        let _ = bridge.send(closure);
    }

    Ok(created)
}

/// Handles one session activation (`POST /web/session/{id}/activate`).
///
/// Publishes [`SessionLoadRequested`] — the same command the TUI's
/// picker publishes, so the store actor's tested path hydrates an
/// archived session or activates a loaded one, and the TUI's view
/// moves with the web's. `content_width` is `None`: the web renderer
/// consumes no TUI layout measurement, and the store actor derives a
/// width from its own layout.
///
/// The publish only enqueues — the store actor is the single writer of
/// the active pointer and applies it on its own turn. The response page
/// renders its sidebar from the active pointer, so this handler waits
/// (bounded) for the activation to land before returning: without the
/// wait the swapped-in page names the *previous* active session, and
/// the correcting `sessions` SSE push races the client's swap and is
/// lost — the visible symptom was a highlight one click behind.
/// Unknown ids never converge (nothing will insert them), so the wait
/// is skipped for them and the render falls through to the store.
///
/// # Errors
///
/// Returns [`ActionError::BadSessionId`] for a malformed id.
pub async fn handle_switch(
    state: &State,
    bus: &BusService,
    session_id: &str,
) -> Result<(), ActionError> {
    let session_id = SessionId::try_from_string(session_id).ok_or(ActionError::BadSessionId)?;
    let known_live = state.read().session.contains(&session_id);
    bus.publish(SessionLoadRequested {
        session_id: session_id.clone(),
        content_width: None,
    })
    .await;
    if known_live {
        wait_for_activation(state, &session_id).await;
    }
    Ok(())
}

/// How long [`handle_switch`] waits for the store actor to apply an
/// activation before rendering the response anyway.
///
/// The happy path is the store actor's dispatch latency — microseconds.
/// The budget only ever binds on a wedged actor or a session the actor
/// refuses to activate; both render today's behavior (the page for the
/// requested session with the stale active mark) instead of hanging.
const ACTIVATION_CONVERGENCE_BUDGET: std::time::Duration = std::time::Duration::from_millis(2_000);

/// Poll interval for [`wait_for_activation`] — short enough that the
/// happy path lands within a tick or two, coarse enough not to spin.
const ACTIVATION_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(5);

/// Waits until the session is the active one, or the budget expires.
///
/// The guard discipline: take the read guard, check, drop it, then
/// sleep — never a guard held across the `.await`.
async fn wait_for_activation(state: &State, session_id: &SessionId) {
    let deadline = tokio::time::Instant::now() + ACTIVATION_CONVERGENCE_BUDGET;
    loop {
        if state.read().session.active_session_id() == session_id {
            return;
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return;
        }
        tokio::time::sleep((deadline - now).min(ACTIVATION_POLL_INTERVAL)).await;
    }
}

/// Handles one `POST /web/session/{id}/rename`.
///
/// Applies exactly the TUI rename confirm's three mutations —
/// `set_title` on the trimmed text, `touch`, `mark_interacted` —
/// under one state write guard, then publishes the existing
/// [`PersistSession`] command, whose tested store path persists the
/// renamed snapshot. Bot sessions rename like any other: the bot
/// refusal exists only on the text-delivery path, and the TUI rename
/// path renames bot sessions. No new schema: the fan-out's
/// subscriptions are untouched.
///
/// # Errors
///
/// Returns [`ActionError`] for a malformed id, an empty-after-trim
/// title, or an unknown session; nothing is published on any refusal.
pub async fn handle_rename(
    state: &State,
    bus: &BusService,
    session_id: &str,
    title: &str,
) -> Result<(), ActionError> {
    let command = {
        let session_id = SessionId::try_from_string(session_id).ok_or(ActionError::BadSessionId)?;
        let title = title.trim();
        if title.is_empty() {
            return Err(ActionError::EmptyTitle);
        }

        let mut guard = state.write();
        let Some(session) = guard.session.get_mut(&session_id) else {
            return Err(ActionError::UnknownSession(session_id));
        };

        // The TUI rename confirm's exact mutation set — nothing more.
        session.set_title(title.to_owned());
        session.touch();
        session.mark_interacted();

        PersistSession { session_id }
    };
    bus.publish(command).await;
    Ok(())
}

/// Handles one `POST /web/skills/toggle` — the skills modal's row
/// click.
///
/// The web mirror of the TUI picker's toggle, resolved through the
/// filter the session actually holds: the name is validated against
/// the session's discovered set (the modal's rows are built from it —
/// the rendered-rows-are-the-accepted-set precedent), then
/// `NameFilter::permit` / `withhold` is applied to the current
/// filter, preserving its mode. An absent filter materializes as the
/// inherited empty deny (the TUI `enable_durably` materialization),
/// so the first toggle on a fresh session writes a present filter
/// carrying exactly that one decision. Publishes nothing: skill
/// enablement has no runtime counterpart — the TUI confirm publishes
/// nothing, and the skill gate reads the session filter live. No
/// `PersistSession` rides along: the web adds no persistence beyond
/// the TUI toggle, which performs none.
///
/// A skill already loaded into context toggles nothing: its body is
/// pinned in history until it is unpinned and pruned, so a withheld
/// marker would promise an unload that never happens — the TUI's
/// loaded-skill toggle no-op, verbatim, and silent like the TUI's.
///
/// # Errors
///
/// Returns [`ActionError`] when the session id is malformed, no live
/// session carries it, or the skill name names nothing in the
/// session's discovered set (the rendered rows are the accepted set).
pub fn handle_skill_toggle(
    state: &State,
    session_id: &str,
    skill: &str,
) -> Result<(), ActionError> {
    let session_id = SessionId::try_from_string(session_id).ok_or(ActionError::BadSessionId)?;

    {
        let guard = state.read();
        let Some(session) = guard.session.get(&session_id) else {
            return Err(ActionError::UnknownSession(session_id));
        };
        if !session
            .discovered_skills()
            .iter()
            .any(|candidate| candidate.name == skill)
        {
            return Err(ActionError::UnknownSkill(skill.to_owned()));
        }
        if session.loaded_skills().contains(skill) {
            return Ok(());
        }
    }

    // Mode-preserving toggle arithmetic on the live filter, written
    // synchronously (the stale-fragment rule: the POST's response
    // fragment re-renders from state that already holds the flip).
    state.with_session(|view| {
        let session = view.session.map().get_or_create(&session_id);
        let enabled = session.is_skill_enabled(skill);
        let mut filter = session
            .skill_filter()
            .cloned()
            .unwrap_or_else(jinn_core_types::NameFilter::inherited);
        if enabled {
            filter.withhold(skill);
        } else {
            filter.permit(skill);
        }
        session.set_skill_filter(Some(filter));
    });

    // TUI parity: the toggle publishes nothing — the sync write is the
    // whole behavior, so this handler is synchronous (no bridge, no
    // await).
    Ok(())
}

/// Handles one `POST /web/skills/load` — the skills modal's
/// force-load button.
///
/// The web mirror of the TUI picker's `<c-l>` load, verbatim: the
/// pinned `skill` tool-call/result pair appends to the session's
/// history (the body wrapped `<skill name="…" location="…">…</skill>`,
/// the tool result pinned relative), a disabled skill is enabled
/// durably through the filter first (the TUI `enable_durably`
/// semantics — a session with no filter materializes one so the
/// permit can be carried), and [`MarkSessionInteracted`] is the only
/// publish. An already-loaded skill appends no pair; it appends the
/// same transient notice the TUI emits ("Skill 'x' is already
/// loaded") instead — one load semantic across surfaces.
///
/// The session-state write happens synchronously before the publish
/// (the stale-fragment rule): the POST's response re-renders modal and
/// transcript from state that already holds the pinned pair.
///
/// # Errors
///
/// Returns [`ActionError`] when the session id is malformed, no live
/// session carries it, or the skill name names nothing in the
/// session's discovered set.
pub async fn handle_skill_load(
    state: &State,
    bus: &BusService,
    session_id: &str,
    skill: &str,
) -> Result<(), ActionError> {
    use jinn_core_types::ChatEntry;
    use jinn_core_types::ChatEntryId;
    use jinn_core_types::PinPosition;
    use jinn_core_types::ToolResultStatus;

    let session_id = SessionId::try_from_string(session_id).ok_or(ActionError::BadSessionId)?;

    // Snapshot what the load needs under one short guard: the
    // discovered skill's body and path (resolved from the session's
    // set — what makes project-local skills loadable), its enablement,
    // and the loaded set. Dropped before any await (guard discipline).
    let (discovered, enabled, loaded) = {
        let guard = state.read();
        let Some(session) = guard.session.get(&session_id) else {
            return Err(ActionError::UnknownSession(session_id));
        };
        let found = session
            .discovered_skills()
            .iter()
            .find(|candidate| candidate.name == skill)
            .cloned()
            .ok_or_else(|| ActionError::UnknownSkill(skill.to_owned()))?;
        let enabled = session.is_skill_enabled(skill);
        let loaded = session.loaded_skills();
        (found, enabled, loaded)
    };

    // Already loaded: the transient notice instead of a duplicate pair
    // — the TUI's exact branch, byte-identical text.
    if loaded.contains(skill) {
        state.with_session(|view| {
            view.session
                .map()
                .get_or_create(&session_id)
                .push_entry(ChatEntry::transient(format!(
                    "Skill '{skill}' is already loaded"
                )));
        });
        return Ok(());
    }

    // Disabled: enable durably before pinning — the name leaves the
    // filter, so neither a later toggle round-trip nor anything reading
    // the filter can re-withhold a skill the user just loaded. An
    // absent filter materializes a present one: permitting a name is a
    // configuration change.
    if !enabled {
        state.with_session(|view| {
            let session = view.session.map().get_or_create(&session_id);
            let mut filter = session
                .skill_filter()
                .cloned()
                .unwrap_or_else(jinn_core_types::NameFilter::inherited);
            filter.permit(skill);
            session.set_skill_filter(Some(filter));
        });
    }

    // The TUI `<c-l>` pin, verbatim: tool call, then the wrapped body
    // as a pinned success tool result sharing the call's id.
    let xml = format!(
        "<skill name=\"{}\" location=\"{}\">\n{}\n</skill>",
        discovered.name, discovered.file_path, discovered.body
    );
    let arguments = serde_json::json!({ "name": discovered.name }).to_string();
    let tool_call_id = ChatEntryId::new().to_string();
    state.with_session(|view| {
        let session = view.session.map().get_or_create(&session_id);
        session.push_entry(ChatEntry::tool_call(
            tool_call_id.clone(),
            "skill",
            arguments,
        ));
        let mut result =
            ChatEntry::tool_result(tool_call_id, "skill", xml, ToolResultStatus::Success);
        result.pin_position = Some(PinPosition::Relative);
        session.push_entry(result);
    });

    bus.publish(MarkSessionInteracted { session_id }).await;
    Ok(())
}
