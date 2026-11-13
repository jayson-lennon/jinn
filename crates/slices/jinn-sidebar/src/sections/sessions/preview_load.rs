//! Requesting a session preview render, and the content identity that decides
//! whether one is needed.
//!
//! The preview popup's lines are rendered by the layout worker rather than the
//! frame loop, so moving the cursor over a large session costs the UI nothing but
//! a spinner. That only works if a render is *asked for* at the right moments,
//! which is what lives here.
//!
//! The identity here is deliberately blind to entries that are still producing.
//! A preview shows *settled* messages: a reply still streaming is represented by
//! a bounded continuation marker, and its text is never rendered. Folding such
//! an entry into the key would move it on every token and re-request a full
//! render for the whole turn, which is the flicker this module exists to
//! prevent. See [`preview_signature`].
//!
//! Requests originate on the keyboard path only. The render pass has no bus
//! handle — the frontend's job is to read state, not to publish — so it records
//! the width it drew at and draws a spinner when nothing is cached. A terminal
//! resize therefore re-requests on the next cursor move rather than instantly;
//! the alternative was handing the render path a publisher for one message.
//!
//! Because requests come from the keyboard, they are also where duplicates are
//! stopped: a second request for content already cached or already rendering
//! would queue a job for lines nothing would read.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

#[cfg(test)]
use jinn_chat_log_view_msg::PREVIEW_ENTRY_COUNT;
use jinn_chat_log_view_msg::{
    PREVIEW_REQUEST_ENTRY_COUNT, PreviewSessionRequested, entry_is_settled,
};
use jinn_core_types::{ChatEntry, SessionId};
use jinn_kernel::common::app_state::AppState;
use jinn_preferences_config::schemas::ChatLogConfig;
use jinn_slices::ConfigLayer;

use crate::sections::sessions::preview::DEFAULT_TOOL_ENTRY_MAX_LINES;

/// A summary of the content a preview would show.
///
/// Folds the O(1) content signatures of the preview's *settled* entries — the
/// most recent ones that have stopped producing. An entry still accumulating
/// tokens is left out of the fold entirely, so its growth does not move the key.
///
/// That omission is the whole point. Folding every trailing entry meant every
/// streamed token moved the signature, invalidated the cache's one slot for the
/// session, and queued a full re-render — one per token, for the length of the
/// turn. The popup then missed its own lookup on every frame it was waiting for
/// that render and drew a spinner over lines it already had.
///
/// `reachable_entries` bounds the fold by *entry count* alone. A preview reaches
/// back until its line budget is full, so which entries it shows depends on how
/// tall each one is, and the set shifts as the conversation grows: a new short
/// entry can push a tall one out of the window without the tall one changing at
/// all. Folding a fixed count of entries would leave the key blind to that, and
/// the preview would keep serving lines for a window that no longer exists.
///
/// The history's *length* is folded as well, so dropping trailing entries stays
/// distinguishable from those entries merely changing content: a pruned history
/// must not serve the longer one's preview.
#[must_use]
pub fn preview_signature(entries: &[ChatEntry], max_entries: usize) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for entry in reachable_entries(entries, max_entries) {
        entry.content_signature().hash(&mut hasher);
    }
    entries.len().hash(&mut hasher);
    hasher.finish()
}

/// Every entry a preview can reach, oldest first.
///
/// The settled entries a preview might render, without a bound on how much it
/// ends up showing — a preview walks back until its *line* budget is full, so
/// this is a reachability question, not a display one.
///
/// Every entry a preview could reach is folded, not the five it happens to show
/// most of the time. The window's membership moves as the conversation grows
/// (see [`preview_signature`]), so a key covering only a fixed count of the
/// newest entries would stop tracking what is actually on screen.
#[must_use]
pub fn reachable_entries(entries: &[ChatEntry], max_entries: usize) -> Vec<&ChatEntry> {
    entries
        .iter()
        .filter(|entry| entry_is_settled(entry))
        .rev()
        .take(max_entries)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

/// The up-to-`max_entries` most recent *settled* entries, oldest first.
///
/// Found by scanning backwards and skipping entries still in production, rather
/// than by taking the trailing `max_entries` and dropping the excluded ones. The
/// difference is a preview that still has something to show: a reply in
/// production at the end of the history does not shrink the window, it
/// contributes a marker beside the entries that did settle.
///
/// Shared with the worker by the entries travelling on the request: the request
/// carries [`PREVIEW_REQUEST_ENTRY_COUNT`] entries, which is more than this
/// window can ever need, so a worker handed that slice finds the same window
/// this function found.
#[must_use]
pub fn settled_window(entries: &[ChatEntry], max_entries: usize) -> Vec<&ChatEntry> {
    entries
        .iter()
        .rev()
        .filter(|entry| entry_is_settled(entry))
        .take(max_entries)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

/// Arms a preview render for `session_id`, returning the request to publish.
///
/// Returns `None` when the preview is already cached at this content and width,
/// or when an identical request is already running — re-requesting would
/// rebuild the lines off-thread only for the render pass to discard them, and
/// each duplicate is a job on a pool that is already busy with chat-log
/// measurement.
#[must_use]
pub fn update_preview(
    state: &mut AppState,
    session_id: &SessionId,
    config: &ConfigLayer,
) -> Option<PreviewSessionRequested> {
    let width = state
        .frontend
        .with_sections(|s| s.sessions.preview_content_width, || 0);

    // A width of zero means the render pass has not yet measured a frame, so
    // there is nothing to wrap for. Requesting anyway — at a nominal width, or
    // at whatever stale value a previous frame left behind — produces lines
    // that can never match the width the render pass looks them up at, and the
    // preview spins forever. Returning nothing instead leaves the cursor move
    // that follows the first frame to publish the real request.
    if width == 0 {
        return None;
    }

    // The signature is computed over the trailing entries but folded with the
    // *whole* history's length, so it is taken from a borrow of the full
    // history rather than the tail that is about to be shared. Skipping ahead
    // first would make dropping an entry indistinguishable from its content
    // changing, and a stale preview would be served after a prune.
    let signature = {
        let session = state.session.get(session_id)?;
        preview_signature(session.history(), PREVIEW_REQUEST_ENTRY_COUNT)
    };

    if preview_is_current(state, session_id, signature, width) {
        // A hit is also a use: it says which session the user is looking at, so
        // it refreshes that preview's recency against eviction.
        state
            .frontend
            .update_sections(|s| s.sessions.preview.touch(session_id));
        return None;
    }

    // Only now is a copy worth making. The worker reads the trailing entries —
    // and needs more of them than the preview window holds, because a reply
    // still in production must not push the settled entries out of the slice —
    // so a longer history's earlier entries are copied on every keystroke for no
    // reader. The borrow of the session has to end first, since `update_sections`
    // below takes the sections lock and the Arc is built from what it borrowed.
    let entries = {
        let session = state.session.get(session_id)?;
        let history = session.history();
        let start = history.len().saturating_sub(PREVIEW_REQUEST_ENTRY_COUNT);
        Arc::<[ChatEntry]>::from(history.get(start..).unwrap_or_default())
    };

    // The generation is bumped here, at the one place a request originates, so a
    // result belonging to a superseded request can be dropped on arrival. It is
    // read out of a local because `update_sections` yields `()`.
    let mut generation = 0;
    state.frontend.update_sections(|s| {
        generation = s
            .sessions
            .preview
            .request(session_id.clone(), signature, width);
    });

    Some(PreviewSessionRequested {
        session_id: session_id.clone(),
        content_width: width,
        generation,
        entries,
        tool_entry_max_lines: config
            .read::<ChatLogConfig>()
            .tool_entry_max_lines
            .unwrap_or(DEFAULT_TOOL_ENTRY_MAX_LINES),
        signature,
    })
}

/// Whether the preview for this session, content, and width is already handled.
///
/// A cache hit means the render pass can draw immediately. An in-flight match
/// means the render is already running and will land on its own. Both are read
/// under one lock so they cannot disagree with each other.
fn preview_is_current(
    state: &AppState,
    session_id: &SessionId,
    signature: u64,
    content_width: u16,
) -> bool {
    state.frontend.with_sections(
        |s| {
            s.sessions
                .preview
                .cached(session_id, signature, content_width)
                .is_some()
                || s.sessions
                    .preview
                    .in_flight_matches(session_id, signature, content_width)
        },
        || false,
    )
}

/// The preview request to publish for the session under the cursor, if one is
/// needed and the popup is actually on screen.
///
/// The keyboard path asks for previews when the cursor *moves*, but the cursor
/// is not the only thing that changes what the popup should show: a session can
/// finish loading into the list, or a frame can measure the width, long after
/// the last keystroke. Every such transition left a popup spinning on a request
/// nobody had made, resolved only by nudging the cursor.
///
/// So the render pass asks too, through the same builder the keyboard path
/// uses, with the same dedupe against the cache and against in-flight renders.
/// A settled cursor therefore asks once and then stays silent: the request is
/// only produced while the cache is empty and nothing is running, and the
/// result's arrival fills the cache and ends the ask.
///
/// Returns `None` — publishing nothing — when the sessions section is not
/// focused, the cursor is on nothing, or the popup has what it needs.
#[must_use]
pub fn request_preview_if_needed(
    state: &mut AppState,
    config: &jinn_slices::ConfigLayer,
) -> Option<PreviewSessionRequested> {
    let session_id = previewed_session_id(state)?;
    update_preview(state, &session_id, config)
}

/// The session the focused section's cursor names, if it previews one.
///
/// Both list sections preview: the sessions tree and the library cell are
/// different orderings of the same in-memory sessions, so the popup reads
/// identically from either door and shares the one preview cache. A section
/// that previews nothing (persona, pins, …) returns `None`, as does a cursor
/// naming a session that is not in memory — an archived library member has
/// no history to render until `<enter>` loads it.
#[must_use]
pub fn previewed_session_id(state: &AppState) -> Option<SessionId> {
    match state.frontend.sidebar_section()? {
        jinn_sidebar_msg::SidebarSectionId::Sessions => state
            .frontend
            .with_sections(|s| s.sessions.selected_id.clone(), || None),
        jinn_sidebar_msg::SidebarSectionId::Library => state
            .frontend
            .with_sections(|s| s.library.selected_id.clone(), || None),
        _ => None,
    }
    .filter(|id| state.session.get(id).is_some())
}

#[cfg(test)]
mod preview_load_tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]

    use super::*;
    use jinn_kernel::protocol::ChatEntry;
    use jinn_session_state::ChatSessionState;

    /// App state holding one loaded session, with a recorded preview width.
    ///
    /// The id is read back off the inserted session rather than minted
    /// separately: the map keys on the session's own id, so an independently
    /// generated one would never be found.
    fn state_with_session(width: u16) -> (AppState, SessionId) {
        let mut state = AppState::default_with_scope_focus();
        let mut session = ChatSessionState::new();
        session.push_entry(ChatEntry::user("hello"));
        let id = session.session_id().clone();
        state.session.insert(session);
        state.session.set_active(id.clone());
        state
            .frontend
            .update_sections(|s| s.sessions.preview_content_width = width);
        (state, id)
    }

    /// The signature the trigger would compute for `id` right now.
    fn signature_of(state: &AppState, id: &SessionId) -> u64 {
        let session = state.session.get(id).expect("session");
        preview_signature(session.history(), PREVIEW_REQUEST_ENTRY_COUNT)
    }

    /// Marks the current session's preview as served, so the trigger sees a hit.
    fn mark_ready(state: &AppState, id: &SessionId, width: u16) {
        let signature = signature_of(state, id);
        state.frontend.update_sections(|s| {
            let generation = s.sessions.preview.request(id.clone(), signature, width);
            s.sessions.preview.complete(
                id.clone(),
                generation,
                signature,
                width,
                Arc::new(vec![ratatui::text::Line::from("hello")]),
            );
        });
    }

    /// Arms a preview render for `id` and publishes its (empty) result, exactly
    /// as the layout worker would once the job came back.
    ///
    /// Returns the signature and width the result was filed under, which is all
    /// the render pass needs to look the preview up again.
    fn complete_preview_render(state: &mut AppState, id: &SessionId) -> (u64, u16) {
        let request = update_preview(state, id, jinn_slices::empty_config_layer())
            .expect("a measured width must request");
        let signature = request.signature;
        let generation = request.generation;
        let width = request.content_width;
        state.frontend.update_sections(|s| {
            s.sessions.preview.complete(
                id.clone(),
                generation,
                signature,
                width,
                Arc::new(Vec::new()),
            );
        });
        (signature, width)
    }

    /// App state whose only session holds no entries at all, with an empty
    /// preview rendered and cached for it at width 46.
    fn state_with_completed_empty_preview() -> (AppState, SessionId, u64, u16) {
        let mut state = AppState::default_with_scope_focus();
        let session = ChatSessionState::new();
        let id = session.session_id().clone();
        state.session.insert(session);
        state.session.set_active(id.clone());
        state
            .frontend
            .update_sections(|s| s.sessions.preview_content_width = 46);
        let (signature, width) = complete_preview_render(&mut state, &id);
        (state, id, signature, width)
    }

    #[rstest::rstest]
    fn a_fresh_session_yields_a_preview_request() {
        // Given app state with a session whose preview has never been served.
        let (mut state, id) = state_with_session(40);

        // When the trigger runs.
        let request = update_preview(&mut state, &id, jinn_slices::empty_config_layer());

        // Then a request for that session is returned.
        assert_eq!(
            request.map(|r| r.session_id),
            Some(id),
            "expected a request for the highlighted session"
        );
    }

    #[rstest::rstest]
    fn a_current_preview_yields_no_request() {
        // Given app state whose preview is already served at the current width.
        let (mut state, id) = state_with_session(40);
        mark_ready(&state, &id, 40);

        // When the trigger runs.
        let request = update_preview(&mut state, &id, jinn_slices::empty_config_layer());

        // Then nothing is requested.
        assert!(request.is_none(), "a served preview must not re-request");
    }

    #[rstest::rstest]
    fn a_width_change_yields_a_request() {
        // Given app state whose preview was served at 40.
        let (mut state, id) = state_with_session(40);
        mark_ready(&state, &id, 40);

        // When the terminal is resized and the trigger runs.
        state
            .frontend
            .update_sections(|s| s.sessions.preview_content_width = 60);
        let request = update_preview(&mut state, &id, jinn_slices::empty_config_layer());

        // Then the preview is re-requested at the new width.
        assert_eq!(request.map(|r| r.content_width), Some(60));
    }

    /// App state whose session holds a reply that is still receiving tokens.
    ///
    /// The entry is unfinished by construction — a `Streamed` timing with no
    /// `finished_at`, which is what every token of a live reply produces. That
    /// is the *only* way to express "still streaming" here: there is no parallel
    /// boolean saying an entry is live, because the request already carries the
    /// entries and both sides can read their timings.
    fn state_with_streaming_session(width: u16) -> (AppState, SessionId) {
        let mut state = AppState::default_with_scope_focus();
        let mut session = ChatSessionState::new();
        session.push_entry(ChatEntry::user("hello"));
        let mut reply = ChatEntry::assistant("hel");
        reply.timing = jinn_core_types::entry_timing::EntryTiming::streamed(jiff::Timestamp::now());
        session.push_entry(reply);
        let id = session.session_id().clone();
        state.session.insert(session);
        state.session.set_active(id.clone());
        state
            .frontend
            .update_sections(|s| s.sessions.preview_content_width = width);
        (state, id)
    }

    /// Appends `token {index}` to the session's reply, leaving it unfinished.
    ///
    /// The one way a test makes a session look mid-turn: the entry's `Streamed`
    /// timing already has no `finished_at`, so growing its text is exactly what a
    /// provider streaming does.
    fn append_token(state: &mut AppState, id: &SessionId, index: usize) {
        state
            .session
            .get_mut(id)
            .expect("session")
            .edit_history()
            .with_last_matching_mut(
                |entry| entry.timing.finished_at().is_none(),
                |entry| {
                    if let jinn_core_types::ChatEntryKind::Assistant(text) = &mut entry.kind {
                        use std::fmt::Write as _;
                        let _ = write!(text, " token{index}");
                    }
                },
            )
            .expect("the streaming reply");
    }

    /// Marks the session's reply finished, as a completed stream does.
    fn finish_reply(state: &mut AppState, id: &SessionId) {
        state
            .session
            .get_mut(id)
            .expect("session")
            .edit_history()
            .with_last_matching_mut(
                |entry| entry.timing.finished_at().is_none(),
                |entry| entry.timing.finish(),
            )
            .expect("the streaming reply");
    }

    /// Whether the draw path has any lines to draw for this session.
    fn is_drawable(state: &AppState, id: &SessionId) -> bool {
        state
            .frontend
            .with_sections(|s| s.sessions.preview.drawable(id).is_some(), || false)
    }

    #[rstest::rstest]
    fn a_streamed_token_yields_no_request() {
        // Given app state whose preview was served while a reply was streaming.
        let (mut state, id) = state_with_streaming_session(40);
        mark_ready(&state, &id, 40);

        // When a further token lands in that reply.
        append_token(&mut state, &id, 0);

        // Then nothing is re-requested. The reply is not in the preview's key at
        // all — it is shown as a continuation marker, and rendering a marker
        // again for every token is the per-token re-render this design removes.
        let request = update_preview(&mut state, &id, jinn_slices::empty_config_layer());
        assert!(
            request.is_none(),
            "a token in a reply still in production must not invalidate a served preview"
        );
    }

    #[rstest::rstest]
    fn a_settled_entry_growing_yields_a_request() {
        // Given app state whose preview was served for a settled entry.
        let (mut state, id) = state_with_session(40);
        mark_ready(&state, &id, 40);

        // When that settled entry's content changes.
        state
            .session
            .get_mut(&id)
            .expect("session")
            .push_entry(ChatEntry::user("hello world"));

        // Then the preview is re-requested. A *settled* entry is in the key, so
        // a change to one the preview shows is a change to what it shows.
        let request = update_preview(&mut state, &id, jinn_slices::empty_config_layer());
        assert!(
            request.is_some(),
            "a changed settled entry must invalidate a served preview"
        );
    }

    #[rstest::rstest]
    fn a_whole_turn_of_tokens_never_leaves_the_popup_without_a_drawable_result() {
        // Given a session whose reply is streaming, with its preview served at
        // the width the popup draws at.
        let (mut state, id) = state_with_streaming_session(40);
        mark_ready(&state, &id, 40);

        // When tokens land, one at a time, for a long reply.
        let mut with_nothing_to_draw = Vec::new();
        for token in 0..20 {
            append_token(&mut state, &id, token);

            // Then at every one of those intermediate states the draw path still
            // has something to draw. The keyed lookup is allowed to miss — the
            // session's content did move — but the session must never be left
            // with *nothing*, because nothing is what the spinner is drawn for,
            // and a spinner per token is the flicker this design removes.
            let drawable = state
                .frontend
                .with_sections(|s| s.sessions.preview.drawable(&id).is_some(), || false);
            if !drawable {
                with_nothing_to_draw.push(token);
            }
        }

        assert!(
            with_nothing_to_draw.is_empty(),
            "the popup had nothing to draw at tokens {with_nothing_to_draw:?}, so it would have \
             shown a spinner over the whole turn"
        );
    }

    #[rstest::rstest]
    fn a_turn_of_tokens_never_leaves_the_popup_without_something_to_draw() {
        // Given a session whose reply is streaming, with its preview served.
        let (mut state, id) = state_with_streaming_session(40);
        mark_ready(&state, &id, 40);

        // When tokens land, one at a time, and the reply finally finishes.
        let mut with_nothing_to_draw = Vec::new();
        for token in 0..20 {
            append_token(&mut state, &id, token);
            if !is_drawable(&state, &id) {
                with_nothing_to_draw.push(format!("token{token}"));
            }
        }
        finish_reply(&mut state, &id);
        if !is_drawable(&state, &id) {
            with_nothing_to_draw.push("after the reply finished".to_owned());
        }

        // Then the session had something to draw at every step of the turn. The
        // turn *does* move the key — once, when the reply settles and becomes a
        // previewed entry — and the popup must not blank or spin at that step.
        assert!(
            with_nothing_to_draw.is_empty(),
            "the popup had nothing to draw at {with_nothing_to_draw:?}, so it would have shown a \
             spinner over its own content"
        );
    }

    #[rstest::rstest]
    fn a_settling_reply_misses_the_keyed_lookup_but_stays_drawable() {
        // Given a streaming session whose preview was served.
        let (mut state, id) = state_with_streaming_session(40);
        mark_ready(&state, &id, 40);
        append_token(&mut state, &id, 0);
        finish_reply(&mut state, &id);
        let current = signature_of(&state, &id);

        // When the keyed lookup and the fallback are resolved.
        let (keyed_hit, drawable) = state.frontend.with_sections(
            |s| {
                (
                    s.sessions.preview.cached(&id, current, 40).is_some(),
                    s.sessions.preview.drawable(&id).is_some(),
                )
            },
            || (false, false),
        );

        // Then the keyed lookup misses while the fallback resolves. Settling is
        // the one moment in a turn the key legitimately moves — the reply has
        // become something the preview shows — and this is the pair of answers
        // the draw path acts on to keep that moment from becoming a spinner.
        assert!(
            !keyed_hit,
            "the keyed lookup must miss once a reply settles, or this case never arises"
        );
        assert!(
            drawable,
            "the fallback must find the lines the session is already holding"
        );
    }

    #[rstest::rstest]
    fn a_resized_popup_misses_the_keyed_lookup_but_stays_drawable() {
        // Given a session whose preview was served at 40 columns.
        let (state, id) = state_with_streaming_session(40);
        mark_ready(&state, &id, 40);

        // When the terminal is resized and the popup is drawn at 60.
        let signature = signature_of(&state, &id);
        let (keyed_hit, drawable) = state.frontend.with_sections(
            |s| {
                (
                    s.sessions.preview.cached(&id, signature, 60).is_some(),
                    s.sessions.preview.drawable(&id).is_some(),
                )
            },
            || (false, false),
        );

        // Then the keyed lookup misses while the fallback resolves. A resize
        // mid-turn is the other routine way the key moves, and it must not blank
        // a popup the user is reading.
        assert!(
            !keyed_hit,
            "the keyed lookup must miss at a new width, or this case never arises"
        );
        assert!(
            drawable,
            "the fallback must find the lines the session is already holding"
        );
    }

    #[rstest::rstest]
    fn an_identical_in_flight_request_is_not_republished() {
        // Given a request already running for a session at this width.
        let (mut state, id) = state_with_session(40);
        update_preview(&mut state, &id, jinn_slices::empty_config_layer()).expect("first request");

        // When the trigger runs again for the same session, content, and width.
        let request = update_preview(&mut state, &id, jinn_slices::empty_config_layer());

        // Then nothing is published — the render is already running and a
        // second job would queue behind it for lines nothing would read.
        assert!(
            request.is_none(),
            "an identical in-flight request must not be republished"
        );
    }

    #[rstest::rstest]
    fn a_different_width_still_yields_a_request_while_one_is_in_flight() {
        // Given a request running for a session at 40.
        let (mut state, id) = state_with_session(40);
        update_preview(&mut state, &id, jinn_slices::empty_config_layer()).expect("first request");

        // When the terminal is resized and the trigger runs again.
        state
            .frontend
            .update_sections(|s| s.sessions.preview_content_width = 60);
        let request = update_preview(&mut state, &id, jinn_slices::empty_config_layer());

        // Then a request is published, because the running one wrapped the text
        // at a width the popup no longer has.
        assert_eq!(request.map(|r| r.content_width), Some(60));
    }

    #[rstest::rstest]
    fn an_unknown_session_yields_no_request() {
        // Given app state with a different session than the one asked about.
        let (mut state, _id) = state_with_session(40);

        // When the trigger runs for an absent session.
        let request = update_preview(
            &mut state,
            &SessionId::new(),
            jinn_slices::empty_config_layer(),
        );

        // Then nothing is requested.
        assert!(
            request.is_none(),
            "an absent session has nothing to preview"
        );
    }

    #[rstest::rstest]
    fn a_request_arms_the_preview_as_in_flight() {
        // Given app state with a never-served session.
        let (mut state, id) = state_with_session(40);

        // When the trigger runs.
        let request = update_preview(&mut state, &id, jinn_slices::empty_config_layer())
            .expect("a fresh session must request");

        // Then the preview is armed at the request's own generation, so the
        // result is recognised as the live one when it lands.
        let armed = state.frontend.with_sections(
            |s| {
                s.sessions
                    .preview
                    .in_flight_matches(&id, request.signature, 40)
            },
            || false,
        );
        assert!(
            armed,
            "the preview must be armed before the request publishes"
        );
    }

    #[rstest::rstest]
    fn an_empty_preview_render_is_armed_for_a_session_with_no_entries() {
        // Given a session with no entries at all — a brand-new session, whose
        // chat view is already showing because there is nothing to load.
        let mut state = AppState::default_with_scope_focus();
        let session = ChatSessionState::new();
        let id = session.session_id().clone();
        state.session.insert(session);
        state.session.set_active(id.clone());
        state
            .frontend
            .update_sections(|s| s.sessions.preview_content_width = 46);

        // When a preview is requested.
        let request = update_preview(&mut state, &id, jinn_slices::empty_config_layer());

        // Then one is published: an empty session still needs rendering for the
        // popup to know it is empty rather than still loading.
        assert!(
            request.is_some(),
            "an empty session still needs rendering to establish it is empty"
        );
    }

    #[rstest::rstest]
    fn a_completed_empty_preview_is_served_as_empty_not_loading() {
        // Given a session with no entries whose empty preview has been rendered
        // and cached.
        let (state, id, signature, width) = state_with_completed_empty_preview();

        // When the render pass looks it up.
        let found = state.frontend.with_sections(
            |s| s.sessions.preview.cached(&id, signature, width).is_some(),
            || false,
        );

        // Then it is found. An empty session is a *completed* preview holding
        // zero lines, not a missing one: the popup tells loading from empty by
        // whether the lookup hit, so reporting a miss here is what puts a
        // spinner on a session that has nothing to wait for.
        assert!(
            found,
            "an empty session's preview was reported as missing, so the popup spins forever"
        );
    }

    /// State focused on the sessions section, with the cursor on its first row.
    ///
    /// The session is marked `Loaded` because the list the popup resolves its
    /// entry through only contains loaded sessions — a session still loading has
    /// no row to preview, which is a different situation entirely.
    fn state_focused_on_sessions() -> AppState {
        let (mut state, id) = state_with_session(46);
        if let Some(session) = state.session.get_mut(&id) {
            session.set_session_state(jinn_session_store_msg::SessionState::Loaded);
        }
        // The scope is pushed before the section is set: `set_sidebar_section`
        // is a no-op on a stack with no sidebar scope on it, so setting the
        // section alone would leave the sidebar unfocused and silent.
        state
            .frontend
            .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
        let active = state.session.active_session_id().clone();
        state
            .frontend
            .update_sections(|s| s.sessions.selected_id = Some(active));
        state
    }

    #[rstest::rstest]
    fn a_stationary_cursor_still_asks_for_its_preview() {
        // Given the sidebar focused on a session whose preview has never been
        // rendered, and the cursor not going to move again.
        let mut state = state_focused_on_sessions();

        // When the render pass asks.
        let request = request_preview_if_needed(&mut state, jinn_slices::empty_config_layer());

        // Then it asks. The cursor is the only thing that used to trigger this,
        // so a session that loaded, or a width that got measured, after the last
        // key left the popup spinning until the user nudged the cursor.
        assert!(
            request.is_some(),
            "nothing asked for the preview, so a stationary cursor would spin forever"
        );
    }

    /// App state like [`state_focused_on_sessions`], but the library section
    /// is the focused previewer and its cursor names `id`.
    fn state_focused_on_library(id: &SessionId) -> AppState {
        let (state, _) = state_with_session(46);
        // The scope is pushed before the section is set: `set_sidebar_section`
        // is a no-op on a stack with no sidebar scope on it.
        state
            .frontend
            .scope_push(jinn_sidebar_msg::SidebarSectionId::Library.focus_scope());
        state
            .frontend
            .update_sections(|s| s.library.selected_id = Some(id.clone()));
        state
    }

    #[rstest::rstest]
    fn a_library_cursor_asks_for_its_members_preview() {
        // Given the sidebar focused on the library, its cursor on an
        // in-memory member — and the sessions cursor on a different session,
        // so a resolver that fell back to the wrong section's cursor is
        // caught.
        let mut member = ChatSessionState::new();
        member.push_entry(ChatEntry::user("hello from the library"));
        let member_id = member.session_id().clone();
        member.set_in_library(true);
        let mut state = state_focused_on_library(&member_id);
        let sessions_cursor = state.session.active_session_id().clone();
        state.session.insert(member);
        state.frontend.update_sections(|s| {
            s.library.selected_id = Some(member_id.clone());
            s.sessions.selected_id = Some(sessions_cursor);
        });

        // When the render pass asks.
        let request = request_preview_if_needed(&mut state, jinn_slices::empty_config_layer());

        // Then it asks for the member's own preview — the same pipeline the
        // sessions section drives, pointed at the library's cursor.
        let request = request.expect("a library cursor must ask for its preview");
        assert_eq!(
            request.session_id, member_id,
            "the request must name the member under the library cursor"
        );
    }

    #[rstest::rstest]
    fn an_archived_member_asks_for_no_preview() {
        // Given the library focused with its cursor naming a member that is
        // not in memory — the archived case.
        let ghost = SessionId::new();
        let mut state = state_focused_on_library(&ghost);

        // When the render pass asks.
        let request = request_preview_if_needed(&mut state, jinn_slices::empty_config_layer());

        // Then nothing is requested: there is no history to render until the
        // member is loaded with `<enter>`.
        assert!(
            request.is_none(),
            "an archived member has no history to preview"
        );
    }

    #[rstest::rstest]
    fn a_non_previewing_section_asks_for_nothing() {
        // Given the sidebar focused on a section with no preview, the
        // sessions cursor still set — a resolver keyed to the cursor rather
        // than the focus would ask anyway.
        let mut state = state_focused_on_sessions();
        state
            .frontend
            .scope_push(jinn_sidebar_msg::SidebarSectionId::Persona.focus_scope());

        // When the render pass asks.
        let request = request_preview_if_needed(&mut state, jinn_slices::empty_config_layer());

        // Then nothing is requested.
        assert!(request.is_none(), "persona previews nothing");
    }

    #[rstest::rstest]
    fn a_stationary_cursor_stops_asking_once_the_preview_is_cached() {
        // Given the sidebar focused, and a preview already rendered and cached
        // for the session the list resolves the cursor to — the same session the
        // render path will ask about, since a cache keyed to any other id would
        // not be the one it finds.
        let mut state = state_focused_on_sessions();
        let id = crate::sections::sessions::state::sorted_open_sessions(&state)[0]
            .id
            .clone();
        let signature = signature_of(&state, &id);
        // Armed first, exactly as a real request is: a result is only accepted
        // for a generation that was actually issued, so completing one that was
        // never armed is refused by design rather than filling the cache.
        let armed = state
            .frontend
            .update_sections(|s| s.sessions.preview.request(id.clone(), signature, 46))
            .expect("the sections cell is attached");
        state.frontend.update_sections(|s| {
            s.sessions
                .preview
                .complete(id.clone(), armed, signature, 46, Arc::new(Vec::new()));
        });

        // When the render pass asks again, as it does every frame.
        let request = request_preview_if_needed(&mut state, jinn_slices::empty_config_layer());

        // Then it stops asking. Publishing per frame would flood the render pool
        // with work that is already done, which is the failure this dedupe
        // exists to prevent.
        assert!(
            request.is_none(),
            "the render pass re-requested a preview it already has"
        );
    }

    #[rstest::rstest]
    fn nothing_is_asked_for_while_the_sessions_section_is_unfocused() {
        // Given the sidebar not focused on sessions, where no popup is drawn.
        let (mut state, _) = state_with_session(46);

        // When the render pass asks.
        let request = request_preview_if_needed(&mut state, jinn_slices::empty_config_layer());

        // Then it asks for nothing: there is no popup on screen to fill.
        assert!(request.is_none());
    }

    #[rstest::rstest]
    fn a_request_waits_for_a_measured_width() {
        // Given app state where no frame has been measured yet — the width is
        // still zero, so there is nothing to wrap for.
        let (mut state, id) = state_with_session(0);

        // When the trigger runs.
        let request = update_preview(&mut state, &id, jinn_slices::empty_config_layer());

        // Then it asks for nothing. A request at a width no frame will ever
        // report produces lines the render pass cannot match, and the popup
        // spins forever; the pre-render pass measures the width before the
        // next cursor move republishes.
        assert!(
            request.is_none(),
            "no request should be made before a width has been measured"
        );
    }

    #[rstest::rstest]
    fn a_request_names_the_width_the_render_pass_looks_up_at() {
        // Given app state carrying the width the pre-render pass recorded.
        let (mut state, id) = state_with_session(3);
        let measured = 46;
        state
            .frontend
            .update_sections(|s| s.sessions.preview_content_width = measured);

        // When the trigger runs.
        let request = update_preview(&mut state, &id, jinn_slices::empty_config_layer())
            .expect("a measured width must request");

        // Then it names exactly that width. A request at any other width can
        // never match the lookup the render pass performs, which is what left
        // the popup spinning on a cache entry that had already been filled.
        assert_eq!(request.content_width, measured);
    }

    #[rstest::rstest]
    fn a_completed_preview_is_found_at_the_width_the_render_pass_derives() {
        // Given a session whose preview has been requested and filled in, and
        // the frame area the render pass will draw.
        let (mut state, id) = state_with_session(3);
        let frame_area = ratatui::layout::Rect::new(0, 0, 100, 40);
        let measured = crate::sections::sessions::preview::preview_content_width(frame_area);
        state
            .frontend
            .update_sections(|s| s.sessions.preview_content_width = measured);
        let (signature, request_width) = complete_preview_render(&mut state, &id);

        // When the render pass looks the preview up, at the width it derives
        // from the same frame rather than reading back what was stored.
        let lookup_width = crate::sections::sessions::preview::preview_content_width(frame_area);
        let found = state.frontend.with_sections(
            |s| {
                s.sessions
                    .preview
                    .cached(&id, signature, lookup_width)
                    .is_some()
            },
            || false,
        );

        // Then the preview is found. The two widths are derived from the same
        // frame by the same function, so a request can never be rendered at
        // one width and looked up at another — the defect that filled the cache
        // and still reported a miss on every frame, spinning forever.
        assert!(
            found,
            "a preview rendered at width {request_width} was not found at the width the render \
             pass looks up at ({lookup_width})",
        );
    }

    #[rstest::rstest]
    fn a_request_carries_the_trailing_history_up_to_its_bound() {
        // Given a session holding eight entries — fewer than the request bound.
        let (mut state, id) = state_with_session(40);
        for i in 0..7 {
            state
                .session
                .get_mut(&id)
                .expect("session")
                .push_entry(ChatEntry::user(format!("m{i}")));
        }

        // When the trigger runs.
        let request =
            update_preview(&mut state, &id, jinn_slices::empty_config_layer()).expect("request");

        // Then the whole history travels. A history shorter than the bound is
        // carried whole: there is nothing to trim, and trimming to the preview's
        // own five would be exactly the bug — the worker has to be able to find
        // the settled window *past* a reply that is still in production.
        assert_eq!(request.entries.len(), 8);
    }

    #[rstest::rstest]
    fn a_request_carries_no_further_back_than_its_bound() {
        // Given a session holding far more entries than the request bound.
        let (mut state, id) = state_with_session(40);
        for i in 0..60 {
            state
                .session
                .get_mut(&id)
                .expect("session")
                .push_entry(ChatEntry::user(format!("m{i}")));
        }

        // When the trigger runs.
        let request =
            update_preview(&mut state, &id, jinn_slices::empty_config_layer()).expect("request");

        // Then the carried slice stops at the bound. Copying a whole long history
        // on every keystroke is the cost a bounded slice exists to avoid, and no
        // five-entry window can ever reach past sixteen entries.
        assert_eq!(request.entries.len(), PREVIEW_REQUEST_ENTRY_COUNT);
    }

    #[rstest::rstest]
    fn a_request_carries_the_newest_entry() {
        // Given a session whose newest entry is distinguishable.
        let (mut state, id) = state_with_session(40);
        for i in 0..7 {
            state
                .session
                .get_mut(&id)
                .expect("session")
                .push_entry(ChatEntry::user(format!("m{i}")));
        }

        // When the trigger runs.
        let request =
            update_preview(&mut state, &id, jinn_slices::empty_config_layer()).expect("request");

        // Then the entries that travelled are the newest ones, so trimming kept
        // the tail rather than the head of the history. Read from `text()` rather
        // than `content_signature()`: the signature folds only content *lengths*,
        // so equally-sized entries are indistinguishable by it and it would
        // compare equal here for the wrong reason.
        assert_eq!(
            request.entries.last().map(ChatEntry::text),
            Some("m6".to_owned()),
            "the newest entry must travel with the request"
        );
        assert_eq!(
            request.entries.first().map(ChatEntry::text),
            Some("hello".to_owned()),
            "the tail must be kept, not the head of the history"
        );
    }

    #[rstest::rstest]
    fn a_signature_moves_when_an_earlier_entry_is_dropped() {
        // Given a session whose history has seven entries.
        let mut session = ChatSessionState::new();
        for i in 0..7 {
            session.push_entry(ChatEntry::assistant(format!("entry {i}")));
        }
        let before = preview_signature(session.history(), PREVIEW_REQUEST_ENTRY_COUNT);

        // When its earliest entries are pruned, leaving the same trailing window.
        let trimmed = ChatSessionState::new();
        let after = {
            let mut kept = trimmed;
            for i in 3..7 {
                kept.push_entry(ChatEntry::assistant(format!("entry {i}")));
            }
            preview_signature(kept.history(), PREVIEW_ENTRY_COUNT)
        };

        // Then the signature moved, so a preview of the longer history is not
        // served for the shorter one — the length fold is what distinguishes a
        // prune from the trailing entries merely being unchanged.
        assert_ne!(before, after);
    }

    #[rstest::rstest]
    fn the_signature_moves_when_a_settled_entry_grows() {
        // Given the signature of a history holding one settled entry.
        let signature_of_text = |text: &str| {
            let mut session = ChatSessionState::new();
            session.push_entry(ChatEntry::user(text));
            preview_signature(session.history(), PREVIEW_REQUEST_ENTRY_COUNT)
        };

        // When that entry's content changes.
        let before = signature_of_text("hel");
        let after = signature_of_text("hello");

        // Then the signature moved, so a cached preview is invalidated. A settled
        // entry is part of what the preview shows, so a change to it is a change
        // to the key.
        assert_ne!(before, after);
    }

    #[rstest::rstest]
    fn the_signature_holds_still_while_an_entry_is_in_production() {
        // Given the signature of a history whose newest entry is still streaming.
        let signature_of_reply = |text: &str| {
            let mut session = ChatSessionState::new();
            session.push_entry(ChatEntry::user("hello"));
            let mut reply = ChatEntry::assistant(text);
            reply.timing =
                jinn_core_types::entry_timing::EntryTiming::streamed(jiff::Timestamp::now());
            session.push_entry(reply);
            preview_signature(session.history(), PREVIEW_REQUEST_ENTRY_COUNT)
        };

        // When that reply grows.
        let before = signature_of_reply("hel");
        let after = signature_of_reply("hello world");

        // Then the signature did *not* move. The entry in production is not part
        // of what the preview shows — it is a marker — so its growth cannot
        // invalidate the cache. Folding it in is what put a spinner over the
        // popup on every token of a turn.
        assert_eq!(
            before, after,
            "an entry in production must not move the preview's key"
        );
    }

    #[rstest::rstest]
    fn the_signature_is_stable_for_unchanged_content() {
        // Given a helper that builds a session holding one assistant entry of
        // the given text and signs it.
        let signature = |text: &str| {
            let mut session = ChatSessionState::new();
            session.push_entry(ChatEntry::assistant(text));
            preview_signature(session.history(), PREVIEW_REQUEST_ENTRY_COUNT)
        };

        // When the same content is signed twice.
        let first = signature("hello");
        let second = signature("hello");

        // Then the signatures match.
        assert_eq!(first, second);
    }

    #[rstest::rstest]
    fn the_signature_moves_when_an_entry_leaves_the_reachable_window() {
        // Given a helper that signs a history of `count` one-line entries.
        let signature_of_history = |count: usize| {
            let mut session = ChatSessionState::new();
            for i in 0..count {
                session.push_entry(ChatEntry::user(format!("entry {i}")));
            }
            preview_signature(session.history(), PREVIEW_REQUEST_ENTRY_COUNT)
        };
        let full = signature_of_history(40);

        // When entries are pruned off the front until one the preview could reach
        // is gone.
        let pruned = signature_of_history(30);

        // Then the signature moved. A preview reaches back until its line budget
        // is full, so pruning shifts which entries are in view; a key blind to
        // that would keep serving lines for a window that no longer exists, and
        // the gap would stay on screen for good.
        assert_ne!(
            full, pruned,
            "pruning entries the preview could reach must invalidate it"
        );
    }

    #[rstest::rstest]
    fn the_signature_moves_when_the_history_grows() {
        // Given a helper that builds a session holding `count` assistant
        // entries of the same text and signs it.
        let signature = |count: usize| {
            let mut session = ChatSessionState::new();
            for _ in 0..count {
                session.push_entry(ChatEntry::assistant("hi"));
            }
            preview_signature(session.history(), PREVIEW_REQUEST_ENTRY_COUNT)
        };

        // When a one-entry and a two-entry history are signed.
        let one_entry = signature(1);
        let two_entries = signature(2);

        // Then they differ, so dropping an entry is not mistaken for a no-op.
        assert_ne!(one_entry, two_entries);
    }
}
