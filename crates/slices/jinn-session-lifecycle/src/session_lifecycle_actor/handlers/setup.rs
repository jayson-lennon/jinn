//! Setup lifecycle command handling.

use jinn_chat_input_msg::ChatEntrySubmitted;
use jinn_kernel::common::actor_deps::BusPublish;
use jinn_session_history_msg::PushChatEntry;
use jinn_session_lifecycle_msg::builtin::BuiltinId;
use jinn_session_lifecycle_msg::{FinishSessionSetup, RunSessionSetup, SessionSetupCompleted};

use crate::command_runner::{LifecycleCommandError, spawn_setup_command};
use crate::session_lifecycle_actor::SessionLifecycleActor;

use super::{no_output_info, setup_complete_msg, strip_ansi};

impl SessionLifecycleActor {
    pub(in crate::session_lifecycle_actor) async fn handle_run_session_setup(
        &mut self,
        payload: &RunSessionSetup,
    ) {
        match &payload.lifecycle_command {
            Some(jinn_preferences_config::schemas::LifecycleCommand::Builtin(id)) => {
                self.run_builtin_setup(&payload.session_id, id, &payload.args)
                    .await;
            }
            Some(jinn_preferences_config::schemas::LifecycleCommand::Shell(_)) | None => {
                self.spawn_shell_setup(payload).await;
            }
        }
    }

    pub(in crate::session_lifecycle_actor) async fn handle_finish_session_setup(
        &mut self,
        payload: &FinishSessionSetup,
    ) {
        self.lifecycle_child = None;

        match (&payload.cwd, &payload.error) {
            (Some(cwd), None) => {
                let home = self.services.paths.home_dir().to_path_buf();
                {
                    self.state.with_session(|view| {
                        if let Some(session) = view.session.map().get_mut(&payload.session_id) {
                            session.set_cwd(cwd.clone());
                            session.set_home(home.clone());
                            session.advance_lifecycle_after_setup();
                        }
                    });
                }

                self.publish(PushChatEntry {
                    session_id: payload.session_id.clone(),
                    entry: setup_complete_msg(cwd),
                    pin: None,
                })
                .await;
                self.publish(SessionSetupCompleted {
                    session_id: payload.session_id.clone(),
                    cwd: cwd.clone(),
                    error: None,
                })
                .await;
            }
            (_, Some(error)) => {
                let existing_cwd = self.existing_cwd(&payload.session_id);
                self.publish(PushChatEntry {
                    session_id: payload.session_id.clone(),
                    entry: jinn_core_types::ChatEntry::error(error),
                    pin: None,
                })
                .await;
                self.publish(SessionSetupCompleted {
                    session_id: payload.session_id.clone(),
                    cwd: existing_cwd,
                    error: Some(error.clone()),
                })
                .await;
            }
            (None, None) => {
                let existing_cwd = {
                    self.state.with_session(|view| {
                        let map = view.session.map();
                        if let Some(session) = map.get_mut(&payload.session_id) {
                            session.advance_lifecycle_after_setup();
                        }
                        map.get(&payload.session_id).map_or_else(
                            || map.default_cwd().clone(),
                            |session| session.cwd().to_path_buf(),
                        )
                    })
                };
                self.publish(PushChatEntry {
                    session_id: payload.session_id.clone(),
                    entry: no_output_info(&existing_cwd),
                    pin: None,
                })
                .await;
                self.publish(SessionSetupCompleted {
                    session_id: payload.session_id.clone(),
                    cwd: existing_cwd,
                    error: None,
                })
                .await;
            }
        }
    }

    async fn spawn_shell_setup(&mut self, payload: &RunSessionSetup) {
        let cwd = self.existing_cwd(&payload.session_id);
        let (cancel_handle, handle) = match spawn_setup_command(&payload.command, &self.shell, &cwd)
        {
            Ok(pair) => pair,
            Err(error) => {
                let error = format!("Failed to start setup command: {error}");
                self.publish(PushChatEntry {
                    session_id: payload.session_id.clone(),
                    entry: jinn_core_types::ChatEntry::error(&error),
                    pin: None,
                })
                .await;
                self.publish(SessionSetupCompleted {
                    session_id: payload.session_id.clone(),
                    cwd: self.existing_cwd(&payload.session_id),
                    error: Some(error),
                })
                .await;
                return;
            }
        };

        let session_id = payload.session_id.clone();
        let bus = self.bus().clone();
        tokio::spawn(async move {
            let result = handle.await;
            let (cwd, error) = match result {
                Ok(Ok(cwd)) => (cwd, None),
                Ok(Err(report)) => (None, Some(format_command_report(&report))),
                Err(_) => (None, Some("Setup command was cancelled".to_owned())),
            };
            bus.publish(FinishSessionSetup {
                session_id,
                cwd,
                error,
            })
            .await;
        });

        self.lifecycle_child = Some(cancel_handle);
    }

    async fn run_builtin_setup(
        &self,
        session_id: &jinn_core_types::SessionId,
        id: &BuiltinId,
        args: &[String],
    ) {
        let Some(handler) = self.builtin_registry.get(id) else {
            let error = format!("unknown builtin lifecycle: {id}");
            tracing::error!(%id, "builtin handler not found in registry");
            self.publish(PushChatEntry {
                session_id: session_id.clone(),
                entry: jinn_core_types::ChatEntry::error(&error),
                pin: None,
            })
            .await;
            self.publish(SessionSetupCompleted {
                session_id: session_id.clone(),
                cwd: self.existing_cwd(session_id),
                error: Some(error),
            })
            .await;
            return;
        };

        match handler.setup(session_id, args) {
            Ok(cwd) => {
                let home = self.services.paths.home_dir().to_path_buf();
                self.state.with_session(|view| {
                    if let Some(session) = view.session.map().get_mut(session_id) {
                        session.set_cwd(cwd.clone());
                        session.set_home(home.clone());
                        session.advance_lifecycle_after_setup();
                    }
                });
                self.publish(PushChatEntry {
                    session_id: session_id.clone(),
                    entry: setup_complete_msg(&cwd),
                    pin: None,
                })
                .await;
                self.publish(SessionSetupCompleted {
                    session_id: session_id.clone(),
                    cwd,
                    error: None,
                })
                .await;
            }
            Err(report) => {
                let error = format!("builtin setup failed: {report:#?}");
                self.publish(PushChatEntry {
                    session_id: session_id.clone(),
                    entry: jinn_core_types::ChatEntry::error(&error),
                    pin: None,
                })
                .await;
                self.publish(SessionSetupCompleted {
                    session_id: session_id.clone(),
                    cwd: self.existing_cwd(session_id),
                    error: Some(error),
                })
                .await;
            }
        }
    }

    pub(super) async fn push_and_save(
        &self,
        session_id: &jinn_core_types::SessionId,
        entry: jinn_core_types::ChatEntry,
    ) {
        self.state.with_session(|view| {
            view.session
                .map()
                .get_or_create(session_id)
                .push_entry(entry.clone());
        });
        self.publish(ChatEntrySubmitted {
            session_id: session_id.clone(),
            entry,
        })
        .await;
        self.publish(jinn_session_store_msg::PersistSession {
            session_id: session_id.clone(),
        })
        .await;
    }

    pub(super) fn existing_cwd(
        &self,
        session_id: &jinn_core_types::SessionId,
    ) -> std::path::PathBuf {
        let state = self.state.read();
        state.session.get(session_id).map_or_else(
            || state.session.default_cwd().clone(),
            |session| session.cwd().to_path_buf(),
        )
    }
}

fn format_command_report(report: &error_stack::Report<LifecycleCommandError>) -> String {
    format_known_lifecycle_error(report)
}

pub(super) fn format_known_lifecycle_error(
    report: &error_stack::Report<LifecycleCommandError>,
) -> String {
    if let Some(error) = report.downcast_ref::<LifecycleCommandError>() {
        match error {
            LifecycleCommandError::CommandFailed {
                exit_code,
                stdout,
                stderr,
            } => {
                let mut parts = vec![format!("Command failed (exit code: {exit_code:?})")];
                if !stdout.is_empty() {
                    parts.push(format!("stdout:\n{}", strip_ansi(stdout)));
                }
                if !stderr.is_empty() {
                    parts.push(format!("stderr:\n{}", strip_ansi(stderr)));
                }
                parts.join("\n\n")
            }
            LifecycleCommandError::InvalidPath { path } => {
                format!(
                    "Path does not exist or cannot be resolved: {}",
                    path.display()
                )
            }
            LifecycleCommandError::NotADirectory { path } => {
                format!("Path is not a directory: {}", path.display())
            }
            LifecycleCommandError::ExecutionFailed => "Failed to execute command".to_owned(),
        }
    } else {
        strip_ansi(&format!("{report:#?}"))
    }
}
