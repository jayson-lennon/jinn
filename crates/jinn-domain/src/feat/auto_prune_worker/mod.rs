//! Auto-prune workers — background history pruning strategies.
//!
//! Each worker implements [`HistoryWorker`] and inspects conversation history
//! to identify stale or redundant entries that can be excluded from LLM context.
//!
//! # Adding a new auto-prune strategy
//!
//! 1. Create a new `foo.rs` file in this module.
//! 2. Implement [`HistoryWorker`] for your strategy struct.
//! 3. Spawn a [`HistoryWorkerActor`] with your worker in `actor_wiring.rs`.
//!
//! [`HistoryWorker`]: crate::feat::history_worker::worker_trait::HistoryWorker
//! [`HistoryWorkerActor`]: crate::feat::history_worker::actor::HistoryWorkerActor

pub mod anchor_shield;
pub mod anchored_assistant;
pub mod broken_edit;
pub mod consecutive_reads;
pub mod double_edit;
pub mod edit_read;
pub mod entry_token_cache;
pub(crate) mod min_age;
pub mod read_edit;
pub mod regex;
pub mod todo_prune;
pub mod tool_age_window;
pub mod trivial_assistant;
pub(crate) use min_age::is_within_min_age;

pub use anchor_shield::AnchorShieldAutoPruneWorker;
pub use anchored_assistant::AnchoredAssistantAutoPruneWorker;
pub use broken_edit::BrokenEditAutoPruneWorker;
pub use consecutive_reads::ConsecutiveReadsAutoPruneWorker;
pub use double_edit::DoubleEditAutoPruneWorker;
pub use edit_read::EditReadAutoPruneWorker;
pub use entry_token_cache::HistoryWorkerChatEntryTokenCache;
pub use read_edit::ReadEditAutoPruneWorker;
pub use regex::RegexAutoPruneWorker;
pub use todo_prune::TodoAutoPruneWorker;
pub use tool_age_window::ToolAgeWindowAutoPruneWorker;
pub use trivial_assistant::TrivialAssistantAutoPruneWorker;

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, reason = "test code")]
    use tempfile::TempDir;

    use crate::common::app_info::PREFS_FILE_NAME;
    use jinn_preferences_config::load_preferences_from;
    use jinn_preferences_config::schemas::auto_prune::{
        AnchorShieldConfig, AnchoredAssistantAutoPruneConfig, BrokenEditAutoPruneConfig,
        ConsecutiveReadsAutoPruneConfig, DoubleEditAutoPruneConfig, EditReadAutoPruneConfig,
        ReadEditAutoPruneConfig, RegexAutoPruneConfig, TodoAutoPruneConfig,
        ToolAgeWindowAutoPruneConfig, TrivialAssistantAutoPruneConfig,
    };
    use jinn_preferences_config::user_preferences::AutoPruneConfig;

    #[rstest::rstest]
    fn default_auto_prune_config_has_defaults_for_every_field() {
        // Given the default auto_prune config.
        let config = AutoPruneConfig::default();

        // Then every field is populated (no drift holes if a new worker is
        // added without a Default entry).
        let serialized = serde_json::to_value(&config).expect("serialize");
        let serde_json::Value::Object(fields) = serialized else {
            panic!("AutoPruneConfig must serialize to an object");
        };
        assert_eq!(fields.len(), 12);
        // And every section equals its own Default impl.
        assert_eq!(config.edit_read, EditReadAutoPruneConfig::default());
        assert_eq!(config.read_edit, ReadEditAutoPruneConfig::default());
        assert_eq!(config.regex, RegexAutoPruneConfig::default());
        assert_eq!(config.broken_edit, BrokenEditAutoPruneConfig::default());
        assert_eq!(config.todo, TodoAutoPruneConfig::default());
        assert_eq!(config.double_edit, DoubleEditAutoPruneConfig::default());
        assert_eq!(
            config.consecutive_reads,
            ConsecutiveReadsAutoPruneConfig::default()
        );
        assert_eq!(
            config.tool_age_window,
            ToolAgeWindowAutoPruneConfig::default()
        );
        assert_eq!(
            config.trivial_assistant,
            TrivialAssistantAutoPruneConfig::default()
        );
        assert_eq!(
            config.anchored_assistant,
            AnchoredAssistantAutoPruneConfig::default()
        );
        assert_eq!(config.anchor_shield, AnchorShieldConfig::default());
        // And the threshold is a nonzero token count.
        assert!(config.accumulation_threshold_tokens > 0);
    }

    #[rstest::rstest]
    fn load_parses_auto_prune_read_edit_config() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(&path, "[auto_prune.read_edit]\nenabled = false\n").expect("write");

        let prefs = load_preferences_from(&path).expect("load");
        assert!(!prefs.auto_prune.read_edit.enabled);
    }

    #[rstest::rstest]
    fn load_parses_auto_prune_consecutive_reads_config() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(
            &path,
            "[auto_prune.consecutive_reads]\nenabled = false\nkeep_last = 5\n",
        )
        .expect("write");

        let prefs = load_preferences_from(&path).expect("load");
        assert!(!prefs.auto_prune.consecutive_reads.enabled);
        assert_eq!(prefs.auto_prune.consecutive_reads.keep_last, 5);
    }

    #[rstest::rstest]
    fn load_parses_auto_prune_tool_age_window_config() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(
            &path,
            "[auto_prune.tool_age_window]\nenabled = false\nmin_age = 50\n",
        )
        .expect("write");

        let prefs = load_preferences_from(&path).expect("load");
        assert!(!prefs.auto_prune.tool_age_window.enabled);
        assert_eq!(prefs.auto_prune.tool_age_window.min_age, 50);
    }

    #[rstest::rstest]
    fn load_parses_auto_prune_read_edit_min_age() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(&path, "[auto_prune.read_edit]\nmin_age = 25\n").expect("write");

        let prefs = load_preferences_from(&path).expect("load");
        assert_eq!(prefs.auto_prune.read_edit.min_age, 25);
    }

    #[rstest::rstest]
    fn load_parses_auto_prune_double_edit_min_age() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(&path, "[auto_prune.double_edit]\nmin_age = 15\n").expect("write");

        let prefs = load_preferences_from(&path).expect("load");
        assert_eq!(prefs.auto_prune.double_edit.min_age, 15);
    }

    #[rstest::rstest]
    fn config_without_min_age_uses_section_defaults() {
        // Given a TOML file with auto_prune sections that omit `min_age`.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(
            &path,
            "[auto_prune.read_edit]\nenabled = true\n\n[auto_prune.double_edit]\nenabled = true\n\n[auto_prune.tool_age_window]\nenabled = true\n",
        )
        .expect("write");

        // When loading.
        let prefs = load_preferences_from(&path).expect("load");

        // Then the omitted fields fall back to each section's Default impl.
        assert_eq!(
            prefs.auto_prune.read_edit.min_age,
            ReadEditAutoPruneConfig::default().min_age
        );
        assert_eq!(
            prefs.auto_prune.double_edit.min_age,
            DoubleEditAutoPruneConfig::default().min_age
        );
        assert_eq!(
            prefs.auto_prune.tool_age_window.min_age,
            ToolAgeWindowAutoPruneConfig::default().min_age
        );
    }

    #[rstest::rstest]
    fn load_without_auto_prune_section_uses_defaults() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(&path, "last_model = 'ollama/llama3'").expect("write");

        let prefs = load_preferences_from(&path).expect("load");
        assert_eq!(prefs.auto_prune, AutoPruneConfig::default());
    }

    #[rstest::rstest]
    fn load_with_anchored_assistant_radius_uses_defaults_for_anchor_shield() {
        // Given a TOML file with only anchored_assistant radius (no anchor_shield section).
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(
            &path,
            "[auto_prune.anchored_assistant]\nenabled = true\nradius = 42\nmin_age = 5\n\n[auto_prune.anchor_shield]\nenabled = true\nradius = 20\n",
        )
        .expect("write");

        // When loading.
        let prefs = load_preferences_from(&path).expect("load");

        // Then anchored_assistant still reads its own radius.
        assert!(prefs.auto_prune.anchored_assistant.enabled);
        assert_eq!(prefs.auto_prune.anchored_assistant.radius, 42);
        assert_eq!(prefs.auto_prune.anchored_assistant.min_age, 5);

        // And anchor_shield uses its own config.
        assert!(prefs.auto_prune.anchor_shield.enabled);
        assert_eq!(prefs.auto_prune.anchor_shield.radius, 20);
    }

    #[rstest::rstest]
    fn load_parses_auto_prune_todo_config() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(&path, "[auto_prune.todo]\nenabled = false\n").expect("write");

        let prefs = load_preferences_from(&path).expect("load");
        assert!(!prefs.auto_prune.todo.enabled);
        // edit_read should still have defaults
        assert!(prefs.auto_prune.edit_read.enabled);
        // read_edit should still have defaults
        assert!(prefs.auto_prune.read_edit.enabled);
    }

    #[rstest::rstest]
    fn absent_min_age_deserializes_to_default_impl_value() {
        // Given TOML sections that omit `min_age`.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(
            &path,
            "[auto_prune.anchored_assistant]\nenabled = true\n\n[auto_prune.consecutive_reads]\nenabled = true\n",
        )
        .expect("write");

        // When loading.
        let prefs = load_preferences_from(&path).expect("load");

        // Then the omitted min_age matches each Default impl (serde default
        // functions and Default impls stay in lockstep; no value pinned).
        assert_eq!(
            prefs.auto_prune.anchored_assistant.min_age,
            AnchoredAssistantAutoPruneConfig::default().min_age
        );
        assert_eq!(
            prefs.auto_prune.consecutive_reads.min_age,
            ConsecutiveReadsAutoPruneConfig::default().min_age
        );
    }

    #[rstest::rstest]
    fn toml_roundtrip_with_renamed_fields() {
        // Given a TOML that uses the legacy aliases (`max_age_entries` and
        // `min_tail_entries`) instead of the new `min_age` field name.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(
            &path,
            "[auto_prune.trivial_assistant]\nenabled = true\nmax_age_entries = 100\nmax_tokens = 80\n\n[auto_prune.broken_edit]\nenabled = true\nmin_tail_entries = 10\n",
        )
        .expect("write");

        // When loading.
        let prefs = load_preferences_from(&path).expect("load");

        // Then the legacy keys are accepted via serde alias and populate
        // the new `min_age` field.
        assert_eq!(prefs.auto_prune.trivial_assistant.min_age, 100);
        assert_eq!(prefs.auto_prune.broken_edit.min_age, 10);
    }

    #[rstest::rstest]
    fn load_accumulation_threshold_from_toml() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(
            &path,
            "[auto_prune]\naccumulation_threshold_tokens = 5000\n",
        )
        .expect("write");

        let prefs = load_preferences_from(&path).expect("load");
        assert_eq!(prefs.auto_prune.accumulation_threshold_tokens, 5000);
    }

    #[rstest::rstest]
    fn accumulation_threshold_defaults_when_section_absent() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        // A sub-table exists but the top-level scalar is omitted.
        std::fs::write(&path, "[auto_prune.read_edit]\nenabled = true\n").expect("write");

        let prefs = load_preferences_from(&path).expect("load");
        assert_eq!(
            prefs.auto_prune.accumulation_threshold_tokens,
            AutoPruneConfig::default().accumulation_threshold_tokens
        );
    }
}
