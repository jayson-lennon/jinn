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
//! `ui.*`, `watchdog.*`, `term`, and `mcp` (the builtin overgraph
//! server). There is deliberately no `[discord]` block; that section
//! reads as its defaults when absent, and a stock install looks exactly
//! like that. Adding it to the template is a product decision, not
//! something this test should force.

#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "test code asserts with expect/panic for clear failure messages"
)]

use jinn_common::template_check::collect_toml_key_paths;

use crate::config_template::{DEFAULT_CONFIG, STREAM_RULES_TEMPLATE};
use crate::registration::register_all_sections;
use crate::schemas::{
    AppletsConfig, CanvasConfig, DiscordConfig, McpServersConfig, RemoteConfig, StreamRuleConfig,
    StreamRuleFileEntry, parse_stream_rules_file,
};

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
    "shell",
    "mcp",
    "discord",
    "canvas",
    "applets",
    "remote",
    "dictation",
    // Read on demand (`ConfigList`).
    "session_lifecycle",
    "project",
    "bot",
    "stream_rules",
];

#[rstest::rstest]
#[test]
fn shipped_rules_parse_through_the_production_schema() {
    // Given the shipped rules file exactly as the runtime embeds it.
    // When reading it back through the production `StreamRuleConfig` list
    // type.
    let rules = parse_stream_rules_file(STREAM_RULES_TEMPLATE)
        .expect("shipped stream_rules.toml must parse")
        .entries()
        .to_vec()
        .iter()
        .map(StreamRuleFileEntry::live_rule)
        .collect::<Vec<StreamRuleConfig>>();

    // Then every entry reads as a live rule, ignoring the fixture fields —
    // production deserialization never sees `positive`/`negative`.
    assert!(
        !rules.is_empty(),
        "the shipped set guards real footguns; an empty file disarms jinn"
    );
}

#[rstest::rstest]
#[test]
fn shipped_rules_fixture_fields_are_unknown_to_the_production_list() {
    // Given the shipped file read as a `jinn.toml`-style config layer.
    let config = jinn_config::testutil::config_layer(STREAM_RULES_TEMPLATE);

    // When the production list type deserializes it.
    let rules = config.get_list::<StreamRuleConfig>();

    // Then it succeeds: `positive` and `negative` are unknown keys to the
    // live schema and are ignored, so a user pasting an entry between files
    // needs no cleanup.
    let Ok(rules) = rules else {
        panic!("production deserialization must ignore the fixture fields: {rules:?}");
    };
    assert_ne!(rules, [] as [StreamRuleConfig; 0]);
}

#[rstest::rstest]
#[test]
fn shipped_rules_carry_complete_fixtures() {
    // Given the shipped rules document.
    let entries = parse_stream_rules_file(STREAM_RULES_TEMPLATE)
        .expect("shipped stream_rules.toml must parse")
        .entries()
        .to_vec();

    // When every entry is checked for its fixture obligations.
    // Then each carries at least one positive and one negative — this is
    // per-entry, deliberately NOT a count assertion: the set is curated and
    // expected to grow.
    for entry in &entries {
        assert!(
            !entry.positive.is_empty(),
            "rule {:?} ships no positive fixture",
            entry.name
        );
        assert!(
            !entry.negative.is_empty(),
            "rule {:?} ships no negative fixture",
            entry.name
        );
    }
}

#[rstest::rstest]
#[test]
fn shipped_rules_all_compile() {
    // Given the shipped rules exactly as the runtime embeds them.
    let rules = parse_stream_rules_file(STREAM_RULES_TEMPLATE)
        .expect("shipped stream_rules.toml must parse")
        .entries()
        .to_vec();

    // When every condition is compiled.
    // Then each compiles. An uncompilable pattern is inert at runtime
    // (warned, never fires), so a typo here would silently ship a rule that
    // enforces nothing.
    for entry in &rules {
        for pattern in &entry.conditions {
            assert!(
                regex::Regex::new(pattern).is_ok(),
                "stream rule pattern does not compile: {:?} in rule {:?}",
                pattern,
                entry.name
            );
        }
    }
}

#[rstest::rstest]
#[test]
fn shipped_rules_carry_source_and_license_attribution() {
    // Given the raw text of the shipped file and the entries parsed from it.
    let entries = parse_stream_rules_file(STREAM_RULES_TEMPLATE)
        .expect("shipped stream_rules.toml must parse")
        .entries()
        .to_vec();

    // When the comment lines are counted.
    // Then the attribution comments are total: one `source:` and one
    // `license:` line per entry, neither more nor less.
    let count = |marker: &str| {
        STREAM_RULES_TEMPLATE
            .lines()
            .filter(|line| line.trim_start().starts_with(&format!("# {marker}:")))
            .count()
    };
    assert_eq!(
        count("source"),
        entries.len(),
        "every entry needs exactly one `# source:` comment"
    );
    assert_eq!(
        count("license"),
        entries.len(),
        "every entry needs exactly one `# license:` comment"
    );
}

#[rstest::rstest]
#[test]
fn shipped_rules_drop_the_removed_trigger_key() {
    // Given the shipped file, parsed as bare TOML.
    let doc = STREAM_RULES_TEMPLATE
        .parse::<toml::Table>()
        .expect("shipped stream_rules.toml parses as TOML");

    // When every entry table is checked for the removed key.
    // Then no entry carries it: the two-mode trigger system is gone, and
    // the shipped set must not teach it. (Parsed, not raw-text: the header
    // prose is allowed to explain that the key was removed.)
    let entries = doc
        .get("stream_rules")
        .and_then(|t| t.get("entry"))
        .and_then(|e| e.as_array())
        .expect("shipped file carries the stream_rules.entry list");
    for entry in entries {
        assert!(
            entry.get("on_trigger").is_none(),
            "shipped entry carries the removed on_trigger key: {entry}"
        );
    }
}

/// `#[case]`-free guard: the one section the template deliberately does
/// not ship a block for still reads as its default, so a stock install
/// is valid rather than incomplete.
#[rstest::rstest]
#[test]
fn sections_absent_from_the_template_read_as_defaults() {
    // Given a document with no `[discord]` table.
    let config = jinn_config::testutil::config_layer("[tools]\ndefault_timeout_secs = 300");
    register_all_sections(&config);

    // When the layer validates and the absent section is read.
    let valid = config.validate();
    let discord = config.get::<DiscordConfig>();

    // Then validation passes and the section reads as its disabled default.
    assert!(valid.is_ok(), "a stock document validates");
    assert!(
        !discord.expect("absent section reads a default").enabled,
        "absent [discord] reads disabled"
    );
}

/// The shipped `[mcp.overgraph]` entry is the builtin graph database
/// server: one shared process (`singleton`) that sessions opt into
/// through the picker (`auto_enable = false`), served by jinn itself
/// (`<jinn>` token, no `command`/`url` of its own).
#[rstest::rstest]
#[test]
fn template_ships_overgraph_as_a_disabled_singleton_builtin() {
    // Given the shipped template parsed as a config layer.
    let config = jinn_config::testutil::config_layer(DEFAULT_CONFIG);
    register_all_sections(&config);

    // When reading the mcp section.
    let mcp = config
        .get::<McpServersConfig>()
        .expect("mcp section parses");

    // Then the overgraph entry exists with the builtin's exact shape.
    let overgraph = mcp
        .get("overgraph")
        .expect("shipped template configures the builtin overgraph server");
    assert_eq!(overgraph.command.as_deref(), Some("<jinn>"));
    assert_eq!(overgraph.args, ["mcp", "overgraph"]);
    assert!(
        overgraph.singleton,
        "the shared graph db needs one owner process"
    );
    assert!(!overgraph.auto_enable, "sessions opt in through the picker");
    assert!(overgraph.url.is_none(), "stdio server needs no url");

    // And the template's own singleton comment carries no transport
    // restriction — the claim was verified wrong and the shipped entry
    // relies on it working over stdio.
    assert!(
        !DEFAULT_CONFIG.contains("only applies for `local_http`"),
        "stale singleton claim reintroduced into default_jinn.toml"
    );
}

/// The shipped `[canvas]` section is active and binds loopback port 0:
/// an OS-assigned ephemeral port per running jinn instance, so two
/// instances never fight over one fixed port (end state: multi-instance
/// isolation). The bind is taken as configured — the shipped default is
/// loopback, but an explicit configured value wins and the schema keeps
/// whatever a user wrote.
#[rstest::rstest]
#[test]
fn template_ships_canvas_as_loopback_port_zero() {
    // Given the shipped template parsed as a config layer.
    let config = jinn_config::testutil::config_layer(DEFAULT_CONFIG);
    register_all_sections(&config);

    // When reading the canvas section.
    let canvas = config.get::<CanvasConfig>().expect("canvas section parses");

    // Then the shipped bind is loopback port 0.
    assert_eq!(canvas.bind, "127.0.0.1:0");
    let addr = canvas.parse_bind().expect("shipped bind parses");
    assert!(addr.ip().is_loopback());
    assert_eq!(addr.port(), 0);

    // And the template's canvas comment promises the per-instance port
    // model, not the retired fixed-port one.
    assert!(
        !DEFAULT_CONFIG.contains("jinn canvas serve"),
        "retired CLI surface named in default_jinn.toml"
    );
}

/// The shipped `[applets]` section is active and binds loopback port 0:
/// an OS-assigned ephemeral port per running jinn instance, so two
/// instances never fight over one fixed port. The bind is taken as
/// configured — the shipped default is loopback, but an explicit
/// configured value wins and the schema keeps whatever a user wrote.
#[rstest::rstest]
#[test]
fn template_ships_applets_as_loopback_port_zero() {
    // Given the shipped template parsed as a config layer.
    let config = jinn_config::testutil::config_layer(DEFAULT_CONFIG);
    register_all_sections(&config);

    // When reading the applets section.
    let applets = config
        .get::<AppletsConfig>()
        .expect("applets section parses");

    // Then the shipped bind is loopback port 0.
    assert_eq!(applets.bind, "127.0.0.1:0");
    let addr = applets.parse_bind().expect("shipped bind parses");
    assert!(addr.ip().is_loopback());
    assert_eq!(addr.port(), 0);
}

/// The shipped `[remote]` section is active and binds loopback port 0:
/// an OS-assigned ephemeral port per running jinn instance, so two
/// instances never fight over one fixed port. An explicit configured
/// value still wins — the schema keeps whatever a user wrote.
#[rstest::rstest]
#[test]
fn template_ships_remote_as_loopback_port_zero() {
    // Given the shipped template parsed as a config layer.
    let config = jinn_config::testutil::config_layer(DEFAULT_CONFIG);
    register_all_sections(&config);

    // When reading the remote section.
    let remote = config.get::<RemoteConfig>().expect("remote section parses");

    // Then the shipped bind is loopback port 0.
    assert_eq!(remote.bind, "127.0.0.1:0");
    let addr = remote.parse_bind().expect("shipped bind parses");
    assert!(addr.ip().is_loopback());
    assert_eq!(addr.port(), 0);
}
