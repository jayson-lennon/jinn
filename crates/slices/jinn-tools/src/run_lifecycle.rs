//! The `run_lifecycle` tool — run a session lifecycle script's setup, or
//! replay a session's recorded teardown.
//!
//! The two actions read their coordinates from different places, and the
//! schema says so: `action` and `session_id` are the only required
//! arguments, and everything else is setup-only.
//!
//! **Setup** carries the script coordinates in the call: the user's
//! `jinn.toml` by name, or — when a `path` is given — that exact file, with
//! the name selecting the entry inside it (the sources never combine). The
//! script's named arguments are supplied in the call and shell-quoted
//! before substitution. Setup records the script name, the arguments in
//! the raw template's parameter order, and the source file on the session
//! *before* the run command publishes, and persists the session — those
//! coordinates are what teardown replays, and they must survive a restart.
//!
//! **Teardown** takes only the session id. Script, path, and argument
//! arguments are ignored — every one of them — and the command re-renders
//! from the session's stored coordinates, the same ones a close would
//! replay. A caller cannot teardown a session with coordinates the session
//! never recorded.
//!
//! Everything dangerous is borrowed, not reimplemented: argument values
//! render through `CommandTemplate`'s shell-quoted path (the same one the
//! arg-input popup feeds), execution goes through the lifecycle actor's
//! terminal-isolated runner, a setup that prints a directory still moves
//! the session there, and the actor enforces one lifecycle command per
//! session at a time — a second call for a busy session is refused with a
//! visible failure, not queued and not clobbered.

use std::collections::BTreeMap;

use error_stack::Report;

use crate::lifecycle_completion_listener_actor::{
    AwaitKind, LifecycleCompletion, LifecycleCompletionListenerActor,
    LifecycleCompletionListenerDeps,
};
use crate::tool_types::{ToolContext, tool_error};

use super::BoxedToolFuture;
use jinn_core_types::session_id::SessionId;
use jinn_core_types::tool_types::{ToolCall, ToolDefinition, ToolResult};
use jinn_kernel::common::state::State;
use jinn_preferences_config::schemas::{LifecycleCommand, SessionLifecycle};
use jinn_session_lifecycle_msg::LifecycleScriptState;
use jinn_session_lifecycle_msg::command_template::CommandTemplate;
use jinn_session_lifecycle_msg::command_template::Param;

/// Why a lifecycle script could not be resolved, read, or rendered.
///
/// Variants carry everything the model needs in their message: the tool
/// boundary renders a failure as tool-result prose, and a `Report`'s
/// `Display` prints only the outermost context.
#[derive(Debug, wherror::Error)]
pub enum LifecycleError {
    /// The configuration's lifecycle section could not be read.
    #[error("cannot read lifecycle scripts: {detail}")]
    ConfigRead {
        /// The reader's complaint.
        detail: String,
    },
    /// No script of this name is defined in the configuration.
    #[error("unknown lifecycle script \"{name}\"; known: {known}")]
    UnknownScript {
        /// The refused name.
        name: String,
        /// The configured script names, comma-joined.
        known: String,
    },
    /// The lifecycle file could not be read.
    #[error("cannot read {file}: {detail}")]
    FileRead {
        /// The resolved file path.
        file: String,
        /// The filesystem's complaint.
        detail: String,
    },
    /// The lifecycle file is not a valid lifecycle document.
    #[error("{file}: {detail}")]
    Malformed {
        /// The resolved file path.
        file: String,
        /// The parser's complaint.
        detail: String,
    },
    /// The file defines no script of this name.
    #[error("{file}: no lifecycle script named \"{name}\"; the file defines: {known}")]
    ScriptNotInFile {
        /// The resolved file path.
        file: String,
        /// The refused name.
        name: String,
        /// The names the file defines, comma-joined.
        known: String,
    },
    /// The requested half is a builtin command, which this tool does not run.
    #[error("lifecycle script \"{name}\" uses a builtin {action}, which this tool does not run")]
    BuiltinCommand {
        /// The script's name.
        name: String,
        /// The requested half: setup or teardown.
        action: String,
    },
    /// The script defines no command for the requested half.
    #[error("lifecycle script \"{name}\" defines no {action} command")]
    MissingCommand {
        /// The script's name.
        name: String,
        /// The requested half: setup or teardown.
        action: String,
    },
    /// A placeholder in the template has no supplied value.
    #[error("missing argument for placeholder <{name}>; the script names: {known}")]
    MissingPlaceholderArg {
        /// The placeholder's name.
        name: String,
        /// Every named placeholder, comma-joined.
        known: String,
    },
    /// The template uses a positional placeholder.
    #[error(
        "script uses positional placeholder ${n}; this tool supplies named arguments \
             keyed by placeholder name"
    )]
    PositionalParam {
        /// The positional index.
        n: usize,
    },
    /// The template uses a splat placeholder.
    #[error("script uses a splat placeholder; this tool supplies named arguments")]
    SplatParam,
    /// The template could not be rendered with the supplied values.
    #[error("cannot render lifecycle command: {detail}")]
    Render {
        /// The renderer's complaint.
        detail: String,
    },
    /// The tool call's arguments were malformed.
    #[error("malformed tool arguments: {detail}")]
    MalformedArguments {
        /// The parser's complaint.
        detail: String,
    },
    /// A map argument entry was not a string.
    #[error("argument \"{key}\" must be a string")]
    ArgumentNotAString {
        /// The offending key.
        key: String,
    },
    /// A required argument was missing.
    #[error("missing required argument: {key}")]
    MissingArgument {
        /// The missing key.
        key: String,
    },
}
use jinn_session_lifecycle_msg::{RunSessionSetup, RunSessionTeardown};

/// The `run_lifecycle` tool definition.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "run_lifecycle".to_owned(),
        description: "Run a session lifecycle script's setup, or replay a session's recorded \
             teardown. Setup names the script — from jinn.toml, or from the exact file a \
             path argument names — and supplies its arguments; the recorded coordinates \
             land on the session before the script runs, so teardown can replay them after \
             a restart. Teardown needs only the session id: it re-runs the teardown the \
             session's stored coordinates name, and any script/path/args supplied with it \
             are ignored. Setup adopts the last path the script prints as the session's \
             working directory. Teardown re-renders by placeholder name, so placeholder \
             order in the two commands is irrelevant. Values are shell-quoted before \
             substitution, so they cannot alter the command."
            .to_owned(),
        prompt_snippet: Some(
            "Run a lifecycle script: `run_lifecycle` with the action and session id — \
             for setup add the script name (and its file's path for a workflow script) \
             and its arguments; for teardown add nothing else, the session's recorded \
             coordinates drive it."
                .to_owned(),
        ),
        prompt_guidelines: vec![
            "Use `run_lifecycle` to give a session its scripted environment — a \
             worktree, a checkout — or to tear that environment down. Setup: supply \
             every argument the script's placeholders name; values are quoted for you; \
             the coordinates are recorded on the session. Teardown: pass only the \
             session id — the recorded coordinates replay, and supplied script/path/args \
             are ignored. A workflow's scripts live in its `lifecycle.toml`: pass that \
             path and the script's name."
                .to_owned(),
        ],
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["setup", "teardown"],
                    "description": "Which half to run: setup reads the script named \
                         below; teardown replays the session's recorded coordinates."
                },
                "session_id": {
                    "type": "string",
                    "description": "The session the script runs for."
                },
                "script": {
                    "type": "string",
                    "description": "Setup only. The lifecycle script's name — from \
                         jinn.toml, or from the file the path argument names. Ignored \
                         by teardown."
                },
                "path": {
                    "type": "string",
                    "description": "Setup only. Path to a lifecycle TOML file to read \
                         the script from instead of jinn.toml (e.g. a workflow's \
                         lifecycle.toml). Ignored by teardown."
                },
                "args": {
                    "type": "object",
                    "description": "Setup only. Named arguments for the script's \
                         <placeholder> parameters, keyed by placeholder name. Ignored \
                         by teardown, which replays the session's stored arguments.",
                    "additionalProperties": { "type": "string" }
                }
            },
            "required": ["action", "session_id"]
        }),
        server_tool_type: None,
    }
}

/// Executes `run_lifecycle`.
pub fn execute(call: ToolCall, ctx: ToolContext) -> BoxedToolFuture {
    Box::pin(async move {
        let Some(action) = string_argument(&call, "action") else {
            return tool_error(&call, "missing required argument: action");
        };
        let kind = match action.as_str() {
            "setup" => AwaitKind::Setup,
            "teardown" => AwaitKind::Teardown,
            other => {
                return tool_error(
                    &call,
                    &format!("unknown action \"{other}\"; expected setup or teardown"),
                );
            }
        };
        let Some(raw_id) = string_argument(&call, "session_id") else {
            return tool_error(&call, "missing required argument: session_id");
        };
        let Some(target_id) = SessionId::try_from_string(&raw_id) else {
            return tool_error(&call, &format!("unknown session: {raw_id}"));
        };
        let Some(state) = ctx.state.clone() else {
            return tool_error(&call, "run_lifecycle is unavailable without shared state");
        };
        let Some(bus) = ctx.bus.clone() else {
            return tool_error(&call, "run_lifecycle is unavailable without a bus");
        };
        let Some(system) = ctx.trouper_system.clone() else {
            return tool_error(
                &call,
                "run_lifecycle is unavailable without the actor system",
            );
        };

        // Per-action validation: the schema requires only action and
        // session_id; every other argument is validated by the action that
        // reads it. Teardown reads none of them.
        match kind {
            AwaitKind::Setup => run_setup(call, ctx, state, bus, system, target_id, action).await,
            AwaitKind::Teardown => {
                run_teardown(call, ctx, state, bus, system, target_id, action).await
            }
        }
    })
}

/// Runs the setup half: resolve the script, render, stamp the session's
/// coordinates, persist, then publish the run command.
///
/// Validation and resolution happen before any side effect: an unreadable
/// file, an unknown script, a missing argument, or a session in the wrong
/// lifecycle state must fail without stamping anything — a stamp onto a
/// session that already ran setup would corrupt the coordinates its
/// teardown will replay.
async fn run_setup(
    call: ToolCall,
    ctx: ToolContext,
    state: State,
    bus: jinn_kernel::common::services::bus_service::BusService,
    system: trouper::system::ActorSystem,
    target_id: SessionId,
    action: String,
) -> ToolResult {
    // The optional path must be read first: a blank script with a path
    // present is a different refusal than a blank script alone.
    let raw_path = string_argument(&call, "path").filter(|p| !p.trim().is_empty());
    let raw_script = string_argument(&call, "script");
    let script_name = match (&raw_script, &raw_path) {
        (Some(name), _) if !name.trim().is_empty() => name.clone(),
        (Some(_), Some(_)) => {
            return tool_error(
                &call,
                "script is required with path: the name selects the entry inside \
                 the file",
            );
        }
        // A present-but-blank name without a path is not a schema miss —
        // JSON `""` satisfies `required`-shaped expectations — so it keeps
        // the historic refusal: the config resolution reports the unknown
        // script "" with the known list.
        (Some(name), None) => name.clone(),
        (None, _) => return tool_error(&call, "setup requires the script argument"),
    };
    let args = match map_argument(&call, "args") {
        Ok(args) => args,
        Err(reason) => return tool_error(&call, &reason.to_string()),
    };

    // Resolve the script and render the command before touching state. The
    // two sources never combine: a path reads that file and never consults
    // the user's config. The source is recorded with the session so its
    // teardown re-reads the same place after a restart.
    let (raw_command, source_path) = match raw_path {
        Some(path) => match resolve_from_path(&path, &ctx, &script_name, &action, AwaitKind::Setup)
        {
            Ok(resolved) => resolved,
            Err(reason) => return tool_error(&call, &reason.to_string()),
        },
        None => match resolve_from_config(&ctx, &script_name, &action, AwaitKind::Setup) {
            Ok(resolved) => resolved,
            Err(reason) => return tool_error(&call, &reason.to_string()),
        },
    };
    let command = match render_command(&raw_command, &args) {
        Ok(rendered) => rendered,
        Err(reason) => return tool_error(&call, &reason.to_string()),
    };

    // State gate before the stamp: setup only from a session that has run
    // nothing yet, and the session must exist to carry the coordinates.
    {
        let guard = state.read();
        let Some(session) = guard.session.get(&target_id) else {
            return tool_error(&call, &format!("unknown session: {target_id}"));
        };
        let current = session.lifecycle_script_state();
        if current != LifecycleScriptState::NothingRan {
            return tool_error(
                &call,
                &format!(
                    "setup refused for session {target_id}: lifecycle state is {current}, \
                     setup requires nothing_ran"
                ),
            );
        }
    }

    // Stamp the coordinates the teardown will replay, ordered against the
    // RAW template's parameter order — the rendered string has no
    // placeholders left to order against — and persist so they survive a
    // restart. Both land before the run command publishes: the actor may
    // process the command before this call returns.
    let ordered = pair_args(&CommandTemplate::parse(&raw_command), &args);
    {
        let mut guard = state.write();
        if let Some(session) = guard.session.get_mut(&target_id) {
            session.set_lifecycle_name(Some(script_name.clone()));
            session.set_lifecycle_args(ordered.clone());
            session.set_lifecycle_source(source_path.clone());
        }
    }
    bus.publish(jinn_session_store_msg::PersistSession {
        session_id: target_id.clone(),
    })
    .await;

    // Listener before publication: no completion event can slip past
    // the subscription.
    let (tx, rx) = tokio::sync::oneshot::channel::<LifecycleCompletion>();
    LifecycleCompletionListenerActor::spawn(LifecycleCompletionListenerDeps {
        system: system.clone(),
        bus: bus.clone(),
        session_id: target_id.clone(),
        kind: AwaitKind::Setup,
        completion: tx,
    });

    // The actor honors the command we publish: it spawns the rendered
    // command terminal-isolated in the session's cwd and adopts the last
    // stdout line as the session's cwd.
    bus.publish(RunSessionSetup {
        session_id: target_id.clone(),
        command: command.clone(),
        args: ordered.values(),
        lifecycle_command: Some(LifecycleCommand::Shell(command.clone())),
    })
    .await;

    await_completion(call, rx, AwaitKind::Setup, &action, &script_name).await
}

/// Runs the teardown half: replay the session's recorded coordinates.
///
/// Script, path, and argument arguments are read from the call by nobody —
/// not validated, not consulted. The command re-resolves from the session's
/// recorded name and source and re-renders from its stored args, exactly
/// what a close replays; the published command names that replay so the
/// wire payload matches what will run. A session with nothing recorded, a
/// session that has not run setup, or a recorded lifecycle that no longer
/// resolves fails naming the session or source — fast, without spending
/// the wait budget on a silent no-op.
async fn run_teardown(
    call: ToolCall,
    ctx: ToolContext,
    state: State,
    bus: jinn_kernel::common::services::bus_service::BusService,
    system: trouper::system::ActorSystem,
    target_id: SessionId,
    action: String,
) -> ToolResult {
    // Replay the recorded coordinates. The stored coordinates are read and
    // never written: a teardown call cannot reshape what it replays.
    let (replayed, recorded_name) = {
        let guard = state.read();
        let Some(session) = guard.session.get(&target_id) else {
            return tool_error(&call, &format!("unknown session: {target_id}"));
        };
        let current = session.lifecycle_script_state();
        if current != LifecycleScriptState::SetupRan {
            return tool_error(
                &call,
                &format!(
                    "teardown refused for session {target_id}: lifecycle state is {current}, \
                     teardown requires setup_ran"
                ),
            );
        }
        match jinn_kernel::session_lifecycle::intent::build_run_session_teardown(
            &guard,
            &target_id,
            &ctx.config,
        ) {
            Some(Ok(replayed)) => {
                let name = session.lifecycle_name().map_or_default(str::to_owned);
                (replayed, name)
            }
            Some(Err(failure)) => {
                return tool_error(
                    &call,
                    &format!("teardown could not be resolved for session {target_id}: {failure}"),
                );
            }
            None => {
                return tool_error(
                    &call,
                    &format!(
                        "teardown refused for session {target_id}: no lifecycle script recorded"
                    ),
                );
            }
        }
    };

    // Listener before publication: no completion event can slip past
    // the subscription.
    let (tx, rx) = tokio::sync::oneshot::channel::<LifecycleCompletion>();
    LifecycleCompletionListenerActor::spawn(LifecycleCompletionListenerDeps {
        system: system.clone(),
        bus: bus.clone(),
        session_id: target_id.clone(),
        kind: AwaitKind::Teardown,
        completion: tx,
    });

    // The actor ignores the published command and args — it re-resolves
    // and re-renders from the session's stored coordinates, which this
    // call did not touch. The payload names the replay so the wire matches
    // what will run.
    bus.publish(RunSessionTeardown {
        session_id: target_id.clone(),
        command: replayed.command,
        args: replayed.args,
    })
    .await;

    await_completion(call, rx, AwaitKind::Teardown, &action, &recorded_name).await
}

/// Awaits the completion event, bounding the wait.
///
/// A script that hangs cannot hang the tool batch forever, but the script
/// keeps running past the timeout either way — the budget reports, it does
/// not kill.
async fn await_completion(
    call: ToolCall,
    rx: tokio::sync::oneshot::Receiver<LifecycleCompletion>,
    kind: AwaitKind,
    action: &str,
    name: &str,
) -> ToolResult {
    let completion = tokio::time::timeout(std::time::Duration::from_secs(300), rx).await;
    match completion {
        Ok(Ok(done)) => match done.error {
            None => {
                let where_to = match kind {
                    AwaitKind::Setup => done
                        .cwd
                        .map_or_default(|cwd| format!(" Session cwd: {cwd}.")),
                    AwaitKind::Teardown => String::new(),
                };
                succeeded(
                    call,
                    &format!("{action} for \"{name}\" completed.{where_to}"),
                )
            }
            Some(error) => failed(call, &format!("{action} for \"{name}\" failed: {error}")),
        },
        _ => failed(call, &wait_budget_message(action, name)),
    }
}

/// The message a wait-budget timeout reports: the command may still be
/// running — the budget bounds the tool's wait, not the script's life.
pub(crate) fn wait_budget_message(action: &str, name: &str) -> String {
    format!(
        "{action} for \"{name}\" did not report completion within the wait budget; \
         the command may still be running"
    )
}

/// Resolves the script's raw command from the user's jinn.toml by name.
///
/// The only reader of the config source: a call carrying a `path` never
/// reaches this function. The source is `None` — jinn.toml is the ambient
/// source, not one a session records.
fn resolve_from_config(
    ctx: &ToolContext,
    script_name: &str,
    action: &str,
    kind: AwaitKind,
) -> Result<(String, Option<String>), Report<LifecycleError>> {
    let scripts =
        ctx.config
            .get_list::<SessionLifecycle>()
            .map_err(|error| LifecycleError::ConfigRead {
                detail: error.to_string(),
            })?;
    let Some(script) = scripts.iter().find(|s| s.name == script_name) else {
        return Err(LifecycleError::UnknownScript {
            name: script_name.to_owned(),
            known: scripts
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        }
        .into());
    };
    half_of(script, script_name, action, kind).map(|raw| (raw.to_owned(), None))
}

/// Resolves the script's raw command from the file at `path`, resolved
/// against the calling session's cwd the way every file-facing tool
/// resolves a path argument. The resolved path is returned as the source —
/// the session records it so teardown re-reads the same file.
fn resolve_from_path(
    path: &str,
    ctx: &ToolContext,
    script_name: &str,
    action: &str,
    kind: AwaitKind,
) -> Result<(String, Option<String>), Report<LifecycleError>> {
    let resolved = crate::tool_paths::resolve_path(path, &ctx.cwd);
    let source = resolved.to_string();
    let text = std::fs::read_to_string(&resolved).map_err(|error| LifecycleError::FileRead {
        file: source.clone(),
        detail: error.to_string(),
    })?;
    let scripts =
        jinn_preferences_config::schemas::session_lifecycle::parse_lifecycle_document(&text)
            .map_err(|error| LifecycleError::Malformed {
                file: source.clone(),
                detail: error.clone(),
            })?;
    let Some(script) = scripts.iter().find(|s| s.name == script_name) else {
        return Err(LifecycleError::ScriptNotInFile {
            file: source,
            name: script_name.to_owned(),
            known: scripts
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        }
        .into());
    };
    half_of(script, script_name, action, kind).map(|raw| (raw.to_owned(), Some(source)))
}

/// Takes the requested half of a script as the raw template string,
/// refusing a builtin command — the tool runs shell, not builtins.
fn half_of<'a>(
    script: &'a SessionLifecycle,
    script_name: &str,
    action: &str,
    kind: AwaitKind,
) -> Result<&'a str, Report<LifecycleError>> {
    let half = match kind {
        AwaitKind::Setup => script.setup.as_ref(),
        AwaitKind::Teardown => script.teardown.as_ref(),
    };
    match half {
        Some(LifecycleCommand::Shell(cmd)) => Ok(cmd.as_str()),
        Some(LifecycleCommand::Builtin(_)) => Err(LifecycleError::BuiltinCommand {
            name: script_name.to_owned(),
            action: action.to_owned(),
        }
        .into()),
        None => Err(LifecycleError::MissingCommand {
            name: script_name.to_owned(),
            action: action.to_owned(),
        }
        .into()),
    }
}

/// Renders the command through a single scan-and-emit pass that shell-quotes
/// every substituted value.
///
/// This is [`CommandTemplate::render_named`] plus the tool's validation: the
/// one-pass scan (emitted output is never re-scanned, so a value carrying a
/// later placeholder's shape is inert) lives in the contract crate, and both
/// the tool's setup render and the teardown replay go through it.
///
/// # Errors
///
/// Returns an error when a template names a placeholder the supplied
/// arguments do not carry, or when the contract render fails.
pub(crate) fn render_command(
    raw: &str,
    args: &BTreeMap<String, String>,
) -> Result<String, Report<LifecycleError>> {
    let template = CommandTemplate::parse(raw);
    validate_named_params(&template, args)?;
    template.render_named(args).map_err(|error| {
        error_stack::Report::new(LifecycleError::Render {
            detail: error.to_string(),
        })
    })
}

/// Validates that every placeholder in `template` is covered by `args`, and
/// that the template uses only named placeholders.
///
/// Shared by the setup render and the pair stamping: both refuse a template
/// naming a placeholder the call does not supply, and both refuse positional
/// or splat placeholders the tool cannot fill.
fn validate_named_params(
    template: &CommandTemplate,
    args: &BTreeMap<String, String>,
) -> Result<(), Report<LifecycleError>> {
    for param in template.params() {
        match param {
            Param::Named(name) => {
                if !args.contains_key(name) {
                    return Err(LifecycleError::MissingPlaceholderArg {
                        name: name.clone(),
                        known: template
                            .params()
                            .iter()
                            .filter_map(|p| match p {
                                Param::Named(n) => Some(n.as_str()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join(", "),
                    }
                    .into());
                }
            }
            Param::Positional(n) => {
                return Err(LifecycleError::PositionalParam { n: *n }.into());
            }
            Param::Splat => {
                return Err(LifecycleError::SplatParam.into());
            }
        }
    }
    Ok(())
}

/// Pairs the named argument map into ordered name-value pairs in the
/// template's first-appearance placeholder order.
///
/// Call [`validate_named_params`] first: pairing assumes every named
/// placeholder has a supplied value.
pub(crate) fn pair_args(
    template: &CommandTemplate,
    args: &BTreeMap<String, String>,
) -> jinn_session_state::fields::LifecycleArgs {
    jinn_session_state::fields::LifecycleArgs::from_pairs(template.params().iter().filter_map(
        |p| match p {
            jinn_session_lifecycle_msg::command_template::Param::Named(name) => {
                args.get(name).map(|value| (name.clone(), value.clone()))
            }
            _ => None,
        },
    ))
}

/// Reads one string argument from the call's JSON arguments.
fn string_argument(call: &ToolCall, key: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(&call.arguments)
        .ok()?
        .get(key)?
        .as_str()
        .map(str::to_owned)
}

/// Reads one string-map argument from the call's JSON arguments.
fn map_argument(
    call: &ToolCall,
    key: &str,
) -> Result<BTreeMap<String, String>, Report<LifecycleError>> {
    let value: serde_json::Value = serde_json::from_str(&call.arguments).map_err(|error| {
        LifecycleError::MalformedArguments {
            detail: error.to_string(),
        }
    })?;
    match value.get(key) {
        Some(serde_json::Value::Object(map)) => {
            let mut args = BTreeMap::new();
            for (k, v) in map {
                match v.as_str() {
                    Some(s) => {
                        args.insert(k.clone(), s.to_owned());
                    }
                    None => {
                        return Err(LifecycleError::ArgumentNotAString { key: k.clone() }.into());
                    }
                }
            }
            Ok(args)
        }
        _ => Err(LifecycleError::MissingArgument {
            key: key.to_owned(),
        }
        .into()),
    }
}

/// Builds a successful result.
fn succeeded(call: ToolCall, content: &str) -> ToolResult {
    ToolResult {
        tool_call_id: call.id,
        name: call.name,
        content: content.to_owned(),
        success: true,
        full_content: None,
        truncation: None,
        pin_position: None,
        attachments: Vec::new(),
    }
}

/// Builds a failed result for a script that ran and reported failure.
fn failed(call: ToolCall, content: &str) -> ToolResult {
    ToolResult {
        tool_call_id: call.id,
        name: call.name,
        content: content.to_owned(),
        success: false,
        full_content: None,
        truncation: None,
        pin_position: None,
        attachments: Vec::new(),
    }
}
