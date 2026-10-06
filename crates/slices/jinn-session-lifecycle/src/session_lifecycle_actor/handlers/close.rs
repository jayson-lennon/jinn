//! Close and teardown completion handling.

use jinn_core_types::SessionId;
use jinn_kernel::common::actor_deps::BusPublish;
use jinn_session_history_msg::PushChatEntry;
use jinn_session_lifecycle_msg::{
    FinishSessionTeardown, LifecycleScriptState, SessionTeardownFinished, TeardownFollowUp,
};
use jinn_session_store_msg::{ArchiveSession, PersistSession};

use crate::session_lifecycle_actor::SessionLifecycleActor;

use super::{teardown_running_msg, teardown_success_msg};

impl SessionLifecycleActor {
    pub(in crate::session_lifecycle_actor) async fn handle_close_session(
        &mut self,
        payload: &jinn_session_lifecycle_msg::CloseSession,
    ) {
        if !self.state.read().session.contains(&payload.session_id) {
            return;
        }
        let teardown = self.close_teardown(&payload.session_id);
        if let Some(teardown) = teardown {
            match teardown {
                jinn_preferences_config::schemas::LifecycleCommand::Shell(command) => {
                    let Some(rendered) = self.begin_teardown(&payload.session_id, &command) else {
                        return;
                    };
                    self.push_and_save(&payload.session_id, teardown_running_msg())
                        .await;
                    self.spawn_teardown(&payload.session_id, &rendered, TeardownFollowUp::Close)
                        .await;
                    return;
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
                }
            }
        }

        self.publish(ArchiveSession {
            session_id: payload.session_id.clone(),
        })
        .await;
    }

    pub(in crate::session_lifecycle_actor) async fn handle_finish_session_teardown(
        &mut self,
        payload: &FinishSessionTeardown,
    ) {
        self.lifecycle_child = None;
        let session_exists = {
            self.state
                .with_session(|view| view.session.map().contains(&payload.session_id))
        };
        if !session_exists {
            return;
        }

        if let Some(error) = &payload.error {
            self.publish(PushChatEntry {
                session_id: payload.session_id.clone(),
                entry: jinn_core_types::ChatEntry::error(format!("Teardown failed: {error}")),
                pin: None,
            })
            .await;
            self.publish(SessionTeardownFinished {
                session_id: payload.session_id.clone(),
                error: Some(error.clone()),
            })
            .await;
            return;
        }

        match payload.follow_up {
            TeardownFollowUp::None => {
                self.advance_after_teardown(&payload.session_id).await;
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
            TeardownFollowUp::Close => {
                self.advance_after_teardown(&payload.session_id).await;
                // The original inline path emitted SessionArchived and
                // SessionClosed before SessionTeardownFinished. Archive is
                // now the store actor's work, so the teardown-finished event
                // is published first to keep the observable ordering stable.
                self.publish(SessionTeardownFinished {
                    session_id: payload.session_id.clone(),
                    error: None,
                })
                .await;
                self.publish(ArchiveSession {
                    session_id: payload.session_id.clone(),
                })
                .await;
            }
            TeardownFollowUp::CloseTree => {
                self.advance_after_teardown(&payload.session_id).await;
                self.publish(SessionTeardownFinished {
                    session_id: payload.session_id.clone(),
                    error: None,
                })
                .await;
                self.publish_archive_tree(&payload.session_id).await;
            }
        }
    }

    fn close_teardown(
        &self,
        session_id: &SessionId,
    ) -> Option<jinn_preferences_config::schemas::LifecycleCommand> {
        let state = self.state.read();
        let session = state.session.get(session_id)?;
        if session.lifecycle_script_state() != LifecycleScriptState::SetupRan {
            return None;
        }
        let name = session.lifecycle_name()?;
        jinn_kernel::session_lifecycle::intent::lifecycle_teardown(&self.services.config, name)
    }

    async fn advance_after_teardown(&self, session_id: &SessionId) {
        let advanced = {
            self.state.with_session(|view| {
                let Some(session) = view.session.map().get_mut(session_id) else {
                    return false;
                };
                session.advance_lifecycle_after_teardown();
                true
            })
        };
        if advanced {
            self.publish(PersistSession {
                session_id: session_id.clone(),
            })
            .await;
        }
    }
}
