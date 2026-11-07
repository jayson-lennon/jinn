//! The stream-rule matcher actor — keeps the installed matcher current.
//!
//! Activation compiles the configured rules and installs the matcher once.
//! This actor owns the same install after that: a [`StreamRulesReloaded`]
//! command (published by a configuration writer, today the stream-rule
//! picker's confirm) re-runs the identical read + compile + install path, so
//! the matcher a fresh stream resolves is built from what the configuration
//! now holds — no restart.
//!
//! The actor is the only mid-session writer of the matcher cell, which keeps
//! the single-writer story intact: activation writes it before any stream
//! runs, and this actor writes it after every confirmed configuration change.

use jinn_boot_msg::JinnTomlReloaded;
use jinn_kernel::Services;
use jinn_kernel::common::actor_deps::BusPublish as _;
use jinn_kernel::common::state::State;
use jinn_session_history_msg::PushChatEntry;
use jinn_slices::StreamRules;
use jinn_slices::cell::TypedCell;
use jinn_stream_rules_msg::StreamRulesReloaded;
use marked_path::{Absolute, MarkedPathBuf};
use trouper::actor::ActorPath;
use trouper::actor::{MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

/// The matcher actor's static trouper path.
pub const STREAM_RULE_MATCHER_PATH: &str = "stream-rule-matcher";

/// The stream-rule matcher actor.
///
/// Rebuilds the compiled set from the live configuration layer and
/// reinstalls it into the matcher cell on every reload event — the
/// picker's confirm (`StreamRulesReloaded`) and the config watcher's
/// file triggers (`StreamRulesReloaded` from `stream_rules.toml`,
/// `JinnTomlReloaded` from rules declared in `jinn.toml` itself).
pub struct StreamRuleMatcherActor {
    cell: TypedCell<StreamRules>,
    config: jinn_config::ConfigLayer,
    preferences_path: MarkedPathBuf<Absolute>,
    /// Publishes the refusal chat entry when a refresh is refused.
    bus: jinn_slices::bus::BusService,
    /// Names the refusal's recipient: the active session at refusal time.
    state: State,
}

impl ServiceActor for StreamRuleMatcherActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "trait contract: init is never called (spawn uses init_with)"
    )]
    async fn init(_args: &trouper::json::Json) -> Result<Self, error_stack::Report<RegistryError>> {
        // Never called: the spawn helper injects the cell and the
        // configuration via `init_with`.
        Err(
            error_stack::IntoReport::into_report(RegistryError::InvalidSpec)
                .attach("StreamRuleMatcherActor is spawned via init_with"),
        )
    }
}

impl StreamRuleMatcherActor {
    /// Spawns the actor at its static path.
    ///
    /// The cell, the configuration layer, and the preferences path are the
    /// same three values activation installed the first matcher from — the
    /// refresh is that path re-run, nothing more.
    pub fn spawn(
        system: &ActorSystem,
        cell: TypedCell<StreamRules>,
        config: jinn_config::ConfigLayer,
        preferences_path: MarkedPathBuf<Absolute>,
        services: &Services,
        state: State,
    ) -> ActorPath {
        let bus = services.bus.clone();
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(ActorPath::new(STREAM_RULE_MATCHER_PATH))
            .init_with(move || {
                Box::pin(async move {
                    Ok(Self {
                        cell,
                        config,
                        preferences_path,
                        bus,
                        state,
                    })
                })
            })
            .handles::<StreamRulesReloaded>()
            .handles::<JinnTomlReloaded>()
            .emits::<PushChatEntry>()
            .start()
    }

    /// Handles a [`StreamRulesReloaded`] by rebuilding the matcher
    /// from the current configuration and reinstalling it.
    ///
    /// The read is the same point-of-use read activation made, so the
    /// rebuilt set observes every configuration write that landed before
    /// the command was published — including the picker's own, which is
    /// why the confirm publishes only after its writes succeed.
    ///
    /// A rebuild whose `project` globs cannot all anchor or compile is
    /// **refused**: the error is logged, the previously installed matcher
    /// stays live, and the refusal reaches the chat as a transient entry
    /// (the config-watch convention — a silently ignored edit reads like
    /// a bug from the user's chair). A refresh must never leave the cell
    /// empty — that would disarm every rule mid-session for one bad glob
    /// — and the bad file will stop the next start anyway.
    pub async fn handle_refresh(&self) {
        let rules = crate::rules::read_rules(&self.config, &self.preferences_path);
        if let Err(report) = crate::install_compiled_matcher(&self.cell, &rules) {
            tracing::error!(
                error = ?report,
                "stream-rule matcher refresh refused; the previously installed matcher \
                     stays live. Fix the project glob in the rules file; it will fail at the \
                     next start"
            );
            let session_id = self.state.read().session.active_session_id().clone();
            let entry = jinn_core_types::ChatEntry::transient(format!(
                "**Stream-rules refresh refused**\n\n{report:?}\n\nThe previously \
                 installed matcher stays live."
            ));
            let publish = self.publish(PushChatEntry::unpinned(session_id, entry));
            publish.await;
        }
    }
}

impl jinn_kernel::common::actor_deps::BusPublish for StreamRuleMatcherActor {
    fn bus(&self) -> &jinn_slices::bus::BusService {
        &self.bus
    }
}

impl MsgHandler<StreamRulesReloaded> for StreamRuleMatcherActor {
    async fn handle(&mut self, _msg: &StreamRulesReloaded, _ctx: &mut MsgCtx<'_>) {
        self.handle_refresh().await;
    }
}

// Rules may also live in jinn.toml itself (`[[stream_rules.entry]]` is a
// registered section of that document), so a jinn.toml reload is a
// trigger here too — the same rebuild, from the same layer.
impl MsgHandler<JinnTomlReloaded> for StreamRuleMatcherActor {
    async fn handle(&mut self, _msg: &JinnTomlReloaded, _ctx: &mut MsgCtx<'_>) {
        self.handle_refresh().await;
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        reason = "test module, panics are acceptable"
    )]
    use super::*;
    use jinn_app_state::AppState;
    use jinn_kernel::common::state::State;
    use jinn_preferences_config::StreamRulesStorage;
    use jinn_slices::StreamContext;

    /// A config layer holding one rule named `name`.
    fn layer_with_rule(name: &str) -> jinn_config::ConfigLayer {
        jinn_config::testutil::config_layer(&format!(
            r#"
            [[stream_rules.entry]]
            name = "{name}"
            conditions = ['REBUILD-MARKER-{name}']
            scopes = ['text']
            body = 'Finish it.'
            "#
        ))
    }

    /// A temp preferences path with no sibling rules file. Returns the
    /// owned dir so the caller keeps it alive as long as the path is in use.
    fn temp_prefs() -> (
        jinn_testutil::JinnTempDir,
        marked_path::MarkedPathBuf<marked_path::Absolute>,
    ) {
        let dir = jinn_testutil::tempdir();
        let path = dir
            .path
            .join("jinn.toml")
            .expect("temp root joins a bare name");
        (dir, path)
    }

    /// An actor over a fresh matcher cell.
    fn actor() -> (
        StreamRuleMatcherActor,
        TypedCell<StreamRules>,
        jinn_testutil::JinnTempDir,
    ) {
        let slices = jinn_slices::Slices::new();
        let cell = slices
            .register(jinn_slices::stream_rules_slot(), StreamRules::empty())
            .expect("a fresh registry never has the stream-rules slot taken");
        let (prefs_dir, prefs) = temp_prefs();
        let (bus, _audit) = jinn_slices::bus::BusService::new_recording();
        let actor = StreamRuleMatcherActor {
            cell: cell.clone(),
            config: layer_with_rule("rebuildable"),
            preferences_path: prefs,
            bus,
            state: State::new(AppState::default()),
        };
        (actor, cell, prefs_dir)
    }

    /// Whether a prose delta naming the rule trips the installed matcher,
    /// probed the way production probes it: the delta runs through `check`,
    /// and the stream's end runs `finalize` — the pair a real response always
    /// performs.
    ///
    /// Without the finalize half, a rule matching the marker's tail would read
    /// as silent here while firing in production at stream completion.
    fn fires(cell: &TypedCell<StreamRules>, marker: &str) -> Option<String> {
        let set = cell.read().installed()?;
        let mut session = set.new_session(&jinn_core_types::SessionId::new());
        session
            .check(marker, StreamContext::text())
            .or_else(|| session.finalize())
            .map(|hit| hit.name.clone())
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_refresh_installs_the_configured_rules() {
        // Given an actor whose cell holds no matcher.
        let (actor, cell, _prefs_dir) = actor();
        assert!(cell.read().installed().is_none());

        // When a reload event is handled.
        actor.handle_refresh().await;

        // Then the configured rule is live in the cell.
        assert_eq!(
            fires(&cell, "REBUILD-MARKER-rebuildable"),
            Some("rebuildable".to_owned())
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_disabled_rule_is_gone_after_a_refresh() {
        // Given a cell whose matcher was refreshed with the rule armed.
        let (actor, cell, _prefs_dir) = actor();
        actor.handle_refresh().await;
        assert!(fires(&cell, "REBUILD-MARKER-rebuildable").is_some());

        // When the rule is disabled underneath the actor and a refresh
        // runs — the picker-confirm shape.
        actor
            .config
            .set_stream_rule_disabled(&actor.preferences_path, "rebuildable", true)
            .expect("the write lands");
        actor.handle_refresh().await;

        // Then the rule no longer fires: the rebuilt set left it out.
        assert_eq!(fires(&cell, "REBUILD-MARKER-rebuildable"), None);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_re_enabled_rule_is_back_after_a_refresh() {
        // Given a cell whose matcher was refreshed with the rule disabled.
        let (actor, cell, _prefs_dir) = actor();
        actor
            .config
            .set_stream_rule_disabled(&actor.preferences_path, "rebuildable", true)
            .expect("the write lands");
        actor.handle_refresh().await;
        assert!(fires(&cell, "REBUILD-MARKER-rebuildable").is_none());

        // When the rule is re-armed and a refresh runs.
        actor
            .config
            .set_stream_rule_disabled(&actor.preferences_path, "rebuildable", false)
            .expect("the write lands");
        actor.handle_refresh().await;

        // Then the rule fires again — a refresh re-arms as well as disarms.
        assert_eq!(
            fires(&cell, "REBUILD-MARKER-rebuildable"),
            Some("rebuildable".to_owned())
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_failing_refresh_keeps_the_previously_installed_matcher() {
        // Given a cell whose matcher holds a working set.
        let (actor, cell, _prefs_dir) = actor();
        actor.handle_refresh().await;
        assert_eq!(
            fires(&cell, "REBUILD-MARKER-rebuildable"),
            Some("rebuildable".to_owned())
        );

        // When the configuration gains a rule whose project glob cannot
        // anchor — the shape a broken hand-edit produces — and a refresh
        // runs over it.
        let poisoned = jinn_config::testutil::config_layer(
            r#"
            [[stream_rules.entry]]
            name = "rebuildable"
            conditions = ['REBUILD-MARKER-rebuildable']
            scopes = ['text']
            body = 'Finish it.'
            project = 'broken-proj'
            "#,
        );
        let (poison_bus, _poison_audit) = jinn_slices::bus::BusService::new_recording();
        let actor = StreamRuleMatcherActor {
            cell: cell.clone(),
            config: poisoned,
            preferences_path: actor.preferences_path,
            bus: poison_bus,
            state: actor.state,
        };
        actor.handle_refresh().await;

        // Then the refresh is refused: the old set still fires, and the
        // broken config never reached the live matcher. The failure will
        // stop the next start instead.
        assert_eq!(
            fires(&cell, "REBUILD-MARKER-rebuildable"),
            Some("rebuildable".to_owned()),
            "a refused refresh must leave the previous matcher live"
        );
    }

    // The config-watch convention: a refused reload is surfaced, not
    // logged only — a silently ignored edit reads like a bug from the
    // user's chair.
    #[rstest::rstest]
    #[tokio::test]
    async fn a_refused_refresh_publishes_a_chat_entry() {
        // Given an actor whose refresh will fail (an unanchorable project
        // glob) and a recording bus.
        let (actor, cell, _prefs_dir) = actor();
        let (bus, audit) = jinn_slices::bus::BusService::new_recording();
        let poisoned = jinn_config::testutil::config_layer(
            r#"
            [[stream_rules.entry]]
            name = "rebuildable"
            conditions = ['REBUILD-MARKER-rebuildable']
            scopes = ['text']
            body = 'Finish it.'
            project = 'broken-proj'
            "#,
        );
        let refusing = StreamRuleMatcherActor {
            cell,
            config: poisoned,
            preferences_path: actor.preferences_path,
            bus,
            state: actor.state,
        };

        // When the refresh runs.
        refusing.handle_refresh().await;

        // Then a PushChatEntry rode the bus: the refusal is visible in
        // the chat, not just the log.
        let pushed = audit.of_type::<PushChatEntry>();
        assert_eq!(
            pushed.len(),
            1,
            "a refused refresh must surface a chat entry"
        );
    }
}
