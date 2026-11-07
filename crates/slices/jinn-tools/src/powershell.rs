//! PowerShell built-in tool — runs Windows-native commands.
//!
//! The sibling of the `bash` tool, for the work that has to happen in
//! PowerShell: Windows modules, the registry, the Windows filesystem, and
//! the tooling that ships with Windows rather than with a POSIX
//! environment.
//!
//! The tool is registered only on Windows. On Linux and macOS the model
//! never sees it: a tool it cannot run is worse than no tool, because the
//! model spends a turn discovering that. The *schema* is compiled on
//! every platform, so its shape is tested everywhere rather than only on
//! the one platform where the tool exists; the *execution* half — spawn,
//! streaming, teardown, result formatting — lives in `execute`, compiled
//! only on Windows.
//!
//! The PowerShell executable is resolved at launch, the same way the
//! POSIX shell is — see [`jinn_common::shell`]. A machine with no
//! PowerShell is a fact about the machine, not a launch failure: the tool
//! reports it to the model when the model reaches for it.

use jinn_core_types::tool_types::ToolDefinition;

#[cfg(windows)]
mod execute;

#[cfg(windows)]
pub use execute::execute;

/// The `powershell` tool's schema.
///
/// The description names the same timeout affordance the `bash` tool's
/// does, because the model reasons about both the same way.
#[must_use]
pub fn definition(default_timeout_secs: u64) -> ToolDefinition {
    let default_secs_str = default_timeout_secs.to_string();
    ToolDefinition {
        name: "powershell".to_owned(),
        description: format!(
            "Execute a PowerShell command in the current working directory. \
            Returns stdout and stderr. Output is truncated to last 2000 lines or 50KB \
            (whichever is hit first). \
            \
            TIMEOUT: the default is {default_secs_str}s. \
            Pass `max_duration_secs` as a tool-call argument to override — NOT the shell `timeout` command. \
            Example: {{\"command\": \"Get-ChildItem\", \"max_duration_secs\": 600}}. \
            Set `max_duration_secs: 0` to disable the timeout for that call."
        ),
        prompt_snippet: Some("Execute PowerShell commands (Get-ChildItem, Get-Content, etc.)".to_owned()),
        prompt_guidelines: vec![
            "Use this tool for Windows-native commands and PowerShell modules; use the bash tool for POSIX shell syntax (pipes, &&, $VAR).".to_owned(),
            "Proactively set max_duration_secs for commands that may run long (builds, tests, network requests, large compiles). Do NOT use the shell `timeout` command — pass max_duration_secs as a tool-call argument.".to_owned(),
        ],
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "PowerShell command to execute"
                },
                "max_duration_secs": {
                    "type": "number",
                    "description": format!("Maximum duration in seconds before the command is killed. Overrides the default of {default_secs_str}s. Set to 0 to disable the timeout for this call. Pass as a tool-call argument — NOT the shell `timeout` command. Example: {{\"command\": \"Get-ChildItem\", \"max_duration_secs\": 600}}.")
                }
            },
            "required": ["command"]
        }),
        server_tool_type: None,
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
    use super::*;

    #[rstest::rstest]
    #[test]
    fn the_definition_names_the_powershell_tool() {
        // Given the tool's schema.
        // When reading its name.
        // Then it is `powershell`.
        assert_eq!(definition(300).name, "powershell");
    }

    #[rstest::rstest]
    #[test]
    fn the_definition_requires_a_command_parameter() {
        // Given the tool's schema.
        let definition = definition(300);

        // When reading its parameters.
        // Then the command is required and the timeout is offered.
        let required = definition
            .parameters
            .get("required")
            .and_then(|value| value.as_array())
            .expect("the schema names its required parameters");
        assert_eq!(
            required,
            &vec![serde_json::Value::String("command".to_owned())]
        );
        assert!(
            definition
                .parameters
                .get("properties")
                .and_then(|value| value.get("max_duration_secs"))
                .is_some(),
            "the schema must offer max_duration_secs"
        );
    }

    #[rstest::rstest]
    #[test]
    fn the_command_runs_with_no_profile_and_non_interactively() {
        // Given the argument list the tool spawns PowerShell with.
        // When reading it.
        // Then the profile is skipped and the shell never prompts.
        let args = jinn_common::shell::powershell_command_args("Get-ChildItem");
        assert_eq!(
            &args[..2],
            &["-NoProfile".to_owned(), "-NonInteractive".to_owned()]
        );
    }
}
