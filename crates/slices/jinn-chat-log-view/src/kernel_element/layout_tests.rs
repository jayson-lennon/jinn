//! Tests for the off-thread chat log layout: the worker measures a session's
//! history, and the completion actor decides whether those counts may be used.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "test code"
)]

use std::collections::HashSet;
use std::sync::Arc;

use jinn_chat_log_view_msg::{ChatLogLayoutComputed, LayoutChatSession, MeasuredEntryCount};
use jinn_core_types::{ChatEntry, SessionId};
use jinn_session_state::ChatSessionState;

use crate::kernel_element::layout_complete::LayoutCompletionActorDeps;
use crate::kernel_element::layout_complete::{LayoutApplied, LayoutCompletionActor};
use crate::kernel_element::layout_supervisor::{LayoutSupervisorActor, LayoutSupervisorActorDeps};
use crate::kernel_element::layout_worker::{MeasureJob, measure, render_preview};
use jinn_kernel::common::app_state::AppState;
use jinn_kernel::common::state::State;

/// State with `count` user entries in its active session, measured at
/// `content_width`.
///
/// The width is published into the session's view state because the chat log
/// does that on every frame, and the completion actor compares a result against
/// it.
fn state_with_entries(count: usize, content_width: u16) -> (State, SessionId) {
    let session_id = SessionId::new();
    let mut session = ChatSessionState::new();
    session.set_session_id(session_id.clone());
    for index in 0..count {
        session.push_entry(ChatEntry::user(format!("message {index}")));
    }
    // Attached: the line cache is a jinn-chat-log-view cell, and this test
    // asserts on the counts it stores.
    let mut state = AppState::default_with_scope_focus();
    state.session.insert(session);
    state.session.set_active(session_id.clone());
    // Set after insertion: the view cell is keyed by session id, so a width
    // written to the session before it joins the map would land in the
    // pre-attach fallback and read back as zero.
    // `get` attaches the registry to the session as a side effect, so the
    // write below lands in the cell the rest of the test reads.
    state
        .session
        .get(&session_id)
        .expect("just inserted")
        .set_content_width(content_width);
    (State::new(state), session_id)
}

/// A layout job over a session's own history.
fn job_for(state: &State, session_id: &SessionId, content_width: u16) -> LayoutChatSession {
    let session = state.read();
    let active = session.session.get(session_id).expect("active session");
    LayoutChatSession {
        session_id: session_id.clone(),
        content_width,
        entries: Arc::from(active.history().to_vec()),
        shown_ignored_blocks: HashSet::new(),
        min_collapse_count: jinn_chat_log_view_msg::DEFAULT_MIN_COLLAPSE_COUNT,
        tool_entry_max_lines: 6,
    }
}

/// The layout inputs a job would be measured with.
fn inputs_for(state: &State, session_id: &SessionId) -> super::history::LayoutInputs {
    super::history::LayoutInputs::snapshot(&state.read(), session_id)
}

#[rstest::rstest]
fn a_shared_history_measures_the_same_counts_a_copied_one_does() {
    // Given a job whose history is shared rather than copied per worker.
    let (state, session_id) = state_with_entries(6, 50);
    let shared = job_for(&state, &session_id, 50);
    let inputs = inputs_for(&state, &session_id);

    // When the same history is measured as a plain owned vector.
    let mut owned = shared.clone();
    owned.entries = shared.entries.to_vec().into();
    let from_shared = measure(&MeasureJob::from(&shared), &inputs);
    let from_owned = measure(&MeasureJob::from(&owned), &inputs);

    // Then both paths agree on every entry's identity and count.
    let shared_rows: Vec<_> = from_shared
        .iter()
        .map(|c| (c.id.clone(), c.wrapped_count))
        .collect();
    let owned_rows: Vec<_> = from_owned
        .iter()
        .map(|c| (c.id.clone(), c.wrapped_count))
        .collect();
    assert_eq!(
        shared_rows, owned_rows,
        "sharing the history must not change what is measured"
    );
}

#[rstest::rstest]
fn cloning_a_layout_job_shares_the_history_instead_of_copying_it() {
    // Given a layout job carrying a shared history.
    let (state, session_id) = state_with_entries(4, 60);
    let job = job_for(&state, &session_id, 60);

    // When the job is cloned.
    let clone = job.clone();

    // Then both jobs point at one entry buffer.
    assert!(
        Arc::ptr_eq(&job.entries, &clone.entries),
        "a cloned job must share the history, not transcribe it"
    );
}

#[rstest::rstest]
fn a_measure_job_shares_the_history_it_was_built_from() {
    // Given a layout job, and the measure job handed to a blocking thread.
    let (state, session_id) = state_with_entries(4, 60);
    let msg = job_for(&state, &session_id, 60);

    // When the job is detached for measurement.
    let job = MeasureJob::from(&msg);

    // Then the measure job reads the same entry buffer the message carries.
    assert!(
        Arc::ptr_eq(&msg.entries, &job.entries),
        "handing work to a blocking thread must share the history, not copy it"
    );
}

#[rstest::rstest]
fn a_measure_job_carries_the_measurement_settings_the_message_had() {
    // Given a layout job at a known width and collapse threshold.
    let (state, session_id) = state_with_entries(4, 47);
    let mut msg = job_for(&state, &session_id, 47);
    msg.min_collapse_count = 9;
    msg.tool_entry_max_lines = 3;

    // When the job is detached for measurement.
    let job = MeasureJob::from(&msg);

    // Then the settings the measurement depends on survive the hand-off.
    assert_eq!(job.content_width, 47);
    // And the collapse threshold is carried across unchanged.
    assert_eq!(job.min_collapse_count, 9);
    // And so is the truncation depth.
    assert_eq!(job.tool_entry_max_lines, 3);
}

#[rstest::rstest]
fn a_layout_job_measures_every_entry() {
    // Given a session with several entries.
    let (state, session_id) = state_with_entries(5, 60);
    let job = job_for(&state, &session_id, 60);
    let inputs = inputs_for(&state, &session_id);

    // When the job is measured.
    let measured = measure(&MeasureJob::from(&job), &inputs);

    // Then every entry has a line count.
    assert_eq!(measured.len(), 5);
    // And every count is at least one line.
    assert!(
        measured.iter().all(|count| count.wrapped_count >= 1),
        "every rendered entry occupies at least one line"
    );
}

#[rstest::rstest]
fn a_layout_job_measures_the_same_counts_the_renderer_would() {
    // Given a session and a job measured at a width.
    let (state, session_id) = state_with_entries(4, 40);
    let job = job_for(&state, &session_id, 40);
    let inputs = inputs_for(&state, &session_id);

    // When the job is measured.
    let measured = measure(&MeasureJob::from(&job), &inputs);

    // Then the counts come back in the session's own entry order.
    let ids: Vec<_> = measured.iter().map(|count| count.id.clone()).collect();
    let history_ids: Vec<_> = state
        .read()
        .session
        .get(&session_id)
        .expect("active session")
        .history()
        .iter()
        .map(|entry| entry.id.clone())
        .collect();
    assert_eq!(ids, history_ids);
}

#[rstest::rstest]
fn a_layout_job_counts_the_lines_the_markdown_renderer_produced() {
    // Given a session with one multi-line entry.
    let session_id = SessionId::new();
    let mut session = ChatSessionState::new();
    session.set_session_id(session_id.clone());
    session.push_entry(ChatEntry::user("a line\nanother line\nand a third"));
    let mut app = AppState::default();
    app.session.insert(session);
    app.session.set_active(session_id.clone());
    let state = State::new(app);
    let job = job_for(&state, &session_id, 0);
    let inputs = inputs_for(&state, &session_id);

    // When the job is measured at width zero, where nothing is re-wrapped.
    let measured = measure(&MeasureJob::from(&job), &inputs);

    // Then the count is the entry's own line count, never a wrapped one —
    // a width of zero would otherwise report a single line for everything.
    assert_eq!(measured.len(), 1);
    assert!(
        measured[0].wrapped_count > 1,
        "a multi-line entry must not collapse to one line, got {}",
        measured[0].wrapped_count
    );
}

#[rstest::rstest]
fn a_completed_layout_stores_the_counts_and_ends_the_load() {
    // Given a loading session with a measured result.
    let (state, session_id) = state_with_entries(3, 60);
    {
        let mut guard = state.write();
        guard.session.begin_load(session_id.clone());
    }
    let job = job_for(&state, &session_id, 60);
    let inputs = inputs_for(&state, &session_id);
    let measured = measure(&MeasureJob::from(&job), &inputs);
    let actor = LayoutCompletionActor::spawnless(LayoutCompletionActorDeps {
        state: state.clone(),
    });

    // When the result is applied.
    let applied = actor.apply(&computed(&session_id, 60, measured));

    // Then the counts were used.
    assert_eq!(applied, LayoutApplied::Applied);
    // And the load guard is cleared.
    assert!(!state.read().session.is_loading());
}

#[rstest::rstest]
fn a_completed_layout_clears_the_load_even_at_a_stale_width() {
    // Given a loading session and a result measured at the wrong width.
    let (state, session_id) = state_with_entries(2, 60);
    {
        let mut guard = state.write();
        guard.session.begin_load(session_id.clone());
    }
    let job = job_for(&state, &session_id, 60);
    let inputs = inputs_for(&state, &session_id);
    let measured = measure(&MeasureJob::from(&job), &inputs);
    let actor = LayoutCompletionActor::spawnless(LayoutCompletionActorDeps {
        state: state.clone(),
    });

    // When the result is applied at a width the chat log is not using.
    let applied = actor.apply(&computed(&session_id, 999, measured));

    // Then the counts were not used.
    assert_eq!(applied, LayoutApplied::DiscardedStaleWidth);
    // But the load guard is still cleared, so the user is not stranded.
    assert!(!state.read().session.is_loading());
}

#[rstest::rstest]
fn a_result_for_a_session_that_has_never_rendered_is_used() {
    // Given a session that has just been loaded and has therefore never
    // painted a frame, so it records no width of its own.
    let (state, session_id) = state_with_entries(2, 60);
    {
        // Forget the width, as a session that has not painted a frame would be.
        state
            .write()
            .session
            .get_mut(&session_id)
            .expect("active session")
            .set_content_width(0);
    }
    assert_eq!(
        state
            .read()
            .session
            .get(&session_id)
            .unwrap()
            .content_width(),
        0,
        "fixture must start with a never-rendered session"
    );
    let job = job_for(&state, &session_id, 60);
    let inputs = inputs_for(&state, &session_id);
    let measured = measure(&MeasureJob::from(&job), &inputs);
    let actor = LayoutCompletionActor::spawnless(LayoutCompletionActorDeps {
        state: state.clone(),
    });

    // When a result measured at the width the chat log is about to render at
    // is applied.
    let applied = actor.apply(&computed(&session_id, 60, measured));

    // Then the counts are used rather than discarded as stale.
    assert_eq!(applied, LayoutApplied::Applied);
    // And the session's own width is still unset, so the next frame is the
    // one that records it.
    assert_eq!(
        state
            .read()
            .session
            .get(&session_id)
            .unwrap()
            .content_width(),
        0
    );
}

#[rstest::rstest]
fn a_result_for_an_inactive_session_is_discarded() {
    // Given a loading session and a result for some other session.
    let (state, session_id) = state_with_entries(2, 60);
    {
        let mut guard = state.write();
        guard.session.begin_load(session_id.clone());
    }
    let other = SessionId::new();
    let job = job_for(&state, &session_id, 60);
    let inputs = inputs_for(&state, &session_id);
    let measured = measure(&MeasureJob::from(&job), &inputs);
    let actor = LayoutCompletionActor::spawnless(LayoutCompletionActorDeps {
        state: state.clone(),
    });

    // When that other session's result is applied.
    let applied = actor.apply(&computed(&other, 60, measured));

    // Then it is discarded.
    assert_eq!(applied, LayoutApplied::DiscardedInactive);
    // And the loading session's own guard is untouched.
    assert!(state.read().session.is_loading());
}

#[rstest::rstest]
fn a_stored_layout_survives_the_first_render_without_re_measuring() {
    // Given a session whose counts have been stored by a completed layout.
    let (state, session_id) = state_with_entries(4, 40);
    {
        let mut guard = state.write();
        guard.session.begin_load(session_id.clone());
    }
    let job = job_for(&state, &session_id, 40);
    let inputs = inputs_for(&state, &session_id);
    let measured = measure(&MeasureJob::from(&job), &inputs);
    // Kept so the frame below can probe with the very keys the measurement
    // stored — that is what proves the count is findable.
    let keys: Vec<_> = measured
        .iter()
        .map(|count| (count.is_expanded, count.variant, count.wrapped_count))
        .collect();
    let widths_before = fingerprint_computations(&state);
    let actor = LayoutCompletionActor::spawnless(LayoutCompletionActorDeps {
        state: state.clone(),
    });
    actor.apply(&computed(&session_id, 40, measured));

    // When the chat log lays out at the same width.
    let session = state.read();
    let active = session.session.get(&session_id).expect("active session");
    let history: Vec<_> = active.history().to_vec();
    let hits = {
        let cache = session
            .frontend
            .line_cache_cell()
            .expect("catalog registered the line-cache cell");
        history
            .iter()
            .zip(keys.iter())
            .map(|(entry, (is_expanded, variant, wrapped_count))| {
                cache.update(|cache| {
                    cache
                        .get(entry, *is_expanded, *variant, 40)
                        .map(|hit| hit.wrapped_count)
                }) == Some(*wrapped_count)
            })
            .collect::<Vec<_>>()
    };
    drop(session);

    // Then every entry is already counted — the frame does no work.
    assert!(
        hits.iter().all(|hit| *hit),
        "the first frame after a layout must find every count in the cache"
    );
    // And no entry was hashed again to get there.
    assert_eq!(
        fingerprint_computations(&state),
        widths_before,
        "a stored layout must not be re-hashed to be used"
    );
}

/// How many content fingerprints the line cache has computed.
fn fingerprint_computations(state: &State) -> u64 {
    state
        .read()
        .frontend
        .line_cache_cell()
        .expect("catalog registered the line-cache cell")
        .read()
        .fingerprint_computations()
}

/// Wraps measured counts in the result message the completion actor consumes.
fn computed(
    session_id: &SessionId,
    content_width: u16,
    measured: Vec<jinn_chat_log_view_msg::MeasuredLineCount>,
) -> ChatLogLayoutComputed {
    ChatLogLayoutComputed {
        session_id: session_id.clone(),
        content_width,
        counts: measured
            .into_iter()
            .map(|count| MeasuredEntryCount {
                entry_id: count.id,
                signature: count.content.signature,
                fingerprint: count.content.fingerprint,
                is_expanded: count.is_expanded,
                variant: count.variant,
                wrapped_count: count.wrapped_count,
            })
            .collect(),
    }
}

/// State with a session that is mid-load, and a supervisor watching it.
fn loading_state_with_supervisor() -> (State, SessionId, LayoutSupervisorActor) {
    let (state, session_id) = state_with_entries(2, 60);
    {
        let mut guard = state.write();
        guard.session.begin_load(session_id.clone());
    }
    let system = trouper::system::ActorSystem::new(trouper::system::SystemConfig::production());
    let supervisor = LayoutSupervisorActor::spawnless(LayoutSupervisorActorDeps {
        state: state.clone(),
        system,
    });
    (state, session_id, supervisor)
}

#[rstest::rstest]
fn an_expired_layout_deadline_ends_the_loading_indicator() {
    // Given a session that is still loading.
    let (state, session_id, supervisor) = loading_state_with_supervisor();
    assert!(state.read().session.is_loading());

    // When the layout deadline expires.
    supervisor.release_guard(&session_id, "layout deadline expired");

    // Then the loading indicator is released.
    assert!(
        !state.read().session.is_loading(),
        "an abandoned measurement must not strand the user behind a spinner"
    );
}

#[rstest::rstest]
fn a_deadline_for_another_session_does_not_release_the_loading_guard() {
    // Given a session that is loading.
    let (state, session_id, supervisor) = loading_state_with_supervisor();
    // And a different session whose deadline is still ticking — the session
    // the user switched away from.
    let departed = SessionId::new();

    // When that other session's deadline expires.
    supervisor.release_guard(&departed, "layout deadline expired");

    // Then the loading session keeps its guard.
    assert!(
        state.read().session.is_loading(),
        "a stale deadline must not release the session that is loading now"
    );
    // And the guard still names the session that is actually loading.
    assert_eq!(
        state
            .read()
            .session
            .session_load_guard()
            .map(|g| &g.session_id),
        Some(&session_id)
    );
}

#[rstest::rstest]
fn a_deadline_for_another_session_does_not_end_the_loading_indicator() {
    // Given a session that is still loading.
    let (state, _session_id, supervisor) = loading_state_with_supervisor();

    // When a stale deadline for a different session expires.
    supervisor.release_guard(&SessionId::new(), "layout deadline expired");

    // Then the user is still behind the loading indicator for the session
    // that is genuinely being measured.
    assert!(
        state.read().session.is_loading(),
        "releasing on a stale id would drop the spinner and send the next \
         frame back to measuring the whole history inline"
    );
}

#[rstest::rstest]
fn an_expired_layout_deadline_writes_nothing_into_the_conversation() {
    // Given a session that is still loading.
    let (state, session_id, supervisor) = loading_state_with_supervisor();

    // When the layout deadline expires.
    supervisor.release_guard(&session_id, "layout deadline expired");

    // Then no entry was added — a measurement that could not be taken off
    // the main thread is not a conversation event.
    assert_eq!(
        state
            .read()
            .session
            .get(&session_id)
            .expect("active session")
            .history()
            .len(),
        2
    );
}

// ---------------------------------------------------------------------------
// Preview rendering — the shared entry-to-lines arithmetic, narrowed
// ---------------------------------------------------------------------------

/// A preview render context at `content_width`, with every per-entry render
/// input pinned off — which is exactly how the sidebar's preview builds it.
fn preview_ctx(content_width: u16) -> crate::chat_log::RenderContext {
    use crate::chat_log::EntryGraphics;
    crate::chat_log::RenderContext {
        content_width,
        is_selected: false,
        is_expanded: false,
        tool_entry_max_lines: 6,
        theme: jinn_theme::default_theme(),
        paired_status: None,
        is_streaming: false,
        is_waiting_on_subagent: false,
        graphics: EntryGraphics::default(),
        session_id: jinn_core_types::SessionId::new(),
    }
}

/// A history of `count` user entries, each one line at a wide width.
fn preview_entries(count: usize) -> Vec<ChatEntry> {
    (0..count)
        .map(|index| ChatEntry::user(format!("entry {index}")))
        .collect()
}

/// The plain text of a rendered line, spans concatenated.
fn line_text(line: &ratatui::text::Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

/// The non-blank text of the rendered lines, in order.
///
/// [`entry_to_lines`] pads each entry with blank lines and each span out to the
/// content width, so both are stripped here: what these tests care about is
/// *which entries* contributed and in what order.
fn visible_text(lines: &[ratatui::text::Line<'_>]) -> Vec<String> {
    lines
        .iter()
        .map(line_text)
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
        .collect()
}

#[rstest::rstest]
fn render_preview_keeps_only_the_last_max_lines() {
    // Given a history whose last 5 entries render to more than 20 lines.
    let entries = preview_entries(50);

    // When rendering a preview that keeps at most 5 lines.
    let lines = render_preview(&entries, &preview_ctx(120), 5, 5);

    // Then the result is exactly the trailing 5 lines, in order.
    let rendered: Vec<String> = lines.iter().map(line_text).collect();
    assert_eq!(rendered.len(), 5, "expected 5 lines, got {rendered:?}");
    let visible = visible_text(&lines);
    assert_eq!(
        visible,
        vec!["entry 48", "entry 49"],
        "truncation must drop from the front so the newest text survives"
    );
}

#[rstest::rstest]
fn render_preview_reads_at_most_max_entries() {
    // Given a history of 50 entries.
    let entries = preview_entries(50);

    // When rendering a preview that reads at most 5 entries.
    let lines = render_preview(&entries, &preview_ctx(120), 5, 1000);

    // Then nothing from before the 5th-from-last entry contributed.
    assert_eq!(
        visible_text(&lines),
        vec!["entry 45", "entry 46", "entry 47", "entry 48", "entry 49"],
        "a preview is bounded to its trailing entries, not the whole history"
    );
}

/// An assistant entry still receiving tokens.
///
/// Unfinished by construction: a `Streamed` timing with no `finished_at`, which
/// is what `ensure_assistant_entry` produces for every token of a live reply.
/// There is no second notion of "in progress" to set up — the entry's own timing
/// is the one the request path and this worker both read.
fn streaming_assistant(text: &str) -> ChatEntry {
    let mut entry = ChatEntry::assistant(text);
    entry.timing = jinn_core_types::entry_timing::EntryTiming::streamed(jiff::Timestamp::now());
    entry
}

#[rstest::rstest]
fn an_entry_in_production_is_never_rendered() {
    // Given a history whose newest entry is still accumulating tokens.
    let mut entries = preview_entries(2);
    entries.push(streaming_assistant("half a sentence that is still going"));

    // When the worker renders its preview.
    let lines = render_preview(&entries, &preview_ctx(120), 5, 100);

    // Then no line is a *rendered* copy of that entry. A rendered entry goes
    // through the markdown renderer and is padded out to the full content width;
    // a marker is plain text at its own natural width. So the absence of any
    // full-width line carrying the text is what proves the entry was never
    // handed to the markdown render — the single thing that must not happen
    // again on every token.
    let rendered_copy = lines
        .iter()
        .any(|line| line_text(line).contains("half a sentence") && line.width() == 120);
    assert!(
        !rendered_copy,
        "an entry in production must not be markdown-rendered, got {:?}",
        lines.iter().map(line_text).collect::<Vec<_>>()
    );
}

#[rstest::rstest]
fn an_entry_in_production_is_shown_as_a_continuation_marker() {
    // Given a history whose newest entry is still accumulating tokens.
    let mut entries = preview_entries(2);
    entries.push(streaming_assistant("half a sentence that is still going"));

    // When the worker renders its preview.
    let visible = visible_text(&render_preview(&entries, &preview_ctx(120), 5, 100));

    // Then a marker stands at its position, carrying the tail of what it has
    // produced so far, and the settled entries are still there beside it. Text
    // short enough to fit the marker's column bound gets no ellipses — there is
    // nothing cut to say.
    assert_eq!(
        visible,
        vec!["entry 0", "entry 1", "half a sentence that is still going"],
        "an in-production entry must be visible as a marker, not rendered or hidden"
    );
}

#[rstest::rstest]
fn a_marker_says_where_its_text_was_cut() {
    // Given a reply long enough that its tail cannot fit the marker's column
    // bound, so text is dropped from the front *and* the marker is cut short.
    let entries = vec![streaming_assistant(&format!(
        "BEGINNING {}",
        "x".repeat(jinn_chat_log_view_msg::PREVIEW_MARKER_COLUMNS * 3)
    ))];

    // When the worker renders its preview.
    let visible = visible_text(&render_preview(&entries, &preview_ctx(40), 5, 1000));

    // Then the marker opens with `…`, saying the reply started earlier than this.
    // Without it a marker would read as the whole reply rather than its tail.
    assert!(
        visible
            .first()
            .is_some_and(|line| line.starts_with('\u{2026}')),
        "a marker that dropped text from the front must say so, got {:?}",
        visible.first()
    );
}

#[rstest::rstest]
fn a_marker_with_no_text_yet_is_a_single_ellipsis() {
    // Given a history whose newest entry has been created but has produced
    // nothing — the window between `begin_streaming` and the first token.
    let mut entries = preview_entries(1);
    entries.push(streaming_assistant(""));

    // When the worker renders its preview.
    let visible = visible_text(&render_preview(&entries, &preview_ctx(120), 5, 100));

    // Then the marker is one `…`. A zero-line marker would make an entry that
    // exists completely invisible, and a session that has only just begun would
    // preview as blank.
    assert_eq!(
        visible,
        vec!["entry 0", "…"],
        "an entry in production with no text must still show a marker"
    );
}

#[rstest::rstest]
fn a_marker_is_bounded_by_its_column_budget() {
    // Given a reply far longer than the marker's column budget, at a content
    // width wide enough that the column bound is reached before the row bound.
    let entries = vec![streaming_assistant(&"x".repeat(100_000))];

    // When the worker renders its preview.
    let lines = render_preview(&entries, &preview_ctx(40), 5, 1000);

    // Then the marker takes only the rows its own 256 columns wrap to at a width
    // of 40 — not a row per 40 columns of a hundred-kilobyte reply, which is
    // where the bound does its work.
    let rows = jinn_chat_log_view_msg::PREVIEW_MARKER_COLUMNS.div_ceil(40);
    assert_eq!(
        lines.len(),
        rows.min(jinn_chat_log_view_msg::PREVIEW_MARKER_MAX_ROWS),
        "a marker must stop at whichever bound binds first, got {} rows",
        lines.len()
    );
}

#[rstest::rstest]
fn a_marker_is_bounded_to_its_row_budget() {
    // Given a reply far longer than the marker's column budget, at the narrowest
    // content width a preview can have — where the column budget would wrap to
    // more rows than the row budget allows.
    let entries = vec![streaming_assistant(&"x".repeat(100_000))];

    // When the worker renders its preview.
    let lines = render_preview(&entries, &preview_ctx(28), 5, 1000);

    // Then the row budget is what binds, so the marker never occupies more than
    // its share of the popup. A reply in production must not be able to take the
    // rows the settled entries need.
    assert_eq!(
        lines.len(),
        jinn_chat_log_view_msg::PREVIEW_MARKER_MAX_ROWS,
        "a marker must be bounded to its row budget, got {} rows",
        lines.len()
    );
}

#[rstest::rstest]
fn a_marker_never_exceeds_the_preview_line_budget() {
    // Given a long reply in a preview whose own budget is smaller than the
    // marker's.
    let entries = vec![streaming_assistant(&"x".repeat(100_000))];

    // When the worker renders its preview.
    let lines = render_preview(&entries, &preview_ctx(40), 5, 3);

    // Then the result fits the budget, so the marker's rows can never be what
    // pushes the popup past its height.
    assert!(
        lines.len() <= 3,
        "preview returned {} lines over a budget of 3",
        lines.len()
    );
}

#[rstest::rstest]
fn an_enormous_entry_renders_only_its_bounded_marker() {
    // Given a request carrying a reply of a megabyte, still in production.
    let request = jinn_chat_log_view_msg::PreviewSessionRequested {
        session_id: jinn_core_types::SessionId::new(),
        content_width: 40,
        generation: 1,
        entries: std::sync::Arc::from(vec![streaming_assistant(&"x".repeat(1_000_000))]),
        tool_entry_max_lines: 6,
        signature: 7,
    };

    // When it is turned into a job and rendered.
    let job = super::layout_worker::PreviewJob::from(&request);
    let lines = render_preview(&job.entries, &preview_ctx(28), 5, 1000);

    // Then the rendered result is the bounded marker, not the reply. A megabyte
    // of text and four kilobytes of it must render to the same eight rows,
    // because the bound is what makes the render's cost independent of how much
    // the model has written so far.
    assert_eq!(
        lines.len(),
        jinn_chat_log_view_msg::PREVIEW_MARKER_MAX_ROWS,
        "a megabyte entry must render as a bounded marker, got {} rows",
        lines.len()
    );
    assert!(
        job.entries[0].text().len() <= 4_096 + 4,
        "the carried text must be bounded to 4096 bytes, got {}",
        job.entries[0].text().len()
    );
}

#[rstest::rstest]
fn a_bounded_entry_keeps_the_tail_of_its_text() {
    // Given an entry whose text is over the bound.
    let text = format!("{}TAIL", "x".repeat(100_000));
    let request = jinn_chat_log_view_msg::PreviewSessionRequested {
        session_id: jinn_core_types::SessionId::new(),
        content_width: 40,
        generation: 1,
        entries: std::sync::Arc::from(vec![ChatEntry::assistant(text)]),
        tool_entry_max_lines: 6,
        signature: 7,
    };

    // When a request for it is turned into a job.
    let job = super::layout_worker::PreviewJob::from(&request);

    // Then what survives is the *end* of the text, because the preview is
    // bottom-anchored and the end is what the reader is looking for.
    assert!(
        job.entries[0].text().ends_with("TAIL"),
        "the bound must keep the tail of the text"
    );
    assert!(
        job.entries[0].text().len() < 5_000,
        "the bound must actually bound, got {} bytes",
        job.entries[0].text().len()
    );
}

#[rstest::rstest]
fn a_bound_lands_on_a_character_boundary() {
    // Given multi-byte text long enough that a byte-wise cut would land inside a
    // character.
    let text = "é".repeat(10_000);
    let request = jinn_chat_log_view_msg::PreviewSessionRequested {
        session_id: jinn_core_types::SessionId::new(),
        content_width: 40,
        generation: 1,
        entries: std::sync::Arc::from(vec![ChatEntry::assistant(text)]),
        tool_entry_max_lines: 6,
        signature: 7,
    };

    // When a request for it is turned into a job.
    let job = super::layout_worker::PreviewJob::from(&request);

    // Then the surviving text is still valid UTF-8 — it was sliced on a
    // character boundary, and reading it back is what proves it. A `&str` cut at
    // an arbitrary byte is not a `&str` at all, and a preview that panicked on a
    // multi-byte reply would take the render with it.
    assert_eq!(
        job.entries[0].text(),
        std::str::from_utf8(job.entries[0].text().as_bytes())
            .expect("the bounded text must be valid UTF-8"),
        "the bound must land on a character boundary"
    );
}

#[rstest::rstest]
fn a_settled_entry_is_still_rendered() {
    // Given a history whose entries have all finished.
    let entries = preview_entries(3);

    // When the worker renders its preview.
    let visible = visible_text(&render_preview(&entries, &preview_ctx(120), 5, 1000));

    // Then every one of them is shown. Settledness excludes only what is still
    // being produced; a preview is mostly settled messages.
    assert_eq!(
        visible,
        vec!["entry 0", "entry 1", "entry 2"],
        "settled entries must render normally"
    );
}

#[rstest::rstest]
fn an_in_production_entry_does_not_displace_settled_ones() {
    // Given more than a window's worth of settled entries, with a reply still in
    // production at the end.
    let mut entries = preview_entries(10);
    entries.push(streaming_assistant("still going"));

    // When the worker renders its preview.
    let visible = visible_text(&render_preview(&entries, &preview_ctx(120), 5, 1000));

    // Then the window is the five most recent *settled* entries, and the
    // in-production one is a marker beside them. Taking the trailing five and
    // then filtering would have pushed four settled entries out of the preview.
    assert_eq!(
        visible,
        vec![
            "entry 5",
            "entry 6",
            "entry 7",
            "entry 8",
            "entry 9",
            "still going",
        ],
        "an in-production entry must not shrink the settled window"
    );
}

#[rstest::rstest]
fn a_finished_reply_becomes_a_settled_entry() {
    // Given a history whose reply was in production and has now finished.
    let mut finished = streaming_assistant("all done now");
    finished.timing.finish();
    let entries = vec![finished];

    // When the worker renders its preview.
    let visible = visible_text(&render_preview(&entries, &preview_ctx(120), 5, 1000));

    // Then it renders as itself. Settledness is read from the entry's own timing,
    // so a finished reply needs no separate signal to come back.
    assert_eq!(
        visible,
        vec!["all done now"],
        "a finished reply must render as a settled entry"
    );
}

#[rstest::rstest]
fn a_streaming_tool_call_is_excluded_from_the_settled_set() {
    // Given a tool call created by `begin_tool_call`: a `Streamed` timing with no
    // `finished_at`, which is exactly the shape a tool call has while its
    // arguments are still arriving.
    let mut streaming = ChatEntry::tool_call("call-1", "grep", "{\"path\": \"/tm");
    streaming.timing = jinn_core_types::entry_timing::EntryTiming::streamed(jiff::Timestamp::now());
    let entries = vec![streaming];

    // When the worker renders its preview.
    let lines = render_preview(&entries, &preview_ctx(120), 5, 1000);

    // Then it is a marker rather than a rendered tool call. A rendered tool call
    // is padded out to the full content width and shows its tool name in the
    // tool styling; a marker is plain text at its own natural width, so the
    // absence of a full-width line is what proves the markdown/tool rendering
    // path was never taken.
    assert!(
        !lines.iter().any(|line| line.width() == 120),
        "a tool call streaming its arguments must not be rendered, got {:?}",
        lines.iter().map(line_text).collect::<Vec<_>>()
    );
    assert_eq!(
        visible_text(&lines),
        vec!["grep: {\"path\": \"/tm"],
        "the marker must carry the arguments streamed so far"
    );
}

#[rstest::rstest]
fn a_short_tail_reaches_back_for_more_entries_to_fill_the_budget() {
    // Given a history where one entry is large enough to overrun the line budget
    // on its own, followed by a tail of tiny ones — at a width where the
    // oversized entry alone exceeds the whole budget.
    let mut entries = Vec::new();
    entries.push(ChatEntry::user(
        std::iter::repeat_n("w", 500).collect::<String>(),
    ));
    for i in 0..3 {
        entries.push(ChatEntry::user(format!("s{i}")));
    }

    // When the worker renders its preview at the real bounds.
    let lines = render_preview(
        &entries,
        &preview_ctx(40),
        jinn_chat_log_view_msg::PREVIEW_ENTRY_COUNT,
        jinn_chat_log_view_msg::PREVIEW_MAX_LINES,
    );

    // Then the budget is *filled*. Bounding the window by a handful of entries
    // would stop after the four here, drain the oversized entry's rows from the
    // front, and leave the preview a few rows short with a gap above them.
    assert_eq!(
        lines.len(),
        20,
        "the preview must fill its budget, got {} rows",
        lines.len()
    );
}

#[rstest::rstest]
fn a_preview_of_only_short_entries_fills_its_budget() {
    // Given a history of nothing but one-line messages.
    let entries = preview_entries(20);

    // When the worker renders its preview at the real reach bound.
    let lines = render_preview(
        &entries,
        &preview_ctx(40),
        jinn_chat_log_view_msg::PREVIEW_ENTRY_COUNT,
        jinn_chat_log_view_msg::PREVIEW_MAX_LINES,
    );

    // Then the budget is met, and met from real entries rather than padding. A
    // handful of one-line messages cannot fill twenty rows; the walk has to keep
    // going back through the history to get there.
    assert_eq!(
        lines.len(),
        20,
        "a preview of short messages must still fill its budget, got {} rows",
        lines.len()
    );
}

#[rstest::rstest]
fn reaching_past_the_entry_bound_does_not_render_unbounded_history() {
    // Given a very long history of one-line messages.
    let entries = preview_entries(500);

    // When the worker renders its preview.
    let lines = render_preview(&entries, &preview_ctx(40), 5, 20);

    // Then it still renders only the reach bound's worth of entries — the walk
    // is bounded, and the budget is filled from a bounded reach rather than from
    // however much history the session happens to have.
    let distinct = visible_text(&lines)
        .into_iter()
        .filter(|text| text.starts_with("entry "))
        .count();
    assert!(
        distinct <= jinn_chat_log_view_msg::PREVIEW_ENTRY_COUNT,
        "a preview must not render an unbounded tail of history, saw {distinct} entries"
    );
}

#[rstest::rstest]
fn a_history_shorter_than_the_budget_is_not_padded_out() {
    // Given a history holding one short message — less content than the budget.
    let entries = preview_entries(1);

    // When the worker renders its preview at the real bounds.
    let lines = render_preview(
        &entries,
        &preview_ctx(40),
        jinn_chat_log_view_msg::PREVIEW_ENTRY_COUNT,
        jinn_chat_log_view_msg::PREVIEW_MAX_LINES,
    );

    // Then it holds what there was, and nothing else. A preview that padded to
    // the budget would invent content the session does not have.
    assert_eq!(
        visible_text(&lines),
        vec!["entry 0"],
        "a session with less content than the budget must render exactly that"
    );
}
