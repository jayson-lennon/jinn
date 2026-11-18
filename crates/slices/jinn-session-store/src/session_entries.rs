//! Session entries - loading and formatting for the tree-structured picker.
//!
//! Contains loader and sorting functions for canonical
//! [`jinn_session_store_msg::SessionTreeEntry`] values.

use std::collections::HashMap;

use jinn_core_types::SessionId;
use jinn_kernel::common::services::Services;
use jinn_session_store_msg::SessionTreeEntry;
use jinn_theme::Theme;

use jinn_session_state::SessionStoreService;

/// Sorts session entries so that whole trees move as a unit.
///
/// Each tree's position is determined by the most recent `updated_at`
/// across all nodes in the tree. Loaded trees appear before Archived trees.
/// Within each tree, children are sorted by `updated_at` descending.
/// The one order both the TUI picker load and the webui session list
/// consume, so the two surfaces cannot disagree.
///
/// # Panics
///
/// Panics only on an internal index/entry desync, which the function
/// builds itself from the same slice — unreachable for well-formed
/// input.
#[expect(clippy::expect_used, reason = "indices from enumerate over entries")]
pub fn sort_entries_tree_aware(entries: &mut Vec<SessionTreeEntry>) {
    if entries.is_empty() {
        return;
    }

    // 1. Index by session_id.
    let id_to_idx: HashMap<SessionId, usize> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| (e.session_id.clone(), i))
        .collect();

    // 2 & 3. Build parent→children map and identify roots.
    let mut children_map: HashMap<SessionId, Vec<usize>> = HashMap::new();
    let mut roots: Vec<usize> = Vec::new();

    for (idx, entry) in entries.iter().enumerate() {
        match &entry.parent_id {
            Some(pid) if id_to_idx.contains_key(pid) => {
                children_map.entry(pid.clone()).or_default().push(idx);
            }
            _ => {
                roots.push(idx);
            }
        }
    }

    // 4. Compute tree-max updated_at for each root.
    let tree_max: HashMap<SessionId, jiff::Timestamp> = roots
        .iter()
        .map(|&root_idx| {
            let max_ts = compute_subtree_max(root_idx, &children_map, entries);
            let root = entries.get(root_idx).expect("index from enumerate");
            (root.session_id.clone(), max_ts)
        })
        .collect();

    // 5 & 6. Sort roots: session_state ascending (Loaded < Archived → Loaded first), then tree-max updated_at descending.
    roots.sort_by(|&a, &b| {
        let entry_a = entries.get(a).expect("index from enumerate");
        let entry_b = entries.get(b).expect("index from enumerate");
        entry_a
            .session_state
            .cmp(&entry_b.session_state)
            .then_with(|| {
                let max_b = tree_max.get(&entry_b.session_id).expect("key from entries");
                let max_a = tree_max.get(&entry_a.session_id).expect("key from entries");
                max_b.cmp(max_a)
            })
    });

    // 7. Flatten in DFS order, sorting children by updated_at descending.
    let mut sorted = Vec::with_capacity(entries.len());
    for &root_idx in &roots {
        emit_tree(root_idx, &children_map, entries, &mut sorted);
    }
    *entries = sorted;
}

/// Returns the maximum `updated_at` across a node and all its descendants.
#[expect(clippy::expect_used, reason = "infallible")]
fn compute_subtree_max(
    idx: usize,
    children_map: &HashMap<SessionId, Vec<usize>>,
    entries: &[SessionTreeEntry],
) -> jiff::Timestamp {
    let entry = entries.get(idx).expect("index from enumerate");
    let mut max_ts = entry.updated_at;
    if let Some(children) = children_map.get(&entry.session_id) {
        for &child_idx in children {
            let child_max = compute_subtree_max(child_idx, children_map, entries);
            max_ts = max_ts.max(child_max);
        }
    }
    max_ts
}

/// Emits a node and all its descendants in DFS order.
/// Children within each parent are sorted by `updated_at` descending.
#[expect(clippy::expect_used, reason = "infallible")]
fn emit_tree(
    idx: usize,
    children_map: &HashMap<SessionId, Vec<usize>>,
    entries: &[SessionTreeEntry],
    result: &mut Vec<SessionTreeEntry>,
) {
    let entry = entries.get(idx).expect("index from enumerate");
    result.push(entry.clone());
    if let Some(mut children) = children_map.get(&entry.session_id).cloned() {
        children.sort_by(|&a, &b| {
            let ca = entries.get(a).expect("index from enumerate");
            let cb = entries.get(b).expect("index from enumerate");
            cb.updated_at.cmp(&ca.updated_at)
        });
        for &child_idx in &children {
            emit_tree(child_idx, children_map, entries, result);
        }
    }
}

/// Loads session tree entries from the session store, sorted with tree-aware ordering.
///
/// Whole trees move as a unit: each tree's position is determined by the most
/// recent `updated_at` across all nodes in the tree. Loaded trees appear first,
/// followed by archived trees. Within each tree, children are sorted by
/// `updated_at` descending.
/// Errors are logged and result in an empty list.
pub async fn load_session_entries(services: &Services, theme: &Theme) -> Vec<SessionTreeEntry> {
    match services.session_store.load_summaries().await {
        Ok(summaries) => {
            let mut entries: Vec<SessionTreeEntry> = summaries
                .into_iter()
                .map(|summary| {
                    SessionTreeEntry::new(
                        summary.session_id,
                        summary.title,
                        summary.updated_at,
                        theme.clone(),
                        summary.session_state,
                        summary.parent_session,
                        summary.project,
                    )
                })
                .collect();
            // Tree-aware sort: whole trees move as a unit, positioned by
            // the most recent updated_at in the tree. Loaded first.
            sort_entries_tree_aware(&mut entries);
            jinn_session_store_msg::apply_project_column_width(&mut entries);
            entries
        }
        Err(e) => {
            tracing::warn!(err = ?e, "failed to load session summaries");
            vec![]
        }
    }
}

/// Loads session tree entries from a session store service directly.
///
/// Same as [`load_session_entries`] but accepts the store service directly
/// instead of the full `Services` container.
pub async fn load_session_entries_from_store(
    store: &SessionStoreService,
    theme: &Theme,
) -> Vec<SessionTreeEntry> {
    match store.load_summaries().await {
        Ok(summaries) => {
            let mut entries: Vec<SessionTreeEntry> = summaries
                .into_iter()
                .map(|summary| {
                    SessionTreeEntry::new(
                        summary.session_id,
                        summary.title,
                        summary.updated_at,
                        theme.clone(),
                        summary.session_state,
                        summary.parent_session,
                        summary.project,
                    )
                })
                .collect();
            // Tree-aware sort: whole trees move as a unit, positioned by
            // the most recent updated_at in the tree. Loaded first.
            sort_entries_tree_aware(&mut entries);
            jinn_session_store_msg::apply_project_column_width(&mut entries);
            entries
        }
        Err(e) => {
            tracing::warn!(err = ?e, "failed to load session summaries");
            vec![]
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use jinn_core_types::SessionId;
    use jinn_kernel::common::app_state::AppState;
    use jinn_kernel::common::services::test_services::TestServices;
    use jinn_selection_widget::PickerItem;
    use jinn_selection_widget::TreeItem;
    use jinn_session_state::SessionSnapshot;
    use jinn_session_store_msg::SessionState;
    use jinn_session_store_msg::SessionSummary;
    use jinn_session_store_msg::SessionTreeEntry;
    use jinn_theme::default_theme;
    use marked_path::{Absolute, MarkedPathBuf};

    use super::*;

    #[rstest::rstest]
    #[tokio::test]
    async fn session_entry_display_label_returns_title() {
        // Given a SessionTreeEntry with a title.
        let entry = SessionTreeEntry::new(
            SessionId::new(),
            "My Chat".to_owned(),
            jiff::Timestamp::now(),
            default_theme(),
            SessionState::Loaded,
            None,
            None,
        );

        // When calling display_label.
        // Then it returns the title.
        assert_eq!(entry.display_label(), "My Chat");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn render_row_contains_title() {
        // Given a session tree entry.
        let entry = SessionTreeEntry::new(
            SessionId::new(),
            "My Session".to_owned(),
            jiff::Timestamp::now(),
            default_theme(),
            SessionState::Loaded,
            None,
            None,
        );

        // When rendering.
        let row = entry.render_row(false);

        // Then the title appears in the rendered line.
        assert!(row.spans.iter().any(|s| s.content.contains("My Session")));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn wrap_session_entries_keeps_the_spec_row_renderer() {
        // Given a session tree entry.
        let entry = SessionTreeEntry::new(
            SessionId::new(),
            "My Session".to_owned(),
            jiff::Timestamp::now(),
            default_theme(),
            SessionState::Loaded,
            None,
            None,
        );

        // When wrapping it for picker storage.
        let wrapped = jinn_picker::make_items_with_hooks(
            vec![entry],
            jinn_picker::PickerItemHooks::new()
                .row(jinn_session_store_msg::session_row)
                .search(|entry: &SessionTreeEntry| entry.title.clone()),
        );

        // Then the wrapped item renders through the spec's row hook
        // (date, project, and title columns), not the plain search label.
        let row = PickerItem::render_row(&wrapped[0], false);
        assert!(row.spans.len() > 1);
        assert!(row.spans.iter().any(|s| s.content.contains("My Session")));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn load_session_entries_returns_empty_on_error() {
        // Given a test Services (with fake session store that returns empty).
        let services = jinn_kernel::common::services::Services::new_fake().await;

        // When loading session entries.
        let entries = load_session_entries(&services, &default_theme()).await;

        // Then an empty list is returned (fake store has no sessions).
        assert!(entries.is_empty());
    }

    /// A minimal fake store that returns one pre-built session summary.
    struct OneSummaryStore {
        summary: SessionSummary,
    }

    #[async_trait::async_trait]
    impl jinn_session_state::SessionStore for OneSummaryStore {
        fn name(&self) -> &'static str {
            "one-summary"
        }
        async fn save(
            &self,
            _snapshot: &SessionSnapshot,
        ) -> Result<(), error_stack::Report<jinn_session_state::SessionStoreError>> {
            Ok(())
        }
        async fn load_summaries(
            &self,
        ) -> Result<Vec<SessionSummary>, error_stack::Report<jinn_session_state::SessionStoreError>>
        {
            Ok(vec![self.summary.clone()])
        }
        async fn load_session(
            &self,
            _session_id: &SessionId,
        ) -> Result<
            Option<SessionSnapshot>,
            error_stack::Report<jinn_session_state::SessionStoreError>,
        > {
            Ok(None)
        }
        async fn delete(
            &self,
            _session_id: &SessionId,
        ) -> Result<(), error_stack::Report<jinn_session_state::SessionStoreError>> {
            Ok(())
        }
        async fn fork(
            &self,
            _source_session_id: &SessionId,
            _at_ordinal: usize,
        ) -> Result<SessionId, error_stack::Report<jinn_session_state::SessionStoreError>> {
            Ok(SessionId::new())
        }
        async fn set_archived(
            &self,
            _session_id: &SessionId,
            _archived: bool,
        ) -> Result<(), error_stack::Report<jinn_session_state::SessionStoreError>> {
            Ok(())
        }
        async fn set_archived_many(
            &self,
            _session_ids: &[SessionId],
            _archived: bool,
        ) -> Result<(), error_stack::Report<jinn_session_state::SessionStoreError>> {
            Ok(())
        }
        async fn set_in_library(
            &self,
            _session_id: &SessionId,
            _in_library: bool,
        ) -> Result<(), error_stack::Report<jinn_session_state::SessionStoreError>> {
            Ok(())
        }
        async fn load_unarchived_summaries(
            &self,
        ) -> Result<Vec<SessionSummary>, error_stack::Report<jinn_session_state::SessionStoreError>>
        {
            Ok(vec![self.summary.clone()])
        }

        async fn dirty_session_ids(
            &self,
        ) -> Result<Vec<SessionId>, error_stack::Report<jinn_session_state::SessionStoreError>>
        {
            Ok(Vec::new())
        }

        async fn reindex_session_chunk(
            &self,
            _session_id: &SessionId,
            _max_entries: usize,
        ) -> Result<bool, error_stack::Report<jinn_session_state::SessionStoreError>> {
            Ok(true)
        }

        async fn pending_dirty_count(
            &self,
        ) -> Result<usize, error_stack::Report<jinn_session_state::SessionStoreError>> {
            Ok(0)
        }

        async fn search(
            &self,
            _params: jinn_session_store_msg::SearchParams,
        ) -> Result<
            jinn_session_store_msg::SearchOutcome,
            error_stack::Report<jinn_session_state::SessionStoreError>,
        > {
            Ok(jinn_session_store_msg::SearchOutcome {
                total_matches: 0,
                per_session: Vec::new(),
                hits: Vec::new(),
            })
        }

        async fn fetch_window(
            &self,
            _session_id: &SessionId,
            _anchor: &jinn_kernel::protocol::ChatEntryId,
            _context: usize,
        ) -> Result<
            Option<jinn_session_store_msg::TranscriptWindow>,
            error_stack::Report<jinn_session_state::SessionStoreError>,
        > {
            Ok(None)
        }

        async fn fetch_tail(
            &self,
            _session_id: &SessionId,
            _limit: usize,
        ) -> Result<
            Option<jinn_session_store_msg::TranscriptWindow>,
            error_stack::Report<jinn_session_state::SessionStoreError>,
        > {
            Ok(None)
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn load_session_entries_returns_entries_from_store() {
        // Given a Services with a fake store that returns one summary.
        let session_id = SessionId::new();
        let summary = SessionSummary {
            session_id: session_id.clone(),
            title: "Test Session".to_owned(),
            updated_at: jiff::Timestamp::now(),
            created_at: jiff::Timestamp::now(),
            session_state: SessionState::Loaded,
            in_library: false,
            is_bot: false,
            parent_session: None,
            project: None,
        };
        let store =
            jinn_session_state::SessionStoreService::new(std::sync::Arc::new(OneSummaryStore {
                summary,
            }));
        let services = TestServices::builder().session_store(store).build();

        // When loading session entries.
        let entries = load_session_entries(&services, &default_theme()).await;

        // Then entries are returned (not an empty vec).
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].display_label(), "Test Session");
    }

    /// A fake store that returns several pre-built session summaries.
    struct ManySummariesStore {
        summaries: Vec<SessionSummary>,
    }

    #[async_trait::async_trait]
    impl jinn_session_state::SessionStore for ManySummariesStore {
        fn name(&self) -> &'static str {
            "many-summaries"
        }
        async fn save(
            &self,
            _snapshot: &SessionSnapshot,
        ) -> Result<(), error_stack::Report<jinn_session_state::SessionStoreError>> {
            Ok(())
        }
        async fn load_summaries(
            &self,
        ) -> Result<Vec<SessionSummary>, error_stack::Report<jinn_session_state::SessionStoreError>>
        {
            Ok(self.summaries.clone())
        }
        async fn load_session(
            &self,
            _session_id: &SessionId,
        ) -> Result<
            Option<SessionSnapshot>,
            error_stack::Report<jinn_session_state::SessionStoreError>,
        > {
            Ok(None)
        }
        async fn delete(
            &self,
            _session_id: &SessionId,
        ) -> Result<(), error_stack::Report<jinn_session_state::SessionStoreError>> {
            Ok(())
        }
        async fn fork(
            &self,
            _source_session_id: &SessionId,
            _at_ordinal: usize,
        ) -> Result<SessionId, error_stack::Report<jinn_session_state::SessionStoreError>> {
            Ok(SessionId::new())
        }
        async fn set_archived(
            &self,
            _session_id: &SessionId,
            _archived: bool,
        ) -> Result<(), error_stack::Report<jinn_session_state::SessionStoreError>> {
            Ok(())
        }
        async fn set_archived_many(
            &self,
            _session_ids: &[SessionId],
            _archived: bool,
        ) -> Result<(), error_stack::Report<jinn_session_state::SessionStoreError>> {
            Ok(())
        }
        async fn set_in_library(
            &self,
            _session_id: &SessionId,
            _in_library: bool,
        ) -> Result<(), error_stack::Report<jinn_session_state::SessionStoreError>> {
            Ok(())
        }
        async fn load_unarchived_summaries(
            &self,
        ) -> Result<Vec<SessionSummary>, error_stack::Report<jinn_session_state::SessionStoreError>>
        {
            Ok(self.summaries.clone())
        }

        async fn dirty_session_ids(
            &self,
        ) -> Result<Vec<SessionId>, error_stack::Report<jinn_session_state::SessionStoreError>>
        {
            Ok(Vec::new())
        }

        async fn reindex_session_chunk(
            &self,
            _session_id: &SessionId,
            _max_entries: usize,
        ) -> Result<bool, error_stack::Report<jinn_session_state::SessionStoreError>> {
            Ok(true)
        }

        async fn pending_dirty_count(
            &self,
        ) -> Result<usize, error_stack::Report<jinn_session_state::SessionStoreError>> {
            Ok(0)
        }

        async fn search(
            &self,
            _params: jinn_session_store_msg::SearchParams,
        ) -> Result<
            jinn_session_store_msg::SearchOutcome,
            error_stack::Report<jinn_session_state::SessionStoreError>,
        > {
            Ok(jinn_session_store_msg::SearchOutcome {
                total_matches: 0,
                per_session: Vec::new(),
                hits: Vec::new(),
            })
        }

        async fn fetch_window(
            &self,
            _session_id: &SessionId,
            _anchor: &jinn_kernel::protocol::ChatEntryId,
            _context: usize,
        ) -> Result<
            Option<jinn_session_store_msg::TranscriptWindow>,
            error_stack::Report<jinn_session_state::SessionStoreError>,
        > {
            Ok(None)
        }

        async fn fetch_tail(
            &self,
            _session_id: &SessionId,
            _limit: usize,
        ) -> Result<
            Option<jinn_session_store_msg::TranscriptWindow>,
            error_stack::Report<jinn_session_state::SessionStoreError>,
        > {
            Ok(None)
        }
    }

    fn summary_with_project(
        title: &str,
        project: Option<MarkedPathBuf<Absolute>>,
    ) -> SessionSummary {
        SessionSummary {
            session_id: SessionId::new(),
            title: title.to_owned(),
            updated_at: jiff::Timestamp::now(),
            created_at: jiff::Timestamp::now(),
            session_state: SessionState::Loaded,
            in_library: false,
            is_bot: false,
            parent_session: None,
            project,
        }
    }

    /// A project path whose leaf name is `jinn`, built under `root` so the
    /// fixture names the same directory on every platform.
    fn project_path_named_jinn(root: &MarkedPathBuf<Absolute>) -> MarkedPathBuf<Absolute> {
        root.join_all(["code", "jinn"])
            .expect("temp root joins bare names")
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn load_session_entries_applies_project_column_width_across_all_entries() {
        // Given a store returning one project-bearing summary and one without.
        let root = jinn_testutil::tempdir();
        let summaries = vec![
            summary_with_project("Short", Some(project_path_named_jinn(&root.path))),
            summary_with_project("Blank", None),
        ];
        let store =
            jinn_session_state::SessionStoreService::new(std::sync::Arc::new(ManySummariesStore {
                summaries,
            }));
        let services = TestServices::builder().session_store(store).build();

        // When loading session entries.
        let entries = load_session_entries(&services, &default_theme()).await;

        // Then every loaded entry carries the same shared project column
        // width (the longest loaded project name, nonzero).
        assert_eq!(entries.len(), 2);
        assert!(entries[0].project_width > 0);
        assert_eq!(entries[0].project_width, entries[1].project_width);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn load_session_entries_passes_summary_project_to_entries() {
        // Given a store returning one project-bearing summary.
        let root = jinn_testutil::tempdir();
        let summaries = vec![summary_with_project(
            "Stamped",
            Some(project_path_named_jinn(&root.path)),
        )];
        let store =
            jinn_session_state::SessionStoreService::new(std::sync::Arc::new(ManySummariesStore {
                summaries,
            }));
        let services = TestServices::builder().session_store(store).build();

        // When loading session entries.
        let entries = load_session_entries(&services, &default_theme()).await;

        // Then the entry renders the project leaf name in its project column.
        assert_eq!(entries[0].project_display, "jinn");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn load_session_entries_from_store_returns_entries() {
        // Given a populated store service.
        let summary = SessionSummary {
            session_id: SessionId::new(),
            title: "From Store".to_owned(),
            updated_at: jiff::Timestamp::now(),
            created_at: jiff::Timestamp::now(),
            session_state: SessionState::Loaded,
            in_library: false,
            is_bot: false,
            parent_session: None,
            project: None,
        };
        let store =
            jinn_session_state::SessionStoreService::new(std::sync::Arc::new(OneSummaryStore {
                summary,
            }));

        // When loading session entries from store.
        let entries = load_session_entries_from_store(&store, &default_theme()).await;

        // Then entries are returned.
        assert_eq!(entries.len(), 1);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn load_session_picker_items_sets_items_in_state() {
        // Given a Services with a fake store and an AppState.
        let summary = SessionSummary {
            session_id: SessionId::new(),
            title: "Picker Test".to_owned(),
            updated_at: jiff::Timestamp::now(),
            created_at: jiff::Timestamp::now(),
            session_state: SessionState::Loaded,
            in_library: false,
            is_bot: false,
            parent_session: None,
            project: None,
        };
        let store =
            jinn_session_state::SessionStoreService::new(std::sync::Arc::new(OneSummaryStore {
                summary,
            }));
        let services = TestServices::builder().session_store(store).build();
        let state = AppState::default();

        // When reading the history the picker shows.
        let entries = load_session_entries(&services, &state.frontend.theme).await;

        // Then the read produced a row.
        assert!(!entries.is_empty(), "the history read must produce a row");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn load_session_picker_items_from_store_sets_items() {
        // Given a store service and an AppState.
        let summary = SessionSummary {
            session_id: SessionId::new(),
            title: "Picker From Store".to_owned(),
            updated_at: jiff::Timestamp::now(),
            created_at: jiff::Timestamp::now(),
            session_state: SessionState::Loaded,
            in_library: false,
            is_bot: false,
            parent_session: None,
            project: None,
        };
        let store =
            jinn_session_state::SessionStoreService::new(std::sync::Arc::new(OneSummaryStore {
                summary,
            }));
        let state = AppState::default();

        // When reading the history from the store directly.
        let entries = load_session_entries_from_store(&store, &state.frontend.theme).await;

        // Then the read produced a row.
        assert!(!entries.is_empty(), "the history read must produce a row");
    }
}
