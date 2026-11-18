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
//!   measurement. The web UI and the TUI share one active session.
//! - rename → [`PersistSession`] after the TUI rename confirm's three
//!   mutations (`set_title`, `touch`, `mark_interacted`) under one
//!   guard — the web adds only an entry point to the one rename
//!   semantic the TUI path already defines.

use jinn_chat_input_msg::EnqueueUserMessage;
use jinn_core_types::ModelSelection;
use jinn_core_types::SessionId;
use jinn_inference_msg::CancelTurn;
use jinn_kernel::State;
use jinn_provider_selection_msg::ProviderSwitch;
use jinn_provider_selection_msg::RefreshModels;
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
            | Self::UnknownModel(_) => 400,
            // A well-formed request naming a session that does not
            // exist: 404.
            Self::UnknownSession(_) => 404,
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
    bus.publish(ProviderSwitch {
        session_id,
        provider_id: ModelSelection::Single(model.to_owned()),
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

/// Handles one session activation (`POST /web/session/{id}/activate`).
///
/// Publishes [`SessionLoadRequested`] — the same command the TUI's
/// picker publishes, so the store actor's tested path hydrates an
/// archived session or activates a loaded one, and the TUI's view
/// moves with the web's. `content_width` is `None`: the web renderer
/// consumes no TUI layout measurement, and the store actor derives a
/// width from its own layout.
///
/// # Errors
///
/// Returns [`ActionError::BadSessionId`] for a malformed id.
pub async fn handle_switch(bus: &BusService, session_id: &str) -> Result<(), ActionError> {
    let session_id = SessionId::try_from_string(session_id).ok_or(ActionError::BadSessionId)?;
    bus.publish(SessionLoadRequested {
        session_id,
        content_width: None,
    })
    .await;
    Ok(())
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
