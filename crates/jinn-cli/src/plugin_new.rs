//! `jinn plugin new` — scaffold a new wasm plugin cargo project.
//!
//! Writes a minimal but complete plugin into `<name>/` in the current
//! directory: a `Cargo.toml` pinned to the SDK, a `main.rs` performing the
//! wire handshake and one placeholder contribution, and a `.gitignore`.
//! The scaffold compiles as-is; building it with
//! `cargo build --target wasm32-wasip2 --release` produces the `.wasm`
//! payload `jinn plugin install` consumes.

use std::path::{Path, PathBuf};

use error_stack::{Report, ResultExt as _};
use wherror::Error;

/// The scaffold failed to write.
#[derive(Debug, Error, PartialEq, Eq)]
#[error(debug)]
pub enum PluginNewError {
    /// The target directory already exists.
    Exists,
    /// A file could not be written.
    Write,
    /// The name is not a valid crate name.
    InvalidName,
    /// The `--sdk` flag named a path that does not exist.
    InvalidSdkPath,
}

/// Where the scaffolded plugin's SDK dependencies come from.
///
/// Defaults to the jinn git repo (published plugins); a local checkout path
/// is for developing plugins against uncommitted local SDK changes, and a
/// git URL with optional `@rev` pins a specific commit/branch/tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SdkSource {
    /// `https://github.com/jayson-lennon/jinn` (the default).
    DefaultGit,
    /// An explicit git URL, with an optional pinned revision.
    Git { url: String, rev: Option<String> },
    /// A local jinn checkout root (the dir containing `crates/`).
    Path(PathBuf),
}

/// Parses a `--sdk` value into an [`SdkSource`].
///
/// Accepted shapes: a git URL (`https://…`, `git://…`, `ssh:`), optionally
/// suffixed `@<rev>`; any other string is treated as a filesystem path,
/// which must exist.
///
/// # Errors
///
/// Returns [`PluginNewError::InvalidSdkPath`] when treated as a path but
/// missing on disk.
pub fn parse_sdk(value: &str) -> Result<SdkSource, Report<PluginNewError>> {
    let is_url = ["https://", "http://", "git://", "ssh://", "git@"]
        .iter()
        .any(|prefix| value.starts_with(prefix));
    if is_url {
        // `@` separates a pinned rev only when it appears in the final path
        // segment — `git@host:repo` has its `@` before any `/`, so it is
        // part of the user, not a rev suffix.
        let rev = value
            .rsplit_once('/')
            .and_then(|(_, tail)| tail.rsplit_once('@'))
            .map(|(_, rev)| rev.to_owned());
        let url = rev
            .as_deref()
            .and_then(|r| value.strip_suffix(r))
            .and_then(|u| u.strip_suffix('@'))
            .unwrap_or(value)
            .to_owned();
        return Ok(SdkSource::Git { url, rev });
    }
    let path = PathBuf::from(value);
    if path.is_dir() {
        Ok(SdkSource::Path(path))
    } else {
        Err(Report::new(PluginNewError::InvalidSdkPath).attach(value.to_owned()))
    }
}

/// The two SDK dependency lines for the given source.
fn sdk_dep_lines(source: &SdkSource, manifest_dir: &Path) -> String {
    match source {
        SdkSource::DefaultGit => [
            r#"jinn-plugin-api = { git = "https://github.com/jayson-lennon/jinn" }"#,
            r#"jinn-plugin-sdk = { git = "https://github.com/jayson-lennon/jinn" }"#,
        ]
        .join("\n"),
        SdkSource::Git { url, rev } => {
            let rev_line = rev
                .as_ref()
                .map_or(String::new(), |r| format!(", rev = \"{r}\""));
            [
                format!(r#"jinn-plugin-api = {{ git = "{url}"{rev_line} }}"#),
                format!(r#"jinn-plugin-sdk = {{ git = "{url}"{rev_line} }}"#),
            ]
            .join("\n")
        }
        SdkSource::Path(root) => {
            // Cargo resolves dependency paths relative to the manifest's dir.
            let api = path_dep_line(
                manifest_dir,
                "jinn-plugin-api",
                &root.join("crates/jinn-plugin-api"),
            );
            let sdk = path_dep_line(
                manifest_dir,
                "jinn-plugin-sdk",
                &root.join("crates/jinn-plugin-sdk"),
            );
            [api, sdk].join("\n")
        }
    }
}

/// One `name = { path = "..." }` line, relative to `from` when possible.
fn path_dep_line(from: &Path, dep: &str, crate_dir: &Path) -> String {
    let rel = crate_dir
        .strip_prefix(from)
        .unwrap_or(crate_dir)
        .to_path_buf();
    format!(r#"{dep} = {{ path = "{}" }}"#, rel.display())
}

/// Crate-name validation: lowercase ASCII alphanumerics and dashes.
fn valid_crate_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The `main.rs` template with the plugin name substituted.
fn main_rs(name: &str) -> String {
    format!(
        r#"//! {name} — a jinn wasm plugin.
//!
//! Wire contract: `Hello` → await `Welcome` → push contributions → exit.
//! The host keeps whatever you pushed cached after your process ends, so
//! a push-once plugin that exits is a complete, correct plugin.

use jinn_plugin_api::{{PluginToHost, SetPersonaEntries}};
use jinn_plugin_sdk::{{PluginOutput, hello, push, welcome}};

fn main() {{
    let mut out = PluginOutput::stdout();
    if hello(&mut out, "{name}").is_err() {{
        return;
    }}
    let Ok(grants) = welcome() else {{
        return;
    }};
    // `grants.read_dirs` are the directories the manifest granted you;
    // `grants.write_dirs` the writable ones ("<plugin_data_dir>:w" if
    // you declared it); `grants.http` the network flag.

    // TODO: replace this placeholder contribution with your plugin's data.
    let _ = &grants;
    let _ = push(
        &mut out,
        PluginToHost::SetPersonaEntries(SetPersonaEntries {{
            personas: vec![],
        }}),
    );
}}
"#
    )
}

/// The `Cargo.toml` template. `{sdk_deps}` is replaced with the two SDK
/// dependency lines for the chosen source (git default, git+rev, or path).
const CARGO_TOML: &str = r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2024"

# Plugin manifest: `jinn plugin build` embeds it into the artifact and
# `plugin add`/`install` applies it. Grants use <config_dir>/<data_dir>
# variables; `:w` marks a grant writable.
[package.metadata.jinn]
grants = []
# grants = ["<config_dir>/personas"]
http = false

[dependencies]
{sdk_deps}
serde_json = "1"

[[bin]]
name = "{name}"
path = "src/main.rs"
"#;

/// The `.gitignore` template.
const GITIGNORE: &str = "/target\n";

/// The post-scaffold instructions.
const INSTRUCTIONS: &str = r#"Next steps:

  cd {name}
  rustup target add wasm32-wasip2        # once per toolchain
  jinn plugin add .                     # build + install in one command
  # restart jinn — plugins activate at startup

{sdk_note}
Declare grants in [package.metadata.jinn] — they are embedded into the
artifact and applied at install (--grant overrides them per install).
Edit src/main.rs to push real data over the wire.
"#;

/// The dependency note printed after scaffolding, per SDK source.
fn sdk_note(source: &SdkSource) -> String {
    match source {
        SdkSource::DefaultGit => {
            "Dependencies resolve from the jinn git repo (first build clones it).".to_owned()
        }
        SdkSource::Git { url, rev } => match rev {
            Some(rev) => format!("Dependencies resolve from {url} pinned at rev {rev}."),
            None => format!("Dependencies resolve from {url} (first build clones it)."),
        },
        SdkSource::Path(root) => format!(
            "Dependencies resolve from your local checkout: {}. SDK changes rebuild on the next build.",
            root.display()
        ),
    }
}

/// Scaffolds the plugin project at `base/<name>`.
///
/// # Errors
///
/// Returns an error if the name is invalid, the directory exists, or any
/// file write fails.
pub fn scaffold(
    base: &Path,
    name: &str,
    sdk: &SdkSource,
) -> Result<std::path::PathBuf, Report<PluginNewError>> {
    if !valid_crate_name(name) {
        return Err(Report::new(PluginNewError::InvalidName)
            .attach(format!("name: {name}"))
            .attach("use lowercase ascii, digits, and dashes"));
    }
    let dir = base.join(name);
    if dir.exists() {
        return Err(Report::new(PluginNewError::Exists).attach(dir.to_string_lossy().to_string()));
    }
    let src = dir.join("src");
    std::fs::create_dir_all(&src)
        .change_context(PluginNewError::Write)
        .attach(dir.to_string_lossy().to_string())?;

    for (path, contents) in [
        (
            dir.join("Cargo.toml"),
            CARGO_TOML
                .replace("{name}", name)
                .replace("{sdk_deps}", &sdk_dep_lines(sdk, &dir)),
        ),
        (src.join("main.rs"), main_rs(name)),
        (dir.join(".gitignore"), GITIGNORE.to_owned()),
    ] {
        std::fs::write(&path, contents)
            .change_context(PluginNewError::Write)
            .attach(path.to_string_lossy().to_string())?;
    }
    Ok(dir)
}

#[expect(clippy::print_stdout, reason = "CLI user-facing output path")]
pub fn report_success(dir: &Path, name: &str, sdk: &SdkSource) {
    println!("Scaffolded plugin at {}", dir.display());
    print!(
        "{}",
        INSTRUCTIONS
            .replace("{name}", name)
            .replace("{sdk_note}", &sdk_note(sdk))
    );
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::print_stdout,
        clippy::print_stderr,
        reason = "test assertions"
    )]
    #![expect(clippy::let_underscore_must_use, reason = "none used")]

    use super::*;

    // Given a lowercase-dash name.
    // Then it validates.
    #[rstest::rstest]
    #[test]
    fn valid_name_accepted() {
        assert!(valid_crate_name("my-plugin"));
    }

    // Given a name with uppercase or underscores.
    // Then it is rejected.
    #[rstest::rstest]
    #[case("")]
    #[case("MyPlugin")]
    #[case("my_plugin")]
    #[case("my plugin")]
    fn invalid_names_rejected(#[case] name: &str) {
        assert!(!valid_crate_name(name));
    }

    // Given an empty base directory.
    // When scaffolding "probe".
    // Then the full project layout exists with the name substituted.
    #[rstest::rstest]
    #[test]
    fn scaffold_writes_project_layout() {
        // Given an empty temp base.
        let base = std::env::temp_dir().join(format!("jinn-plugin-new-{}", std::process::id()));
        std::fs::create_dir_all(&base).expect("base");

        // When scaffolding with the default git SDK source.
        let dir = scaffold(&base, "probe", &SdkSource::DefaultGit).expect("scaffold");

        // Then the layout exists.
        assert!(dir.join("Cargo.toml").is_file());
        assert!(dir.join("src/main.rs").is_file());
        assert!(dir.join(".gitignore").is_file());
        let manifest = std::fs::read_to_string(dir.join("Cargo.toml")).expect("read");
        assert!(manifest.contains("name = \"probe\""));
        // And the jinn manifest section is present (build gates on it).
        assert!(manifest.contains("[package.metadata.jinn]"));
        // And dependencies resolve from the jinn git repo, not crates.io.
        assert!(manifest.contains("git = \"https://github.com/jayson-lennon/jinn\""));
        let main = std::fs::read_to_string(dir.join("src/main.rs")).expect("read");
        assert!(main.contains("\"probe\""));

        let _ = std::fs::remove_dir_all(&base);
    }

    // Given an existing directory with the plugin's name.
    // When scaffolding.
    // Then it fails with Exists and writes nothing.
    #[rstest::rstest]
    #[test]
    fn scaffold_refuses_existing_directory() {
        // Given a base with an existing "probe" dir.
        let base = std::env::temp_dir().join(format!("jinn-plugin-new-e-{}", std::process::id()));
        std::fs::create_dir_all(base.join("probe")).expect("base");

        let result = scaffold(&base, "probe", &SdkSource::DefaultGit);

        // Then it fails with Exists.
        let Err(report) = result else {
            panic!("expected Exists");
        };
        assert_eq!(report.current_context(), &PluginNewError::Exists);

        let _ = std::fs::remove_dir_all(&base);
    }

    // Given an invalid name.
    // When scaffolding.
    // Then it fails with InvalidName.
    #[rstest::rstest]
    #[test]
    fn scaffold_rejects_invalid_name() {
        // Given any base.
        let base = std::env::temp_dir();

        let result = scaffold(&base, "Not A Name", &SdkSource::DefaultGit);

        // Then it fails with InvalidName.
        let Err(report) = result else {
            panic!("expected InvalidName");
        };
        assert_eq!(report.current_context(), &PluginNewError::InvalidName);
    }

    // Given an https URL with an @rev suffix.
    // Then it parses into Git with the rev split off.
    #[rstest::rstest]
    #[test]
    fn parse_sdk_https_url_with_rev() {
        // Given a pinned URL.
        let value = "https://github.com/jayson-lennon/jinn@v0.106.0";

        // When parsing.
        let source = parse_sdk(value).expect("parse");

        // Then it is Git with url + rev.
        assert_eq!(
            source,
            SdkSource::Git {
                url: "https://github.com/jayson-lennon/jinn".to_owned(),
                rev: Some("v0.106.0".to_owned()),
            }
        );
    }

    // Given an ssh URL whose only @ is in the user part.
    // Then the whole string stays the URL with no rev.
    #[rstest::rstest]
    #[test]
    fn parse_sdk_ssh_url_keeps_user() {
        // Given an ssh URL.
        let value = "git@github.com:jayson-lennon/jinn";

        // When parsing.
        let source = parse_sdk(value).expect("parse");

        // Then it is Git with no rev.
        assert_eq!(
            source,
            SdkSource::Git {
                url: value.to_owned(),
                rev: None,
            }
        );
    }

    // Given a path that does not exist.
    // Then parsing fails with InvalidSdkPath.
    #[rstest::rstest]
    #[test]
    fn parse_sdk_missing_path_rejected() {
        // Given a nonexistent dir.
        // When parsing.
        let result = parse_sdk("/nonexistent/jinn-checkout");

        // Then it fails with InvalidSdkPath.
        let Err(report) = result else {
            panic!("expected InvalidSdkPath");
        };
        assert_eq!(report.current_context(), &PluginNewError::InvalidSdkPath);
    }
}
