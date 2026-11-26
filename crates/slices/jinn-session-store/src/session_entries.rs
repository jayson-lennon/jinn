//! Session entries — loading the tree-structured picker's rows.
//!
//! The tree-aware order and the summaries → entries build live in the
//! contract crate ([`jinn_session_store_msg::session_entries`]), where a
//! contract-only consumer can reach them; this module re-exports the sort
//! under its long-standing path and holds the two loaders that read the
//! store.

use jinn_kernel::common::services::Services;
use jinn_session_store_msg::SessionTreeEntry;
use jinn_theme::Theme;

use jinn_session_state::SessionStoreService;

pub use jinn_session_store_msg::session_entries::sort_entries_tree_aware;

/// Loads session tree entries from the session store, sorted with tree-aware ordering.
///
/// Whole trees move as a unit: each tree's position is determined by the most
/// recent `updated_at` across all nodes in the tree. Loaded trees appear first,
/// followed by archived trees. Within each tree, children are sorted by
/// `updated_at` descending.
/// Errors are logged and result in an empty list.
pub async fn load_session_entries(services: &Services, theme: &Theme) -> Vec<SessionTreeEntry> {
    match services.session_store.load_summaries().await {
        Ok(summaries) => jinn_session_store_msg::session_tree_entries(summaries, theme),
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
        Ok(summaries) => jinn_session_store_msg::session_tree_entries(summaries, theme),
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
