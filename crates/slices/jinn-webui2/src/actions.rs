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

use jinn_chat_input_msg::EnqueueUserMessage;
use jinn_core_types::{
    ChatEntry, ChatEntryId, ContextOverride, ModelSelection, NameFilter, PinPosition, SessionId,
};
use jinn_inference_msg::CancelTurn;
use jinn_kernel::common::actor_deps::BusPublish as _;
use jinn_mcp_msg::McpRowBadge;
use jinn_preferences_config::protocol::app_state_command::{AppStateUpdate, UpdateAppState};
use jinn_preferences_config::schemas::mcp::McpServersConfig;
use jinn_provider_selection_msg::{ProviderSwitch, RefreshModels};
use jinn_session_history_msg::{PinChatEntry, SetEntryContextOverride, UnpinChatEntry};
use jinn_session_lifecycle_msg::command_template::CommandTemplate;
use jinn_session_msg::MarkSessionInteracted;
use jinn_session_store_msg::{
    ArchiveSession, LoadSessionPickerEntries, PersistSession, SessionForkRequested,
    SessionLoadRequested, SessionTreeEntry, SetSessionLibrary,
};
use jinn_webui2_msg::{
    AppletRowBody, AppletRowsBody, EntriesPageBody, EntryUpsertBody, ErrorBody, ImageUploadBody,
    LifecycleOptionBody, McpBadgeBody, McpRowBody, McpRowsBody, ModelRowBody, ModelRowsBody,
    NewSessionOptionsBody, PersonaRowBody, PersonaRowsBody, PickerRowBody, PromptTemplateBody,
    PushInfoBody, PushKeysBody, ServerMsg, SkillRowBody, SkillRowsBody, TextRunBody, ThemeBody,
    TitleBody, ToolRowBody, ToolRowsBody,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

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
    /// There is no active session.
    NoActiveSession,
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
    /// The entry is pinned, so a pin overrides any exclusion.
    EntryIsPinned,
    /// The theme is not one of the three modes.
    UnknownTheme(String),
    /// The session to archive is running a turn.
    ArchiveSessionBusy,
    /// The request named no configured `[[project.entry]]`.
    UnknownProject(String),
    /// A configured project path is neither `~`-prefixed nor absolute.
    BadProjectPath(String),
    /// The lifecycle value is not blank and not a configured script name.
    UnknownLifecycle(String),
    /// The market's data cell carries no listing with that id.
    UnknownMarketModel(String),
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
            Self::NoActiveSession => "no active session".to_owned(),
            Self::UnknownSkill => "unknown skill".to_owned(),
            Self::UnknownTool => "unknown tool".to_owned(),
            Self::UnknownMcpServer => "unknown mcp server".to_owned(),
            Self::UnknownPersona => "unknown persona".to_owned(),
            Self::UnknownEntry => "unknown entry".to_owned(),
            Self::EntryIsPinned => "entry is pinned — unpin it to change its context".to_owned(),
            Self::UnknownTheme(theme) => {
                format!("unknown theme `{theme}` (want light, dark, or system)")
            }
            Self::ArchiveSessionBusy => "session is busy — cancel the turn first".to_owned(),
            Self::UnknownProject(project) => {
                format!("project {project:?} is not a configured [[project.entry]]")
            }
            Self::BadProjectPath(path) => {
                format!("configured project path {path:?} is neither ~-prefixed nor absolute")
            }
            Self::UnknownMarketModel(id) => {
                format!("no market listing named {id}")
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

pub(crate) fn refused(reason: &Refusal) -> Reply {
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

/// A dash opened on the page. The statistics are not in the reply: they
/// ride the outbound actor's next tick, so the push rule (and its
/// "nothing while idle" guarantee) stays in one place.
pub(crate) fn dash_open(out: &OutboundHandle) -> Reply {
    let session_id = out.state().read().session.active_session_id().clone();
    out.dash_opened(&session_id);
    Reply::default()
}

/// The dash on the page closed.
pub(crate) fn dash_close(out: &OutboundHandle) -> Reply {
    out.dash_closed();
    Reply::default()
}

/// Loads history until `entry_id` is held, so the page can scroll to it.
///
/// The reply is a stream of the page's own older-entry pages, built newest
/// first by walking back from the target and sent oldest first, so each
/// prepends cleanly before the ones the page already holds. One request, one
/// round trip, and nothing about the page's tail changes shape. A target
/// that is not an entry of the active session is refused.
///
/// The reply carries the asking connection's cancel flag, which is reset
/// first: a cancel that arrived after an earlier jump finished would
/// otherwise leave it set and drop this jump's pages before the first one.
pub(crate) fn jump_to_entry(
    out: &OutboundHandle,
    entry_id: &ChatEntryId,
    jump_cancelled: &Arc<AtomicBool>,
) -> Reply {
    jump_cancelled.store(false, Ordering::Release);
    // One read of the history, so every page comes from the same instant.
    let guard = out.state().read();
    let session_id = guard.session.active_session_id().clone();
    let Some(session) = guard.session.get(&session_id) else {
        return refused_with("no such entry");
    };
    let Some(target) = session.find_entry_index_by_id(entry_id) else {
        return refused_with("no such entry");
    };
    // A pin in the newest page is already held: the page's tail runs from
    // that page's first entry to the end, so there is nothing to load, and
    // sending the page again would prepend a second copy of every entry in
    // it. The page scrolls on its own; the reply is empty.
    if target >= crate::page::tail(session.history()).entries_before {
        return Reply::none();
    }
    let windows = pages_up_to(session.history(), target + 1);
    drop(guard);
    // Marked before any frame is built, so a later reconcile never re-sends
    // a page this reply is about to deliver.
    for window in &windows {
        out.mark_delivered(&session_id, window);
    }
    let frames = windows
        .into_iter()
        .map(|window| {
            ServerMsg::EntriesPage(Box::new(EntriesPageBody {
                session_id: session_id.clone(),
                entries: window.entries,
                entries_before: window.entries_before,
            }))
        })
        .collect();
    Reply {
        frames,
        cancel_flag: Some(Arc::clone(jump_cancelled)),
    }
}

/// Stops the jump this connection asked for. Sets the cancel flag the
/// jump's reply was stamped with, so the reader stops sending its pages
/// — the ones already sent stay. A cancel that arrives after the jump
/// finished finds no reply to stop and does nothing.
pub(crate) fn cancel_jump(jump_cancelled: &Arc<AtomicBool>) -> Reply {
    jump_cancelled.store(true, Ordering::Release);
    Reply::none()
}

/// Every page from the start of the history up to (and including) index
/// `end`, oldest first: the run a jump streams, in the order the page
/// prepends them.
fn pages_up_to(history: &[ChatEntry], end: usize) -> Vec<crate::page::EntryWindow> {
    let mut windows = Vec::new();
    let mut window_end = end;
    while let Some(window) = crate::page::page_before(history, window_end) {
        window_end = window.entries_before;
        windows.push(window);
        if window_end == 0 {
            break;
        }
    }
    windows.reverse();
    windows
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
/// Incremental when the page's cursor is an entry of the active session,
/// its revision is the session's current one (nothing but appends can have
/// happened), **and** what came after the cursor fits one window: the
/// reply is then just those entries, as upserts — or, when the cursor is
/// the newest entry and nothing came after it, the one frame
/// [`ServerMsg::Current`], so the answer is never silence. Any other case
/// — a fresh page, a cursor the session no longer holds, a revision from
/// before a restart, or a gap larger than one window — answers with the
/// bounded tail as a [`ServerMsg::Snapshot`]. The page's view is replaced
/// wholesale there, so a wrong guess can never leave a gap, and a
/// reconnect can never replay an unbounded stretch of history.
pub fn hello(
    out: &OutboundHandle,
    last_entry_id: Option<&ChatEntryId>,
    revision: Option<u64>,
) -> Reply {
    let session_id = out.state().read().session.active_session_id().clone();
    let resumed = resume_window(
        out.state(),
        &session_id,
        last_entry_id,
        revision,
        out.revision(&session_id),
    );
    // The push key rides on every `hello` reply, after whatever the cursor
    // chose. It is a runtime fact — each instance generates its own key pair
    // — so a page can only learn it from the server it is talking to, and
    // the reply is the one moment the page is guaranteed to be listening.
    let frames = match resumed {
        Some(window) => {
            out.mark_delivered(&session_id, &window);
            let frames = upserts_of(out.state(), &session_id, &window);
            if frames.is_empty() {
                vec![ServerMsg::Current]
            } else {
                frames
            }
        }
        None => snapshot_reply(out).frames,
    };
    Reply {
        frames: frames
            .into_iter()
            .chain(std::iter::once(push_info(out)))
            .collect(),
        ..Reply::default()
    }
}

/// The frame a page subscribes with: whether this server can push, and the
/// key it signs with. `supported` is false when the data dir could not be
/// read or written, which the page reports as unavailable — a different
/// state from a page served over plain http.
fn push_info(out: &OutboundHandle) -> ServerMsg {
    let public_key = out.push().store().public_key();
    ServerMsg::PushInfo(PushInfoBody {
        supported: public_key.is_some(),
        public_key,
    })
}

/// The gap after the page's cursor, when a resume is safe.
///
/// `None` unless the cursor is held, the revision matches, and the gap
/// fits one window. The history is read once; the window is built from
/// that read and the revision checked against the tracker beside it.
fn resume_window(
    state: &jinn_kernel::State,
    session_id: &SessionId,
    last_entry_id: Option<&ChatEntryId>,
    revision: Option<u64>,
    current_revision: u64,
) -> Option<crate::page::EntryWindow> {
    let cursor = last_entry_id?;
    if revision != Some(current_revision) {
        return None;
    }
    let guard = state.read();
    let session = guard.session.get(session_id)?;
    let cursor_index = session.find_entry_index_by_id(cursor)?;
    crate::page::gap_after(session.history(), cursor_index)
}

/// One upsert per entry in a resume gap, each at its live index.
///
/// A gap is the run of entries at the very end of the history, so the
/// index of its *k*-th entry is the history's length minus the entries
/// after it — no id lookup needed.
fn upserts_of(
    state: &jinn_kernel::State,
    session_id: &SessionId,
    gap: &crate::page::EntryWindow,
) -> Vec<ServerMsg> {
    let guard = state.read();
    let Some(session) = guard.session.get(session_id) else {
        return Vec::new();
    };
    let first_index = session.history().len().saturating_sub(gap.entries.len());
    gap.entries
        .iter()
        .enumerate()
        .map(|(offset, envelope)| {
            ServerMsg::EntryUpsert(Box::new(EntryUpsertBody {
                session_id: session_id.clone(),
                index: first_index + offset,
                entry: envelope.clone(),
            }))
        })
        .collect()
}

/// Answers one page of entries older than the ones the page holds.
///
/// `before` names the oldest entry the page holds. It must be an entry of
/// the active session with something before it; anything else is refused,
/// since there is no older history to load for it.
pub fn more_entries(out: &OutboundHandle, before: &ChatEntryId) -> Reply {
    let guard = out.state().read();
    let session_id = guard.session.active_session_id().clone();
    let Some(session) = guard.session.get(&session_id) else {
        return refused_with("no older entries");
    };
    let Some(before_index) = session.find_entry_index_by_id(before) else {
        return refused_with("no older entries");
    };
    let Some(window) = crate::page::page_before(session.history(), before_index) else {
        return refused_with("no older entries");
    };
    drop(guard);
    out.mark_delivered(&session_id, &window);
    Reply::one(ServerMsg::EntriesPage(Box::new(EntriesPageBody {
        session_id,
        entries: window.entries,
        entries_before: window.entries_before,
    })))
}

/// The bounded-tail snapshot for the active session, recorded as delivered.
fn snapshot_reply(out: &OutboundHandle) -> Reply {
    let frame = snapshot::snapshot(
        out.state(),
        out.revisions(),
        out.settles(),
        &out.tearing_down(),
    );
    let session_id = frame.body.session_id.clone().unwrap_or_default();
    out.mark_delivered(&session_id, &frame.window);
    let crate::snapshot::SnapshotFrame { body, .. } = frame;
    let mut body = body;
    crate::outbound::with_status_figure(out, &mut body);
    Reply::one(ServerMsg::Snapshot(Box::new(body)))
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
/// Mirrors the terminal's own activation (`jinn_chat_log_view`'s
/// `activate_session`): the pointer moves at once, then one
/// `SessionLoadRequested` carries the switch to the store actor. The
/// terminal asks only when its line cache says the session is unmeasured;
/// the web has no line cache, so it always asks, and the store actor's
/// in-memory branch is a pointer move with no disk read.
///
/// Nothing is read and nothing is answered here. This slice holds no
/// session store: whether the id names a session is the store actor's to
/// decide, and the page's whole view of the switched-to session reaches it
/// through the outbound actor's `ActiveSessionChanged` snapshot, exactly as
/// a switch made in the terminal reaches every page.
pub async fn activate_session(out: &OutboundHandle, session_id: SessionId) -> Reply {
    {
        let mut guard = out.state().write();
        guard.session.begin_load(session_id.clone());
        guard.session.set_active(session_id.clone());
    }
    tracing::info!(
        session_id = %session_id,
        "webui: session load requested — the store actor takes it from here"
    );
    out.publish(SessionLoadRequested {
        session_id,
        content_width: None,
    })
    .await;
    Reply::none()
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
/// The Archive picker's rows.
///
/// Parity: the terminal's own session picker (`open_session_picker`),
/// message for message. Publishes the existing `LoadSessionPickerEntries`
/// and nothing else: the store actor reads the summaries, writes the rows
/// into the picker's cell, and announces `SessionPickerEntriesLoaded`. The
/// outbound actor turns that into the `PickerRows` frame, in the filter
/// recorded here — so the reply itself carries no frame.
///
/// This slice holds no session store, so nothing is read here. A publish
/// only enqueues: it does not wait for the store actor to handle the
/// command, so the cell cannot be read back in this handler, which is why
/// the rows arrive as a push.
pub async fn picker_rows(out: &OutboundHandle, filter: String) -> Reply {
    out.remember_picker_filter(&filter);
    out.publish(LoadSessionPickerEntries).await;
    Reply::none()
}

/// The stored-session picker's rows: the terminal's own tree filter over
/// the entries, each visible row with its depth and title runs.
///
/// Drives the same [`TreePickerState`] the terminal's picker cell holds:
/// with no filter every row shows in tree order; with one, each
/// whitespace-separated term must match a title, the ancestors of every
/// match stay in place above it, and the strongest tree ranks first. Only
/// the visible list is read back — the rendering the state also knows how
/// to do is the terminal's, not this surface's.
pub(crate) fn session_tree_rows(
    entries: Vec<SessionTreeEntry>,
    filter: &str,
) -> Vec<PickerRowBody> {
    let mut tree = jinn_selection_widget::TreePickerState::<SessionTreeEntry>::with_items(entries);
    if !filter.is_empty() {
        tree.insert_text(filter);
    }
    let rows = (0..tree.filtered_count())
        .filter_map(|idx| visible_row(&tree, idx))
        .collect();
    cap_whole_trees(rows, PICKER_ROW_CAP)
}

/// One visible row of the tree, as the browser draws it.
fn visible_row(
    tree: &jinn_selection_widget::TreePickerState<SessionTreeEntry>,
    idx: usize,
) -> Option<PickerRowBody> {
    let visible = tree.visible_entry(idx);
    let entry = tree.filtered_item(idx);
    let offsets = tree.filtered_match_indices(idx).unwrap_or(&[]);
    // `idx` is below `filtered_count`, so both are present; an out-of-range
    // read is not a row the page could draw.
    let (Some(entry), Some(visible)) = (entry, visible) else {
        return None;
    };
    let project = (!entry.project_display.is_empty()).then(|| entry.project_display.clone());
    PickerRowBody {
        session_id: entry.session_id.clone(),
        title: entry.title.clone(),
        state: entry.session_state,
        parent_id: entry.parent_id.clone(),
        project,
        depth: visible.depth,
        updated_at: entry.updated_at,
        title_runs: runs(jinn_selection_widget::split_matched_runs(
            &entry.title,
            offsets,
        )),
    }
    .into()
}

/// Keeps the first whole trees that fit in `cap` rows, in display order.
///
/// A depth-0 row starts a tree; the rows after it belong to it. Trees are
/// taken in order until the next one would not fit, so every tree the page
/// receives is complete — a parent is never shown without the children the
/// filter found under it. The one exception is a first tree larger than
/// the cap on its own: it is cut at the cap rather than left out.
pub(crate) fn cap_whole_trees(rows: Vec<PickerRowBody>, cap: usize) -> Vec<PickerRowBody> {
    let kept_end = whole_trees_end(&rows, cap);
    // A first tree too large for the cap is cut at the cap, not skipped.
    let kept_end = if kept_end == 0 && !rows.is_empty() {
        cap.min(rows.len())
    } else {
        kept_end
    };
    rows.into_iter().take(kept_end).collect()
}

/// The index just past the last whole tree that fits in `cap` rows.
fn whole_trees_end(rows: &[PickerRowBody], cap: usize) -> usize {
    let mut taken = 0;
    for (start, end) in trees(rows) {
        if taken + (end - start) > cap {
            return taken;
        }
        taken = end;
    }
    taken
}

/// Where each tree starts and ends in the display-ordered rows: a tree
/// begins at a depth-0 row and runs until the next one (or the end).
///
/// The walk starts at row 0, so rows ahead of the first root — should any
/// ever appear — are their own leading group rather than dropped from the
/// cap's count.
fn trees(rows: &[PickerRowBody]) -> Vec<(usize, usize)> {
    // Each root closes the group before it and opens the next; the end of
    // the rows closes the last.
    let starts = std::iter::once(0usize).chain(root_starts(rows).into_iter().filter(|&i| i > 0));
    starts
        .clone()
        .skip(1)
        .chain(std::iter::once(rows.len()))
        .zip(starts)
        .map(|(end, start)| (start, end))
        .collect()
}

/// The index of every depth-0 row, in display order.
fn root_starts(rows: &[PickerRowBody]) -> Vec<usize> {
    rows.iter()
        .enumerate()
        .filter_map(|(idx, row)| (row.depth == 0).then_some(idx))
        .collect()
}

/// The model picker's rows for a filter, from the shared row builder.
///
/// The same `load_provider_entries` → `sorted_entries` → `filter_entries`
/// path the terminal's picker runs, over the same search text it matches
/// by — so both surfaces list the same rows, in the
/// same score order, for the same filter. An empty filter keeps every row
/// in the sorted order, the active one promoted to the top.
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

    let sorted = {
        let registry = out.services().provider_registry.read();
        let api_keys = out.services().api_keys.read();
        let all = jinn_provider_selection_msg::load_provider_entries(
            &registry,
            &api_keys,
            model_cache.as_ref(),
            &theme,
        );
        jinn_provider_selection_msg::sorted_entries(&all, filter, &active)
    };
    jinn_provider_selection_msg::provider_rows::filter_entries(&sorted, filter)
        .into_iter()
        .map(|filtered| {
            let segments = filtered.segments();
            ModelRowBody {
                provider_id: filtered.entry.provider_id,
                model: filtered.entry.model,
                provider_name: filtered.entry.provider_name,
                is_available: filtered.entry.is_available,
                is_active: filtered.entry.is_active,
                model_runs: segments_to_runs(segments.model, |s| (s.text, s.matched)),
                provider_runs: segments_to_runs(segments.provider, |s| (s.text, s.matched)),
            }
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

// ---------------------------------------------------------------------------
// Prompt templates
// Parity: chat-input `intent.rs::handle_insert_char` (the `#` popup's list)
// ---------------------------------------------------------------------------

/// A session's prompt templates for the composer's `#` popup: name and
/// description, in the store's own order.
///
/// No filter and no ranking. The terminal's popup filters on each
/// keystroke, but this surface ships the whole list and the browser filters
/// it as the user types, so the server is asked nothing per keystroke and
/// one list serves every keystroke. The order is the discovery scan's — the
/// same list the terminal's popup shows for an empty filter.
pub(crate) fn prompt_template_bodies(
    session: &jinn_session_state::ChatSessionState,
) -> Vec<PromptTemplateBody> {
    session
        .discovered_prompt_templates()
        .templates()
        .iter()
        .map(|template| PromptTemplateBody {
            name: template.name.clone(),
            description: template.description.clone(),
        })
        .collect()
}

/// The shared filter's text runs, as the wire carries them.
fn runs(runs: Vec<jinn_selection_widget::TextRun>) -> Vec<TextRunBody> {
    segments_to_runs(runs, |run| (run.text, run.matched))
}

/// One contract crate's matched/plain segments, as the wire carries them.
///
/// The provider and persona filters each carry their own `TextSegment`
/// type — same shape, different crate — so the caller names how to read
/// one segment's text and matched flag, and this is the one conversion.
fn segments_to_runs<S>(segments: Vec<S>, part: impl Fn(S) -> (String, bool)) -> Vec<TextRunBody> {
    segments
        .into_iter()
        .map(|segment| {
            let (text, matched) = part(segment);
            TextRunBody { text, matched }
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
    // The dash's badges follow the click that opened the picker, not only
    // the next runtime event.
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
    jinn_persona_msg::filter_rows(&rows, filter)
        .into_iter()
        .map(|filtered| {
            let segments = filtered.segments();
            PersonaRowBody {
                name: filtered.row.name,
                description: filtered.row.description,
                is_active: filtered.row.is_active,
                name_runs: segments_to_runs(segments.name, |s| (s.text, s.matched)),
                description_runs: segments_to_runs(segments.description, |s| (s.text, s.matched)),
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
// Applet rows
// Same publish pair as the TUI `<leader>sa` picker's opener: the shared
// rescan/wake pair, so both surfaces launch an applet the same way.
// ---------------------------------------------------------------------------

/// The applets modal's rows for a filter, with the applets server's serving
/// phase.
///
/// Publishes the rescan and the wake the TUI's picker opener publishes — in
/// that order — then answers from the two applets cells as they stand. The
/// reply is deliberately not waited on: the store actor's rescan lands
/// strictly after it, and the outbound actor pushes the fresh catalog to the
/// open modal when it does. The filter is remembered so that push answers in
/// the view the page is holding.
pub async fn applet_rows(out: &OutboundHandle, filter: String) -> Reply {
    out.publish(jinn_applets_msg::RescanApplets).await;
    out.publish(jinn_applets_msg::WakeAppletsServer).await;
    out.remember_applet_filter(&filter);
    Reply::one(ServerMsg::AppletRows(applet_rows_body_from_cells(
        out, &filter,
    )))
}

/// The cells as they stand, in the filter asked. Both guards are opened,
/// cloned, and dropped inside the block — none crosses the caller.
pub(crate) fn applet_rows_body_from_cells(out: &OutboundHandle, filter: &str) -> AppletRowsBody {
    let (catalog, (serving, bind)) = {
        let catalog = out.applets_catalog_cell().read().clone();
        let serving = {
            let picker = out.applets_picker_cell().read();
            picker
                .serving
                .as_ref()
                .map_or((false, String::new()), |address| {
                    (true, address.bind.clone())
                })
        };
        (catalog, serving)
    };
    applet_rows_body(&catalog, serving, &bind, filter)
}

/// One saved applet as the wire carries it: the title split for the filter,
/// and the href the row opens.
///
/// The href is the applets slice's own URL formula — the resolved bind plus
/// `/applets/<id>/` with the trailing slash, so relative sibling references
/// in the artifact resolve — and only while the server is serving **and** the
/// id passes the validity gate. A hand-edited `applets.json` could carry a
/// traversal id; a refused id renders the disabled row instead of an href,
/// which is the modal's one injection surface.
#[must_use]
fn applet_row_body(
    entry: &jinn_applets_msg::AppletEntry,
    bind: Option<&str>,
    runs: Vec<TextRunBody>,
) -> AppletRowBody {
    let href = bind.and_then(|bind| {
        jinn_applets_msg::is_valid_app_id(&entry.id)
            .then(|| format!("http://{bind}/applets/{}/", entry.id))
    });
    AppletRowBody {
        id: entry.id.clone(),
        title: entry.title.clone(),
        title_runs: runs,
        href,
    }
}

/// The rows for the catalog under a filter, with the serving phase.
///
/// An empty filter keeps the catalog's own order — the store actor has
/// already sorted it newest first. Otherwise the shared fuzzy filter over the
/// titles orders by score, exactly the TUI picker's matcher; the matched
/// runs are cut on grapheme boundaries so a `<mark>` never splits a cluster.
pub(crate) fn applet_rows_body(
    catalog: &jinn_applets_msg::AppletsCatalog,
    serving: bool,
    bind: &str,
    filter: &str,
) -> AppletRowsBody {
    let rows: Vec<AppletRowBody> = if filter.is_empty() {
        catalog
            .entries
            .iter()
            .map(|entry| {
                let plain = vec![TextRunBody {
                    text: entry.title.clone(),
                    matched: false,
                }];
                applet_row_body(entry, serving.then_some(bind), plain)
            })
            .collect()
    } else {
        let titles: Vec<&str> = catalog.entries.iter().map(|e| e.title.as_str()).collect();
        jinn_selection_widget::fuzzy_filter(&titles, filter)
            .into_iter()
            .filter_map(|hit| {
                let entry = catalog.entries.get(hit.index)?;
                let segments =
                    jinn_selection_widget::split_matched_runs(&entry.title, &hit.match_indices);
                Some(applet_row_body(
                    entry,
                    serving.then_some(bind),
                    runs(segments),
                ))
            })
            .collect()
    };
    AppletRowsBody {
        filter: filter.to_owned(),
        serving,
        bind: if serving {
            bind.to_owned()
        } else {
            String::new()
        },
        rows,
    }
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

/// Sets a session's Library membership.
///
/// Parity: the sidebar's `L`, message for message. Publishes the existing
/// `SetSessionLibrary` command and nothing else: the store actor is the
/// single writer of the membership flag, mirrors it into the in-memory
/// session, and announces `SessionLibraryMembershipChanged`, which the
/// outbound actor turns into the session-list frame. Nothing is validated
/// here — this slice holds no session store, so whether the id is known is
/// the store actor's to decide.
pub async fn set_library(out: &OutboundHandle, session_id: SessionId, in_library: bool) -> Reply {
    out.publish(SetSessionLibrary {
        session_id,
        in_library,
    })
    .await;
    Reply::none()
}

/// Archives a session without running its teardown lifecycle.
///
/// Publishes the existing `ArchiveSession` command and nothing else: the
/// store actor writes the archived snapshot all-or-nothing, removes the
/// session, and announces `SessionRemoved`, `SessionArchived`, and — when
/// this was the session the user was reading — the successor's
/// `ActiveSessionChanged`. The outbound actor turns those into the sessions
/// frame and the snapshot that move every page, so the reply itself carries
/// no frame: a TUI-initiated archive reaches the page the same way.
///
/// This slice holds no session store, so the command is published whether
/// the id names a live session, an archived one, or nothing: the store
/// actor is the one authority on what the store holds, and its own
/// `SessionArchiveFailed` reaches the page when the archive cannot run.
pub async fn archive_session(out: &OutboundHandle, session_id: SessionId) -> Reply {
    if session_is_working(out, &session_id) {
        return refused(&Refusal::ArchiveSessionBusy);
    }
    out.publish(ArchiveSession { session_id }).await;
    Reply::none()
}

/// Closes a session the way the terminal's `x` does: runs its teardown
/// lifecycle, then archives it — or archives it at once when no teardown is
/// due.
///
/// Parity: the sidebar's `x`
/// (`jinn_sidebar`'s `handle_session_close_with_lifecycle`), message for
/// message. Publishes the existing `CloseSession` command and nothing else:
/// the lifecycle actor owns the teardown, the archive, the fallback when
/// nothing is due, and a failed teardown (the session then stays open), so
/// this slice adds no disposal logic of its own and the two surfaces cannot
/// drift. Nothing is replied — every later fact reaches the page through
/// the outbound actor's pushes, exactly as a TUI-initiated close would. In
/// particular the fallback archives silently: a toast would need a
/// server-to-page message the terminal has no counterpart for.
///
/// A session running a turn is refused with the same sentence
/// `archive_session` refuses with — one busy refusal cannot disagree with
/// itself.
pub async fn teardown_session(out: &OutboundHandle, session_id: SessionId) -> Reply {
    if session_is_working(out, &session_id) {
        return refused(&Refusal::ArchiveSessionBusy);
    }
    out.publish(jinn_session_lifecycle_msg::CloseSession { session_id })
        .await;
    Reply::none()
}

/// Whether the named session is in memory and running a turn — the
/// terminal's own `validate_session_close` test, on the session the page
/// named rather than the sidebar's selection. A session not in memory is
/// not busy: the store actor is the one authority on what the store holds.
fn session_is_working(out: &OutboundHandle, session_id: &SessionId) -> bool {
    out.state()
        .read()
        .session
        .get(session_id)
        .is_some_and(|session| session.phase().is_working())
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
// Pin / Fork
// Parity: the terminal's `p` on the selected entry
// (`jinn_chat_log_view::chat_entry_selection::intent::handle_pin_selected`)
// and its `f` on the selected entry
// (`jinn_chat_log_view::chat_entry_selection::intent::handle_fork_from_entry`)
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

/// Forks the active session at one entry — the terminal's `f` on the selected
/// entry (`jinn_chat_log_view::chat_entry_selection::intent::handle_fork_from_entry`).
///
/// The `at_ordinal` is the entry's index among the session's **persisted**
/// entries — transients excluded — because that is the list the store slices:
/// a fork is cut from the source's snapshot, which drops transients. Indexing
/// the live history instead would count a transient (skill discovery's table,
/// a model refresh's summary) ahead of the entry and copy history the user
/// forked past. The loading guard is armed for the source exactly as the
/// terminal arms it: the store actor re-points it at the child. Nothing is
/// replied; the child reaches every page through the `ActiveSessionChanged`
/// snapshot its restore publishes.
pub async fn fork(out: &OutboundHandle, entry_id: ChatEntryId) -> Reply {
    // Decided under one write guard, which drops before the publish: the
    // state lock is never held across an await.
    let decision = {
        let state = out.state();
        let mut guard = state.write();
        let session_id = guard.session.active_session_id().clone();
        let Some(session) = guard.session.get(&session_id) else {
            return refused(&Refusal::NoActiveSession);
        };
        let Some(at_ordinal) = persisted_ordinal(session, &entry_id) else {
            return refused(&Refusal::UnknownEntry);
        };
        guard.session.begin_load(session_id.clone());
        (session_id, at_ordinal)
    };
    let (source_session_id, at_ordinal) = decision;
    out.publish(SessionForkRequested {
        source_session_id,
        at_ordinal,
    })
    .await;
    Reply::none()
}

/// The entry's index among the session's non-transient entries, or `None`
/// when the session holds no entry with that id.
///
/// This is the ordinal a fork is cut at: `SessionSnapshot` is built from the
/// live history with every `Transient` removed, and `forked_from` takes
/// `entries[..=at_ordinal]` of that list. A transient is never the answer —
/// the page cannot name one — so a transient id resolves to `None`.
fn persisted_ordinal(
    session: &jinn_kernel::ChatSessionState,
    entry_id: &ChatEntryId,
) -> Option<usize> {
    session
        .history()
        .iter()
        .filter(|entry| !matches!(entry.kind, jinn_core_types::ChatEntryKind::Transient(_)))
        .position(|entry| entry.id == *entry_id)
}

// ---------------------------------------------------------------------------
// Context toggle
// Parity: the terminal's `x` on the selected entry
// (`jinn_chat_log_view::chat_entry_selection::intent::handle_ignore_selected`)
// ---------------------------------------------------------------------------

/// Flips one entry of the active session between forced-include and
/// forced-exclude.
///
/// A forced entry flips to the other forced value; an entry still at the
/// default flips its *effective* state, exactly as the terminal's `x`
/// does — an in-context entry is forced out, a kind that is out of context
/// by default is forced in. The result is always a forced value: nothing
/// here returns an entry to the default, which is the terminal's `r`. The
/// page can only name an entry it was sent, so an unknown id is refused and
/// never falls back to the store. A pinned entry is refused too, because a
/// pin overrides exclusion and the write would silently do nothing. One
/// message is published and nothing is replied; the session actor applies
/// the override, persists the session, and publishes
/// `ContextOverrideChanged`, which the outbound actor turns into the
/// upserts the page sees.
pub async fn toggle_context(out: &OutboundHandle, entry_id: ChatEntryId) -> Reply {
    // Decided under one read guard, which drops before the publish: the
    // state lock is never held across an await.
    let decision = {
        let state = out.state();
        let guard = state.read();
        let session_id = guard.session.active_session_id().clone();
        let Some(session) = guard.session.get(&session_id) else {
            return refused(&Refusal::NoActiveSession);
        };
        let Some(entry) = session
            .find_entry_index_by_id(&entry_id)
            .and_then(|index| session.history().get(index))
        else {
            return refused(&Refusal::UnknownEntry);
        };
        if entry.is_pinned() {
            return refused(&Refusal::EntryIsPinned);
        }
        let target = match entry.context_override() {
            ContextOverride::ForcedInclude => ContextOverride::ForcedExclude,
            ContextOverride::Default if entry.is_in_context() => ContextOverride::ForcedExclude,
            ContextOverride::ForcedExclude | ContextOverride::Default => {
                ContextOverride::ForcedInclude
            }
        };
        (session_id, target)
    };
    let (session_id, target) = decision;
    out.publish(SetEntryContextOverride {
        session_id,
        entry_id,
        value: target,
    })
    .await;
    Reply::none()
}

// ---------------------------------------------------------------------------
// New session
// The same creation path as the TUI project/lifecycle pickers' confirm
// and Discord `/new`
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
/// Validates against the config layer (the project must name a configured
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
/// The session is created active instance-wide, and the handler announces
/// that the pointer moved with the one `ActiveSessionChanged` every
/// activation path publishes — the kernel's intent handler publishes it for
/// the terminal's own new-session action, and this path runs the shared
/// creation outside that handler, so it publishes the same fact itself. The
/// outbound actor turns it into the snapshot every page follows, so nothing
/// is replied here: the asking page is answered by the same push every other
/// page is.
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
    out.publish(jinn_kernel::protocol::system::ActiveSessionChanged {
        session_id: created,
    })
    .await;
    Reply::none()
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

/// Registers or refreshes a browser's push subscription.
///
/// Replaces any row with the same endpoint, so the subscription a page
/// re-sends on every load is the one that works now — a row made under
/// another key or another origin never outlives the load that replaced it.
/// Nothing is published and nothing is answered: the page asked, the server
/// holds it, and the next settle reaches that browser.
pub fn push_subscribe(out: &OutboundHandle, endpoint: String, keys: PushKeysBody) -> Reply {
    // The endpoint is a bearer capability: it is stored, never logged.
    out.push()
        .store()
        .add(crate::push::store::Subscription { endpoint, keys });
    Reply::none()
}

/// Drops a browser's push subscription: the page asked to stop receiving
/// notifications. An endpoint the server does not hold is not an error —
/// the outcome the page asked for already holds.
pub fn push_unsubscribe(out: &OutboundHandle, endpoint: &str) -> Reply {
    out.push().store().remove(endpoint);
    Reply::none()
}

/// A notification was tapped: the session it names becomes the instance's
/// active one, exactly as a click on its rail row does — so the terminal
/// switches too, and one tap takes every surface to the message.
pub async fn notification_opened(out: &OutboundHandle, session_id: SessionId) -> Reply {
    activate_session(out, session_id).await
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
