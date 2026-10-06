//! Single-session and whole-tree archiving.

use std::collections::HashMap;

use jinn_core_types::SessionId;
use jinn_core_types::SessionProfile;
use jinn_kernel::common::actor_deps::BusPublish;
use jinn_session_state::{ChatSessionState, SessionSnapshot, snapshot_frozen_node};
use jinn_session_store_msg::SessionState;
use jinn_session_store_msg::{ArchiveSession, ArchiveSessionTree};

use jinn_session_msg::{
    SessionArchiveFailed, SessionArchived, SessionClosed, SessionRemoved, SessionSeed,
};

use crate::session_store_actor::SessionStoreActor;

impl SessionStoreActor {
    /// Archives a session without running a teardown script.
    pub(crate) async fn handle_archive_session(&self, payload: &ArchiveSession) {
        self.archive_members(std::slice::from_ref(&payload.session_id))
            .await;
    }

    /// Archives a session and all descendants, all-or-nothing.
    pub(crate) async fn handle_archive_session_tree(&self, payload: &ArchiveSessionTree) {
        let Some(members) = self.guarded_tree_closure(&payload.root).await else {
            return;
        };
        self.archive_members(&members).await;
    }

    /// Resolves the tree and aborts before any side effect when a member is busy.
    async fn guarded_tree_closure(&self, root: &SessionId) -> Option<Vec<SessionId>> {
        let members = self.resolve_tree_closure(root).await;
        let busy = {
            let state = self.state.read();
            members.iter().any(|id| {
                state.session.get(id).is_some_and(|session| {
                    !matches!(session.phase(), jinn_session_msg::PhaseKind::Idle)
                })
            })
        };
        if busy {
            tracing::warn!(root = %root, "tree action aborted: a member session is busy");
            self.publish_archive_failed(&members, "a member session is busy")
                .await;
            return None;
        }
        Some(members)
    }

    /// Announces that archiving each member did not complete and it stays live.
    ///
    /// One event per member, so a listener tracking several tinted sessions
    /// clears every one of them rather than only the tree root.
    async fn publish_archive_failed(&self, members: &[SessionId], error: &str) {
        for session_id in members {
            self.publish(SessionArchiveFailed {
                session_id: session_id.clone(),
                error: error.to_owned(),
            })
            .await;
        }
        self.raise_archive_failed_hint(members, error);
    }

    /// Tells the user which session failed to archive, and why.
    ///
    /// A failed archive used to be a silent no-op: the only listener of
    /// `SessionArchiveFailed` cleared the row's in-flight tint, so the keypress
    /// looked like it had done nothing. The hint is the first place the cause
    /// reaches the screen, and it names the session because a tree archive can
    /// fail on any one of its members.
    fn raise_archive_failed_hint(&self, members: &[SessionId], error: &str) {
        let Some(status) = self
            .services
            .slices
            .reader::<jinn_status_bar_msg::StatusBarState>(&jinn_status_bar_msg::status_bar_slot())
        else {
            // The status bar is not activated in every host (a headless test
            // app, an embedder running a bare actor set). A hint with nowhere
            // to land must not be an error of its own.
            tracing::debug!(%error, "no status bar to report a failed archive to");
            return;
        };
        let session = members
            .first()
            .map_or_else(String::new, ToString::to_string);
        let message = if members.len() == 1 {
            format!("could not archive session {session}: {error}")
        } else {
            format!(
                "could not archive {} sessions from {session}: {error}",
                members.len()
            )
        };
        status.update(|state| state.hint = Some(message));
    }

    /// Resolves a root's subtree across loaded sessions and store summaries.
    async fn resolve_tree_closure(&self, root: &SessionId) -> Vec<SessionId> {
        let mut parent_of = self.parent_links_from_memory();
        match self.services.session_store.load_summaries().await {
            Ok(summaries) => {
                for summary in summaries {
                    parent_of
                        .entry(summary.session_id)
                        .or_insert(summary.parent_session);
                }
            }
            Err(error) => {
                tracing::warn!(
                    root = %root,
                    ?error,
                    "could not read store for tree closure; using memory only"
                );
            }
        }
        jinn_session_list::descendant_closure(root, &parent_of)
    }

    /// Snapshots parent links for every loaded session.
    fn parent_links_from_memory(&self) -> HashMap<SessionId, Option<SessionId>> {
        self.state
            .read()
            .session
            .iter()
            .map(|(id, session)| (id.clone(), session.parent_session().clone()))
            .collect()
    }

    /// Archives all requested members durably before changing live state.
    async fn archive_members(&self, members: &[SessionId]) {
        let Some(snapshots) = self.archive_snapshots(members).await else {
            self.publish_archive_failed(members, "could not capture every member snapshot")
                .await;
            return;
        };
        if let Err(error) = self
            .services
            .session_store
            .archive_snapshots(&snapshots)
            .await
        {
            tracing::warn!(
                ?error,
                member_count = members.len(),
                "archive write failed; live sessions remain intact"
            );
            self.publish_archive_failed(members, "archiving the session write failed")
                .await;
            return;
        }

        for session_id in members {
            if !self.state.read().session.contains(session_id) {
                continue;
            }
            self.snapshot_before_removal(session_id);
            // Read before the removal: `remove_and_replace` points the active
            // id at whichever session `HashMap` iteration yields first, so
            // after it there is no way back to whether this was the session the
            // user was reading.
            let was_active = self.state.read().session.active_session_id() == session_id;
            let (removed_parent, mcp_enablement) = self.remove_and_replace(session_id);
            self.publish(SessionRemoved {
                session_id: session_id.clone(),
                removed_parent,
                was_active,
            })
            .await;
            self.publish(SessionArchived {
                session_id: session_id.clone(),
            })
            .await;
            self.publish(SessionClosed {
                session_id: session_id.clone(),
            })
            .await;
            if let Some(enablement) = mcp_enablement {
                self.publish(enablement).await;
            }
        }
    }

    /// Captures one complete archived snapshot for each requested member.
    ///
    /// A member that is live uses the authoritative in-memory state. A member
    /// that is not live is loaded from the store so an archive tree can update
    /// persisted descendants without making them live first.
    async fn archive_snapshots(&self, members: &[SessionId]) -> Option<Vec<SessionSnapshot>> {
        let mut snapshots = Vec::with_capacity(members.len());
        for session_id in members {
            let mut snapshot = {
                let state = self.state.read();
                state
                    .session
                    .get(session_id)
                    .map(ChatSessionState::capture_snapshot)
            };
            if snapshot.is_none() {
                snapshot = match self.services.session_store.load_session(session_id).await {
                    Ok(Some(snapshot)) => Some(snapshot),
                    Ok(None) => continue,
                    Err(error) => {
                        tracing::warn!(
                            ?error,
                            session_id = %session_id,
                            "could not load member for archive; leaving live state intact"
                        );
                        return None;
                    }
                };
            }
            let Some(mut snapshot) = snapshot else {
                continue;
            };
            // A snapshot read back from the store carries revision 0 — the
            // store does not persist a revision, because the number is only
            // meaningful within one process run. A member with no live core has
            // no counter to draw from, so its revision comes from the store's
            // record of what it has already accepted, plus one. Writing the
            // stored value itself would be refused as stale, and writing a
            // fixed 1 would be refused for any member the store has written
            // more than once.
            if snapshot.revision.get() == 0 {
                snapshot.revision = self.next_storable_revision(session_id).await;
            }
            snapshot.metadata.session_state = SessionState::Archived;
            snapshots.push(snapshot);
        }
        (!snapshots.is_empty()).then_some(snapshots)
    }

    /// Returns a revision the store will accept for a session with no live core.
    ///
    /// Falls back to 1 — above the zero an unwritten session holds, and the
    /// same floor a freshly created session's first capture clears — when the
    /// store cannot report its own record.
    async fn next_storable_revision(
        &self,
        session_id: &SessionId,
    ) -> jinn_session_state::SessionRevision {
        let floor = match self
            .services
            .session_store
            .last_accepted_revision(session_id)
            .await
        {
            Ok(floor) => floor,
            Err(error) => {
                tracing::warn!(
                    ?error,
                    session_id = %session_id,
                    "could not read the store's last accepted revision; \
                     archiving with a minimal revision"
                );
                jinn_session_state::SessionRevision::new(0)
            }
        };
        jinn_session_state::SessionRevision::new(floor.get() + 1)
    }

    /// Captures immutable tree statistics before dropping the live session.
    fn snapshot_before_removal(&self, session_id: &SessionId) {
        // Read the working intervals BEFORE the session is dropped, from the
        // work-time cell. The frozen node carries them so archiving this
        // member does not change the tree's working time.
        let working = self.working_intervals(session_id);
        let frozen = self
            .state
            .read()
            .session
            .get(session_id)
            .map(|session| snapshot_frozen_node(session, working));
        if let Some(frozen) = frozen {
            self.state.with_session(|view| {
                view.session.insert_frozen_node(frozen);
            });
        }
    }

    /// Removes a session, creates a seeded replacement, and returns the
    /// removed session's persisted parent plus any replacement MCP notice.
    fn remove_and_replace(
        &self,
        session_id: &SessionId,
    ) -> (
        Option<SessionId>,
        Option<jinn_mcp_msg::McpEnablementChanged>,
    ) {
        let (fresh_session, enablement) = {
            let app_state = self.services.app_state_storage.read();
            let mut profile = SessionProfile::from_model_selection(
                app_state.last_model.clone().unwrap_or_default(),
            );
            profile.reasoning_effort = app_state.reasoning_effort;
            let seed = SessionSeed::from_config(&self.services.config);
            profile.tool_filter.clone_from(&seed.tool_filter);
            profile.skill_filter.clone_from(&seed.skill_filter);

            let mut fresh = ChatSessionState::new_with_profile(profile);
            fresh.set_enabled_mcp_servers(seed.enabled_mcp.clone());
            let enablement =
                seed.has_auto_enabled_mcp()
                    .then(|| jinn_mcp_msg::McpEnablementChanged {
                        session_id: fresh.session_id().clone(),
                        enabled: seed.enabled_mcp,
                    });
            (fresh, enablement)
        };

        let removed_parent = self
            .state
            .read()
            .session
            .get(session_id)
            .and_then(|session| session.parent_session().clone());
        self.state.with_session(|view| {
            view.session
                .map()
                .remove_and_replace(session_id, fresh_session);
        });
        (removed_parent, enablement)
    }
}
