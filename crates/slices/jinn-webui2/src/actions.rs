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
use jinn_core_types::{ChatEntry, ChatEntryId, ModelSelection, SessionId};
use jinn_inference_msg::CancelTurn;
use jinn_kernel::common::actor_deps::BusPublish as _;
use jinn_preferences_config::protocol::app_state_command::{AppStateUpdate, UpdateAppState};
use jinn_provider_selection_msg::ProviderSwitch;
use jinn_session_msg::MarkSessionInteracted;
use jinn_session_store_msg::{PersistSession, SessionLoadRequested};
use jinn_webui2_msg::{
    EntryEnvelope, EntryUpsertBody, ErrorBody, ModelRowBody, ModelRowsBody, PickerRowBody,
    PickerRowsBody, ServerMsg, SkillRowBody, SkillRowsBody, TextRunBody, TitleBody,
};
use marked_path::MarkedPathAccess as _;

use crate::{outbound::OutboundHandle, protocol::Reply, snapshot};

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
}

impl Refusal {
    fn message(&self) -> &'static str {
        match self {
            Self::SessionBusy => "session is busy — queue the message instead",
            Self::EmptyText => "message is empty",
            Self::UnknownSession => "unknown session",
            Self::NoActiveSession => "no active session",
            Self::StoreUnavailable => "session store unavailable",
        }
    }
}

fn refused(reason: &Refusal) -> Reply {
    Reply::one(ServerMsg::Error(ErrorBody {
        message: reason.message().to_owned(),
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
            ServerMsg::EntryUpsert(EntryUpsertBody {
                session_id: session_id.clone(),
                index,
                entry: EntryEnvelope::new(entry.clone()),
            })
        })
        .collect()
}

fn snapshot_reply(out: &OutboundHandle) -> Reply {
    let session_id = out.state().read().session.active_session_id().clone();
    out.mark_delivered(&session_id);
    Reply::one(ServerMsg::Snapshot(snapshot::snapshot(
        out.state(),
        out.revisions(),
    )))
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
pub async fn send(out: &OutboundHandle, text: String) -> Reply {
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
    enqueue(out, text).await
}

/// Queues a message for after the running turn.
///
/// The same `EnqueueUserMessage` a send publishes; the turn dispatcher's
/// existing busy-phase routing queues it. Works whether idle or busy.
pub async fn queue(out: &OutboundHandle, text: String) -> Reply {
    enqueue(out, text).await
}

async fn enqueue(out: &OutboundHandle, text: String) -> Reply {
    if text.trim().is_empty() {
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
        entry: ChatEntry::user(text),
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
async fn wait_for_activation(state: &jinn_kernel::State, session_id: &SessionId) {
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
    let (active, theme) = {
        let guard = out.state().read();
        let active = guard
            .session
            .active_session()
            .profile()
            .model
            .display_str()
            .to_owned();
        (active, guard.frontend.theme.clone())
    };
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
    let matched = filter_model_rows(&all, &filter);
    let rows = jinn_provider_selection_msg::sorted_entries(&matched, &filter, &active)
        .into_iter()
        .map(|entry| ModelRowBody {
            provider_id: entry.provider_id,
            model: entry.model,
            provider_name: entry.provider_name,
            is_available: entry.is_available,
            is_active: entry.is_active,
        })
        .collect();

    Reply::one(ServerMsg::ModelRows(ModelRowsBody { filter, rows }))
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
    out.remember_skill_filter(&session_id, &filter);
    Reply::one(ServerMsg::SkillRows(SkillRowsBody {
        session_id,
        filter,
        rows,
    }))
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
