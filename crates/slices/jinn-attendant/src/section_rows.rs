//! Row building for the Attendants sidebar section.
//!
//! The attendant's *domain* — who its rows are, what their latest report
//! says, whether a report is stale — lives here in the owning slice. The
//! sidebar's section (`jinn-sidebar::sections::attendants_section`) renders
//! these rows but never reconstructs them, which keeps the dependency one
//! way: sidebar → attendant.
//!
//! Staleness: a report is stale when the parent session has produced history
//! since the report was published — that is the parent resuming work. The
//! comparison is per-attendant, so one attendant's fresh report never clears
//! a sibling's stale one.

use jinn_app_state::AppState;
use jinn_attendant_msg::AttendantReport;

/// One attendant row: the session id, its display name, and its latest report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttendantRow {
    /// The attendant session's id.
    pub session_id: jinn_core_types::SessionId,
    /// The session name — the attendant's identity in the UI.
    pub name: String,
    /// The most recent report body, if the attendant has ever reported.
    pub latest_report: Option<String>,
    /// Whether the latest report predates the parent's latest activity.
    pub is_stale: bool,
    /// Whether the attendant is still being composed, so nothing will run.
    /// Rendered beside the name, never inside it, so a rename can never
    /// reach the marker.
    pub is_prepping: bool,
    /// Whether the attendant runs on its parent's completion. Its own
    /// marker, because "fires on its own" is a different fact from "cannot
    /// fire at all" and one glyph cannot say both.
    pub fires_on_parent_completion: bool,
}

/// The session whose attendants the sidebar section is showing.
///
/// An attendant is read in the context of the parent it reports to, so
/// viewing one resolves to that parent — which is also what makes an
/// attendant's siblings visible from inside a sibling.
#[must_use]
pub fn attendant_context_of(state: &AppState) -> jinn_core_types::SessionId {
    attendant_context_of_split(&state.session)
}

/// Split-borrow variant of [`attendant_context_of`].
#[must_use]
pub fn attendant_context_of_split(
    session: &jinn_session_state::SessionMap,
) -> jinn_core_types::SessionId {
    let active = session.active_session();
    match active.parent_session() {
        Some(parent) => parent.clone(),
        None => active.session_id().clone(),
    }
}

/// Every attendant in the active session's context, sorted by name, with
/// report and staleness data.
///
/// The section shows the attendants *belonging to the session being read*:
/// sitting on a parent shows that parent's attendants, and sitting on one of
/// its attendants shows the whole set — the same list, because the attendant
/// has no attendants of its own and its siblings are the relevant context.
/// A section that listed every attendant in the store would answer a
/// question the user did not ask from a screen they are not on.
#[must_use]
pub fn attendant_rows(state: &AppState) -> Vec<AttendantRow> {
    attendant_rows_split(&state.session, &state.frontend)
}

/// Split-borrow variant of [`attendant_rows`].
///
/// Same rows in the same order — one implementation, so a caller holding
/// mutable frontend state (the sidebar's removal path) resolves the list
/// through exactly the function the render pass uses rather than rebuilding it.
#[must_use]
pub fn attendant_rows_split(
    session: &jinn_session_state::SessionMap,
    _frontend: &jinn_kernel::state::frontend_state::FrontendState,
) -> Vec<AttendantRow> {
    let context = attendant_context_of_split(session);
    let mut rows: Vec<AttendantRow> = session
        .iter()
        .filter(|(_, session)| {
            session.is_attendant()
                && session.parent_session().as_ref() == Some(&context)
                && session.session_state() == jinn_session_store_msg::SessionState::Loaded
        })
        .map(|(id, attendant)| {
            let latest = attendant.latest_attendant_report();
            let parent_activity = attendant
                .parent_session()
                .as_ref()
                .and_then(|parent_id| session.get(parent_id))
                .map(|parent| *parent.last_history_activity_at());
            let is_stale = match (latest, parent_activity) {
                (Some(report), Some(activity)) => report.published_at < activity,
                _ => false,
            };
            AttendantRow {
                session_id: id.clone(),
                name: attendant.title().unwrap_or("Untitled Session").to_owned(),
                latest_report: latest.map(|report: &AttendantReport| report.body.clone()),
                is_stale,
                is_prepping: attendant.attendant_is_prepping(),
                fires_on_parent_completion: attendant.attendant_fires_on_parent_completion(),
            }
        })
        .collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    rows
}

/// The highlighted attendant's full report log, for the history picker's
/// opener.
///
/// Returns `None` when nothing is highlighted or the selection is not a
/// loaded attendant; an attendant with no reports yields an empty vec, which
/// the picker renders as its never-reported state.
#[must_use]
pub fn highlighted_reports(state: &AppState) -> Option<Vec<jinn_attendant_msg::AttendantReport>> {
    let id = state.frontend.with_sections(
        |sections: &jinn_sidebar_msg::SidebarSections| sections.attendant.selected_id.clone(),
        || None,
    )?;
    state
        .session
        .get(&id)
        .map(|a| a.attendant_reports().to_vec())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, reason = "test code")]

    /// Builds state with `parent` active and `attendants` installed under it.
    ///
    /// The section is scoped to the active session's context, so every
    /// fixture has to say which session the user is reading.
    fn state_viewing(parent: ChatSessionState, attendants: Vec<ChatSessionState>) -> AppState {
        let mut state = AppState::default_with_scope_focus();
        let parent_id = parent.session_id().clone();
        state.session.insert(parent);
        for attendant in attendants {
            state.session.insert(attendant);
        }
        state.session.set_active(parent_id);
        state
    }

    use super::*;
    use jinn_app_state::AppState;
    use jinn_session_state::ChatSessionState;

    #[rstest::rstest]
    #[test]
    fn a_report_goes_stale_when_the_parent_resumes() {
        // Given an attendant whose report predates the parent's history.
        let mut parent = ChatSessionState::new();
        let mut attendant = ChatSessionState::new_attendant(&parent, true);
        // The report is published before the parent resumes.
        attendant.append_attendant_report("finding".to_owned());
        // The parent has since produced history (resumed work).
        parent.push_entry(jinn_core_types::chat_entry::ChatEntry::user(
            "parent resumed",
        ));
        let state = state_viewing(parent, vec![attendant]);

        // When the section rows are built.
        let rows = attendant_rows(&state);

        // Then the report renders stale.
        assert_eq!(rows.len(), 1);
        assert!(
            rows.first().expect("a row").is_stale,
            "a report older than the parent's resume is stale"
        );
    }

    #[rstest::rstest]
    #[test]
    fn a_fresh_report_is_not_stale() {
        // Given an attendant whose report postdates the parent's history.
        let mut parent = ChatSessionState::new();
        let entry = jinn_core_types::chat_entry::ChatEntry::user("parent worked");
        parent.push_entry(entry);
        let attendant = ChatSessionState::new_attendant(&parent, true);
        let state = state_viewing(parent, vec![attendant]);

        // When the section rows are built.
        let rows = attendant_rows(&state);

        // Then the report is fresh — it was published after the resume.
        assert_eq!(rows.len(), 1);
        assert!(!rows.first().expect("a row").is_stale);
    }

    #[rstest::rstest]
    #[test]
    fn a_sibling_fresh_report_never_clears_another_attendants_stale_one() {
        // Given two attendants of the same parent: one reported before the
        // parent resumed, one after.
        let mut parent = ChatSessionState::new();
        parent.push_entry(jinn_core_types::chat_entry::ChatEntry::user("first work"));
        let parent_id = parent.session_id().clone();

        let mut early = ChatSessionState::new_attendant(&parent, true);
        early.set_parent_session(parent_id.clone());
        early.set_title("early".to_owned());
        early.append_attendant_report("early finding".to_owned());

        let mut late = ChatSessionState::new_attendant(&parent, true);
        late.set_parent_session(parent_id.clone());
        late.set_title("late".to_owned());

        // The parent resumes work after the early report.
        parent.push_entry(jinn_core_types::chat_entry::ChatEntry::user(
            "parent resumed",
        ));

        // The late attendant reports after the resume.
        late.append_attendant_report("late finding".to_owned());

        let state = state_viewing(parent, vec![early, late]);

        // When the section rows are built.
        let rows = attendant_rows(&state);

        // Then each attendant's staleness is its own: the early report is
        // stale, the late one fresh — a sibling's report never launders it.
        let early_row = rows.iter().find(|r| r.name == "early").expect("early row");
        let late_row = rows.iter().find(|r| r.name == "late").expect("late row");
        assert!(early_row.is_stale);
        assert!(!late_row.is_stale);
    }

    #[rstest::rstest]
    #[test]
    fn renaming_an_attendant_updates_its_row_name() {
        // Given an attendant with a name.
        let parent = ChatSessionState::new();
        let mut attendant = ChatSessionState::new_attendant(&parent, true);
        attendant.set_title("before".to_owned());
        let id = attendant.session_id().clone();
        let mut state = state_viewing(parent, vec![attendant]);

        // When the attendant is renamed.
        state
            .session
            .get_mut(&id)
            .expect("the attendant")
            .set_title("after".to_owned());

        // Then the section row carries the new name on the next build.
        let rows = attendant_rows(&state);
        assert_eq!(rows.first().expect("a row").name, "after");
    }

    #[rstest::rstest]
    #[test]
    fn an_attendant_that_never_reported_has_no_latest_report() {
        // Given an attendant with no reports.
        let parent = ChatSessionState::new();
        let state = state_viewing(
            parent.clone(),
            vec![ChatSessionState::new_attendant(&parent, true)],
        );

        // When the section rows are built.
        let rows = attendant_rows(&state);

        // Then the row has no latest report — the renderer shows the
        // never-reported marker, and it is not stale (nothing to outdate).
        assert_eq!(rows.len(), 1);
        assert_eq!(rows.first().expect("a row").latest_report, None);
        assert!(!rows.first().expect("a row").is_stale);
    }

    #[rstest::rstest]
    #[test]
    fn a_triggered_attendant_with_no_report_is_distinct_from_no_attendant() {
        // Given a parent with one attendant that has a trigger but no
        // report, and no other attendants.
        let parent = ChatSessionState::new();
        let mut attendant = ChatSessionState::new_attendant(&parent, true);
        attendant.set_attendant_trigger(jinn_attendant_msg::AttendantTrigger::ParentCompleted);
        let state = state_viewing(parent, vec![attendant]);

        // When the section rows are built.
        let rows = attendant_rows(&state);

        // Then the section has content — one row, never-reported — which is
        // distinct from a parent with no attendants at all (zero rows, the
        // section collapses).
        assert_eq!(rows.len(), 1);
        assert_eq!(rows.first().expect("a row").latest_report, None);

        // And an otherwise identical state without the attendant has none.
        let mut empty = AppState::default_with_scope_focus();
        empty.session.insert(ChatSessionState::new());
        assert_eq!(attendant_rows(&empty), [] as [AttendantRow; 0]);
    }
}
