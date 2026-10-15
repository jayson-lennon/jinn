//! Provider configuration - TOML schema and I/O.
//!
//! Defines [`ProvidersConfig`] as the root of `providers.toml`,
//! along with loading, saving, and auto-creation logic. The config file
//! lives at `~/.config/jinn/providers.toml` and is auto-created on
//! first run with commented-out examples for every known backend.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use error_stack::{Report, ResultExt as _};
use jinn_common::app_paths::AppPaths;
use serde::{Deserialize, Serialize};
use wherror::Error;

/// Default provider configuration template, embedded at compile time.
pub(crate) const DEFAULT_CONFIG: &str = include_str!("default_providers.toml");

/// Errors that can occur during config I/O or parsing.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Filesystem I/O failure.
    #[error("config I/O error")]
    Io,
    /// TOML parsing or structural error.
    #[error("config parse error")]
    Parse,
    /// Semantic validation error (duplicate names, missing targets, etc.).
    #[error("config validation error")]
    Validation,
}

/// Root of `providers.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvidersConfig {
    /// User-defined provider entries, keyed by provider name.
    /// The map key is the provider's identity (`ProviderId` source) and
    /// the TOML table name — `[providers.<name>]`.
    pub providers: BTreeMap<String, ProviderEntry>,
    /// User-defined aliases (short names → provider entries).
    #[serde(default)]
    pub aliases: Vec<AliasEntry>,
    /// The last-selected default provider (persisted across sessions).
    #[serde(default)]
    pub default_provider: Option<String>,
    /// Pinned OpenRouter routing endpoints, keyed by full model id.
    /// Written by the endpoint picker; a model with no row auto-routes.
    /// See [`EndpointDefault`].
    #[serde(default)]
    pub endpoint_defaults: Vec<EndpointDefault>,
}

/// A pinned OpenRouter routing endpoint for one model.
///
/// Declared at the config root as `[[endpoint_defaults]]` tables. The
/// `model` value is the full model id as shown in the picker (e.g.
/// `"openrouter/anthropic/claude-sonnet-4-20250514"`) and `tag` is the
/// upstream routing tag sent to OpenRouter (e.g. `"anthropic"`).
///
/// Only the tag is persisted: the human-readable provider name is fetched
/// from OpenRouter at display time and would drift if it were stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointDefault {
    /// Full model id this endpoint applies to.
    pub model: String,
    /// The OpenRouter routing tag to force for this model.
    pub tag: String,
}

/// A single configured provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderEntry {
    /// Backend type string, parsed via `Backend::from_str`.
    /// E.g. `"openrouter"`, `"ollama"`, `"openai"`.
    pub backend: String,
    /// Model identifiers available under this provider.
    /// E.g. `["openai/gpt-oss-120b", "anthropic/claude-sonnet-4-20250514"]`.
    pub models: Vec<String>,
    /// Optional base URL override. Required for OpenAI-compatible local
    /// providers like `LMStudio`. Omitted for cloud providers with
    /// well-known endpoints.
    #[serde(default)]
    pub base_url: Option<String>,
    /// Environment variable name holding the API key.
    /// E.g. `"OPENROUTER_API_KEY"`.
    /// Ignored for providers where `requires_key` is `false`.
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// Whether this provider type requires an API key.
    /// Defaults to `true`. Set to `false` for local providers (Ollama).
    #[serde(default = "default_true")]
    pub requires_key: bool,
    /// Extra JSON body parameters passed to the LLM builder.
    /// Used for vendor-specific options like z.ai's `enable_thinking`.
    /// Maps to `LLMBuilder::extra_body()`.
    #[serde(default)]
    pub extra_body: Option<serde_json::Value>,
    /// Manual override for the maximum context length in tokens.
    /// When set, applies to all models under this provider and takes
    /// precedence over API-discovered values.
    #[serde(default)]
    pub context_length: Option<u32>,
    /// Per-model metadata overrides. Keys match ids in `models`;
    /// fields left unset fall back to the block-level values above.
    /// See [`ModelInfoEntry`].
    #[serde(default)]
    pub model_info: Vec<ModelInfoEntry>,
}

/// Per-model metadata override in `providers.toml`.
///
/// Declared under a provider block as `[[providers.model_info]]` tables.
/// Each entry overrides the provider-block defaults for a single model id.
/// Fields left unset inherit the block-level values, then API-discovered
/// cache data, then models.dev reference data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfoEntry {
    /// Model id — must match an entry in the same provider's `models` list.
    pub id: String,
    /// Manual override for the maximum context length in tokens.
    #[serde(default)]
    pub context_length: Option<u32>,
    /// Input modalities the model accepts, e.g. `["text"]` or
    /// `["text", "image"]`. Replaces the discovered value when set;
    /// `None` keeps the discovered value.
    #[serde(default)]
    pub input_modalities: Option<Vec<String>>,
    /// Extra JSON body parameters for this model, overriding the
    /// provider-block `extra_body` when set.
    #[serde(default)]
    pub extra_body: Option<serde_json::Value>,
}

/// A named alias for a provider entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AliasEntry {
    /// Short name shown in the picker.
    pub name: String,
    /// Full provider ID of the target entry. E.g. `"ollama/llama3"`.
    pub target: String,
}

/// Alloy rotation strategy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AlloyStrategy {
    /// Cycle through models in order, incrementing after each LLM call.
    #[default]
    RoundRobin,
    /// Pick a random model on each LLM call.
    Random,
}

/// Default value for boolean fields that default to `true`.
const fn default_true() -> bool {
    true
}

/// Returns the path to the provider config file.
///
/// Derives from [`AppPaths`] (`~/.config/jinn/providers.toml`), so the
/// `JINN_PATHS_ROOT` override is honored here too.
#[must_use]
pub fn config_path() -> PathBuf {
    AppPaths::default().config_path()
}

/// Loads the provider config from disk.
///
/// If the file does not exist, creates the default config and then loads it.
///
/// # Errors
///
/// Returns [`ConfigError::Io`] if the file cannot be read or created.
/// Returns [`ConfigError::Parse`] if the TOML is malformed.
pub fn load_config() -> Result<ProvidersConfig, Report<ConfigError>> {
    let path = config_path();
    load_config_from(&path)
}

/// Loads config from a specific path (testable without touching real config).
pub(crate) fn load_config_from<P>(path: P) -> Result<ProvidersConfig, Report<ConfigError>>
where
    P: AsRef<Path>,
{
    let path = path.as_ref();

    if !path.exists() {
        create_default_config_to(path)?;
    }

    let content = std::fs::read_to_string(path)
        .change_context(ConfigError::Io)
        .attach("failed to read providers config")?;

    toml::from_str(&content)
        .change_context(ConfigError::Parse)
        .attach("failed to parse providers config")
        .map_err(|report| {
            if content.contains("[[providers]]") {
                report.attach(
                    "legacy [[providers]] array syntax is no longer supported; \
                     providers are now declared as [providers.<name>] map tables \
                     (e.g. [[providers]] name = \"ollama\" ... becomes [providers.ollama] ...)",
                )
            } else {
                report
            }
        })
}

/// Creates the default config file at the standard location.
///
/// Creates parent directories as needed and writes the embedded template.
///
/// # Errors
///
/// Returns [`ConfigError::Io`] if directory creation or file writing fails.
pub fn create_default_config() -> Result<PathBuf, Report<ConfigError>> {
    let path = config_path();
    create_default_config_to(&path)?;
    Ok(path)
}

/// Creates the default config file at an explicit path.
pub(crate) fn create_default_config_to<P>(path: P) -> Result<(), Report<ConfigError>>
where
    P: AsRef<Path>,
{
    let path = path.as_ref();

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .change_context(ConfigError::Io)
            .attach("failed to create config directory")?;
    }

    std::fs::write(path, DEFAULT_CONFIG)
        .change_context(ConfigError::Io)
        .attach("failed to write default providers config")
}

/// Outcome of [`init_default_providers_to`].
#[derive(Debug)]
pub enum InitProvidersOutcome {
    /// Template was written to a previously-missing path.
    Created,
    /// Existing file was overwritten (caller passed `force: true`).
    Overwritten,
}

/// Error returned by [`init_default_providers_to`].
#[derive(Debug, wherror::Error)]
#[error(debug)]
pub struct InitProvidersError;

/// Writes the commented providers template to an explicit path.
///
/// - If `path` does not exist: writes the template, returns [`InitProvidersOutcome::Created`].
/// - If `path` exists and `force` is false: fails without touching the file —
///   `providers.toml` is hand-authored, so a silent clobber would destroy user edits.
/// - If `path` exists and `force` is true: overwrites, returns [`InitProvidersOutcome::Overwritten`].
///
/// # Errors
///
/// Returns an error if the file exists and `force` is false, or the write fails.
pub fn init_default_providers_to<P>(
    path: P,
    force: bool,
) -> Result<InitProvidersOutcome, Report<InitProvidersError>>
where
    P: AsRef<Path>,
{
    let path = path.as_ref();
    let existed = path.exists();

    if existed && !force {
        return Err(Report::new(InitProvidersError))
            .attach("providers.toml already exists; pass --force to overwrite")
            .attach(format!("path: {}", path.display()));
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .change_context(InitProvidersError)
            .attach("failed to create config directory")?;
    }

    std::fs::write(path, DEFAULT_CONFIG)
        .change_context(InitProvidersError)
        .attach("failed to write default providers config")?;

    if existed {
        Ok(InitProvidersOutcome::Overwritten)
    } else {
        Ok(InitProvidersOutcome::Created)
    }
}

/// Saves the config back to disk.
///
/// When the file already exists it is patched in place through the
/// comment-preserving `DocumentPatcher`, so user comments, blank lines, and
/// field ordering survive. A first save (no existing file) writes a clean,
/// comment-free serialization.
///
/// # Errors
///
/// Returns [`ConfigError::Io`] if writing fails.
/// Returns [`ConfigError::Parse`] if serialization fails.
pub fn save_config(config: &ProvidersConfig) -> Result<(), Report<ConfigError>> {
    let path = config_path();
    save_config_to(config, &path)
}

pub(crate) fn save_config_to<P>(
    config: &ProvidersConfig,
    path: P,
) -> Result<(), Report<ConfigError>>
where
    P: AsRef<Path>,
{
    let path = path.as_ref();

    // If the file already exists, patch it in place to preserve user-written
    // comments, blank lines, and field ordering.
    if path.exists() {
        let existing = std::fs::read_to_string(path)
            .change_context(ConfigError::Io)
            .attach("failed to read existing providers config")?;
        let mut doc: toml_edit::DocumentMut =
            existing
                .parse()
                .change_context(ConfigError::Parse)
                .attach("failed to parse existing providers config")?;

        let mut patcher = jinn_common::toml_patch::DocumentPatcher::new();
        patcher.register_array_key(["aliases"], "name");
        patcher.register_array_key(["endpoint_defaults"], "model");
        patcher.register_array_key(["providers", "*", "model_info"], "id");
        // `providers`, each provider block, and `aliases` are named sections
        // and keep header form. Unregistered, a table is by definition a
        // *value* inside whatever encloses it and would be written inline.
        //
        // The provider block needs the wildcard: `providers` alone does not
        // cover `providers.alpha`, and each provider is a section in its own
        // right rather than a field of `providers`.
        patcher.register_section(["providers"]);
        patcher.register_section(["providers", "*"]);
        patcher.register_section(["aliases"]);

        let new_value = toml::Value::try_from(config)
            .change_context(ConfigError::Parse)
            .attach("failed to serialize ProvidersConfig")?;
        let new_table: toml::value::Table = match new_value {
            toml::Value::Table(t) => t,
            _ => {
                return Err(Report::new(ConfigError::Parse)
                    .attach("ProvidersConfig serialized to non-table TOML value"));
            }
        };
        patcher
            .apply(&new_table, doc.as_table_mut())
            .change_context(ConfigError::Parse)
            .attach("failed to patch providers config document")?;

        std::fs::write(path, doc.to_string())
            .change_context(ConfigError::Io)
            .attach("failed to write providers config")?;
        return Ok(());
    }

    // First-save path: emit a clean, comment-free serialization.
    let content = toml::to_string_pretty(config)
        .change_context(ConfigError::Parse)
        .attach("failed to serialize providers config")?;

    std::fs::write(path, content)
        .change_context(ConfigError::Io)
        .attach("failed to write providers config")
}

#[cfg(test)]
pub(crate) mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]

    use tempfile::TempDir;

    use super::*;

    /// A fully-specified [`ProvidersConfig`] with every field set explicitly
    /// (no `..Default::default()`, no unset `Option`s). Serves two purposes:
    /// the input for the consolidated round-trip tests, and the schema
    /// fixture the template completeness check enumerates (`jinn-common`'s
    /// `template_check` walks its serialized key paths, `None` included).
    ///
    /// Sentinel values deliberately differ from anything the template
    /// ships, so a serde bug that silently drops a field still fails the
    /// equality assertion.
    #[must_use]
    pub(crate) fn explicit_providers_config() -> ProvidersConfig {
        ProvidersConfig {
            providers: [(
                "fixture-provider".to_owned(),
                ProviderEntry {
                    backend: "fixture-backend".to_owned(),
                    models: vec!["fixture/model-a".to_owned(), "fixture/model-b".to_owned()],
                    base_url: Some("http://fixture.invalid/v1".to_owned()),
                    api_key_env: Some("FIXTURE_API_KEY".to_owned()),
                    requires_key: false,
                    extra_body: Some(serde_json::json!({ "fixture_option": true })),
                    context_length: Some(123_456),
                    model_info: vec![ModelInfoEntry {
                        id: "fixture/model-b".to_owned(),
                        context_length: Some(654_321),
                        input_modalities: Some(vec!["text".to_owned(), "image".to_owned()]),
                        extra_body: Some(serde_json::json!({ "model_fixture": 1 })),
                    }],
                },
            )]
            .into_iter()
            .collect(),
            aliases: vec![AliasEntry {
                name: "fixture-alias".to_owned(),
                target: "fixture-provider/fixture/model-a".to_owned(),
            }],
            default_provider: Some("fixture-provider/fixture/model-a".to_owned()),
            endpoint_defaults: vec![
                EndpointDefault {
                    model: "fixture-provider/fixture/model-a".to_owned(),
                    tag: "fixture-upstream".to_owned(),
                },
                EndpointDefault {
                    model: "fixture-provider/fixture/model-b".to_owned(),
                    tag: "fixture-upstream-2".to_owned(),
                },
            ],
        }
    }

    /// A minimal provider entry with every optional field unset.
    fn entry(backend: &str, models: &[&str]) -> ProviderEntry {
        ProviderEntry {
            backend: backend.to_owned(),
            models: models.iter().map(|m| (*m).to_owned()).collect(),
            base_url: None,
            api_key_env: None,
            requires_key: true,
            extra_body: None,
            context_length: None,
            model_info: Vec::new(),
        }
    }

    #[rstest::rstest]
    fn init_providers_writes_template_when_missing() {
        // Given a path to a nonexistent file.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("providers.toml");

        // When initializing the default providers config (no force).
        let outcome = init_default_providers_to(&path, false).expect("init");

        // Then the file is created with the template bytes.
        assert!(matches!(outcome, InitProvidersOutcome::Created));
        let on_disk = std::fs::read_to_string(&path).expect("read");
        assert_eq!(on_disk, DEFAULT_CONFIG);
    }

    #[rstest::rstest]
    fn init_providers_refuses_existing_without_force() {
        // Given an existing file with hand-authored content.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("providers.toml");
        let marker = "# user-managed\n[providers.ollama]\n";
        std::fs::write(&path, marker).expect("write");

        // When initializing without --force.
        let result = init_default_providers_to(&path, false);

        // Then the call fails and the file is unchanged.
        assert!(result.is_err());
        let on_disk = std::fs::read_to_string(&path).expect("read");
        assert_eq!(on_disk, marker);
    }

    #[rstest::rstest]
    fn init_providers_overwrites_when_force() {
        // Given an existing file with stale content.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("providers.toml");
        std::fs::write(&path, "# stale\n").expect("write");

        // When initializing with --force.
        let outcome = init_default_providers_to(&path, true).expect("init");

        // Then the file is overwritten with the template bytes.
        assert!(matches!(outcome, InitProvidersOutcome::Overwritten));
        let on_disk = std::fs::read_to_string(&path).expect("read");
        assert_eq!(on_disk, DEFAULT_CONFIG);
    }

    /// Writes a well-formed new-format TOML config to a temp file and loads it.
    fn load_test_config() -> ProvidersConfig {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("providers.toml");
        let toml = r#"
[providers.ollama]
backend = "ollama"
requires_key = false
models = ["llama3", "codellama"]

[[aliases]]
name = "fast"
target = "ollama/llama3""#;
        std::fs::write(&path, toml).expect("write");
        load_config_from(&path).expect("load")
    }

    #[rstest::rstest]
    fn load_config_parses_provider_count_and_models() {
        // Given a well-formed TOML config.
        // When loading it.
        let config = load_test_config();

        // Then provider count and models are correct.
        assert_eq!(config.providers.len(), 1);
        assert_eq!(
            config.providers["ollama"].models,
            vec!["llama3", "codellama"]
        );
    }

    #[rstest::rstest]
    fn load_config_uses_table_name_as_provider_key() {
        // Given a well-formed TOML config.
        // When loading it.
        let config = load_test_config();

        // Then the map key is the provider name (no name field on disk).
        assert!(config.providers.contains_key("ollama"));
    }

    #[rstest::rstest]
    fn load_config_parses_alias_fields() {
        // Given a well-formed TOML config.
        // When loading it.
        let config = load_test_config();

        // Then the alias name and target match.
        assert_eq!(config.aliases.len(), 1);
        assert_eq!(config.aliases[0].name, "fast");
        assert_eq!(config.aliases[0].target, "ollama/llama3");
    }

    #[rstest::rstest]
    fn malformed_toml_render_includes_parse_detail() {
        // Given a providers.toml with a missing comma (unclosed array).
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("providers.toml");
        let toml = "[providers.ollama]\nbackend = \"ollama\"\nmodels = [\"llama3\"\n";
        std::fs::write(&path, toml).expect("write");

        // When loading.
        let result = load_config_from(&path);

        // Then the rendered report carries the TOML parse detail with line info.
        let report = result.expect_err("malformed toml must fail");
        let rendered = format!("{report:?}");
        assert!(
            rendered.contains("TOML parse error"),
            "missing TOML parse detail: {rendered}"
        );
        assert!(
            rendered.contains("line"),
            "missing line information: {rendered}"
        );
    }

    #[rstest::rstest]
    fn legacy_array_syntax_fails_with_new_syntax_hint() {
        // Given a legacy [[providers]] array-format file.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("providers.toml");
        let toml =
            "[[providers]]\nname = \"ollama\"\nbackend = \"ollama\"\nmodels = [\"llama3\"]\n";
        std::fs::write(&path, toml).expect("write");

        // When loading.
        let result = load_config_from(&path);

        // Then the error names the new [providers.<name>] syntax.
        let report = result.expect_err("legacy syntax must fail");
        let rendered = format!("{report:?}");
        assert!(
            rendered.contains("[providers.<name>]"),
            "hint missing: {rendered}"
        );
    }

    #[rstest::rstest]
    fn duplicate_provider_keys_fail_toml_parse() {
        // Given a file declaring the same provider table twice.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("providers.toml");
        let toml = "[providers.ollama]\nbackend = \"ollama\"\nmodels = [\"llama3\"]\n\n[providers.ollama]\nbackend = \"ollama\"\nmodels = [\"llama3\"]\n";
        std::fs::write(&path, toml).expect("write");

        // When loading.
        let result = load_config_from(&path);

        // Then parsing fails.
        assert!(result.is_err());
    }

    #[rstest::rstest]
    fn load_config_creates_default_when_missing() {
        // Given a temp directory with no config file.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("providers.toml");

        assert!(!path.exists());

        // When loading config.
        let config = load_config_from(&path).expect("load");

        // Then the file is created and parseable.
        assert!(path.exists());
        // The default config has one uncommented provider.
        assert!(!config.providers.is_empty());
    }

    #[rstest::rstest]
    fn save_config_writes_valid_toml() {
        // Given a config with providers.
        let config = ProvidersConfig {
            providers: BTreeMap::from([("test".to_owned(), entry("openrouter", &["gpt-4"]))]),
            aliases: vec![],
            default_provider: Some("test/gpt-4".to_owned()),
            endpoint_defaults: Vec::new(),
        };

        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("providers.toml");

        // When saving and reloading.
        save_config_to(&config, &path).expect("save");
        let reloaded = load_config_from(&path).expect("reload");

        // Then the round-tripped config matches.
        assert_eq!(reloaded.providers.len(), 1);
        assert!(reloaded.providers.contains_key("test"));
        assert_eq!(reloaded.default_provider.as_deref(), Some("test/gpt-4"));
    }

    #[rstest::rstest]
    fn first_save_writes_providers_alphabetically() {
        // Given a config with providers inserted in non-alphabetical order.
        let config = ProvidersConfig {
            providers: BTreeMap::from([
                ("zeta".to_owned(), entry("openai", &["gpt-4"])),
                ("alpha".to_owned(), entry("openai", &["gpt-4"])),
            ]),
            aliases: vec![],
            default_provider: None,
            endpoint_defaults: Vec::new(),
        };

        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("providers.toml");

        // When saving to a path with no existing file (first-save path).
        save_config_to(&config, &path).expect("save");
        let written = std::fs::read_to_string(&path).expect("read back");

        // Then alpha serializes before zeta.
        let alpha_pos = written.find("[providers.alpha]").expect("alpha block");
        let zeta_pos = written.find("[providers.zeta]").expect("zeta block");
        assert!(alpha_pos < zeta_pos, "providers not written alphabetically");
    }

    #[rstest::rstest]
    fn patch_save_over_shipped_template_preserves_all_comments() {
        // Given the shipped template written to disk.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("providers.toml");
        std::fs::write(&path, DEFAULT_CONFIG).expect("write template");

        // When loading, mutating one field, and saving.
        let mut config = load_config_from(&path).expect("load");
        config.default_provider = Some("ollama/llama3".to_owned());
        save_config_to(&config, &path).expect("save");

        // Then every comment line from the template survives verbatim.
        let written = std::fs::read_to_string(&path).expect("read back");
        for original_line in DEFAULT_CONFIG.lines().filter(|l| l.starts_with('#')) {
            assert!(
                written.contains(original_line),
                "expected comment line preserved: {original_line}"
            );
        }
    }

    #[rstest::rstest]
    fn config_path_uses_dirs_config_dir() {
        // Given the standard config path.
        // When reading it.
        let path = config_path();

        // Then it ends with jinn/providers.toml, separator-portably.
        assert_eq!(
            path.ancestors()
                .find(|a| a.ends_with("jinn"))
                .map(|a| path.strip_prefix(a).map(Path::to_path_buf)),
            Some(Ok(Path::new("providers.toml").to_path_buf()))
        );
    }

    #[rstest::rstest]
    fn load_config_parses_extra_body_on_named_provider() {
        // Given a config with extra_body under a named provider.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("providers.toml");
        let toml = r#"
[providers.zai]
backend = "zai"
api_key_env = "ZAI_API_KEY"
models = ["glm-5.1"]

[providers.zai.extra_body]
enable_thinking = true
tool_stream = true"#;
        std::fs::write(&path, toml).expect("write");

        // When loading.
        let config = load_config_from(&path).expect("load");

        // Then extra_body lands on the named provider.
        assert_eq!(config.providers.len(), 1);
        let extra = config.providers["zai"]
            .extra_body
            .as_ref()
            .expect("extra_body");
        assert_eq!(extra["enable_thinking"], true);
        assert_eq!(extra["tool_stream"], true);
    }

    #[rstest::rstest]
    fn explicit_providers_save_then_load_round_trips() {
        // Given a fully-specified providers config fixture.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("providers.toml");
        let config = explicit_providers_config();

        // When saving and reloading.
        save_config_to(&config, &path).expect("save");
        let reloaded = load_config_from(&path).expect("load");

        // Then every field survives the round-trip exactly.
        assert_eq!(reloaded, config);
    }

    #[rstest::rstest]
    fn save_config_preserves_user_comments() {
        // Given a comment-rich providers.toml written by the user.
        let original = "# my favorite provider\n[providers.ollama]\nbackend = \"ollama\"\nmodels = [\"llama3\"]\n";
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("providers.toml");
        std::fs::write(&path, original).expect("write");

        // When loading, mutating default_provider, and saving.
        let mut config = load_config_from(&path).expect("load");
        config.default_provider = Some("ollama/llama3".to_owned());
        save_config_to(&config, &path).expect("save");

        // Then the original comment is preserved verbatim.
        let written = std::fs::read_to_string(&path).expect("read");
        assert!(
            written.contains("# my favorite provider"),
            "comment was wiped: {written}"
        );
        assert!(written.contains("default_provider = \"ollama/llama3\""));
    }

    #[rstest::rstest]
    fn save_config_appends_new_provider_at_end() {
        // Given a single-provider config.
        let original = "# existing\n[providers.alpha]\nbackend = \"x\"\nmodels = [\"a\"]\n";
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("providers.toml");
        std::fs::write(&path, original).expect("write");

        // When saving a config with alpha + beta.
        let config = ProvidersConfig {
            providers: BTreeMap::from([
                ("alpha".to_owned(), entry("x", &["a"])),
                ("beta".to_owned(), entry("x", &["b"])),
            ]),
            aliases: vec![],
            default_provider: None,
            endpoint_defaults: Vec::new(),
        };
        save_config_to(&config, &path).expect("save");

        // Then beta appears after alpha (appended), and the comment survives.
        let written = std::fs::read_to_string(&path).expect("read");
        let alpha_pos = written.find("[providers.alpha]").expect("alpha");
        let beta_pos = written.find("[providers.beta]").expect("beta");
        assert!(alpha_pos < beta_pos, "beta not appended after alpha");
        assert!(written.contains("# existing"));
    }

    #[rstest::rstest]
    fn save_config_preserves_alias_block_comments_on_mutation() {
        // Given a providers.toml with a comment-rich alias block.
        let original = "[providers.ollama]\nbackend = \"ollama\"\nrequires_key = false\nmodels = [\"llama3\", \"codellama\"]\n\n# shortcut for my favorite model\n[[aliases]]\nname = \"fast\"\ntarget = \"ollama/llama3\"\n\n# another alias\n[[aliases]]\nname = \"smart\"\ntarget = \"ollama/codellama\"\n";
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("providers.toml");
        std::fs::write(&path, original).expect("write");

        // When loading and changing only the first alias's target.
        let mut config = load_config_from(&path).expect("load");
        config.aliases[0].target = "ollama/codellama".to_owned();
        save_config_to(&config, &path).expect("save");

        // Then both comments are preserved, only the target changed.
        let written = std::fs::read_to_string(&path).expect("read");
        assert!(
            written.contains("# shortcut for my favorite model"),
            "first alias comment lost"
        );
        assert!(
            written.contains("# another alias"),
            "second alias comment lost"
        );
        assert!(
            written.contains("target = \"ollama/codellama\""),
            "target not updated"
        );
        assert!(
            !written.contains("target = \"ollama/llama3\""),
            "old target still present"
        );
    }

    #[rstest::rstest]
    fn save_config_actually_writes_to_disk() {
        // If save_config were a no-op, the file would not exist after the call.
        // Given a config to persist and a target path.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("providers.toml");
        let config = ProvidersConfig {
            providers: BTreeMap::from([("test-save".to_owned(), entry("ollama", &["llama3"]))]),
            aliases: vec![],
            default_provider: Some("test-save/llama3".to_owned()),
            endpoint_defaults: Vec::new(),
        };

        // When saving it to disk.
        save_config_to(&config, &path).expect("save");

        // Then the file exists on disk.
        assert!(path.exists(), "save_config should write the file");
        let content = std::fs::read_to_string(&path).expect("read back");
        assert!(content.contains("test-save"));
        assert!(content.contains("llama3"));
    }

    #[rstest::rstest]
    fn create_default_config_returns_actual_path() {
        // If it returned an empty PathBuf, the path would not point to a file.
        // Given a path with no config file yet.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("providers.toml");

        // When creating the default config there.
        create_default_config_to(&path).expect("create");

        // Then the path is valid and the file exists with content.
        assert!(path.exists(), "default config file should exist");
        let content = std::fs::read_to_string(&path).expect("read");
        assert!(!content.is_empty(), "default config should have content");
    }

    #[rstest::rstest]
    fn default_true_makes_requires_key_default_to_true() {
        // Verifies that a provider entry without requires_key in TOML defaults to true.
        // Given a provider entry with no requires_key field.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("providers.toml");
        // No requires_key field - should default to true.
        std::fs::write(
            &path,
            "[providers.openai]\nbackend = \"openai\"\nmodels = [\"gpt-4\"]\napi_key_env = \"OPENAI_API_KEY\"\n",
        )
        .expect("write");

        // When loading it.
        let config = load_config_from(&path).expect("load");

        // Then requires_key defaults to true.
        assert!(
            config.providers["openai"].requires_key,
            "requires_key should default to true"
        );
    }

    /// Build a realistic new-format providers.toml with comments in every position
    /// we care about: banner, section dividers, per-provider comments, inline
    /// trailing comments, commented-out examples, and mid-block field comments.
    fn realistic_providers_toml() -> &'static str {
        r#"
# jinn provider configuration
#
# This is a heavily-commented real-world style file.
# Edit freely — comments survive TUI mutations.

# --- Providers ---

# my primary chat backend
[providers.openrouter]
backend = "openrouter"
api_key_env = "OPENROUTER_API_KEY"   # never checked in
models = [
    "anthropic/claude-sonnet-4-20250514",
    "google/gemini-2.5-flash",
]

# local fallback
[providers.ollama]
backend = "ollama"
requires_key = false
base_url = "http://localhost:11434"
models = ["llama3"]

# --- Aliases ---

# quick picker shortcuts
[[aliases]]
name = "smart"
target = "openrouter/anthropic/claude-sonnet-4-20250514"

# local = fast
[[aliases]]
name = "fast"
target = "ollama/llama3"

# --- Default ---

default_provider = "smart"   # what opens on launch

# --- Examples (commented out, must survive as comments) ---
# [providers.sample]
# backend = "sample""#
    }

    #[rstest::rstest]
    fn save_config_round_trip_preserves_all_comment_styles() {
        // Given a comment-rich providers.toml fixture.
        let original = realistic_providers_toml();
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("providers.toml");
        std::fs::write(&path, original).expect("write");

        // When loading and immediately re-saving without mutation.
        let config = load_config_from(&path).expect("load");
        save_config_to(&config, &path).expect("save");
        let written = std::fs::read_to_string(&path).expect("read");

        // Then every comment category survives byte-for-byte.
        let expectations = [
            "# jinn provider configuration",
            "# This is a heavily-commented real-world style file.",
            "# Edit freely",
            "# --- Providers ---",
            "# my primary chat backend",
            "# never checked in",
            "# local fallback",
            "# --- Aliases ---",
            "# quick picker shortcuts",
            "# local = fast",
            "# --- Default ---",
            "# what opens on launch",
            "# --- Examples (commented out, must survive as comments) ---",
            "# [providers.sample]",
            "# backend = \"sample\"",
        ];
        for needle in expectations {
            assert!(
                written.contains(needle),
                "expected comment preserved: {needle:?}\nactual output:\n{written}",
            );
        }
    }

    #[rstest::rstest]
    fn save_config_mutating_one_field_preserves_all_unrelated_comments() {
        // Given the comment-rich providers.toml fixture.
        let original = realistic_providers_toml();
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("providers.toml");
        std::fs::write(&path, original).expect("write");

        // When mutating a single field (add a model to openrouter).
        let mut config = load_config_from(&path).expect("load");
        config
            .providers
            .get_mut("openrouter")
            .expect("openrouter exists")
            .models
            .push("openai/gpt-4o".to_owned());
        save_config_to(&config, &path).expect("save");
        let written = std::fs::read_to_string(&path).expect("read");

        // Then the new model appears, AND every other comment survives.
        assert!(written.contains("\"openai/gpt-4o\""), "new model written");
        assert!(written.contains("# my primary chat backend"));
        assert!(written.contains("# local fallback"));
        assert!(written.contains("# --- Aliases ---"));
        assert!(written.contains("# local = fast"));
        assert!(written.contains("# --- Default ---"));
        assert!(written.contains("# --- Examples"));
        assert!(written.contains("# never checked in"));
    }

    #[rstest::rstest]
    fn save_config_deleting_one_alias_preserves_only_its_comment() {
        // Given the comment-rich providers.toml fixture.
        let original = realistic_providers_toml();
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("providers.toml");
        std::fs::write(&path, original).expect("write");

        // When deleting only the 'fast' alias.
        let mut config = load_config_from(&path).expect("load");
        config.aliases.retain(|a| a.name != "fast");
        save_config_to(&config, &path).expect("save");
        let written = std::fs::read_to_string(&path).expect("read");

        // Then the 'fast' alias block AND its '# local = fast' comment are gone,
        // but the 'smart' alias's '# quick picker shortcuts' comment survives.
        assert!(!written.contains("\"fast\""), "fast alias removed");
        assert!(!written.contains("# local = fast"), "fast comment removed");
        assert!(
            written.contains("# quick picker shortcuts"),
            "smart comment preserved"
        );
        assert!(written.contains("\"smart\""), "smart alias preserved");
    }

    #[rstest::rstest]
    fn load_config_parses_model_info_tables() {
        // Given a config with [[providers.<name>.model_info]] tables.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("providers.toml");
        let toml = r#"
[providers.ollama]
backend = "ollama"
requires_key = false
models = ["llama3", "codellama"]

[[providers.ollama.model_info]]
id = "llama3"
context_length = 8192
input_modalities = ["text", "image"]"#;
        std::fs::write(&path, toml).expect("write");

        // When loading.
        let config = load_config_from(&path).expect("load");

        // Then the model_info entry attaches to the named provider with all fields.
        let info = &config.providers["ollama"].model_info;
        assert_eq!(info.len(), 1);
        assert_eq!(info[0].id, "llama3");
        assert_eq!(info[0].context_length, Some(8192));
        assert_eq!(
            info[0].input_modalities,
            Some(vec!["text".to_owned(), "image".to_owned()])
        );
        assert!(info[0].extra_body.is_none());
    }

    #[rstest::rstest]
    fn save_config_preserves_model_info_block_comments() {
        // Given a user-authored config with commented model_info blocks.
        let original = "# my vision model\n[providers.ollama]\nbackend = \"ollama\"\nrequires_key = false\nmodels = [\"llama3\"]\n\n# vision-capable local model\n[[providers.ollama.model_info]]\nid = \"llama3\"\ncontext_length = 8192\ninput_modalities = [\"text\", \"image\"]\n";
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("providers.toml");
        std::fs::write(&path, original).expect("write");

        // When loading, changing the context length, and saving.
        let mut config = load_config_from(&path).expect("load");
        config
            .providers
            .get_mut("ollama")
            .expect("ollama exists")
            .model_info[0]
            .context_length = Some(16384);
        save_config_to(&config, &path).expect("save");

        // Then the model_info block comment survives and the value updates in place.
        let written = std::fs::read_to_string(&path).expect("read");
        assert!(
            written.contains("# vision-capable local model"),
            "model_info comment wiped: {written}"
        );
        assert!(written.contains("context_length = 16384"));
        assert!(!written.contains("context_length = 8192"));
    }

    #[rstest::rstest]
    fn save_config_preserves_unknown_alloy_block() {
        // Given a providers.toml containing a legacy [[alloys]] block.
        // AlloyEntry is removed, but the block must survive round-trips
        // so user config (and comments) is never silently erased.
        let original = r#"# my setup
[providers.ollama]
backend = "ollama"
models = ["llama3"]
requires_key = false

# balanced alloy for varied responses
[[alloys]]
name = "balanced"
models = ["ollama/llama3", "openrouter/anthropic/claude-sonnet-4"]
strategy = "round_robin""#;
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("providers.toml");
        std::fs::write(&path, original).expect("write");

        // When loading and re-saving without mutation.
        let config = load_config_from(&path).expect("load");
        save_config_to(&config, &path).expect("save");
        let written = std::fs::read_to_string(&path).expect("read");

        // Then the unknown [[alloys]] block is preserved as-is, comment and all.
        assert!(
            written.contains("# balanced alloy for varied responses"),
            "alloy comment lost: {written}"
        );
        assert!(written.contains("name = \"balanced\""));
    }

    #[rstest::rstest]
    fn load_config_without_endpoint_defaults_key_yields_empty_list() {
        // Given a providers.toml written before endpoint pinning existed.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("providers.toml");
        std::fs::write(
            &path,
            "[providers.ollama]\nbackend = \"ollama\"\nmodels = [\"llama3\"]\n",
        )
        .expect("write");

        // When loading.
        let config = load_config_from(&path).expect("load");

        // Then the endpoint defaults are an empty list (every model auto-routes).
        assert_eq!(config.endpoint_defaults, [] as [EndpointDefault; 0]);
    }

    #[rstest::rstest]
    fn load_config_parses_endpoint_defaults_rows() {
        // Given a providers.toml with two [[endpoint_defaults]] rows.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("providers.toml");
        let toml = r#"
[providers.openrouter]
backend = "openrouter"
models = ["anthropic/claude-sonnet-4-20250514"]

[[endpoint_defaults]]
model = "openrouter/anthropic/claude-sonnet-4-20250514"
tag = "anthropic"

[[endpoint_defaults]]
model = "openrouter/openai/gpt-oss-120b"
tag = "openai"
"#;
        std::fs::write(&path, toml).expect("write");

        // When loading.
        let config = load_config_from(&path).expect("load");

        // Then both rows load with their model id and routing tag.
        assert_eq!(
            config.endpoint_defaults,
            vec![
                EndpointDefault {
                    model: "openrouter/anthropic/claude-sonnet-4-20250514".to_owned(),
                    tag: "anthropic".to_owned(),
                },
                EndpointDefault {
                    model: "openrouter/openai/gpt-oss-120b".to_owned(),
                    tag: "openai".to_owned(),
                },
            ]
        );
    }

    #[rstest::rstest]
    fn endpoint_default_row_missing_tag_fails_to_parse() {
        // Given a providers.toml whose [[endpoint_defaults]] row omits `tag`.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("providers.toml");
        let toml = r#"
[providers.openrouter]
backend = "openrouter"
models = ["anthropic/claude-sonnet-4-20250514"]

[[endpoint_defaults]]
model = "openrouter/anthropic/claude-sonnet-4-20250514"
"#;
        std::fs::write(&path, toml).expect("write");

        // When loading.
        let result = load_config_from(&path);

        // Then parsing fails — a half-written pin is a configuration error,
        // not a silent auto-route.
        assert!(result.is_err());
    }

    #[rstest::rstest]
    fn save_config_round_trips_multiple_endpoint_default_rows() {
        // Given a config carrying two [[endpoint_defaults]] rows.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("providers.toml");
        let config = ProvidersConfig {
            providers: BTreeMap::from([(
                "openrouter".to_owned(),
                entry("openrouter", &["anthropic/claude-sonnet-4-20250514"]),
            )]),
            aliases: vec![],
            default_provider: None,
            endpoint_defaults: vec![
                EndpointDefault {
                    model: "openrouter/anthropic/claude-sonnet-4-20250514".to_owned(),
                    tag: "anthropic".to_owned(),
                },
                EndpointDefault {
                    model: "openrouter/openai/gpt-oss-120b".to_owned(),
                    tag: "openai".to_owned(),
                },
            ],
        };

        // When saving and reloading.
        save_config_to(&config, &path).expect("save");
        let reloaded = load_config_from(&path).expect("reload");

        // Then both rows survive intact.
        assert_eq!(reloaded.endpoint_defaults, config.endpoint_defaults);
    }

    #[rstest::rstest]
    fn save_config_changing_one_endpoint_default_preserves_the_other_rows_comments() {
        // Given a providers.toml whose endpoint rows each carry a comment.
        let original = r#"[providers.openrouter]
backend = "openrouter"
models = ["anthropic/claude-sonnet-4-20250514", "openai/gpt-oss-120b"]

# prefix cache matters most here
[[endpoint_defaults]]
model = "openrouter/anthropic/claude-sonnet-4-20250514"
tag = "anthropic"

# cheap but slower
[[endpoint_defaults]]
model = "openrouter/openai/gpt-oss-120b"
tag = "openai"
"#;
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("providers.toml");
        std::fs::write(&path, original).expect("write");

        // When loading, re-pinning only the first model, and saving.
        let mut config = load_config_from(&path).expect("load");
        config.endpoint_defaults[0].tag = "anthropic-us".to_owned();
        save_config_to(&config, &path).expect("save");
        let written = std::fs::read_to_string(&path).expect("read");

        // Then the edited row's value changed in place, and both comments survive.
        assert!(written.contains("tag = \"anthropic-us\""), "tag updated");
        assert!(written.contains("# prefix cache matters most here"));
        assert!(written.contains("# cheap but slower"));
        assert!(written.contains("tag = \"openai\""), "other row untouched");
    }

    #[rstest::rstest]
    fn save_config_removing_the_last_endpoint_default_leaves_an_empty_array() {
        // Given a providers.toml with a single [[endpoint_defaults]] row.
        let original = r#"[providers.openrouter]
backend = "openrouter"
models = ["openai/gpt-oss-120b"]

[[endpoint_defaults]]
model = "openrouter/openai/gpt-oss-120b"
tag = "openai"
"#;
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("providers.toml");
        std::fs::write(&path, original).expect("write");

        // When loading, clearing the row (the auto-route sentinel), and saving.
        let mut config = load_config_from(&path).expect("load");
        config.endpoint_defaults.clear();
        save_config_to(&config, &path).expect("save");
        let written = std::fs::read_to_string(&path).expect("read");

        // Then the key is present and empty rather than vanished, so a second
        // save does not oscillate between absent and re-added.
        assert!(
            written.contains("endpoint_defaults= []") || written.contains("endpoint_defaults = []"),
            "expected an inline empty array, got:\n{written}"
        );
    }

    #[rstest::rstest]
    fn save_config_over_template_with_endpoint_defaults_writes_past_the_commented_example() {
        // Given the shipped template, whose endpoint_defaults example is commented out.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("providers.toml");
        std::fs::write(&path, DEFAULT_CONFIG).expect("write template");

        // When loading, pinning an endpoint, and saving.
        let mut config = load_config_from(&path).expect("load");
        config.endpoint_defaults.push(EndpointDefault {
            model: "openrouter/anthropic/claude-sonnet-4-20250514".to_owned(),
            tag: "anthropic".to_owned(),
        });
        save_config_to(&config, &path).expect("save");
        let reloaded = load_config_from(&path).expect("reload");

        // Then the pin is live in the file, and the template's own comments are intact.
        assert_eq!(reloaded.endpoint_defaults.len(), 1);
        assert_eq!(reloaded.endpoint_defaults[0].tag, "anthropic");
        let written = std::fs::read_to_string(&path).expect("read");
        for line in DEFAULT_CONFIG.lines().filter(|l| l.starts_with('#')) {
            assert!(
                written.contains(line),
                "expected template comment preserved: {line}"
            );
        }
    }
}
