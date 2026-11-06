//! The stream-rule persist actor — writes the confirmed set off the
//! key-handler thread, then asks for the matcher refresh.
//!
//! Handles [`PersistStreamRuleDisabled`], the command the picker's confirm
//! publishes. Every pending change loops through the configuration layer's
//! comment-preserving storage boundary on
//! `tokio::task::spawn_blocking` — the write stats and rewrites the user's
//! `jinn.toml`, and a slow disk must not hold the bus thread. The writes
//! run first-to-last until one fails; only when all of them landed does the
//! actor publish [`StreamRulesReloaded`], so the matcher never
//! recompiles from a half-applied set. The first failure aborts the loop,
//! pushes an `Error` entry into the chat through the shared failure
//! formatter, and publishes no refresh — the file and the live matcher
//! still agree.

use jinn_config::ConfigLayer;
use jinn_core_types::SessionId;
use jinn_preferences_config::StreamRulesStorage;
use jinn_preferences_config::StreamRulesStorageError;
use jinn_session_history_msg::PushChatEntry;
use jinn_stream_rules_msg::StreamRulesReloaded;
use trouper::actor::ActorPath;
use trouper::actor::MsgHandler;
use trouper::actor::ServiceActor;
use trouper::context::MsgCtx;
use trouper::inbox::OverloadPolicy;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

use jinn_stream_rule_picker_msg::PersistStreamRuleDisabled;
use jinn_stream_rule_picker_msg::StreamRuleDisabledChange;
use jinn_stream_rule_picker_msg::stream_rule_failure_line;
use marked_path::{Absolute, MarkedPathBuf};

/// The persist actor's static trouper path.
pub const STREAM_RULE_PERSIST_PATH: &str = "stream-rule-persist";

/// One blocking write, owned so it can cross the thread boundary.
struct PersistJob {
    preferences_path: MarkedPathBuf<Absolute>,
    changes: Vec<StreamRuleDisabledChange>,
}

/// The stream-rule persist actor.
pub struct StreamRulePersistActor {
    config: ConfigLayer,
}

impl ServiceActor for StreamRulePersistActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "trait contract: init is never called (spawn uses init_with)"
    )]
    async fn init(_args: &trouper::json::Json) -> Result<Self, error_stack::Report<RegistryError>> {
        // Never called: the spawn helper injects the deps via `init_with`.
        Err(
            error_stack::IntoReport::into_report(RegistryError::InvalidSpec)
                .attach("StreamRulePersistActor is spawned via init_with"),
        )
    }
}

impl StreamRulePersistActor {
    /// Spawns the actor at its static trouper path and returns the path.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "port convention: spawn takes owned deps and clones into init_with"
    )]
    pub fn spawn(system: &ActorSystem, config: ConfigLayer) -> ActorPath {
        let path = ActorPath::new(STREAM_RULE_PERSIST_PATH);
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(path.clone())
            .init_with({
                let config = config.clone();
                move || {
                    let config = config.clone();
                    Box::pin(async move { Ok(Self { config }) })
                }
            })
            .handles::<PersistStreamRuleDisabled>()
            // trouper enforces an emit gate: an outbound message that the
            // actor has not declared here is dead-lettered at flush and
            // never routes. The reload event must follow the writes; the
            // error entry must follow a failure.
            .emits::<StreamRulesReloaded>()
            .emits::<PushChatEntry>()
            .mailbox(16, OverloadPolicy::Block)
            .start();
        path
    }

    /// Handles a persist command end to end.
    ///
    /// `session_id` rides the command, not the state: the confirm resolved
    /// the active session at keypress time, and a report belongs in the
    /// chat the user was looking at.
    pub async fn handle_persist(&self, msg: &PersistStreamRuleDisabled, ctx: &mut MsgCtx<'_>) {
        let config = self.config.clone();
        let job = PersistJob {
            preferences_path: msg.preferences_path.clone(),
            changes: msg.changes.clone(),
        };
        let result = tokio::task::spawn_blocking(move || persist_blocking(&config, &job))
            .await
            .unwrap_or_else(|error| {
                Err(error_stack::Report::from(
                    StreamRulesStorageError::WorkerPanicked {
                        detail: error.to_string(),
                    },
                ))
            });

        match result {
            Ok(()) => ctx.publish(StreamRulesReloaded),
            Err(failure) => Self::report_error(&msg.session_id, &failure, ctx),
        }
    }

    /// Pushes an `Error` entry naming why the persist failed.
    ///
    /// A storage failure renders through the shared
    /// [`stream_rule_failure_line`] wording, matching every other surface.
    fn report_error(
        session_id: &SessionId,
        failure: &error_stack::Report<StreamRulesStorageError>,
        ctx: &mut MsgCtx<'_>,
    ) {
        let line = stream_rule_failure_line(failure.current_context());
        tracing::warn!(%line, "stream-rule persist failed");
        ctx.publish(PushChatEntry {
            session_id: session_id.clone(),
            entry: jinn_core_types::ChatEntry::error(format!("Stream-rule change failed: {line}")),
            pin: None,
        });
    }
}

/// Loops every pending change through the storage boundary.
///
/// Runs on a blocking worker: each write patches and rewrites the user's
/// `jinn.toml`. The loop stops at the first failure — partial writes stay
/// on disk, the refresh does not run, and the chat names what went wrong.
fn persist_blocking(
    config: &ConfigLayer,
    job: &PersistJob,
) -> Result<(), error_stack::Report<StreamRulesStorageError>> {
    for change in &job.changes {
        config.set_stream_rule_disabled(&job.preferences_path, &change.name, change.disabled)?;
    }
    Ok(())
}

impl MsgHandler<PersistStreamRuleDisabled> for StreamRulePersistActor {
    async fn handle(&mut self, msg: &PersistStreamRuleDisabled, ctx: &mut MsgCtx<'_>) {
        self.handle_persist(msg, ctx).await;
    }
}

#[cfg(test)]
mod test_support {
    #![allow(clippy::expect_used, reason = "test fixture, panics are acceptable")]

    /// The document the failing backends serve: one rule named `alpha`, so
    /// the loop reaches the write and fails *there* — not at the
    /// unknown-name pre-check.
    pub fn alpha_doc() -> toml_edit::DocumentMut {
        "[[stream_rules.entry]]\nname = \"alpha\"\nconditions = ['a']\nbody = \"A.\"\n"
            .parse()
            .expect("test document parses")
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
    use super::test_support::alpha_doc;
    use super::*;
    use jinn_preferences_config::StreamRulesStorageError;
    use std::sync::Arc;

    #[rstest::rstest]
    fn a_change_loop_persists_every_change_in_order() {
        // Given a layer over a temp jinn.toml defining two rules, and a
        // job toggling both — one off, one on.
        let dir = jinn_testutil::tempdir();
        let path = dir
            .path
            .join("jinn.toml")
            .expect("temp root joins a bare name");
        std::fs::write(
            &path,
            "[stream_rules]\ndisabled_rules = [\"beta\"]\n\n[[stream_rules.entry]]\nname = \"alpha\"\nconditions = ['a']\nbody = \"A.\"\n\n[[stream_rules.entry]]\nname = \"beta\"\nconditions = ['b']\nbody = \"B.\"\n",
        )
        .expect("write");
        let layer = ConfigLayer::load(Arc::new(jinn_config::FilesystemConfigStorage::new(
            path.clone(),
        )))
        .expect("layer loads");
        let job = PersistJob {
            preferences_path: path.clone(),
            changes: vec![
                StreamRuleDisabledChange {
                    name: "alpha".to_owned(),
                    disabled: true,
                },
                StreamRuleDisabledChange {
                    name: "beta".to_owned(),
                    disabled: false,
                },
            ],
        };

        // When the blocking loop runs.
        persist_blocking(&layer, &job).expect("persist succeeds");

        // Then the file holds exactly the confirmed set: alpha off, beta
        // back on — both writes landed, in one pass.
        let array = layer
            .get::<jinn_preferences_config::schemas::StreamRulesConfig>()
            .expect("section reads")
            .disabled_rules;
        assert_eq!(array, vec!["alpha".to_owned()]);
    }

    #[rstest::rstest]
    fn the_first_failing_change_stops_the_loop_with_the_shared_line() {
        // Given a job whose second change names a rule no source defines.
        let dir = jinn_testutil::tempdir();
        let path = dir
            .path
            .join("jinn.toml")
            .expect("temp root joins a bare name");
        std::fs::write(
            &path,
            "[[stream_rules.entry]]\nname = \"alpha\"\nconditions = ['a']\nbody = \"A.\"\n",
        )
        .expect("write");
        let layer = ConfigLayer::load(Arc::new(jinn_config::FilesystemConfigStorage::new(
            path.clone(),
        )))
        .expect("layer loads");
        let job = PersistJob {
            preferences_path: path,
            changes: vec![
                StreamRuleDisabledChange {
                    name: "alpha".to_owned(),
                    disabled: true,
                },
                StreamRuleDisabledChange {
                    name: "ghost".to_owned(),
                    disabled: true,
                },
            ],
        };

        // When the blocking loop runs.
        let result = persist_blocking(&layer, &job);

        // Then it fails with the shared formatter's wording, naming the
        // unknown rule — the same string the status line would have shown.
        let report = result.expect_err("the ghost rule must fail the loop");
        let line = stream_rule_failure_line(report.current_context());
        assert!(
            line.contains("`ghost`") && line.contains("reopen the picker"),
            "was: {line}"
        );
    }

    #[rstest::rstest]
    fn a_failed_config_backend_surfaces_its_detail_through_the_formatter() {
        // Given a layer whose backend fails every document write, over a
        // document that carries the rule the job names — the failure must
        // come from the write, not from an unknown name.
        struct Failing;
        impl jinn_config::ConfigDocumentStorage for Failing {
            fn name(&self) -> &'static str {
                "Failing"
            }
            fn read(
                &self,
            ) -> Result<toml_edit::DocumentMut, error_stack::Report<jinn_config::ConfigError>>
            {
                Ok(alpha_doc())
            }
            fn write(
                &self,
                _doc: &toml_edit::DocumentMut,
            ) -> Result<(), error_stack::Report<jinn_config::ConfigError>> {
                Err(error_stack::Report::new(
                    jinn_config::ConfigError::Storage {
                        detail: "disk unplugged".to_owned(),
                    },
                ))
            }
        }
        let layer = ConfigLayer::load(Arc::new(Failing)).expect("layer loads");
        let dir = jinn_testutil::tempdir();
        let job = PersistJob {
            preferences_path: dir
                .path
                .join("jinn.toml")
                .expect("temp root joins a bare name"),
            changes: vec![StreamRuleDisabledChange {
                name: "alpha".to_owned(),
                disabled: true,
            }],
        };

        // When the blocking loop runs.
        let result = persist_blocking(&layer, &job);

        // Then the failure carries the Write family through the shared
        // formatter — the boundary summarizes a failed persistence as
        // "did not persist", and that summary is what the chat entry shows.
        let report = result.expect_err("the failing backend must fail the loop");
        let line = stream_rule_failure_line(report.current_context());
        assert!(
            line.contains("could not be written") && line.contains("did not persist"),
            "was: {line}"
        );
    }

    #[rstest::rstest]
    fn a_storage_error_formats_with_the_shared_helper() {
        // Given the three storage error families.
        let errors = [
            StreamRulesStorageError::UnknownRule {
                name: "x".to_owned(),
            },
            StreamRulesStorageError::Read {
                detail: "r".to_owned(),
            },
            StreamRulesStorageError::Write {
                detail: "w".to_owned(),
            },
        ];

        // When each crosses the shared formatter.
        let lines: Vec<String> = errors.iter().map(stream_rule_failure_line).collect();

        // Then each keeps its distinguishing content — the formatter is
        // the one place the wording lives.
        assert!(lines[0].contains("`x`"));
        assert!(lines[1].contains("could not be read"));
        assert!(lines[2].contains("could not be written"));
    }
}

#[cfg(test)]
mod bus_tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::test_support::alpha_doc;
    use super::*;
    use jinn_testutil::bus_harness::TestHarness;
    use jinn_testutil::bus_harness::await_recorded;
    use std::sync::Arc;
    use std::time::Duration;

    /// A layer over a temp jinn.toml defining `alpha`, plus the path and
    /// the dir itself — kept alive by the caller, because the actor's
    /// write lands after the helper returns.
    fn layer_over_pref_dir() -> (ConfigLayer, MarkedPathBuf<Absolute>, tempfile::TempDir) {
        let dir = jinn_testutil::tempdir();
        let path = dir
            .path
            .join("jinn.toml")
            .expect("temp root joins a bare name");
        std::fs::write(
            &path,
            "[[stream_rules.entry]]\nname = \"alpha\"\nconditions = ['a']\nbody = \"A.\"\n",
        )
        .expect("write");
        let layer = ConfigLayer::load(Arc::new(jinn_config::FilesystemConfigStorage::new(
            path.clone(),
        )))
        .expect("layer loads");
        (layer, path, dir.dir)
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_published_persist_command_writes_then_refreshes_the_matcher() {
        // Given the actor spawned over a real bus, on a config defining
        // `alpha`, with recorders on both of its outbound messages.
        let (layer, prefs_path, _dir) = layer_over_pref_dir();
        let harness = TestHarness::new().await;
        let _ = StreamRulePersistActor::spawn(harness.system(), layer.clone());
        let refreshes = harness.spawn_recorder::<StreamRulesReloaded>().await;
        let pushes = harness.spawn_recorder::<PushChatEntry>().await;
        let session_id = SessionId::new();

        // When a confirm's command arrives disabling alpha.
        harness
            .publish(PersistStreamRuleDisabled {
                session_id: session_id.clone(),
                preferences_path: prefs_path.clone(),
                changes: vec![StreamRuleDisabledChange {
                    name: "alpha".to_owned(),
                    disabled: true,
                }],
            })
            .await;
        let seen = await_recorded(&refreshes, 1, Duration::from_secs(2)).await;

        // Then the refresh follows the write: exactly one was published,
        // and the file holds the new array — the matcher will recompile
        // from what the write produced.
        assert_eq!(seen.len(), 1, "expected one refresh, got {seen:?}");
        assert!(refreshes.drain().is_empty(), "no second refresh");
        let array = layer
            .get::<jinn_preferences_config::schemas::StreamRulesConfig>()
            .expect("section reads")
            .disabled_rules;
        assert_eq!(array, vec!["alpha".to_owned()]);
        // And no error entry was pushed.
        assert!(
            pushes.is_empty(),
            "unexpected chat pushes: {:?}",
            pushes.len()
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_failed_write_pushes_a_chat_error_and_no_refresh() {
        // Given the actor over a layer whose backend fails every write,
        // over a document that carries the rule the command names — the
        // failure must come from the write, not from an unknown name.
        struct Failing;
        impl jinn_config::ConfigDocumentStorage for Failing {
            fn name(&self) -> &'static str {
                "Failing"
            }
            fn read(
                &self,
            ) -> Result<toml_edit::DocumentMut, error_stack::Report<jinn_config::ConfigError>>
            {
                Ok(alpha_doc())
            }
            fn write(
                &self,
                _doc: &toml_edit::DocumentMut,
            ) -> Result<(), error_stack::Report<jinn_config::ConfigError>> {
                Err(error_stack::Report::new(
                    jinn_config::ConfigError::Storage {
                        detail: "disk unplugged".to_owned(),
                    },
                ))
            }
        }
        let layer = ConfigLayer::load(Arc::new(Failing)).expect("layer loads");
        let dir = jinn_testutil::tempdir();
        let harness = TestHarness::new().await;
        let _ = StreamRulePersistActor::spawn(harness.system(), layer);
        let refreshes = harness.spawn_recorder::<StreamRulesReloaded>().await;
        let pushes = harness.spawn_recorder::<PushChatEntry>().await;
        let session_id = SessionId::new();

        // When the confirm's command arrives.
        harness
            .publish(PersistStreamRuleDisabled {
                session_id: session_id.clone(),
                preferences_path: dir
                    .path
                    .join("jinn.toml")
                    .expect("temp root joins a bare name"),
                changes: vec![StreamRuleDisabledChange {
                    name: "alpha".to_owned(),
                    disabled: true,
                }],
            })
            .await;
        let pushed = await_recorded(&pushes, 1, Duration::from_secs(2)).await;

        // Then the chat carries one Error entry naming the failure, and
        // the matcher was never asked to refresh.
        assert_eq!(pushed.len(), 1, "expected one push, got {pushed:?}");
        assert!(refreshes.is_empty(), "a failed write must not refresh");
    }
}
