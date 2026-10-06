//! Teardown command handling and the pre-archive tree guard.

use std::collections::HashMap;

use jinn_core_types::SessionId;
use jinn_kernel::common::actor_deps::BusPublish;
use jinn_session_history_msg::PushChatEntry;
use jinn_session_lifecycle_msg::CommandTemplate;
use jinn_session_lifecycle_msg::LifecycleScriptState;
use jinn_session_lifecycle_msg::builtin::BuiltinId;
use jinn_session_lifecycle_msg::{
    FinishSessionTeardown, RunSessionTeardown, SessionTeardownFinished, TeardownFollowUp,
};
use jinn_session_msg::PhaseKind;
use jinn_session_store_msg::{ArchiveSessionTree, PersistSession};

use crate::command_runner::spawn_teardown_command;
use crate::session_lifecycle_actor::SessionLifecycleActor;

use super::setup::format_known_lifecycle_error;
use super::{teardown_running_msg, teardown_success_msg};

impl SessionLifecycleActor {
    pub(in crate::session_lifecycle_actor) async fn handle_run_session_teardown(
        &mut self,
        payload: &RunSessionTeardown,
    ) {
        let teardown = self.lifecycle_teardown(&payload.session_id);
        let Some(teardown) = teardown else {
            return;
        };

        match teardown {
            jinn_preferences_config::schemas::LifecycleCommand::Shell(command) => {
                let Some(rendered) = self.begin_teardown(&payload.session_id, &command) else {
                    return;
                };
                self.push_and_save(&payload.session_id, teardown_running_msg())
                    .await;
                self.spawn_teardown(&payload.session_id, &rendered, TeardownFollowUp::None)
                    .await;
            }
            jinn_preferences_config::schemas::LifecycleCommand::Builtin(id) => {
                if !self.run_builtin_teardown(&payload.session_id, &id).await {
                    self.publish(SessionTeardownFinished {
                        session_id: payload.session_id.clone(),
                        error: Some("teardown failed".to_owned()),
                    })
                    .await;
                    return;
                }
                self.publish(PushChatEntry {
                    session_id: payload.session_id.clone(),
                    entry: teardown_success_msg(),
                    pin: None,
                })
                .await;
                self.publish(SessionTeardownFinished {
                    session_id: payload.session_id.clone(),
                    error: None,
                })
                .await;
            }
        }
    }

    pub(in crate::session_lifecycle_actor) async fn handle_teardown_session_tree(
        &mut self,
        payload: &jinn_session_lifecycle_msg::TeardownSessionTree,
    ) {
        if self.guarded_tree_closure(&payload.root).await.is_none() {
            return;
        }
        if !self.state.read().session.contains(&payload.root) {
            return;
        }

        let teardown = self.lifecycle_teardown(&payload.root);
        let should_teardown = {
            let state = self.state.read();
            state.session.get(&payload.root).is_some_and(|session| {
                session.lifecycle_script_state() == LifecycleScriptState::SetupRan
            })
        };

        let Some(teardown) = teardown.filter(|_| should_teardown) else {
            self.publish_archive_tree(&payload.root).await;
            return;
        };

        match teardown {
            jinn_preferences_config::schemas::LifecycleCommand::Shell(command) => {
                let Some(rendered) = self.begin_teardown(&payload.root, &command) else {
                    return;
                };
                self.push_and_save(&payload.root, teardown_running_msg())
                    .await;
                self.spawn_teardown(&payload.root, &rendered, TeardownFollowUp::CloseTree)
                    .await;
            }
            jinn_preferences_config::schemas::LifecycleCommand::Builtin(id) => {
                if !self.run_builtin_teardown(&payload.root, &id).await {
                    self.publish(SessionTeardownFinished {
                        session_id: payload.root.clone(),
                        error: Some("teardown failed".to_owned()),
                    })
                    .await;
                    return;
                }
                self.publish(PushChatEntry {
                    session_id: payload.root.clone(),
                    entry: teardown_success_msg(),
                    pin: None,
                })
                .await;
                self.publish(SessionTeardownFinished {
                    session_id: payload.root.clone(),
                    error: None,
                })
                .await;
                self.publish_archive_tree(&payload.root).await;
            }
        }
    }

    pub(super) async fn spawn_teardown(
        &mut self,
        session_id: &SessionId,
        rendered: &str,
        follow_up: TeardownFollowUp,
    ) {
        let cwd = self.existing_cwd(session_id);
        let (cancel_handle, handle) = match spawn_teardown_command(rendered, &self.shell, &cwd) {
            Ok(pair) => pair,
            Err(error) => {
                self.publish(FinishSessionTeardown {
                    session_id: session_id.clone(),
                    follow_up,
                    error: Some(format!("Failed to start teardown command: {error}")),
                })
                .await;
                return;
            }
        };

        let session_id = session_id.clone();
        let bus = self.bus().clone();
        tokio::spawn(async move {
            let error = match handle.await {
                Ok(Ok(())) => None,
                Ok(Err(report)) => Some(format_known_lifecycle_error(&report)),
                Err(_) => Some("Teardown command was cancelled".to_owned()),
            };
            bus.publish(FinishSessionTeardown {
                session_id,
                follow_up,
                error,
            })
            .await;
        });
        self.lifecycle_child = Some(cancel_handle);
    }

    pub(super) fn begin_teardown(
        &mut self,
        session_id: &SessionId,
        command: &str,
    ) -> Option<String> {
        self.state.with_session(|view| {
            let session = view.session.map().get_mut(session_id)?;
            let args = session.lifecycle_args().to_vec();
            Some(if args.is_empty() {
                command.to_owned()
            } else {
                CommandTemplate::parse(command).render(&args)
            })
        })
    }

    pub(super) async fn run_builtin_teardown(
        &self,
        session_id: &SessionId,
        id: &BuiltinId,
    ) -> bool {
        let Some(handler) = self.builtin_registry.get(id) else {
            let error = format!("unknown builtin lifecycle: {id}");
            tracing::error!(%id, "builtin handler not found in registry for teardown");
            self.publish(PushChatEntry {
                session_id: session_id.clone(),
                entry: jinn_core_types::ChatEntry::error(&error),
                pin: None,
            })
            .await;
            return false;
        };

        let args = {
            let state = self.state.read();
            state
                .session
                .get(session_id)
                .map(|session| session.lifecycle_args().to_vec())
                .unwrap_or_default()
        };
        if handler.teardown(session_id, &args) {
            self.state.with_session(|view| {
                if let Some(session) = view.session.map().get_mut(session_id) {
                    session.advance_lifecycle_after_teardown();
                }
            });
            self.publish(PersistSession {
                session_id: session_id.clone(),
            })
            .await;
            true
        } else {
            let error = format!("builtin teardown failed for: {id}");
            self.publish(PushChatEntry {
                session_id: session_id.clone(),
                entry: jinn_core_types::ChatEntry::error(&error),
                pin: None,
            })
            .await;
            false
        }
    }

    pub(super) async fn publish_archive_tree(&self, root: &SessionId) {
        self.publish(ArchiveSessionTree { root: root.clone() })
            .await;
    }

    fn lifecycle_teardown(
        &self,
        session_id: &SessionId,
    ) -> Option<jinn_preferences_config::schemas::LifecycleCommand> {
        // Bound, not returned directly: the state read guard must drop
        // before the `?` unwinds, or it outlives the borrow.
        let name = self
            .state
            .read()
            .session
            .get(session_id)?
            .lifecycle_name()?
            .to_owned();
        jinn_kernel::session_lifecycle::intent::lifecycle_teardown(&self.services.config, &name)
    }

    async fn guarded_tree_closure(&self, root: &SessionId) -> Option<Vec<SessionId>> {
        let members = self.resolve_tree_closure(root).await;
        let busy = {
            let state = self.state.read();
            members.iter().any(|id| {
                state
                    .session
                    .get(id)
                    .is_some_and(|session| !matches!(session.phase(), PhaseKind::Idle))
            })
        };
        if busy {
            tracing::warn!(root = %root, "tree action aborted: a member session is busy");
            self.publish_tree_teardown_failed(&members, "a member session is busy")
                .await;
            return None;
        }
        Some(members)
    }

    /// Announces a per-member teardown failure for an aborted tree action.
    ///
    /// One event per member, so a listener tracking several in-flight sessions
    /// clears every one of them rather than only the tree root.
    async fn publish_tree_teardown_failed(&self, members: &[SessionId], error: &str) {
        for session_id in members {
            self.publish(SessionTeardownFinished {
                session_id: session_id.clone(),
                error: Some(error.to_owned()),
            })
            .await;
        }
    }

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

    fn parent_links_from_memory(&self) -> HashMap<SessionId, Option<SessionId>> {
        self.state
            .read()
            .session
            .iter()
            .map(|(id, session)| (id.clone(), session.parent_session().clone()))
            .collect()
    }
}
