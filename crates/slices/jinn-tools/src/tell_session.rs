//! The `tell_session` tool — send a text message to another session.
//!
//! One tool, five deliveries, none of them silent:
//!
//! - **Interrupt to a busy session** (`mode: "interrupt"`) — the message
//!   rides the target's steering buffer, landing as a `User` entry at its
//!   next prompt-assembly boundary, ahead of anything queued. The
//!   in-flight turn is never cancelled.
//! - **Idle manual-trigger bot** — starts a run now, seeded through
//!   its template with the message folded in (see `jinn_bot::
//!   run_runtime`; the composition, not the cancel-happy manual path).
//! - **Busy bot** — the seeded entry queues behind the in-flight
//!   turn through the ordinary enqueue handler. A busy bot is doing
//!   real work; delivery never cancels it.
//! - **Any other live session** — receives an inter-session entry whose
//!   text names its origin: `[Message from session <id> (<title>)]: `
//!   followed by the message. The prefix is baked in here, once, so the
//!   model, the transcript, search, and export all read the same string.
//! - **Unknown id** — a failed result naming the target. No send is ever
//!   silently dropped.
//!
//! `queue` is the default mode: an absent mode delivers exactly as
//! `queue` does. An interrupt aimed at an idle target falls back to the
//! ordinary delivery — there is nothing to interrupt. The interrupt
//! fragment carries the same attribution prefix as any other delivery,
//! so a steered message still names where it came from.
//!
//! Bot-only gates still apply: a bot in prep mode is still
//! being composed, so a message to it fails rather than dispatching a
//! half-written prompt.

use jinn_bot::run_runtime;
use jinn_chat_input_msg::{EnqueueUserMessage, SubmitSteeringMessage};
use jinn_core_types::chat_entry::ChatEntry;
use jinn_core_types::session_id::SessionId;
use jinn_core_types::tool_types::{ToolCall, ToolDefinition, ToolResult};
use jinn_session_state::ChatSessionState;
use jinn_slices::PhaseView;

use unicode_segmentation::UnicodeSegmentation;

use crate::tool_types::{ToolContext, tool_error};

use super::BoxedToolFuture;

/// Maximum grapheme clusters of the source title carried in the attribution
/// prefix before it is truncated.
pub(crate) const MAX_TITLE_GRAPHEMES: usize = 32;

/// The attribution prefix marking where an inter-session message came from:
/// `[Message from session <id> (<title>)]: `. The title part is omitted when
/// there is no usable title. The one composition site for the format.
pub(crate) fn attribution_prefix(source_id: &SessionId, title: Option<&str>) -> String {
    match title {
        Some(title) => format!("[Message from session {source_id} ({title})]: "),
        None => format!("[Message from session {source_id}]: "),
    }
}

/// The source title carried into the entry: `None` when absent, empty, or
/// itself an attribution prefix (a relayed message's title would nest the
/// header inside itself); otherwise clipped to [`MAX_TITLE_GRAPHEMES`]
/// grapheme clusters with a single ellipsis when clipped.
pub(crate) fn processed_title(title: Option<&str>) -> Option<String> {
    title
        .filter(|t| !t.is_empty() && !t.starts_with("[Message"))
        .map(|t| {
            if t.graphemes(true).count() <= MAX_TITLE_GRAPHEMES {
                return t.to_owned();
            }
            let head: String = t.graphemes(true).take(MAX_TITLE_GRAPHEMES).collect();
            format!("{head}…")
        })
}

/// The `tell_session` tool definition.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "tell_session".to_owned(),
        description: "Send a message to another session by its session id. An idle bot starts a run seeded from its template plus your message; a busy bot receives it when its current turn finishes; any other session receives it as a new user turn. With mode \"interrupt\" on a busy target, the message lands as a steering fragment at its next prompt-assembly boundary instead of queueing."
            .to_owned(),
        prompt_snippet: Some(
            "Deliver a message to a named session: `tell_session` with the target session id and the text. Pass mode \"interrupt\" to steer a busy session's current turn; the default mode queues."
                .to_owned(),
        ),
        prompt_guidelines: vec![
            "Use `tell_session` to hand work to a specific session — a stage bot in a workflow run, or any session whose id you know. Never guess an id; use one you were given or can read."
                .to_owned(),
        ],
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "session_id": {
                    "type": "string",
                    "description": "The target session's id."
                },
                "message": {
                    "type": "string",
                    "description": "The message text the target session receives. Markdown format is supported."
                },
                "mode": {
                    "type": "string",
                    "enum": ["queue", "interrupt"],
                    "description": "How the message is delivered to a busy target: \"queue\" (the default) waits for its current turn to finish; \"interrupt\" lands it as a steering fragment at the target's next prompt-assembly boundary, without cancelling in-flight work. An interrupt to an idle target delivers ordinarily."
                }
            },
            "required": ["session_id", "message"]
        }),
        server_tool_type: None,
    }
}

/// Executes `tell_session`.
pub fn execute(call: ToolCall, ctx: ToolContext) -> BoxedToolFuture {
    Box::pin(async move {
        let Some(message) = string_argument(&call, "message") else {
            return tool_error(&call, "missing required argument: message");
        };
        let Some(raw_id) = string_argument(&call, "session_id") else {
            return tool_error(&call, "missing required argument: session_id");
        };
        let mode = match string_argument(&call, "mode") {
            None => Mode::Queue,
            Some(raw) => match Mode::parse(&raw) {
                Some(mode) => mode,
                None => {
                    return tool_error(
                        &call,
                        &format!("unknown mode: {raw:?} (valid values: \"queue\", \"interrupt\")"),
                    );
                }
            },
        };
        let Some(target_id) = SessionId::try_from_string(&raw_id) else {
            return tool_error(&call, &format!("unknown session: {raw_id}"));
        };
        let Some(state) = ctx.state.clone() else {
            return tool_error(&call, "tell_session is unavailable without shared state");
        };
        let Some(bus) = ctx.bus.clone() else {
            return tool_error(&call, "tell_session is unavailable without a bus");
        };

        let outcome = {
            let mut guard = state.write();
            let caller_id = ctx.session_id.clone().unwrap_or_else(|| {
                // No calling session (headless/scripted send): the source is
                // the target's own id, rendered as an unnamed origin.
                target_id.clone()
            });
            // The caller's title feeds the attribution prefix; clone it out
            // before the map is borrowed mutably for the target.
            let caller_title = guard
                .session
                .get(&caller_id)
                .and_then(|caller| caller.title().map(str::to_owned));
            let Some(target) = guard.session.get_mut(&target_id) else {
                return tool_error(&call, &format!("unknown session: {target_id}"));
            };
            let delivery =
                delivery_for(&caller_id, caller_title.as_deref(), target, &message, mode);
            // A delivered message marks the target interacted: a programmatic
            // enqueue does not, and without the mark `is_persistable()` gates
            // every save — the run would turn and silently never reach disk.
            // A refused message changes nothing.
            if !matches!(delivery, Delivery::Refused(_)) {
                target.mark_interacted();
            }
            delivery
        };

        match outcome {
            Delivery::Run { entry, reset } => {
                // Persist a reset that actually excluded something, or a
                // restarted bot would regain the full history.
                if !reset.is_empty() {
                    bus.publish(jinn_session_store_msg::PersistSession {
                        session_id: target_id.clone(),
                    })
                    .await;
                }
                bus.publish(EnqueueUserMessage {
                    session_id: target_id.clone(),
                    entry,
                })
                .await;
                succeeded(
                    call,
                    &format!("Message delivered to bot {target_id}; run dispatched."),
                )
            }
            Delivery::QueuedRun { entry } => {
                bus.publish(EnqueueUserMessage {
                    session_id: target_id.clone(),
                    entry,
                })
                .await;
                succeeded(
                    call,
                    &format!(
                        "Message delivered to bot {target_id}; queued behind its \
                         current turn."
                    ),
                )
            }
            Delivery::Turn(entry) => {
                bus.publish(EnqueueUserMessage {
                    session_id: target_id.clone(),
                    entry,
                })
                .await;
                succeeded(
                    call,
                    &format!("Message delivered to session {target_id} as a user turn."),
                )
            }
            Delivery::Interrupt(text) => {
                bus.publish(SubmitSteeringMessage {
                    session_id: target_id.clone(),
                    text,
                })
                .await;
                succeeded(
                    call,
                    &format!(
                        "Message steered into session {target_id}'s current turn; \
                         it lands at the next prompt-assembly boundary."
                    ),
                )
            }
            Delivery::Refused(reason) => tool_error(&call, &format!("{target_id}: {reason}")),
        }
    })
}

/// The delivery a target session takes for an inbound message.
enum Delivery {
    /// An idle bot: dispatch a seeded run now.
    Run {
        entry: ChatEntry,
        reset: Vec<jinn_core_types::ChatEntryId>,
    },
    /// A busy bot: the seeded entry queues behind the in-flight turn.
    QueuedRun { entry: ChatEntry },
    /// Any other session: an ordinary user turn.
    Turn(ChatEntry),
    /// A busy session under `mode: "interrupt"`: the message rides the
    /// target's steering buffer, landing at its next prompt-assembly
    /// boundary. The in-flight turn is never cancelled.
    Interrupt(String),
    /// Delivery refused; the reason names why.
    Refused(&'static str),
}

/// The requested delivery mode: the default `queue` waits for a busy
/// target's current turn to finish; `interrupt` lands the message in the
/// target's steering buffer at its next prompt-assembly boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Queue,
    Interrupt,
}

impl Mode {
    /// Parses a `mode` argument: `None` marks an unrecognized value (the
    /// caller reports it; a typo must fail visibly, not silently queue).
    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "queue" => Some(Self::Queue),
            "interrupt" => Some(Self::Interrupt),
            _ => None,
        }
    }
}

/// Decides the delivery path for `message` against the target's live state.
fn delivery_for(
    source_id: &SessionId,
    source_title: Option<&str>,
    target: &mut ChatSessionState,
    message: &str,
    mode: Mode,
) -> Delivery {
    if target.is_bot() {
        if target.bot_is_prepping() {
            return Delivery::Refused("bot is in prep mode; nothing runs for it yet");
        }
        if target.phase() != PhaseView::Idle && mode == Mode::Interrupt {
            // A busy bot is steering like any other busy target: the
            // message lands at its next prompt-assembly boundary. No run
            // is prepared — the seeded dispatch, including its history
            // reset, belongs to the queue path the caller declined.
            return Delivery::Interrupt(interrupt_text(source_id, source_title, message));
        }
        let (entry, reset) = run_runtime::prepare_message_run(target, message);
        let Some(entry) = entry else {
            // `prepare_message_run` always yields an entry — the message
            // itself is the floor. Kept as a guard, not a reachable arm.
            return Delivery::Refused("run produced no dispatch");
        };
        return if target.phase() == PhaseView::Idle {
            Delivery::Run { entry, reset }
        } else {
            Delivery::QueuedRun { entry }
        };
    }
    if mode == Mode::Interrupt && target.phase() != PhaseView::Idle {
        return Delivery::Interrupt(interrupt_text(source_id, source_title, message));
    }
    // Non-bot: the message rides as an inter-session entry whose text
    // names its origin. The prefix is baked in here — the one composition
    // site — so the model, the transcript, search, and export all read the
    // same self-describing string. The title is not minted by delivery:
    // title minting stays keyed to genuine user entries.
    let title = processed_title(source_title);
    let text = format!(
        "{}{message}",
        attribution_prefix(source_id, title.as_deref())
    );
    Delivery::Turn(ChatEntry::intersession(
        source_id.clone(),
        title,
        target.session_id().clone(),
        text,
    ))
}

/// Composes the steered text: the attribution prefix baked into the
/// message, through the one prefix-composition site. The steered text is
/// a plain `User` entry once drained, so the prefix is the fragment's
/// only origin marker.
fn interrupt_text(source_id: &SessionId, source_title: Option<&str>, message: &str) -> String {
    format!(
        "{}{message}",
        attribution_prefix(source_id, processed_title(source_title).as_deref())
    )
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
