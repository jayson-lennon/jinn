//! The `[browser]` section — the browser daemon's Chrome binary.
//!
//! One key: `binary`, an absolute path to the browser executable. Unset
//! (the shipped state) means auto-detect: PATH names first, then
//! platform install paths, branded Google Chrome preferred, Chromium
//! fallback. A configured path must be absolute — a relative value is a
//! section-validation error at launch — and must exist on disk, checked
//! at daemon start: an unresolvable configured path refuses the daemon
//! start with a visible error naming the configured path (the
//! `[shell]` refusal pattern, scoped to the daemon because the browser
//! is optional, not essential to a jinn launch).
//!
//! The daemon resolves this section once, at its own startup, no matter
//! which client spawned it (the bundled MCP server, a luau script, or
//! `jinn browser start`) — a config edit applies at the next daemon
//! start (`jinn browser restart`).
//!
//! The process that reads it lives in `jinn-browser`'s daemon. This
//! module is data plus the path grammar — no daemon, no I/O.

use error_stack::Report;
use serde::{Deserialize, Serialize};
use wherror::Error;

/// Why a configured `[browser]` value cannot be used.
#[derive(Debug, Error)]
#[error(debug)]
pub struct BrowserBinaryError;

/// `[browser]` preferences.
///
/// Serialized as `[browser]` in `jinn.toml`. The single field names the
/// browser executable the daemon launches; when absent the daemon
/// auto-detects.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserConfig {
    /// Absolute path to the browser executable the daemon launches
    /// (e.g. `"/usr/bin/google-chrome-stable"`). Unset = auto-detect:
    /// PATH first, then platform install paths; branded Google Chrome
    /// preferred, Chromium fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary: Option<String>,
}

impl jinn_config::Configurable for BrowserConfig {
    const KEY: &'static str = "browser";
}

impl BrowserConfig {
    /// Validates the configured binary as an absolute path.
    ///
    /// An unset value is valid — it selects auto-detection. A set value
    /// must be an absolute path (the `[shell]` pattern): a relative path
    /// would resolve against whatever directory the daemon happened to
    /// start from, making the launch nondeterministic.
    ///
    /// # Errors
    ///
    /// Returns [`BrowserBinaryError`] when a configured value is a
    /// relative path. Existence on disk is *not* checked here — the file
    /// may appear or disappear between launches; the daemon checks it at
    /// start.
    pub fn validate(&self) -> Result<(), Report<BrowserBinaryError>> {
        match &self.binary {
            None => Ok(()),
            Some(path) => {
                if std::path::Path::new(path).is_absolute() {
                    Ok(())
                } else {
                    Err(error_stack::Report::new(BrowserBinaryError)
                        .attach(format!(
                            "[browser] binary is not an absolute path: {path:?}"
                        ))
                        .attach("set an absolute path, e.g. /usr/bin/google-chrome-stable"))
                }
            }
        }
    }

    /// The configured value when valid.
    ///
    /// # Errors
    ///
    /// Returns [`BrowserBinaryError`] through [`Self::validate`] when a
    /// configured value is not absolute.
    pub fn resolved_binary(&self) -> Result<Option<&str>, Report<BrowserBinaryError>> {
        self.validate()?;
        Ok(self.binary.as_deref())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test assertions need expect")]

    use super::BrowserConfig;
    use serde::Deserialize;

    fn from_table(raw: &str) -> BrowserConfig {
        #[derive(Deserialize)]
        struct Wrapper {
            browser: BrowserConfig,
        }
        toml::from_str::<Wrapper>(raw)
            .expect("browser table parses")
            .browser
    }

    #[rstest::rstest]
    #[test]
    fn absent_section_deserializes_to_auto_detect() {
        // Given an empty `[browser]` table.
        // When deserializing.
        let config = from_table("[browser]\n");

        // Then no binary is configured — the daemon auto-detects.
        assert_eq!(config.binary, None);
        assert!(config.validate().is_ok());
    }

    #[rstest::rstest]
    #[test]
    fn configured_absolute_path_is_preserved_and_valid() {
        #[cfg(target_os = "windows")]
        let raw = r"[browser]
binary = 'C:\Program Files\Google\Chrome\Application\chrome.exe'
";
        #[cfg(not(target_os = "windows"))]
        let raw = "[browser]\nbinary = \"/usr/bin/google-chrome-stable\"\n";

        // When deserializing and validating.
        let config = from_table(raw);
        let result = config.validate();

        // Then the value survives round-trip and validates.
        assert_eq!(
            config.binary.as_deref(),
            if cfg!(target_os = "windows") {
                Some(r"C:\Program Files\Google\Chrome\Application\chrome.exe")
            } else {
                Some("/usr/bin/google-chrome-stable")
            }
        );
        assert!(result.is_ok());
    }

    #[rstest::rstest]
    #[test]
    fn a_relative_configured_path_is_a_validation_error() {
        // Given a table with a relative binary path.
        let config = from_table("[browser]\nbinary = \"google-chrome\"\n");

        // When validating.
        let result = config.validate();

        // Then validation fails naming the problem — a relative path
        // would resolve against the daemon's launch directory.
        let error = result.expect_err("a relative path must be rejected");
        let text = format!("{error:?}");
        assert!(text.contains("absolute"), "got: {text}");
        assert!(text.contains("google-chrome"), "got: {text}");
    }

    #[rstest::rstest]
    #[test]
    fn existence_is_not_a_schema_layer_concern() {
        // Given a table pointing at a path that does not exist on this
        // machine.
        let nonexistent = if cfg!(target_os = "windows") {
            r"C:\definitely\not\here\chrome.exe"
        } else {
            "/definitely/not/here/chrome"
        };
        let config = from_table(&format!("[browser]\nbinary = {nonexistent:?}\n"));

        // When validating.
        let result = config.validate();

        // Then the schema layer accepts it — existence is checked at
        // daemon start, where the refusal can name the configured path
        // to the user. A file may also appear or disappear between
        // launches.
        assert!(result.is_ok());
    }

    #[rstest::rstest]
    #[test]
    fn serializing_the_default_omits_the_binary_key() {
        #[derive(Deserialize)]
        struct Repr {
            browser: BrowserConfig,
        }

        // Given the default config.
        let config = BrowserConfig::default();

        // When serializing then re-parsing.
        let s = toml::to_string(&config).expect("serialize");
        let reparsed: BrowserConfig = toml::from_str::<Repr>(&format!("[browser]\n{s}"))
            .expect("reparse")
            .browser;

        // Then it equals the original, and the empty table carried no
        // binary key (auto-detect stays the shipped behavior).
        assert_eq!(config, reparsed);
        assert!(!s.contains("binary"));
    }
}
