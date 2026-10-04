//! `[[stream_rules.entry]]` round-trips through the configuration layer.
//!
//! A rule body is the guidance the model resumes under, so it is written as
//! multi-line markdown and hand-edited alongside prose comments explaining
//! *why* a rule exists. Both properties are what these cases pin: the patcher
//! must preserve the author's comments and unknown keys, and must render a
//! multi-line body in the same form on every save so the file does not churn
//! under the user.
//!
//! They live with the schema they exercise rather than with the slice that
//! compiles the rules, because what they pin is the config layer's
//! patch-and-reread behavior for this section's array key.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

use jinn_common::app_info::PREFS_FILE_NAME;
use jinn_config::{ConfigLayer, FilesystemConfigStorage};
use jinn_preferences_config::schemas::StreamRuleConfig;
use tempfile::TempDir;

/// A layer over the temp `jinn.toml` the test just wrote.
fn layer_for(path: &std::path::Path) -> ConfigLayer {
    ConfigLayer::load(std::sync::Arc::new(FilesystemConfigStorage::new(
        path.to_path_buf(),
    )))
    .expect("layer loads")
}

/// A layer over a document the test just wrote, plus the path to re-read.
fn layer_over(body: &str) -> (ConfigLayer, std::path::PathBuf, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join(PREFS_FILE_NAME);
    std::fs::write(&path, body).expect("write");
    (layer_for(&path), path, dir)
}

#[rstest::rstest]
fn load_parses_table_array_stream_rules() {
    // Given a jinn.toml using the [[stream_rules.entry]]
    // table array syntax.
    let (config, _path, _dir) = layer_over(
        r#"[[stream_rules.entry]]
name = "ts-no-any"
description = "Never widen a type to `any`"
conditions = [': any']
scopes = ["tool:edit(*.ts)"]
body = "Use `unknown` instead."
"#,
    );

    // When reading the stream rule list.
    let rules = config.get_list::<StreamRuleConfig>().expect("rules read");

    // Then the entry is populated field for field.
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].name, "ts-no-any");
    assert_eq!(rules[0].conditions, vec![": any".to_owned()]);
    assert_eq!(rules[0].scopes, vec!["tool:edit(*.ts)".to_owned()]);
}

#[rstest::rstest]
fn absent_scopes_read_as_empty() {
    // Given a rule with no scopes key.
    let (config, _path, _dir) = layer_over(
        "[[stream_rules.entry]]\nname = \"no-todos\"\nconditions = ['TODO']\nbody = \"No TODOs.\"\n",
    );

    // When reading the list.
    let rules = config.get_list::<StreamRuleConfig>().expect("rules read");

    // Then its scopes are empty, which means every stream.
    assert!(rules[0].scopes.is_empty());
}

#[rstest::rstest]
fn put_stream_rule_list_preserves_entry_block_and_comments() {
    // Given a jinn.toml with a commented stream rule.
    let original = "# keep this explanation\n[[stream_rules.entry]]\nname = \"ts-no-any\"\ndescription = \"no any\"\nconditions = [': any']\nbody = \"Use unknown.\"\n";
    let (config, path, _dir) = layer_over(original);

    // When re-saving the same list through the layer.
    let rules = config.get_list::<StreamRuleConfig>().expect("rules read");
    config
        .put_list::<StreamRuleConfig>(&rules)
        .expect("rules write");

    // Then the comment and entry survive.
    let written = std::fs::read_to_string(&path).expect("read");
    assert!(written.contains("# keep this explanation"));
    assert!(written.contains("name = \"ts-no-any\""));
    assert!(written.contains("conditions = [\": any\"]"));
}

#[rstest::rstest]
fn put_stream_rule_list_preserves_an_undeclared_key_inside_an_entry() {
    // Given a stream rule carrying a key no field of the struct describes.
    let (config, path, _dir) = layer_over(
        "[[stream_rules.entry]]\nname = \"ts-no-any\"\nconditions = [': any']\nbody = \"Use unknown.\"\nseverity = \"fatal\"\n",
    );

    // When re-saving the same list through the layer.
    let rules = config.get_list::<StreamRuleConfig>().expect("rules read");
    config
        .put_list::<StreamRuleConfig>(&rules)
        .expect("rules write");

    // Then the user's key is left alone, so a newer jinn's field survives
    // an older one saving over the same entry.
    let written = std::fs::read_to_string(&path).expect("read");
    assert!(
        written.contains("severity = \"fatal\""),
        "undeclared key lost: {written}"
    );
}

#[rstest::rstest]
fn put_stream_rule_list_deletes_block_on_entry_removal() {
    // Given a jinn.toml with two stream rules.
    let (config, path, _dir) = layer_over(
        "# keep\n[[stream_rules.entry]]\nname = \"alpha\"\nconditions = ['a']\nbody = \"A.\"\n\n# delete me\n[[stream_rules.entry]]\nname = \"beta\"\nconditions = ['b']\nbody = \"B.\"\n",
    );

    // When saving with only alpha kept.
    let mut rules = config.get_list::<StreamRuleConfig>().expect("rules read");
    rules.retain(|r| r.name == "alpha");
    config
        .put_list::<StreamRuleConfig>(&rules)
        .expect("rules write");

    // Then beta's block and its comment are removed.
    //
    // This is the assertion that the section key must stay dotted: the
    // patcher drops unmatched entries by resolving the list's parent table
    // from the key's head, and refuses to do so for a single-segment key.
    let written = std::fs::read_to_string(&path).expect("read");
    assert!(written.contains("# keep"));
    assert!(written.contains("name = \"alpha\""));
    assert!(
        !written.contains("beta"),
        "removed entry lingers: {written}"
    );
    assert!(!written.contains("# delete me"));
}

#[rstest::rstest]
fn put_stream_rule_list_appends_new_entry_at_end() {
    // Given a jinn.toml with one stream rule.
    let (config, path, _dir) =
        layer_over("[[stream_rules.entry]]\nname = \"alpha\"\nconditions = ['a']\nbody = \"A.\"\n");

    // When adding a new rule and saving the list.
    let mut rules = config.get_list::<StreamRuleConfig>().expect("rules read");
    rules.push(StreamRuleConfig {
        name: "beta".to_owned(),
        conditions: vec!["b".to_owned()],
        body: "B.".to_owned(),
        ..Default::default()
    });
    config
        .put_list::<StreamRuleConfig>(&rules)
        .expect("rules write");

    // Then beta appears after alpha.
    let written = std::fs::read_to_string(&path).expect("read");
    let alpha_pos = written.find("name = \"alpha\"").expect("alpha");
    let beta_pos = written.find("name = \"beta\"").expect("beta");
    assert!(alpha_pos < beta_pos);
}

#[rstest::rstest]
fn multi_line_body_round_trips_byte_for_byte() {
    // Given a rule whose body is three lines of markdown.
    let body = "Use `unknown` instead.\n\nNever widen a type to `any` to silence an error.\n";
    let (config, _path, _dir) = layer_over(
        "[[stream_rules.entry]]\nname = \"ts-no-any\"\nconditions = [': any']\nbody = \"Use `unknown` instead.\"\n",
    );

    // When reading the list back with the multi-line body written in.
    let mut rules = config.get_list::<StreamRuleConfig>().expect("rules read");
    rules[0].body = body.to_owned();
    config
        .put_list::<StreamRuleConfig>(&rules)
        .expect("rules write");
    let reread = config.get_list::<StreamRuleConfig>().expect("rules read");

    // Then the body is exactly what went in.
    assert_eq!(reread[0].body, body);
}

#[rstest::rstest]
fn multi_line_body_renders_as_a_multi_line_basic_string() {
    // Given a rule whose body is multi-line.
    let (config, path, _dir) = layer_over(
        "[[stream_rules.entry]]\nname = \"ts-no-any\"\nconditions = [': any']\nbody = \"short\"\n",
    );

    // When saving it with a multi-line body.
    let mut rules = config.get_list::<StreamRuleConfig>().expect("rules read");
    rules[0].body = "line one\nline two\n".to_owned();
    config
        .put_list::<StreamRuleConfig>(&rules)
        .expect("rules write");

    // Then it renders as a `"""` block, not a `'''` literal block.
    //
    // The patcher emits the form the TOML spec renders a newline-containing
    // basic string in. A hand-written `'''` block would be rewritten to
    // `"""` on the first save, so the file would churn under the user once.
    let written = std::fs::read_to_string(&path).expect("read");
    assert!(
        written.contains("body = \"\"\""),
        "not a basic block: {written}"
    );
    assert!(
        !written.contains("body = '''"),
        "rendered as a literal block: {written}"
    );
}

#[rstest::rstest]
fn saving_the_same_list_twice_produces_an_identical_file() {
    // Given a multi-rule jinn.toml with comments.
    let (config, path, _dir) = layer_over(
        "# top\n[[stream_rules.entry]]\nname = \"alpha\"\n# why alpha matters\nconditions = [': any']\nscopes = [\"tool\"]\nbody = \"A.\\nMore A.\\n\"\n\n# second\n[[stream_rules.entry]]\nname = \"beta\"\nconditions = ['TODO']\nbody = \"B.\"\n",
    );

    // When saving the list twice.
    let rules = config.get_list::<StreamRuleConfig>().expect("rules read");
    config
        .put_list::<StreamRuleConfig>(&rules)
        .expect("first write");
    let first = std::fs::read_to_string(&path).expect("first read");
    let reread = config.get_list::<StreamRuleConfig>().expect("rules reread");
    config
        .put_list::<StreamRuleConfig>(&reread)
        .expect("second write");

    // Then the second save changes nothing.
    let second = std::fs::read_to_string(&path).expect("second read");
    assert_eq!(first, second);
}

#[rstest::rstest]
fn put_stream_rule_list_leaves_a_sibling_section_untouched() {
    // Given a document whose next section holds active configuration.
    let (config, path, _dir) = layer_over(
        "# rules\n[[stream_rules.entry]]\nname = \"alpha\"\nconditions = ['a']\nbody = \"A.\"\n\n[chat_log]\ntool_entry_max_lines = 6\n",
    );

    // When the rule list is written back.
    let rules = config.get_list::<StreamRuleConfig>().expect("rules read");
    config
        .put_list::<StreamRuleConfig>(&rules)
        .expect("rules write");

    // Then the section after the rules array is still active and unchanged.
    let written = std::fs::read_to_string(&path).expect("read");
    assert!(
        written.contains("\n[chat_log]\ntool_entry_max_lines = 6\n"),
        "sibling section disturbed: {written}"
    );
}
