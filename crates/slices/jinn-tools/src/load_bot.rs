//! The `load_bot` tool — attach a bot as a child of the calling session,
//! loading it from a workflow-local TOML file or from the call itself.
//!
//! Two load methods feed one path. A workflow loads bots that belong to
//! the *workflow*, not to the user's jinn.toml: a run's bots ship as TOML
//! files beside the workflow's prompt files, and starting the workflow is
//! what brings them alive — `path` loads one such file. An agent with no
//! file hands the entry's TOML text to the call directly — `toml` — and
//! the bot is stamped `inline`.
//!
//! Either way the bot is built through the same path the saved-bot picker
//! uses, attached as a child of the loading session, and stamped with its
//! source (its file path, or `inline`).
//!
//! The stamp is the whole save story: a loaded bot refuses to be written
//! into jinn.toml (see the properties save path in jinn-bot), so a
//! workflow's tuning — and an ad-hoc bot — can never leak into the user's
//! personal configuration, and its source stays the only writer of its
//! configuration. The stamp is per-bot, not inherited: a bot a loaded bot
//! later creates is an ordinary personal one.

use crate::tool_types::{ToolContext, tool_error};

use super::BoxedToolFuture;
use jinn_bot::saved_create;
use jinn_core_types::BotSourcePath;
use jinn_core_types::tool_types::{ToolCall, ToolDefinition, ToolResult};
use jinn_preferences_config::schemas::BotEntryConfig;
use marked_path::{Absolute, MarkedPathBuf};

/// The `load_bot` tool definition.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "load_bot".to_owned(),
        description: "Attach a bot as a child of this session. The bot comes from a \
             workflow-local TOML file (path) or from the call's toml argument — \
             give exactly one of the two. Either form uses the same shape as an \
             [[bot.entry]] block in jinn.toml. The loaded bot belongs to its \
             source (that file, or this call): it cannot be saved to jinn.toml."
            .to_owned(),
        prompt_snippet: Some(
            "Attach a bot: `load_bot` with the bot file's path, or with the bot \
             entry's toml text itself."
                .to_owned(),
        ),
        prompt_guidelines: vec![
            "Use `load_bot` to bring up the bots a workflow needs, from the TOML \
             files that ship beside the workflow, or to attach an ad-hoc bot by \
             handing the entry's toml text to the call directly. Give exactly one \
             of `path` or `toml`. The result names the created bot's session id — \
             that is the id its messages are addressed to."
                .to_owned(),
        ],
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the bot TOML file. Relative paths resolve \
                         against this session's working directory."
                },
                "toml": {
                    "type": "string",
                    "description": "The bot entry's TOML text itself, instead of a file. \
                         Same shape as an [[bot.entry]] block in jinn.toml — a bare \
                         entry's keys, an `entry = [...]` array with one entry, or the \
                         full `[[bot.entry]]` document shape."
                },
                "set_name": {
                    "type": "string",
                    "description": "Optional replacement for the bot's name from the \
                         file — for labeling instances of one shared seed (e.g. \
                         \"implementor-002\"). Behavior and configuration are unaffected."
                }
            },
            "required": []
        }),
        server_tool_type: None,
    }
}

/// Executes `load_bot`.
///
/// Takes exactly one of `path` or `toml`; parses the text as a single
/// `[[bot.entry]]`-shaped document — either a bare entry's keys or a
/// document wrapping one `entry` array — then builds the bot under the
/// state read lock (the build needs the parent's *state*, not just its
/// id), stamps the source marker (the resolved file path, or `inline`),
/// inserts, and publishes the creation's messages through the bus in the
/// order the picker path uses.
pub fn execute(call: ToolCall, ctx: ToolContext) -> BoxedToolFuture {
    Box::pin(async move {
        let raw_path = string_argument(&call, "path");
        let raw_toml = string_argument(&call, "toml");
        let (source, entry) = match (raw_path, raw_toml) {
            (Some(_), Some(_)) => {
                return tool_error(&call, "give exactly one of `path` or `toml`, not both");
            }
            (None, None) => {
                return tool_error(
                    &call,
                    "missing required argument: give exactly one of `path` or `toml`",
                );
            }
            (Some(path), None) => match load_from_file(&path, &ctx) {
                Ok(ok) => ok,
                Err(message) => return tool_error(&call, &message),
            },
            (None, Some(text)) => match parse_bot_entry(&text) {
                Ok(entry) => ("inline".to_owned(), entry),
                Err(reason) => return tool_error(&call, &reason.to_string()),
            },
        };
        let set_name = string_argument(&call, "set_name");
        let Some(state) = ctx.state.clone() else {
            return tool_error(&call, "load_bot is unavailable without shared state");
        };
        let Some(bus) = ctx.bus.clone() else {
            return tool_error(&call, "load_bot is unavailable without a bus");
        };
        let Some(parent_id) = ctx.session_id.clone() else {
            return tool_error(&call, "load_bot is unavailable without a session context");
        };

        // A caller that is itself a bot cannot host one: the
        // creation path requires a non-bot parent.
        {
            let guard = state.read();
            let Some(caller) = guard.session.get(&parent_id) else {
                return tool_error(&call, "calling session is not live");
            };
            if caller.is_bot() {
                return tool_error(&call, "only a non-bot session can load a bot");
            }
        }

        // Build under the read lock — `build` snapshots the parent's state —
        // then stamp, insert, and hand the caller the child's id. An
        // explicit `set_name` relabels the instance after the build: the
        // seed file's name stays the shared identity; this instance's
        // display name becomes the caller's label.
        let display_name = set_name.unwrap_or_else(|| entry.name.clone());
        let (created, messages, bot_id) = {
            let guard = state.read();
            let Some(parent) = guard.session.get(&parent_id) else {
                return tool_error(&call, "calling session is not live");
            };
            let mut bot = saved_create::build(&entry, parent);
            bot.set_title(display_name.clone());
            bot.set_workflow_source(BotSourcePath::new(source.clone()));
            let messages = saved_create::creation_messages(&parent_id, &bot);
            let id = bot.session_id().clone();
            (bot, messages, id)
        };
        {
            let mut guard = state.write();
            guard.session.insert(created);
            if let Some(parent) = guard.session.get_mut(&parent_id) {
                parent.mark_interacted();
            }
        }
        for closure in messages.messages {
            closure(&bus);
        }

        succeeded(
            call,
            &format!(
                "Attached bot \"{}\" as {bot_id} (loaded from {source}).",
                display_name
            ),
        )
    })
}

/// Loads a bot entry from a file path.
///
/// Resolves a relative path against the calling session's cwd (the same
/// relative-base every file-facing tool honors), reads the file, and
/// parses exactly one entry from it. `Ok` carries the resolved path
/// string (the stamp's source) and the entry; `Err` carries the finished
/// error message, ready for [`tool_error`].
fn load_from_file(raw_path: &str, ctx: &ToolContext) -> Result<(String, BotEntryConfig), String> {
    let Some(parent_id) = ctx.session_id.as_ref() else {
        return Err("load_bot is unavailable without a session context".to_owned());
    };
    let Some(state) = ctx.state.as_ref() else {
        return Err("load_bot is unavailable without shared state".to_owned());
    };
    // Resolve the path against the session's cwd, the same relative-base
    // every file-facing tool honors.
    let cwd = {
        let guard = state.read();
        let Some(caller) = guard.session.get(parent_id) else {
            return Err("calling session is not live".to_owned());
        };
        caller.cwd().clone()
    };
    let path = {
        let candidate = std::path::Path::new(raw_path);
        if candidate.is_absolute() {
            MarkedPathBuf::<Absolute>::new(candidate).expect("checked absolute above")
        } else {
            cwd.join(candidate)
                .expect("absolute parent joins a bare candidate")
        }
    };
    let source = path.to_string();
    let entry =
        std::fs::read_to_string(&path).map_err(|error| format!("cannot read {source}: {error}"))?;
    let entry = parse_bot_entry(&entry).map_err(|reason| format!("{source}: {reason}"))?;
    Ok((source, entry))
}

/// Why a bot file could not be loaded as exactly one named entry.
///
/// Variants carry everything the model needs in their message: the tool
/// boundary renders a failure as tool-result prose, and a `Report`'s
/// `Display` prints only the outermost context.
#[derive(Debug, wherror::Error)]
pub enum BotParseError {
    /// The parsed entry carried no name.
    #[error("bot entry has no name")]
    NoName,
    /// The file names no entry at all.
    #[error("the file names no bot entry")]
    EmptyFile,
    /// The file carries several entries where exactly one is required.
    #[error("the file carries {count} entries; load them one file per bot")]
    MultipleEntries {
        /// How many entries the file carried.
        count: usize,
    },
    /// The file matched none of the three accepted document shapes.
    #[error("not a valid bot entry: {detail}")]
    NotAnEntry {
        /// The parser's complaint.
        detail: String,
    },
}

/// Parses a bot file in any of the three shapes such a file might
/// reasonably carry: the full document shape jinn.toml uses
/// (`[[bot.entry]]`, which nests the array under an `bot`
/// table — the shape the shipped autocoder bot files and the tool's
/// own description promise), the flat single-entry document
/// (`{ entry = [ ... ] }`), or one bare entry's keys. All converge on
/// exactly one entry; anything else is refused with the reason.
///
/// # Errors
///
/// Returns [`BotParseError`] naming why the file is not exactly one
/// named entry.
pub(crate) fn parse_bot_entry(
    text: &str,
) -> Result<BotEntryConfig, error_stack::Report<BotParseError>> {
    if let Ok(entry) = toml::from_str::<BotEntryConfig>(text) {
        if entry.name.trim().is_empty() {
            return Err(BotParseError::NoName.into());
        }
        return Ok(entry);
    }
    #[derive(serde::Deserialize)]
    struct Document {
        bot: Section,
    }
    /// The pre-rename section key, still read so shipped workflow files
    /// keep loading. A file carrying both keys resolves through the
    /// [`Document`] shape above — `bot` wins on collision.
    #[derive(serde::Deserialize)]
    struct LegacyDocument {
        attendant: Section,
    }
    #[derive(serde::Deserialize)]
    struct Section {
        entry: Vec<BotEntryConfig>,
    }
    if let Ok(doc) = toml::from_str::<Document>(text) {
        return one_entry(&doc.bot.entry);
    }
    if let Ok(doc) = toml::from_str::<LegacyDocument>(text) {
        return one_entry(&doc.attendant.entry);
    }
    #[derive(serde::Deserialize)]
    struct Wrapped {
        entry: Vec<BotEntryConfig>,
    }
    match toml::from_str::<Wrapped>(text) {
        Ok(wrapped) => one_entry(&wrapped.entry),
        Err(error) => Err(BotParseError::NotAnEntry {
            detail: error.to_string(),
        }
        .into()),
    }
}

/// Requires exactly one named entry from a parsed entry list.
fn one_entry(
    entries: &[BotEntryConfig],
) -> Result<BotEntryConfig, error_stack::Report<BotParseError>> {
    match entries {
        [one] => {
            if one.name.trim().is_empty() {
                return Err(BotParseError::NoName.into());
            }
            Ok(one.clone())
        }
        [] => Err(BotParseError::EmptyFile.into()),
        _ => Err(BotParseError::MultipleEntries {
            count: entries.len(),
        }
        .into()),
    }
}

/// Reads one string argument from the call's JSON arguments.
fn string_argument(call: &ToolCall, key: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(&call.arguments)
        .ok()?
        .get(key)?
        .as_str()
        .map(str::to_owned)
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
