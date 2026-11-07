//! The `[shell]` section — the executables jinn runs commands through.
//!
//! Both keys name an absolute path to an executable. A key that is set
//! selects that executable; a key that is absent leaves the choice to
//! jinn's own detection. A set path that names no file is a launch-time
//! error, never a silent fall-through: on Windows the detection chain's
//! worst case is the WSL launcher, which would build Linux binaries
//! inside a VM.
//!
//! Paths are written with **forward slashes**. A backslash in a TOML
//! basic string is an escape character, so `"C:\Program Files\…"` does
//! not parse — and a parse failure takes the whole file with it.

use serde::{Deserialize, Serialize};

/// The `[shell]` section of `jinn.toml`.
///
/// # Behavior: Event-Dependent.
///
/// The resolved shells are an artifact: resolved at launch, and a bus
/// handler re-resolves and swaps them whole when the reload event lands.
/// A resolution that fails keeps the previous one — a running jinn never
/// loses a working shell to one bad edit.
///
/// Serialized as `[shell]`. Both fields are optional: an absent section
/// reads as both-absent, which leaves every executable to jinn's
/// detection.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellConfig {
    /// Absolute path to the POSIX shell jinn runs commands through.
    ///
    /// On Windows this must be a Git Bash (or MSYS2/Cygwin) `bash.exe`.
    /// There is deliberately no WSL fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bash_path: Option<String>,

    /// Absolute path to the PowerShell executable the `powershell` tool
    /// runs through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub powershell_path: Option<String>,
}

impl jinn_config::Configurable for ShellConfig {
    const KEY: &'static str = "shell";
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test code")]

    use jinn_config::Configurable;

    use super::ShellConfig;

    #[rstest::rstest]
    #[test]
    fn an_absent_section_reads_as_both_paths_unset() {
        // Given a layer with no `[shell]` table.
        let config = jinn_config::testutil::config_layer("");

        // When reading the section.
        let section = config.get::<ShellConfig>().expect("section reads");

        // Then both paths are unset, leaving the choice to detection.
        assert_eq!(section.bash_path, None);
        assert_eq!(section.powershell_path, None);
    }

    #[rstest::rstest]
    #[test]
    fn a_section_reads_both_configured_paths() {
        // Given a layer carrying both paths.
        let config = jinn_config::testutil::config_layer(
            "[shell]\n\
             bash_path = \"C:/Program Files/Git/bin/bash.exe\"\n\
             powershell_path = \"C:/Program Files/PowerShell/7/pwsh.exe\"",
        );

        // When reading the section.
        let section = config.get::<ShellConfig>().expect("section reads");

        // Then both paths come from the document.
        assert_eq!(
            section.bash_path.as_deref(),
            Some("C:/Program Files/Git/bin/bash.exe")
        );
        assert_eq!(
            section.powershell_path.as_deref(),
            Some("C:/Program Files/PowerShell/7/pwsh.exe")
        );
    }

    #[rstest::rstest]
    #[test]
    fn a_section_reading_only_the_bash_path_leaves_powershell_unset() {
        // Given a layer carrying only the bash path.
        let config = jinn_config::testutil::config_layer("[shell]\nbash_path = \"/bin/bash\"");

        // When reading the section.
        let section = config.get::<ShellConfig>().expect("section reads");

        // Then the unset key stays unset.
        assert_eq!(section.bash_path.as_deref(), Some("/bin/bash"));
        assert_eq!(section.powershell_path, None);
    }

    #[rstest::rstest]
    #[test]
    fn the_section_owns_the_shell_key() {
        // Given the section type.
        // When reading its key.
        // Then it is the `[shell]` table.
        assert_eq!(ShellConfig::KEY, "shell");
    }
}
