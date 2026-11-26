//! What each `ClientMsg` does — the TUI's own behaviour, message for message.
//!
//! Every action here does what the terminal's intent handler does for the
//! same gesture: the same synchronous `State` writes, the same bus
//! commands, in the same order. There is no steering, so a send is always
//! an enqueue; everything else is parity. The parity source for each is
//! named on the handler.
//!
//! A handler publishes on the bus the outbound handle carries, so the
//! actor that performs the action is the one the TUI drives — never this
//! slice.

use std::time::Duration;

use jinn_chat_input_msg::EnqueueUserMessage;
use jinn_core_types::{ChatEntry, ChatEntryId, ModelSelection, NameFilter, PinPosition, SessionId};
use jinn_inference_msg::CancelTurn;
use jinn_kernel::common::actor_deps::BusPublish as _;
use jinn_mcp_msg::McpRowBadge;
use jinn_preferences_config::protocol::app_state_command::{AppStateUpdate, UpdateAppState};
use jinn_preferences_config::schemas::mcp::McpServersConfig;
use jinn_provider_selection_msg::{ProviderSwitch, RefreshModels};
use jinn_session_history_msg::{PinChatEntry, UnpinChatEntry};
use jinn_session_lifecycle_msg::command_template::CommandTemplate;
use jinn_session_msg::MarkSessionInteracted;
use jinn_session_store_msg::{PersistSession, SessionLoadRequested, SetSessionLibrary};
use jinn_webui2_msg::{
    EntryEnvelope, EntryUpsertBody, ErrorBody, ImageUploadBody, LifecycleOptionBody, McpBadgeBody,
    McpRowBody, McpRowsBody, ModelRowBody, ModelRowsBody, NewSessionOptionsBody, PersonaRowBody,
    PersonaRowsBody, PickerRowBody, PickerRowsBody, ServerMsg, SkillRowBody, SkillRowsBody,
    TextRunBody, ThemeBody, TitleBody, ToolRowBody, ToolRowsBody,
};
use marked_path::MarkedPathAccess as _;

use crate::{
    outbound::{OutboundHandle, RowsKind},
    protocol::Reply,
    snapshot,
};

/// The most picker rows one answer carries.
pub const PICKER_ROW_CAP: usize = 50;

/// Why a request could not be served, as the page reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The session is running a turn, so a send would silently steer.
    SessionBusy,
    /// The text was empty or whitespace.
    EmptyText,
    /// No session with that id is loaded.
    UnknownSession,
    /// There is no active session.
    NoActiveSession,
    /// The session store could not be read.
    StoreUnavailable,
    /// The active session has not discovered a skill by that name.
    UnknownSkill,
    /// The active session's registry offers no tool by that name.
    UnknownTool,
    /// No `[mcp.<name>]` entry carries that name.
    UnknownMcpServer,
    /// The persona catalog has no entry by that name.
    UnknownPersona,
    /// The active session holds no entry with that id.
    UnknownEntry,
    /// The theme is not one of the three modes.
    UnknownTheme(String),
    /// No live session and nothing in the store carries that id.
    UnknownLibraryTarget(SessionId),
    /// The request named no configured `[[project.entry]]`.
    UnknownProject(String),
    /// A configured project path is neither `~`-prefixed nor absolute.
    BadProjectPath(String),
    /// The lifecycle value is not blank and not a configured script name.
    UnknownLifecycle(String),
    /// The args parsed to fewer values than the lifecycle's setup declares.
    MissingArgs {
        /// The lifecycle whose minimum was missed.
        lifecycle: String,
        /// How many args the request carried.
        have: usize,
        /// How many the setup template requires.
        want: usize,
    },
}

impl Refusal {
    /// The sentence the page shows the user.
    fn sentence(&self) -> String {
        match self {
            Self::SessionBusy => "session is busy — queue the message instead".to_owned(),
            Self::EmptyText => "message is empty".to_owned(),
            Self::UnknownSession => "unknown session".to_owned(),
            Self::NoActiveSession => "no active session".to_owned(),
            Self::StoreUnavailable => "session store unavailable".to_owned(),
            Self::UnknownSkill => "unknown skill".to_owned(),
            Self::UnknownTool => "unknown tool".to_owned(),
            Self::UnknownMcpServer => "unknown mcp server".to_owned(),
            Self::UnknownPersona => "unknown persona".to_owned(),
            Self::UnknownEntry => "unknown entry".to_owned(),
            Self::UnknownTheme(theme) => {
                format!("unknown theme `{theme}` (want light, dark, or system)")
            }
            Self::UnknownLibraryTarget(id) => {
                format!("no session with id {id} is live or stored")
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
        }
    }
}

fn refused(reason: &Refusal) -> Reply {
    let message = reason.sentence();
    Reply::one(ServerMsg::Error(ErrorBody { message }))
}

/// The reply for a refusal whose text is not one of the fixed vocabulary:
/// an image ingestion failure, which names the offender it is about.
fn refused_with(message: impl Into<String>) -> Reply {
    Reply::one(ServerMsg::Error(ErrorBody {
        message: message.into(),
    }))
}

/// The answer to a frame that is not a `ClientMsg`. The socket stays open.
#[must_use]
pub fn malformed() -> Reply {
    Reply::one(ServerMsg::Error(ErrorBody {
        message: "unrecognized message".to_owned(),
    }))
}

// ---------------------------------------------------------------------------
// Hello
// ---------------------------------------------------------------------------

/// Answers the first frame on every connect.
///
/// Incremental when the page's cursor is an entry of the active session
/// **and** its revision is the session's current one — nothing but appends
/// can have happened since. Any other case, including a cursor the session
/// no longer holds and a revision from before a restart, is a full
/// [`ServerMsg::Snapshot`]: the page's view is replaced wholesale, so a
/// wrong guess can never leave a gap.
pub fn hello(
    out: &OutboundHandle,
    last_entry_id: Option<&ChatEntryId>,
    revision: Option<u64>,
) -> Reply {
    let state = out.state();
    let session_id = state.read().session.active_session_id().clone();

    let Some(cursor_index) = resumable_cursor(state, &session_id, last_entry_id) else {
        return snapshot_reply(out);
    };
    if revision != Some(out.revision(&session_id)) {
        return snapshot_reply(out);
    }

    let frames = entries_after(state, &session_id, cursor_index);
    out.mark_delivered(&session_id);
    Reply { frames }
}

/// The cursor's position, when the active session still holds it.
fn resumable_cursor(
    state: &jinn_kernel::State,
    session_id: &SessionId,
    last_entry_id: Option<&ChatEntryId>,
) -> Option<usize> {
    let guard = state.read();
    let id = last_entry_id.as_ref()?;
    guard.session.get(session_id)?.find_entry_index_by_id(id)
}

/// Every entry after a position, as upserts carrying their live index.
fn entries_after(
    state: &jinn_kernel::State,
    session_id: &SessionId,
    cursor_index: usize,
) -> Vec<ServerMsg> {
    let guard = state.read();
    let Some(session) = guard.session.get(session_id) else {
        return Vec::new();
    };
    session
        .history()
        .iter()
        .enumerate()
        .skip(cursor_index + 1)
        .map(|(index, entry)| {
            ServerMsg::EntryUpsert(Box::new(EntryUpsertBody {
                session_id: session_id.clone(),
                index,
                entry: EntryEnvelope::new(entry.clone()),
            }))
        })
        .collect()
}

fn snapshot_reply(out: &OutboundHandle) -> Reply {
    let session_id = out.state().read().session.active_session_id().clone();
    out.mark_delivered(&session_id);
    Reply::one(ServerMsg::Snapshot(Box::new(snapshot::snapshot(
        out.state(),
        out.revisions(),
    ))))
}

// ---------------------------------------------------------------------------
// Send / queue / cancel
// Parity: chat-input `intent.rs::handle_submit_message`, kernel `cancel.rs`
// ---------------------------------------------------------------------------

/// Sends a message on the idle active session.
///
/// Mirrors the terminal's submit: `MarkSessionInteracted` first, then
/// `EnqueueUserMessage`. A busy session is refused — the web has no
/// steering, so a send while a turn runs would silently become a steer.
pub async fn send(out: &OutboundHandle, text: String, images: Vec<ImageUploadBody>) -> Reply {
    let busy = out
        .state()
        .read()
        .session
        .active_session()
        .phase()
        .is_working();
    if busy {
        return refused(&Refusal::SessionBusy);
    }
    enqueue(out, text, images).await
}

/// Queues a message for after the running turn.
///
/// The same `EnqueueUserMessage` a send publishes; the turn dispatcher's
/// existing busy-phase routing queues it. Works whether idle or busy.
pub async fn queue(out: &OutboundHandle, text: String, images: Vec<ImageUploadBody>) -> Reply {
    enqueue(out, text, images).await
}

/// Publishes one message on the idle-or-busy active session, images and all.
///
/// Ingestion runs first, before any state is read: every image refusal is
/// answered before the session lookup matters, and nothing is published
/// until every image has passed. A send may carry images and no text.
async fn enqueue(out: &OutboundHandle, text: String, images: Vec<ImageUploadBody>) -> Reply {
    let attachments = match crate::attachment::ingest_send_images(&text, &images).await {
        Ok(attachments) => attachments,
        Err(refusal) => return refused_with(refusal),
    };
    if text.trim().is_empty() && attachments.is_empty() {
        return refused(&Refusal::EmptyText);
    }
    let session_id = out.state().read().session.active_session_id().clone();
    // Order is parity: the terminal marks the session interacted before
    // it enqueues, and each publish is awaited so the order holds.
    out.publish(MarkSessionInteracted {
        session_id: session_id.clone(),
    })
    .await;
    out.publish(EnqueueUserMessage {
        session_id,
        entry: ChatEntry::user_with_attachments(text, attachments),
    })
    .await;
    Reply::none()
}

/// Cancels the running turn.
///
/// Mirrors the terminal: `CancelTurn` with `drain_buffers_to_input: true`,
/// the value the TUI passes. The web draft does not receive the drained
/// text.
pub async fn cancel(out: &OutboundHandle) -> Reply {
    let session_id = out.state().read().session.active_session_id().clone();
    out.publish(CancelTurn {
        session_id,
        drain_buffers_to_input: true,
    })
    .await;
    Reply::none()
}

// ---------------------------------------------------------------------------
// Activate / switch model / rename
// ---------------------------------------------------------------------------

/// Switches the instance's active session.
///
/// Mirrors the terminal: `begin_load` and `set_active`, then
/// `SessionLoadRequested`. The terminal only requests the load when its
/// line cache says the session is unmeasured; the web has no line cache,
/// so it always requests it, and the store's no-op for an in-memory
/// session is harmless.
pub async fn activate_session(out: &OutboundHandle, session_id: SessionId) -> Reply {
    let state = out.state();
    // A session that is only archived is not in memory, and that is not a
    // reason to refuse: the store actor reads it from disk and unarchives
    // it on the same load request. Only an id the store does not hold can
    // never become active, so only that one is refused, before anything
    // is published.
    if !state.read().session.contains(&session_id) {
        let stored = out.session_store().load_session(&session_id).await;
        match stored {
            Ok(Some(_)) => {}
            Ok(None) => return refused(&Refusal::UnknownSession),
            Err(_) => return refused(&Refusal::StoreUnavailable),
        }
    }
    {
        let mut guard = state.write();
        guard.session.begin_load(session_id.clone());
        guard.session.set_active(session_id.clone());
    }
    out.publish(SessionLoadRequested {
        session_id: session_id.clone(),
        content_width: None,
    })
    .await;
    // The store actor applies the activation on its own turn, and an
    // archived session is read from disk first. The snapshot below is
    // drawn from live state, so it waits (bounded) for that to land; a
    // snapshot drawn mid-load would describe the session just left.
    wait_for_activation(state, &session_id).await;
    // The page is now looking at a different session, so it is told so
    // wholesale: which session it views, that session's title, model and
    // phase, and its transcript from the start. Everything the page shows
    // is replaced in the one arm that already applies a snapshot.
    //
    // The load ends with a reconcile that broadcasts the session's entries
    // as upserts, before this reply is built, so the page can receive the
    // same entries twice: once as upserts and once in the snapshot below.
    // That is harmless only because the page applies an upsert by id, so a
    // repeat replaces rather than duplicates.
    snapshot_reply(out)
}

/// How long an activation is given to land before the page is answered
/// with whatever state is current. Long enough for a disk read, short
/// enough that a store that never answers does not hang the click.
const ACTIVATION_BUDGET: Duration = Duration::from_millis(2_000);

/// How often the active pointer is re-read while waiting.
const ACTIVATION_POLL: Duration = Duration::from_millis(5);

/// Waits until `session_id` is the active session, or the budget runs out.
///
/// The read guard is taken, checked, and dropped before each sleep; it is
/// never held across the await.
pub(crate) async fn wait_for_activation(state: &jinn_kernel::State, session_id: &SessionId) {
    let deadline = tokio::time::Instant::now() + ACTIVATION_BUDGET;
    while *state.read().session.active_session_id() != *session_id {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return;
        }
        tokio::time::sleep((deadline - now).min(ACTIVATION_POLL)).await;
    }
}

/// Switches the active session's model.
///
/// Mirrors the terminal's picker confirm: `ProviderSwitch` and
/// `UpdateAppState{SetLastModel}`, with no synchronous model write — the
/// label reaches the page when `ProviderSwitched` is published.
pub async fn switch_model(out: &OutboundHandle, provider_id: String) -> Reply {
    let session_id = out.state().read().session.active_session_id().clone();
    let selection = ModelSelection::Single(provider_id);
    out.publish(ProviderSwitch {
        session_id,
        provider_id: selection.clone(),
    })
    .await;
    out.publish(UpdateAppState {
        updates: vec![AppStateUpdate::SetLastModel(Some(selection))],
    })
    .await;
    Reply::none()
}

/// Renames the active session.
///
/// Mirrors the terminal's rename confirm: a trimmed, non-empty title is
/// written with `set_title`, `touch` and `mark_interacted`, then
/// `PersistSession`.
pub async fn rename(out: &OutboundHandle, title: String) -> Reply {
    let title = title.trim().to_owned();
    if title.is_empty() {
        return refused(&Refusal::EmptyText);
    }
    let session_id = write_title(out.state(), &title);
    let Some(session_id) = session_id else {
        return refused(&Refusal::NoActiveSession);
    };
    out.publish(PersistSession {
        session_id: session_id.clone(),
    })
    .await;
    Reply::one(ServerMsg::Title(TitleBody {
        session_id,
        title: title.trim().to_owned(),
    }))
}

fn write_title(state: &jinn_kernel::State, title: &str) -> Option<SessionId> {
    let mut guard = state.write();
    let session_id = guard.session.active_session_id().clone();
    let session = guard.session.get_mut(&session_id)?;
    session.set_title(title.to_owned());
    session.touch();
    session.mark_interacted();
    Some(session_id)
}

// ---------------------------------------------------------------------------
// Picker rows / model rows
// ---------------------------------------------------------------------------

/// The stored-session picker's rows for a filter.
///
/// Every session the store knows — loaded or archived — whose title
/// matches the filter, ranked the way the terminal's session picker ranks
/// them (the same fuzzy matcher, best score first) and capped at
/// [`PICKER_ROW_CAP`]. An empty filter lists them newest first.
pub async fn picker_rows(out: &OutboundHandle, filter: String) -> Reply {
    let rows = match stored_rows(out, &filter).await {
        Ok(rows) => rows,
        Err(report) => {
            tracing::warn!(error = %report, "second web UI could not read stored sessions");
            return refused(&Refusal::StoreUnavailable);
        }
    };
    Reply::one(ServerMsg::PickerRows(PickerRowsBody { filter, rows }))
}

async fn stored_rows(
    out: &OutboundHandle,
    filter: &str,
) -> Result<Vec<PickerRowBody>, error_stack::Report<jinn_session_state::SessionStoreError>> {
    let store = out.session_store();
    let mut summaries = store.load_unarchived_summaries().await?;
    summaries.extend(store.load_archive_rows().await?);
    let _ = &mut summaries;
    let matched = match_titles(summaries, filter);
    Ok(matched.into_iter().take(PICKER_ROW_CAP).collect())
}

/// Filters and ranks stored sessions by title with the terminal picker's matcher.
pub(crate) fn match_titles(
    summaries: Vec<jinn_session_store_msg::SessionSummary>,
    filter: &str,
) -> Vec<PickerRowBody> {
    use fuzzy_matcher::FuzzyMatcher as _;

    let matcher = fuzzy_matcher::skim::SkimMatcherV2::default();
    let mut scored: Vec<(i64, PickerRowBody)> = summaries
        .into_iter()
        .filter_map(|summary| {
            let title = summary.title.clone();
            let row = PickerRowBody {
                session_id: summary.session_id,
                title,
                state: summary.session_state,
                parent_id: summary.parent_session,
                project: summary.project.map(|p| {
                    // The directory's name, as the terminal picker shows it.
                    p.file_name()
                        .map_or_else(|| p.to_string(), |n| n.to_string_lossy().into_owned())
                }),
            };
            // An empty filter keeps everything, unranked; otherwise only
            // titles the matcher accepts, best first.
            let score = if filter.is_empty() {
                0
            } else {
                matcher.fuzzy_match(&row.title, filter)?
            };
            Some((score, row))
        })
        .collect();
    scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    scored.into_iter().map(|(_, row)| row).collect()
}

/// The text a model row is matched by: its model, then its provider.
///
/// The same two fields, in the same order, the terminal's model picker
/// searches. Built here rather than shared because the picker's helper
/// lives in that slice's implementation crate, which no sibling may
/// depend on; the fields themselves are the contract's.
fn model_search_text(entry: &jinn_provider_selection_msg::ProviderPickerEntry) -> String {
    format!("{} {}", entry.model, entry.provider_name)
}

/// Narrows provider rows to those a filter matches, best match first.
///
/// A row is matched by its search text with the same fuzzy matcher the
/// terminal's picker uses. An empty filter keeps every row, in the order
/// given.
fn filter_model_rows(
    entries: &[jinn_provider_selection_msg::ProviderPickerEntry],
    filter: &str,
) -> Vec<jinn_provider_selection_msg::ProviderPickerEntry> {
    use fuzzy_matcher::FuzzyMatcher as _;

    if filter.is_empty() {
        return entries.to_vec();
    }
    let matcher = fuzzy_matcher::skim::SkimMatcherV2::default();
    let mut scored: Vec<(i64, jinn_provider_selection_msg::ProviderPickerEntry)> = entries
        .iter()
        .filter_map(|entry| {
            let text = model_search_text(entry);
            let score = matcher.fuzzy_match(&text, filter)?;
            Some((score, entry.clone()))
        })
        .collect();
    scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    scored.into_iter().map(|(_, entry)| entry).collect()
}

/// The model picker's rows for a filter, from the shared row builder.
///
/// The same `load_provider_entries` → `sorted_entries` path the terminal's
/// picker seeds from, so the two lists cannot drift: available models
/// first, the active one promoted to the top when unfiltered.
pub fn model_rows(out: &OutboundHandle, filter: String) -> Reply {
    // The filter is remembered so a `ModelCacheStored` push — a refresh
    // landing — can answer the view this picker is holding.
    let session_id = out.state().read().session.active_session_id().clone();
    out.remember_filter(&session_id, crate::outbound::RowsKind::Model, &filter);
    let rows = model_row_bodies(out, &filter);
    Reply::one(ServerMsg::ModelRows(ModelRowsBody {
        session_id,
        filter,
        rows,
    }))
}

/// The model picker's rows for a filter, built from the shared row builder.
///
/// A free function over the handle so a push builds the same rows a reply
/// does — one builder, two callers.
pub(crate) fn model_row_bodies(out: &OutboundHandle, filter: &str) -> Vec<ModelRowBody> {
    let active = out
        .state()
        .read()
        .session
        .active_session()
        .profile()
        .model
        .display_str()
        .to_owned();
    let theme = out.state().read().frontend.theme.clone();
    let model_cache = out.provider_cell().read().model_cache.clone();

    let all = {
        let registry = out.services().provider_registry.read();
        let api_keys = out.services().api_keys.read();
        jinn_provider_selection_msg::load_provider_entries(
            &registry,
            &api_keys,
            model_cache.as_ref(),
            &theme,
        )
    };
    // The terminal's picker matches a row by its search text with the
    // shared fuzzy matcher and keeps only the rows that score; the sort
    // helper alone reorders and never narrows, so the filter is applied
    // here first, the same way, for the same result.
    let matched = filter_model_rows(&all, filter);
    jinn_provider_selection_msg::sorted_entries(&matched, filter, &active)
        .into_iter()
        .map(|entry| ModelRowBody {
            provider_id: entry.provider_id,
            model: entry.model,
            provider_name: entry.provider_name,
            is_available: entry.is_available,
            is_active: entry.is_active,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Skill rows
// Parity: jinn-skills `skill_picker_reload.rs::build_skill_entries` (the
// sort), `jinn-skills-msg::filter_skills` (the match core)
// ---------------------------------------------------------------------------

/// The skills modal's rows for the active session and a filter.
///
/// One row per discovered skill the shared filter accepts, in the order
/// the terminal picker lists them: sorted case-insensitively by name,
/// then filtered by `filter_skills` — the same match core, over the same
/// `{name} {description}` search text. Enablement is the gate the prompt
/// assembler applies (`is_skill_enabled`), read under the same guard as
/// the discovered set so a row can never pair a skill with another
/// moment's filter. With no active session the answer is an empty list.
pub fn skill_rows(out: &OutboundHandle, filter: String) -> Reply {
    let state = out.state();
    let session_id = state.read().session.active_session_id().clone();
    let rows = {
        let guard = state.read();
        guard
            .session
            .get(&session_id)
            .map_or_else(Vec::new, |session| skill_row_bodies(session, &filter))
    };
    out.remember_filter(&session_id, RowsKind::Skills, &filter);
    Reply::one(ServerMsg::SkillRows(SkillRowsBody {
        session_id,
        filter,
        rows,
    }))
}

/// Toggles one skill's enablement on the active session and answers
/// with the fresh rows.
///
/// Applied at once, through the filter rather than by rewriting it: an
/// enabled skill is withheld and a disabled one permitted, on the
/// session's current filter — an absent filter materializes as the
/// inherited one, because permitting a name is a configuration change
/// and a session with no filter cannot carry one. This is the terminal
/// picker's durable permit (`enable_durably`), so an allow-mode session
/// stays in allow mode; rewriting the filter as a blocklist, as the
/// picker's confirm does, would demote it. Nothing is published — the
/// filter is the session's own and the page re-reads it from the rows.
///
/// A skill already loaded into context is not toggled: its body stays
/// pinned in history, so a disabled marker would promise an unload that
/// never happens. A skill the session has not discovered is refused.
pub fn toggle_skill(out: &OutboundHandle, skill: &str) -> Reply {
    let state = out.state();
    let session_id = state.read().session.active_session_id().clone();
    let filter = out.last_filter(&session_id, RowsKind::Skills);
    let outcome = {
        let mut guard = state.write();
        guard
            .session
            .get_mut(&session_id)
            .map_or(Toggle::Unknown, |session| {
                apply_skill_toggle(session, skill)
            })
    };
    match outcome {
        Toggle::Toggled | Toggle::Loaded => skill_rows(out, filter),
        Toggle::Unknown => refused(&Refusal::UnknownSkill),
    }
}

/// What a toggle found: the skill flipped, the skill is loaded into
/// context and so stays as it is, or the session never discovered it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Toggle {
    Toggled,
    Loaded,
    Unknown,
}

/// Loads one skill's body into the active session's context and answers
/// with the pair it pinned.
///
/// Mirrors the terminal picker's force-load. The body rides a `skill`
/// tool call followed by a pinned result, so it stays in context until
/// it is unpinned and pruned. A disabled skill is permitted durably
/// first — the load would be useless otherwise — and an already-loaded
/// skill gets a transient notice instead of a second pair. One message
/// is published: the session was interacted with, so a store flush is
/// due.
///
/// The pair reaches the page in this reply. Appending to history
/// publishes nothing on its own — the turn machinery publishes
/// `HistoryAppended` for its own appends — so a handler that appends
/// and stays silent would leave the transcript one reconcile behind.
pub async fn load_skill(out: &OutboundHandle, skill: &str) -> Reply {
    let state = out.state();
    let session_id = state.read().session.active_session_id().clone();
    let filter = out.last_filter(&session_id, RowsKind::Skills);

    // The load's three outcomes, decided and written under one guard; the
    // guard drops before the publish below, so no lock is held across
    // its await.
    let outcome = {
        let mut guard = state.write();
        let Some(session) = guard.session.get_mut(&session_id) else {
            return refused(&Refusal::NoActiveSession);
        };
        let Some(found) = session
            .discovered_skills()
            .iter()
            .find(|s| s.name == skill)
            .cloned()
        else {
            return refused(&Refusal::UnknownSkill);
        };
        if session.loaded_skills().contains(skill) {
            session.push_entry(ChatEntry::transient(format!(
                "Skill '{skill}' is already loaded"
            )));
            Load::AlreadyLoaded
        } else {
            // A disabled skill is permitted durably first: a load that
            // left it withheld would pin a body the prompt never sees.
            if !session.is_skill_enabled(skill) {
                let mut filter = session
                    .skill_filter()
                    .cloned()
                    .unwrap_or_else(NameFilter::inherited);
                filter.permit(skill);
                session.set_skill_filter(Some(filter));
            }
            let tool_call_id = ChatEntryId::new().to_string();
            let location = found.file_path.to_string();
            let xml = format!(
                "<skill name=\"{skill}\" location=\"{location}\">\n{}\n</skill>",
                found.body
            );
            let arguments = serde_json::json!({ "name": skill }).to_string();
            session.push_entry(ChatEntry::tool_call(
                tool_call_id.clone(),
                "skill",
                arguments,
            ));
            let mut result = ChatEntry::tool_result(
                tool_call_id,
                "skill",
                xml,
                jinn_core_types::ToolResultStatus::Success,
            );
            result.pin_position = Some(jinn_core_types::PinPosition::Relative);
            session.push_entry(result);
            Load::Loaded
        }
    };

    // The notice, or the pair, reaches the page through this reply;
    // history appends publish nothing by themselves.
    let pushed = match outcome {
        Load::AlreadyLoaded => 1,
        Load::Loaded => 2,
    };
    let mut reply = Reply::none();
    reply
        .frames
        .extend(last_n_upserts(state, &session_id, pushed));
    if outcome == Load::Loaded {
        out.publish(MarkSessionInteracted {
            session_id: session_id.clone(),
        })
        .await;
    }
    reply
        .frames
        .push(skill_rows_frame(state, &session_id, &filter));
    reply
}

/// What a load did: pinned a fresh pair, or found the skill already in
/// context and said so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Load {
    Loaded,
    AlreadyLoaded,
}

/// The upserts for a session's newest `n` entries, read after a write so
/// each carries the index it now sits at.
fn last_n_upserts(state: &jinn_kernel::State, session_id: &SessionId, n: usize) -> Vec<ServerMsg> {
    let guard = state.read();
    let Some(session) = guard.session.get(session_id) else {
        return Vec::new();
    };
    let history = session.history();
    let start = history.len().saturating_sub(n);
    history
        .iter()
        .skip(start)
        .enumerate()
        .map(|(offset, entry)| crate::frames::upsert_at(session_id, start + offset, entry))
        .collect()
}

/// The fresh skill-rows frame, in the filter the session last asked for.
fn skill_rows_frame(state: &jinn_kernel::State, session_id: &SessionId, filter: &str) -> ServerMsg {
    let rows = {
        let guard = state.read();
        guard
            .session
            .get(session_id)
            .map_or_else(Vec::new, |session| skill_row_bodies(session, filter))
    };
    ServerMsg::SkillRows(SkillRowsBody {
        session_id: session_id.clone(),
        filter: filter.to_owned(),
        rows,
    })
}

/// Flips one skill's enablement on a session's filter. A skill loaded
/// into context is left alone and reported as such — the caller answers
/// with rows either way, so the page never shows an error for a click
/// the UI already discourages.
fn apply_skill_toggle(session: &mut jinn_session_state::ChatSessionState, skill: &str) -> Toggle {
    if !session.discovered_skills().iter().any(|s| s.name == skill) {
        return Toggle::Unknown;
    }
    if session.loaded_skills().contains(skill) {
        return Toggle::Loaded;
    }
    let enabled = session.is_skill_enabled(skill);
    let mut filter = session
        .skill_filter()
        .cloned()
        .unwrap_or_else(NameFilter::inherited);
    if enabled {
        filter.withhold(skill);
    } else {
        filter.permit(skill);
    }
    session.set_skill_filter(Some(filter));
    Toggle::Toggled
}

/// One session's skill rows: every skill the shared filter accepts, each
/// paired with its enablement and loaded-ness, with the matched runs of
/// name and description split for the browser to highlight.
///
/// Sorted case-insensitively by name before filtering, so the rows keep
/// the terminal picker's order and an empty filter lists all of them.
pub(crate) fn skill_row_bodies(
    session: &jinn_session_state::ChatSessionState,
    filter: &str,
) -> Vec<SkillRowBody> {
    let mut skills: Vec<jinn_skills_msg::Skill> = session.discovered_skills().to_vec();
    skills.sort_by_key(|skill| skill.name.to_lowercase());
    let loaded = session.loaded_skills();
    jinn_skills_msg::filter_skills(&skills, filter)
        .into_iter()
        .map(|filtered| {
            let enabled = session.is_skill_enabled(&filtered.skill.name);
            let segments = filtered.segments();
            SkillRowBody {
                enabled,
                loaded: loaded.contains(&filtered.skill.name),
                is_project: matches!(
                    filtered.skill.source,
                    jinn_skills_msg::SkillSource::Project { .. }
                ),
                name: filtered.skill.name,
                description: filtered.skill.description,
                name_runs: runs(segments.name),
                description_runs: runs(segments.description),
            }
        })
        .collect()
}

/// The shared filter's text runs, as the wire carries them.
fn runs(runs: Vec<jinn_selection_widget::TextRun>) -> Vec<TextRunBody> {
    runs.into_iter()
        .map(|run| TextRunBody {
            text: run.text,
            matched: run.matched,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tool rows / toggle
// Parity: jinn-tools `<leader>st` picker confirm (deny-over-off-rows), via
// the shared seed vocabulary in jinn-tools-msg
// ---------------------------------------------------------------------------

/// The tools modal's rows for the active session and a filter.
///
/// Built by the shared seed vocabulary — the same provider and bot gates
/// and the same case-insensitive sort the terminal's picker seeds from —
/// over the session's registry view, its bot facts, and its live tool
/// filter, read under one guard so a row can never pair a tool with
/// another moment's filter. An absent registry yields no rows. The
/// filter is then the shared fuzzy filter over `{name} {description}`,
/// with the matched runs split for the browser to highlight.
pub fn tool_rows(out: &OutboundHandle, filter: String) -> Reply {
    let session_id = out.state().read().session.active_session_id().clone();
    let rows = tool_row_bodies(out.state(), &session_id, &filter);
    out.remember_filter(&session_id, RowsKind::Tools, &filter);
    Reply::one(ServerMsg::ToolRows(ToolRowsBody {
        session_id,
        filter,
        rows,
    }))
}

/// Toggles one tool's enablement on the active session and answers with
/// the fresh rows.
///
/// The terminal picker's confirm applied per click: the session's tool
/// filter becomes a **deny** filter over every row that renders disabled
/// after the flip — never a delta, never an allow-mode write, and an
/// allow-mode session is demoted to its withheld names exactly as the
/// terminal's confirm demotes one. The desired state is derived here
/// from the state the server holds; the wire carries only the name.
///
/// Nothing is published: the terminal's picker path publishes none, and
/// the filter is the session's own. A tool the session's registry does
/// not offer was never rendered, so it is refused with nothing written.
pub fn toggle_tool(out: &OutboundHandle, tool: &str) -> Reply {
    let state = out.state();
    let session_id = state.read().session.active_session_id().clone();
    let filter = out.last_filter(&session_id, RowsKind::Tools);
    // The registry cell and the session are read under the one write
    // guard, so the post-click rows are derived from the same registry
    // view the modal rendered — a tool that was never offered is refused
    // with nothing written.
    let outcome = {
        let mut guard = state.write();
        let registry = guard.tool_registry();
        guard
            .session
            .get_mut(&session_id)
            .map_or(ToolToggle::Unknown, |session| {
                apply_tool_toggle(session, &session_id, registry, tool)
            })
    };
    match outcome {
        ToolToggle::Toggled => tool_rows(out, filter),
        ToolToggle::Unknown => refused(&Refusal::UnknownTool),
    }
}

/// What a tool toggle found: the filter was rewritten, or the registry
/// offers no such tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolToggle {
    Toggled,
    Unknown,
}

/// Flips one tool on a session's filter by rewriting the whole deny list
/// from the post-click row state.
fn apply_tool_toggle(
    session: &mut jinn_session_state::ChatSessionState,
    session_id: &SessionId,
    registry: Option<jinn_slices::TypedCell<jinn_tools_msg::ToolRegistry>>,
    tool: &str,
) -> ToolToggle {
    let rows = seed_rows(registry, session, session_id);
    let Some(clicked) = rows.iter().find(|row| row.name == tool) else {
        return ToolToggle::Unknown;
    };
    let committed = jinn_tools_msg::committed_tool_filter(&toggled_rows(&rows, clicked));
    session.set_tool_filter(Some(committed));
    ToolToggle::Toggled
}

/// The rows as they read after `clicked` is flipped — the pure post-click
/// state the commit derives from.
fn toggled_rows(
    rows: &[jinn_tools_msg::WebToolRow],
    clicked: &jinn_tools_msg::WebToolRow,
) -> Vec<jinn_tools_msg::WebToolRow> {
    rows.iter()
        .map(|row| {
            if row.name == clicked.name {
                jinn_tools_msg::WebToolRow {
                    enabled: !row.enabled,
                    ..row.clone()
                }
            } else {
                row.clone()
            }
        })
        .collect()
}

/// One session's seeded tool rows: the registry view gated and sorted by
/// the shared vocabulary, with enablement read from the live filter.
///
/// Takes the registry cell and the session rather than a guard so the same
/// seed serves a read (the rows) and a read-modify-write (the toggle),
/// each under its own single guard — the registry is never read twice
/// under different guards. An absent registry yields no rows.
fn seed_rows(
    registry: Option<jinn_slices::TypedCell<jinn_tools_msg::ToolRegistry>>,
    session: &jinn_session_state::ChatSessionState,
    session_id: &SessionId,
) -> Vec<jinn_tools_msg::WebToolRow> {
    let provider_name = session.model_selection().provider_name().to_owned();
    let is_bot = session.is_bot();
    let bot_has_parent = session.parent_session().is_some();
    let definitions: Vec<jinn_core_types::ToolDefinition> =
        registry.map_or_default(|registry| registry.read().tools_for_session(session_id));
    jinn_tools_msg::modal_rows(
        &definitions,
        &provider_name,
        is_bot,
        bot_has_parent,
        session.tool_filter(),
    )
}

// ---------------------------------------------------------------------------
// MCP rows / toggle
// Parity: jinn-mcp-slice inspector toggle, via the shared row builder in
// jinn-mcp-msg
// ---------------------------------------------------------------------------

/// The MCP servers modal's rows for the active session and a filter.
///
/// The shared builder over the `[mcp.<name>]` catalog, the session's
/// enabled set, and the runtime cell's **effective** status projection —
/// so a singleton reports its shared connection, never a per-session
/// bucket that never fills. Each badge is the shared derivation, so a
/// disabled server reads `off` whatever its connection last reported.
pub fn mcp_rows(out: &OutboundHandle, filter: String) -> Reply {
    let session_id = out.state().read().session.active_session_id().clone();
    let rows = mcp_row_bodies(out, &session_id, &filter);
    out.remember_filter(&session_id, RowsKind::Mcp, &filter);
    Reply::one(ServerMsg::McpRows(McpRowsBody {
        session_id,
        filter,
        rows,
    }))
}

/// Toggles one MCP server's enablement on the active session and answers
/// with the fresh rows.
///
/// XORs the server in or out of the session's enabled set — the desired
/// state derived here, written synchronously before the publish — and
/// publishes the existing `McpEnablementChanged` carrying the **full**
/// desired set, never a delta: that is its documented contract, and the
/// coordinator diffs it against its spawned-actor map. No `PersistSession`
/// rides along; the terminal's toggle publishes none. A server outside
/// the configured set was never rendered, so it is refused with nothing
/// written and nothing published.
pub async fn toggle_mcp(out: &OutboundHandle, server: &str) -> Reply {
    let configured = out
        .config()
        .get::<McpServersConfig>()
        .is_ok_and(|servers| servers.0.contains_key(server));
    if !configured {
        return refused(&Refusal::UnknownMcpServer);
    }
    let state = out.state();
    let session_id = state.read().session.active_session_id().clone();
    // The write lands under one guard, dropped before the publish below.
    let desired = {
        let mut guard = state.write();
        let Some(session) = guard.session.get_mut(&session_id) else {
            return refused(&Refusal::NoActiveSession);
        };
        let mut enabled = session.enabled_mcp_servers().clone();
        if !enabled.remove(server) {
            enabled.insert(server.to_owned());
        }
        session.set_enabled_mcp_servers(enabled.clone());
        enabled
    };
    out.publish(jinn_mcp_msg::McpEnablementChanged {
        session_id: session_id.clone(),
        enabled: desired,
    })
    .await;
    let filter = out.last_filter(&session_id, RowsKind::Mcp);
    mcp_rows(out, filter)
}

/// The shared row builder's inputs, read under one guard each: the
/// configured catalog as `(name, description)` pairs, the session's
/// enabled set, and the effective status projection.
pub(crate) fn mcp_row_bodies(
    out: &OutboundHandle,
    session_id: &SessionId,
    filter: &str,
) -> Vec<McpRowBody> {
    let servers: Vec<(String, String)> =
        out.config()
            .get::<McpServersConfig>()
            .map_or_default(|config| {
                config
                    .0
                    .into_iter()
                    .map(|(name, server)| (name, server.description_for_picker()))
                    .collect()
            });
    let enabled = out
        .state()
        .read()
        .session
        .get(session_id)
        .map_or_default(|session| session.enabled_mcp_servers().clone());
    let statuses = out.mcp_runtime_cell().read().effective_statuses(session_id);
    jinn_mcp_msg::filter_rows(
        &jinn_mcp_msg::build_rows(&servers, &enabled, &statuses),
        filter,
    )
    .into_iter()
    .map(|filtered| {
        let segments = filtered.row.segments(&filtered.match_indices);
        McpRowBody {
            badge: mcp_badge_body(McpRowBadge::derive(
                filtered.row.enabled,
                filtered.row.status,
            )),
            name: filtered.row.name,
            description: filtered.row.description,
            enabled: filtered.row.enabled,
            name_runs: runs(segments.name),
            description_runs: runs(segments.description),
        }
    })
    .collect()
}

/// The shared derivation's badge, as the wire carries it.
fn mcp_badge_body(badge: McpRowBadge) -> McpBadgeBody {
    match badge {
        McpRowBadge::Off => McpBadgeBody::Off,
        McpRowBadge::Starting => McpBadgeBody::Starting,
        McpRowBadge::Running => McpBadgeBody::Running,
        McpRowBadge::Dead => McpBadgeBody::Dead,
    }
}

// ---------------------------------------------------------------------------
// Persona rows / switch
// Parity: jinn-persona picker confirm, via the shared row builder in
// jinn-persona-msg
// ---------------------------------------------------------------------------

/// The persona modal's rows for the active session and a filter.
///
/// The shared builder over the personas cell — the terminal picker's own
/// rows, in the cell's name-sorted order — with the active mark read from
/// the session at row build, then the shared fuzzy filter over
/// `{name} {description}` with the matched runs split for the browser.
pub fn persona_rows(out: &OutboundHandle, filter: String) -> Reply {
    let session_id = out.state().read().session.active_session_id().clone();
    let rows = persona_row_bodies(out, &session_id, &filter);
    out.remember_filter(&session_id, RowsKind::Persona, &filter);
    Reply::one(ServerMsg::PersonaRows(PersonaRowsBody {
        session_id,
        filter,
        rows,
    }))
}

/// Switches the active session's persona.
///
/// The terminal picker's confirm: the name is validated against the
/// catalog the modal renders (the offered set is the accepted set), the
/// session's persona is written synchronously before the publish, and the
/// same two messages the terminal publishes are published in its order —
/// `UpdateAppState(SetPersona)`, the startup-session seed, then
/// `MarkSessionInteracted`.
///
/// A persona with no `<compaction>` handoff block is a loud error, not a
/// silent downgrade: the shared message is appended to the session before
/// the publishes, and its upsert rides this reply — nothing pushes an
/// idle-time history append to the page, so the reply is the only
/// delivery path. A persona outside the catalog is refused with nothing
/// written, appended, or published.
pub async fn switch_persona(out: &OutboundHandle, persona: &str) -> Reply {
    // The offered set is the accepted set: the modal's rows are built from
    // this cell, so a name outside it was never rendered.
    let Some(chosen) = out
        .personas_cell()
        .read()
        .entries
        .iter()
        .find(|entry| entry.name == persona)
        .cloned()
    else {
        return refused(&Refusal::UnknownPersona);
    };
    let state = out.state();
    let session_id = state.read().session.active_session_id().clone();
    let blockless = chosen.handoff_prompt.is_empty();

    // The switch, and the block-less warning, land under one guard that
    // drops before the publishes below.
    {
        let mut guard = state.write();
        let Some(session) = guard.session.get_mut(&session_id) else {
            return refused(&Refusal::NoActiveSession);
        };
        session.set_persona_name(chosen.name.clone());
        if blockless {
            session.push_entry(ChatEntry::error(
                jinn_persona_msg::missing_compaction_block_message(&chosen.name),
            ));
        }
    }

    out.publish(UpdateAppState {
        updates: vec![AppStateUpdate::SetPersona(Some(chosen.name.clone()))],
    })
    .await;
    out.publish(MarkSessionInteracted {
        session_id: session_id.clone(),
    })
    .await;

    let mut reply = Reply::none();
    if blockless {
        reply.frames.extend(last_n_upserts(state, &session_id, 1));
    }
    reply
}

/// One session's persona rows: every persona the shared filter accepts,
/// with the active mark computed once at row build.
fn persona_row_bodies(
    out: &OutboundHandle,
    session_id: &SessionId,
    filter: &str,
) -> Vec<PersonaRowBody> {
    let active = out
        .state()
        .read()
        .session
        .get(session_id)
        .map(|session| session.persona_name().to_owned());
    let rows = {
        let personas = out.personas_cell().read();
        jinn_persona_msg::PersonaRow::rows(&personas.entries, active.as_deref())
    };
    let plain = |segments: Vec<jinn_persona_msg::TextSegment>| -> Vec<TextRunBody> {
        segments
            .into_iter()
            .map(|segment| TextRunBody {
                text: segment.text,
                matched: segment.matched,
            })
            .collect()
    };
    jinn_persona_msg::filter_rows(&rows, filter)
        .into_iter()
        .map(|filtered| {
            let segments = filtered.segments();
            PersonaRowBody {
                name: filtered.row.name,
                description: filtered.row.description,
                is_active: filtered.row.is_active,
                name_runs: plain(segments.name),
                description_runs: plain(segments.description),
            }
        })
        .collect()
}

/// One session's tool rows, filtered and split for the browser.
fn tool_row_bodies(
    state: &jinn_kernel::State,
    session_id: &SessionId,
    filter: &str,
) -> Vec<ToolRowBody> {
    let rows = {
        let guard = state.read();
        guard
            .session
            .get(session_id)
            .map_or_else(Vec::new, |session| {
                seed_rows(guard.tool_registry(), session, session_id)
            })
    };
    jinn_tools_msg::filter_rows(&rows, filter)
        .into_iter()
        .map(|filtered| {
            let segments = filtered.row.segments(&filtered.match_indices);
            ToolRowBody {
                name: filtered.row.name,
                description: filtered.row.description,
                enabled: filtered.row.enabled,
                name_runs: runs(segments.name),
                description_runs: runs(segments.description),
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Theme / library / model refresh
// ---------------------------------------------------------------------------

/// The color modes the page accepts.
const THEMES: [&str; 3] = ["light", "dark", "system"];

/// Sets the page's color mode.
///
/// The mode is validated against the three-value vocabulary (an unknown
/// value was never rendered by the menu — the unrecognized-value refusal is
/// the answer, never a silent default), then published as
/// `UpdateAppState(SetWebTheme)` for the app-state actor to persist. The
/// reply carries the submitted value, not the state cache: the actor
/// applies the diff asynchronously, so a cache read would race it.
pub async fn set_theme(out: &OutboundHandle, theme: String) -> Reply {
    if !THEMES.contains(&theme.as_str()) {
        return refused(&Refusal::UnknownTheme(theme));
    }
    out.publish(UpdateAppState {
        updates: vec![AppStateUpdate::SetWebTheme(Some(theme.clone()))],
    })
    .await;
    Reply::one(ServerMsg::Theme(ThemeBody { theme }))
}

/// Adds a session to, or removes it from, the library.
///
/// Publishes the existing `SetSessionLibrary` command and nothing else: the
/// store actor persists the column, mirrors the flag into the in-memory
/// session, and announces `SessionLibraryMembershipChanged` — which the
/// outbound actor turns into a fresh sessions frame for every page. `/web2`
/// writes no membership state of its own; a second writer would race the
/// store actor's single-writer path.
///
/// An id neither live nor stored is refused before the publish.
pub async fn set_library(out: &OutboundHandle, session_id: SessionId, in_library: bool) -> Reply {
    let known_live = out.state().read().session.contains(&session_id);
    if !known_live {
        match out.session_store().load_session(&session_id).await {
            Ok(Some(_)) => {}
            Ok(None) => return refused(&Refusal::UnknownLibraryTarget(session_id)),
            Err(_) => return refused(&Refusal::StoreUnavailable),
        }
    }
    out.publish(SetSessionLibrary {
        session_id,
        in_library,
    })
    .await;
    Reply::none()
}

/// Asks the provider registry to rediscover its models.
///
/// Publishes the existing `RefreshModels` command (one declarant:
/// `DiscoverActor`, the TUI's own discovery pipeline). It validates nothing
/// and blocks on nothing: discovery is async, and the rows arrive when the
/// model cache lands and the outbound actor pushes them. The page keeps its
/// button busy until that push.
pub async fn refresh_models(out: &OutboundHandle) -> Reply {
    out.publish(RefreshModels).await;
    Reply::none()
}

// ---------------------------------------------------------------------------
// Pin
// Parity: the terminal's `p` on the selected entry
// (`jinn_chat_log_view::chat_entry_selection::intent::handle_pin_selected`)
// ---------------------------------------------------------------------------

/// Toggles the pin on one entry of the active session.
///
/// An unpinned entry is pinned at [`PinPosition::Relative`] — where the
/// terminal's `p` pins it — and a pinned one is unpinned. The page can
/// only name an entry it was sent, so the entry is always in memory: an
/// unknown id is refused and never falls back to the store. One message
/// is published and nothing is replied; the session actor applies the
/// pin, marks the session interacted so it persists, and publishes
/// `ChatEntryPinChanged`, which the outbound actor turns into the upserts
/// the page sees.
pub async fn toggle_pin(out: &OutboundHandle, entry_id: ChatEntryId) -> Reply {
    // Decided under one read guard, which drops before the publish: the
    // state lock is never held across an await.
    let decision = {
        let state = out.state();
        let guard = state.read();
        let session_id = guard.session.active_session_id().clone();
        let Some(session) = guard.session.get(&session_id) else {
            return refused(&Refusal::NoActiveSession);
        };
        let Some(pinned) = session
            .find_entry_index_by_id(&entry_id)
            .and_then(|index| session.history().get(index))
            .map(ChatEntry::is_pinned)
        else {
            return refused(&Refusal::UnknownEntry);
        };
        (session_id, pinned)
    };
    let (session_id, pinned) = decision;
    if pinned {
        out.publish(UnpinChatEntry {
            session_id,
            entry_id,
        })
        .await;
    } else {
        out.publish(PinChatEntry {
            session_id,
            entry_id,
            position: PinPosition::Relative,
        })
        .await;
    }
    Reply::none()
}

// ---------------------------------------------------------------------------
// New session
// Parity: the original web UI's `delivery::handle_create`, which is the
// TUI project/lifecycle pickers' confirm and Discord `/new`
// ---------------------------------------------------------------------------

/// The new-session form's choices, read from the live configuration.
///
/// Projects are listed as their configured path string — the value a
/// `CreateSession` sends back. Lifecycles are the configured script names
/// with the argument count each setup declares; a `Builtin` setup carries
/// no template and so takes none. The blank lifecycle is the form's own
/// first row and is deliberately not listed.
pub fn new_session_options(out: &OutboundHandle) -> Reply {
    let config = out.config();
    let projects: Vec<String> = config
        .get_list::<jinn_preferences_config::schemas::ProjectConfig>()
        .unwrap_or_default()
        .into_iter()
        .map(|project| project.path.to_string_lossy().into_owned())
        .collect();
    let lifecycles: Vec<LifecycleOptionBody> = config
        .get_list::<jinn_preferences_config::schemas::SessionLifecycle>()
        .unwrap_or_default()
        .into_iter()
        .map(|lifecycle| LifecycleOptionBody {
            param_count: match lifecycle.setup.as_ref() {
                Some(jinn_preferences_config::schemas::LifecycleCommand::Shell(shell)) => {
                    CommandTemplate::parse(shell).param_count()
                }
                Some(jinn_preferences_config::schemas::LifecycleCommand::Builtin(_)) | None => 0,
            },
            // The template's own spelling of each argument, not a number the
            // page would have to invent a label for.
            params: match lifecycle.setup.as_ref() {
                Some(jinn_preferences_config::schemas::LifecycleCommand::Shell(shell)) => {
                    CommandTemplate::parse(shell)
                        .params()
                        .iter()
                        .map(ToString::to_string)
                        .collect()
                }
                Some(jinn_preferences_config::schemas::LifecycleCommand::Builtin(_)) | None => {
                    Vec::new()
                }
            },
            name: lifecycle.name,
        })
        .collect();
    Reply::one(ServerMsg::NewSessionOptions(NewSessionOptionsBody {
        projects,
        lifecycles,
    }))
}

/// Creates a session through the shared creation path.
///
/// The original web UI's `handle_create`, message for message. Validates
/// against the config layer (the project must name a configured
/// `[[project.entry]]`; the lifecycle must be blank or a configured
/// `[[session_lifecycle.script]]`; the args must meet the lifecycle's
/// parameter minimum), then — under one state write guard — stashes the
/// `PendingSessionCreation` (the Discord precedent: the stash and the call
/// are one synchronous unit, so this stash cannot race an abandoned
/// picker's) and calls `handle_session_lifecycle_setup`, the one function
/// the TUI pickers and Discord `/new` end in. The returned closures ride
/// `jinn_kernel::Bridge`, the same drain the TUI's intent handler uses — no
/// new schema, no typed publish.
///
/// The session is created active instance-wide. This handler publishes no
/// `ActiveSessionChanged` of its own: the shared path's closures announce
/// the creation, and the store actor publishes the activation fact on the
/// paths that move the pointer. The outbound actor turns that fact into the
/// snapshot every page follows; the reply here is the snapshot the asking
/// page needs so it never waits on the event.
pub async fn create_session(
    out: &OutboundHandle,
    project: String,
    lifecycle: String,
    args: String,
) -> Reply {
    let created = match create(out, &project, &lifecycle, &args) {
        Ok(created) => created,
        Err(refusal) => return refused(&refusal),
    };
    wait_for_activation(out.state(), &created).await;
    snapshot_reply(out)
}

/// The validated creation: every refusal happens before any state is
/// written, so a refused request leaves nothing behind.
fn create(
    out: &OutboundHandle,
    project: &str,
    lifecycle: &str,
    args: &str,
) -> Result<SessionId, Refusal> {
    use jinn_kernel::session_lifecycle::intent::handle_session_lifecycle_setup;
    use jinn_session_lifecycle_msg::command_template::parse_quoted_args;

    let config = out.config();
    let arg_values = parse_quoted_args(args);

    let project_dir = resolve_project(config, project)?;
    validate_lifecycle(config, lifecycle, &arg_values)?;

    // One synchronous unit: stash the project, run the shared creation,
    // collect the closures — the guard drops when the block ends, before
    // any publish (guard discipline, and the reason the stash cannot go
    // stale).
    let closures = {
        let mut guard = out.state().write();
        guard.frontend.pending_creation = Some(jinn_kernel::PendingSessionCreation {
            project_dir: project_dir.clone(),
            starting_cwd: project_dir,
        });
        handle_session_lifecycle_setup(&mut guard, lifecycle, &arg_values, None, config).messages
    };

    // The created session is the one the shared path made active.
    let created = out.state().read().session.active_session_id().clone();

    let bridge = jinn_kernel::Bridge::new(out.bus().clone());
    for closure in closures {
        // A closed bridge is the process shutting down; the TUI's own
        // drain ignores the error the same way.
        let _ = bridge.send(closure);
    }
    Ok(created)
}

/// The configured project's directory, or the refusal.
fn resolve_project(
    config: &jinn_config::ConfigLayer,
    project: &str,
) -> Result<marked_path::MarkedPathBuf<marked_path::Absolute>, Refusal> {
    let chosen = config
        .get_list::<jinn_preferences_config::schemas::ProjectConfig>()
        .unwrap_or_default()
        .into_iter()
        .find(|entry| entry.path.to_string_lossy() == project)
        .ok_or_else(|| Refusal::UnknownProject(project.to_owned()))?;

    // `~` expansion — the Discord handler's exact form.
    match chosen.path.strip_prefix("~") {
        Ok(rest) => jinn_chat_input_msg::home_dir()
            .join(rest)
            .map_err(|_path_error| {
                Refusal::BadProjectPath(chosen.path.to_string_lossy().into_owned())
            }),
        Err(_) => marked_path::MarkedPathBuf::<marked_path::Absolute>::new(chosen.path.clone())
            .map_err(|_path_error| {
                Refusal::BadProjectPath(chosen.path.to_string_lossy().into_owned())
            }),
    }
}

/// The lifecycle must be blank or exactly a configured name —
/// `find_lifecycle` silently falls back to blank for an unknown name, so
/// it is refused here or a bogus name would stamp a session that runs no
/// script. The arg minimum is the setup template's parameter count; `$@`
/// splat templates add none.
fn validate_lifecycle(
    config: &jinn_config::ConfigLayer,
    lifecycle: &str,
    args: &[String],
) -> Result<(), Refusal> {
    let lifecycles: Vec<jinn_preferences_config::schemas::SessionLifecycle> =
        config.get_list().unwrap_or_default();
    let chosen = lifecycles
        .iter()
        .find(|candidate| candidate.name == lifecycle);
    match chosen {
        None if !lifecycle.is_empty() => Err(Refusal::UnknownLifecycle(lifecycle.to_owned())),
        None => Ok(()),
        Some(chosen) => {
            let param_count = match chosen.setup.as_ref() {
                Some(jinn_preferences_config::schemas::LifecycleCommand::Shell(raw)) => {
                    CommandTemplate::parse(raw).param_count()
                }
                Some(jinn_preferences_config::schemas::LifecycleCommand::Builtin(_)) | None => 0,
            };
            if args.len() < param_count {
                return Err(Refusal::MissingArgs {
                    lifecycle: lifecycle.to_owned(),
                    have: args.len(),
                    want: param_count,
                });
            }
            Ok(())
        }
    }
}
