//! Template validation tests for the shipped `default_jinn.toml`.
//!
//! Two properties, using [`jinn_common::template_check`] for the
//! expansion and key-path collection:
//!
//! 1. The template parses, both as shipped and with every marked example
//!    region expanded (what a user gets by uncommenting everything).
//! 2. Every key the template documents belongs to a section that is
//!    actually read — no dead keys.
//!
//! **Completeness is deliberately not asserted.** The providers template
//! can be checked against one aggregate `ProvidersConfig`, so
//! `check_template_activates_and_documents` has a schema value to
//! compare. `jinn.toml` is an umbrella document with no aggregate
//! struct — 17 independent sections, each read on its own — so there is
//! no single schema to be complete *against*. Asserting it would
//! require inventing a type the runtime deliberately does not have.
//!
//! Nor is it asserted that every section appears in the template. The
//! template ships active tables for `tools`, `chat_log`, `skills`,
//! `session_lifecycle.script`, `context_curation.*`, `provider.*`,
//! `ui.*`, `watchdog.*`, and `term`. There is deliberately no `[mcp]`
//! block and no `[discord]` block; both sections read as their defaults
//! when absent, and a stock install looks exactly like that. Adding
//! either block to the template is a product decision, not something
//! this test should force.

#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "test code asserts with expect/panic for clear failure messages"
)]

use jinn_common::template_check::collect_toml_key_paths;

use crate::config_template::DEFAULT_CONFIG;
use crate::registration::register_all_sections;
use crate::schemas::{DiscordConfig, McpServersConfig, StreamRuleConfig};

/// The template with every `(uncomment below to activate)` region expanded.
fn expanded_template() -> String {
    jinn_common::template_check::expand_marked_examples(DEFAULT_CONFIG)
}

#[rstest::rstest]
#[test]
fn jinn_template_parses_as_written() {
    // Given the shipped template exactly as jinn loads it on first run.
    // When parsing.
    let result = DEFAULT_CONFIG.parse::<toml::Value>();

    // Then it succeeds.
    assert!(
        result.is_ok(),
        "template does not parse: {:?}",
        result.err()
    );
}

#[rstest::rstest]
#[test]
fn jinn_template_with_all_examples_activated_parses() {
    // Given the template with every marked example expanded.
    let expanded = expanded_template();

    // When parsing.
    let result = expanded.parse::<toml::Value>();

    // Then it succeeds.
    assert!(
        result.is_ok(),
        "expanded template does not parse: {:?}\n---\n{expanded}",
        result.err()
    );
}

#[rstest::rstest]
#[test]
fn every_documented_section_validates_against_its_schema() {
    // Given the expanded template and the launch-time section roster.
    let expanded = expanded_template();
    let config = jinn_config::testutil::config_layer(&expanded);
    register_all_sections(&config);

    // When the layer validates the document.
    let result = config.validate();

    // Then every section the template documents is one a reader can
    // deserialize — no example ships a field that does not exist.
    assert!(
        result.is_ok(),
        "template section failed validation: {:?}",
        result.err().map(|e| e.to_string())
    );
}

#[rstest::rstest]
#[test]
fn every_documented_top_level_table_belongs_to_a_registered_section() {
    // Given the expanded template and the top-level tables it activates.
    let expanded = expanded_template();
    let doc = expanded.parse::<toml::Value>().expect("expanded parses");
    let roots: std::collections::BTreeSet<String> = collect_toml_key_paths(&doc)
        .iter()
        .filter_map(|path| path.first().cloned())
        .collect();

    // And the launch-time roster, registered against the same document.
    let config = jinn_config::testutil::config_layer(&expanded);
    register_all_sections(&config);

    // When each root is tested against a section some reader owns. The
    // `ConfigList` sections are included even though the launch roster
    // cannot register them: they are read on demand, and a documented
    // table still needs an owner.
    let is_owned = |root: &str| {
        OWNED_KEYS
            .iter()
            .any(|key| *key == root || key.starts_with(&format!("{root}.")))
    };
    let unowned: Vec<&String> = roots
        .iter()
        .filter(|root| !is_owned(root.as_str()))
        .collect();

    // Then every top-level table the template documents is a section some
    // reader owns.
    assert!(
        unowned.is_empty(),
        "template documents top-level tables no section reads: {unowned:#?}"
    );
}

/// The dotted keys of every section the template may document.
///
/// The 14 `Configurable` sections are the launch-validated roster (see
/// [`crate::registration`]); the `ConfigList` sections are read on
/// demand and cannot be registered, but they are still owned.
const OWNED_KEYS: &[&str] = &[
    // Launch-validated (`Configurable`).
    "tools",
    "chat_log",
    "skills",
    "context_curation.compaction",
    "provider.request_retry",
    "provider.web_search",
    "ui.cwd_selector",
    "ui.minimap",
    "watchdog.stall",
    "watchdog.tool_call",
    "context_curation.auto_prune",
    "term",
    "mcp",
    "discord",
    // Read on demand (`ConfigList`).
    "session_lifecycle",
    "project",
    "attendant",
    "stream_rules",
];

#[rstest::rstest]
#[test]
fn shipped_shell_rules_all_compile() {
    // Given the shipped rules exactly as the template ships them.
    let config = jinn_config::testutil::config_layer(DEFAULT_CONFIG);

    // When reading them back out of the config layer.
    let rules = config.get_list::<StreamRuleConfig>();

    // Then the list reads and every pattern compiles. An uncompilable
    // pattern is inert at runtime (warned, never fires), so a typo here would
    // silently ship a rule that enforces nothing.
    let Ok(rules) = rules else {
        panic!("stream rules list does not read: {rules:?}");
    };
    assert!(
        !rules.is_empty(),
        "the template ships rules that guard real footguns"
    );
    for rule in &rules {
        for pattern in &rule.conditions {
            assert!(
                regex::Regex::new(pattern).is_ok(),
                "stream rule pattern does not compile: {:?} in rule {:?}",
                pattern,
                rule.name
            );
        }
    }
}

/// `#[case]`-free guard: the two sections the template deliberately does
/// not ship a block for still read as their defaults, so a stock
/// install is valid rather than incomplete.
#[rstest::rstest]
#[test]
fn sections_absent_from_the_template_read_as_defaults() {
    // Given a document with no `[mcp]` and no `[discord]` table.
    let config = jinn_config::testutil::config_layer("[tools]\ndefault_timeout_secs = 300");
    register_all_sections(&config);

    // When the layer validates and both absent sections are read.
    let valid = config.validate();
    let mcp = config.get::<McpServersConfig>();
    let discord = config.get::<DiscordConfig>();

    // Then validation passes and both read as their disabled defaults.
    assert!(valid.is_ok(), "a stock document validates");
    assert!(
        mcp.expect("absent section reads a default").is_empty(),
        "absent [mcp] reads no servers"
    );
    assert!(
        !discord.expect("absent section reads a default").enabled,
        "absent [discord] reads disabled"
    );
}
