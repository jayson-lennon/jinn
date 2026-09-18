//! Resolve the Discord `/new` input requirements for the configured lifecycle.
//!
//! The Discord bot drives a lifecycle script selected interactively by the
//! user during `/new` (chosen from the `[[session_lifecycle]]` list, not from
//! any `[discord]` config field). Each lifecycle's `setup_command` declares
//! its positional parameters (`$1`, `<name>`, `$@`). This module is the
//! frontend-agnostic bridge between that template and the bot's interactive
//! collection loop: given the user's `[[session_lifecycle]]` list and the
//! chosen lifecycle name, it reports how many args the bot must collect and
//! the prompt text to show.
//!
//! It reuses [`CommandTemplate`] from `jinn-domain` so the Discord prompt and
//! arg-count semantics match the TUI path exactly.

use jinn_domain::feat::session_lifecycle::command_template::{CommandTemplate, Param};
use jinn_preferences_config::schemas::LifecycleCommand;
use jinn_preferences_config::schemas::SessionLifecycle;

/// The resolved input requirements for a lifecycle's `setup_command`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleInputSpec {
    /// Number of non-splat positional slots the bot must collect.
    /// Splat-only (`$@`) and zero-param lifecycles report `0`.
    pub param_count: usize,
    /// Prompt text to display to the user. Empty when `param_count == 0`.
    /// Example: `Please enter: <branch> <target>`.
    pub prompt: String,
}

/// Resolve the param requirements and prompt text for the named lifecycle.
///
/// Looks up `lifecycle_name` in `lifecycles` and inspects its `setup`
/// command:
/// - Missing lifecycle → `None` (caller reports an error).
/// - Missing setup, or a `Builtin(_)` setup → `{ 0, "" }` (no prompt).
/// - Shell setup with no params → `{ 0, "" }`.
/// - Shell setup with params → `{ param_count, "Please enter: {display}" }`.
///
/// Splat (`$@`/`$*`) contributes 0 to `param_count`, mirroring the TUI
/// validator's `arg_count < param_count` check — splat accepts any number
/// (including zero) of trailing args.
#[must_use]
pub fn resolve_lifecycle_inputs(
    lifecycles: &[SessionLifecycle],
    lifecycle_name: &str,
) -> Option<LifecycleInputSpec> {
    let lifecycle = lifecycles.iter().find(|l| l.name == lifecycle_name)?;

    // A lifecycle with no setup, a builtin setup, or a zero-param shell
    // setup all require no Discord-side input.
    match &lifecycle.setup {
        None | Some(LifecycleCommand::Builtin(_)) => {}
        Some(LifecycleCommand::Shell(shell)) => {
            let template = CommandTemplate::parse(shell);
            let param_count = template.param_count();
            if param_count > 0 {
                return Some(LifecycleInputSpec {
                    param_count,
                    prompt: format!("Please enter: {}", prompt_tokens(&template)),
                });
            }
        }
    }
    Some(LifecycleInputSpec {
        param_count: 0,
        prompt: String::new(),
    })
}

/// Render every param (including splat) as a display token, space-joined.
///
/// Unlike [`CommandTemplate::display`] (which echoes the whole command string
/// including static text like `script.sh`), this yields only the param tokens —
/// `<branch>`, `<1>`, `<args>` — matching the approved acceptance criteria:
/// `Please enter: <1> <2>`.
fn prompt_tokens(template: &CommandTemplate) -> String {
    template
        .params()
        .iter()
        .map(|p| match p {
            Param::Named(name) => format!("<{name}>"),
            Param::Positional(n) => format!("<{n}>"),
            Param::Splat => "<args>".to_owned(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        reason = "test code"
    )]

    use super::*;
    use jinn_domain::feat::session_lifecycle::builtin::BuiltinId;
    use jinn_preferences_config::schemas::LifecycleCommand;
    use jinn_preferences_config::schemas::SessionLifecycle;

    fn shell_lifecycle(name: &str, setup: &str) -> SessionLifecycle {
        SessionLifecycle {
            name: name.to_owned(),
            description: None,
            setup: Some(LifecycleCommand::Shell(setup.to_owned())),
            teardown: None,
        }
    }

    #[rstest::rstest]
    #[test]
    fn numeric_params_produce_count_and_prompt() {
        // Given a lifecycle with two numeric positional params.
        let lifecycles = vec![shell_lifecycle("test", "script.sh $1 $2")];

        // When resolving inputs.
        let spec = resolve_lifecycle_inputs(&lifecycles, "test").expect("found");

        // Then the count is 2 and the prompt renders the display form.
        assert_eq!(spec.param_count, 2);
        assert_eq!(spec.prompt, "Please enter: <1> <2>");
    }

    #[rstest::rstest]
    #[test]
    fn named_params_produce_count_and_prompt() {
        // Given a lifecycle with two named params.
        let lifecycles = vec![shell_lifecycle("test", "script.sh <branch> <target>")];

        // When resolving inputs.
        let spec = resolve_lifecycle_inputs(&lifecycles, "test").expect("found");

        // Then the count is 2 and the prompt preserves the names.
        assert_eq!(spec.param_count, 2);
        assert_eq!(spec.prompt, "Please enter: <branch> <target>");
    }

    #[rstest::rstest]
    #[test]
    fn three_named_params_produce_three_count() {
        // Given a lifecycle with three named params.
        let lifecycles = vec![shell_lifecycle(
            "test",
            "script.sh <branch> <target> <profile>",
        )];

        // When resolving inputs.
        let spec = resolve_lifecycle_inputs(&lifecycles, "test").expect("found");

        // Then the count is 3 and all three names appear in the prompt.
        assert_eq!(spec.param_count, 3);
        assert_eq!(spec.prompt, "Please enter: <branch> <target> <profile>");
    }

    #[rstest::rstest]
    #[test]
    fn no_params_lifecycle_skips_prompt() {
        // Given a lifecycle with a zero-param setup command.
        let lifecycles = vec![shell_lifecycle("test", "setup.sh")];

        // When resolving inputs.
        let spec = resolve_lifecycle_inputs(&lifecycles, "test").expect("found");

        // Then the count is 0 and the prompt is empty (bot skips collection).
        assert_eq!(spec.param_count, 0);
        assert_eq!(spec.prompt, "");
    }

    #[rstest::rstest]
    #[test]
    fn single_param_lifecycle_produces_count_and_prompt() {
        // Given a lifecycle with one named positional param.
        let lifecycles = vec![shell_lifecycle("test", "script.sh <env>")];

        // When resolving inputs.
        let spec = resolve_lifecycle_inputs(&lifecycles, "test").expect("found");

        // Then the count is 1 and the prompt shows the single param.
        assert_eq!(spec.param_count, 1);
        assert_eq!(spec.prompt, "Please enter: <env>");
    }

    #[rstest::rstest]
    #[test]
    fn splat_only_lifecycle_counts_as_zero() {
        // Given a lifecycle whose only param is the splat.
        let lifecycles = vec![shell_lifecycle("test", "splat.sh $@")];

        // When resolving inputs.
        let spec = resolve_lifecycle_inputs(&lifecycles, "test").expect("found");

        // Then the count is 0 (splat excluded) and no prompt is shown.
        assert_eq!(spec.param_count, 0);
        assert_eq!(spec.prompt, "");
    }

    #[rstest::rstest]
    #[test]
    fn missing_lifecycle_returns_none() {
        // Given a lifecycle list that does not contain the requested name.
        let lifecycles = vec![shell_lifecycle("other", "script.sh $1")];

        // When resolving inputs for a name not in the list.
        let spec = resolve_lifecycle_inputs(&lifecycles, "ghost");

        // Then None is returned (caller reports the missing lifecycle).
        assert!(spec.is_none());
    }

    #[rstest::rstest]
    #[test]
    fn builtin_lifecycle_counts_as_zero() {
        // Given a lifecycle whose setup is a compiled-in builtin handler.
        let lifecycles = vec![SessionLifecycle {
            name: "bench".to_owned(),
            description: None,
            setup: Some(LifecycleCommand::Builtin(BuiltinId(
                "hello-world".to_owned(),
            ))),
            teardown: None,
        }];

        // When resolving inputs.
        let spec = resolve_lifecycle_inputs(&lifecycles, "bench").expect("found");

        // Then the count is 0 and no prompt is shown (builtins take no args
        // from the Discord collection path).
        assert_eq!(spec.param_count, 0);
        assert_eq!(spec.prompt, "");
    }

    #[rstest::rstest]
    #[test]
    fn missing_setup_counts_as_zero() {
        // Given a lifecycle with no setup command at all.
        let lifecycles = vec![SessionLifecycle {
            name: "blank".to_owned(),
            description: None,
            setup: None,
            teardown: None,
        }];

        // Then the count is 0 and the prompt is empty: the lifecycle exists,
        // so we proceed with no args rather than erroring as if it were missing.
        let spec = resolve_lifecycle_inputs(&lifecycles, "blank").expect("found");
        assert_eq!(spec.param_count, 0);
        assert_eq!(spec.prompt, "");
    }

    #[rstest::rstest]
    #[test]
    fn mixed_positional_and_splat_counts_non_splat_only() {
        // Given a lifecycle with one positional param and a splat.
        let lifecycles = vec![shell_lifecycle("test", "script.sh <branch> $@")];

        // When resolving inputs.
        let spec = resolve_lifecycle_inputs(&lifecycles, "test").expect("found");

        // Then the count is 1 (splat excluded) and the prompt shows the named
        // param followed by the splat display token.
        assert_eq!(spec.param_count, 1);
        assert_eq!(spec.prompt, "Please enter: <branch> <args>");
    }
}
