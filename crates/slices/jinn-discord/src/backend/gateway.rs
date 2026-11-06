//! The poise gateway task + `BotData`.
//!
//! This module owns the Discord-side of the bot: a [`poise`] framework running
//! the gateway websocket, slash commands (`/new`), and a plain-message handler.
//! It bridges Discord to the jinn actor bus via the shared [`State`] (read
//! session history) and [`Bridge`] (publish commands).
//!
//! Inbound access is deny-by-default: both the plain-message handler and every
//! slash command gate on `[discord].authorized_users` via
//! [`crate::authorize`] — an empty list authorizes nobody.

use std::sync::Arc;

use crate::backend::session_route::{
    InboundOutcome, classify_inbound, is_forwardable_message_type,
};
use jinn_preferences_config::schemas::discord::DiscordConfig;

use crate::{
    BridgeEvent, CreateThreadReason, DiscordThreadCreateFailed, DiscordThreadCreated,
    DiscordThreadMap, FinalReply, ForumChannelError, GatewayRequest, authorize, read_final_reply,
    split_message,
};
use derive_more::Debug;
use error_stack::Report;
use jinn_chat_input_msg::{EnqueueUserMessage, SubmitSteeringMessage};
use jinn_core_types::{ChatEntry, SessionId};
use jinn_discord_msg::DiscordStatusUpdate;
use jinn_kernel::{Bridge, State};
use jinn_session_state::SessionReadProjection;
use jinn_session_store_msg::SessionLoadRequested;
use poise::serenity_prelude as serenity;
use wherror::Error;

use crate::backend::commands;
use crate::backend::feat::discord::to_thread::{refusal_reason, to_thread_decision};

/// Error spawning the Discord gateway.
///
/// Returned when the bot token is missing or the poise framework cannot start.
#[derive(Debug, Error)]
#[error(debug)]
pub struct SpawnError;

pub(crate) type BotError = Box<dyn std::error::Error + Send + Sync>;
pub(crate) type BotContext<'a> = poise::Context<'a, BotData, BotError>;

/// Shared context passed to every poise command and event handler.
///
/// Held behind `Arc<UserData>` by poise. Clones cheaply — every field is either
/// an `Arc`, a `Bridge` (which is `Clone`), or a channel handle.
#[derive(Debug, Clone)]
pub struct BotData {
    /// Shared jinn `State` (sessions, frontend, etc.). The bot reads session
    /// history from here to extract the final reply.
    pub state: State,
    /// Clone of the actor bus — used to publish `EnqueueUserMessage`,
    /// `SubmitSteeringMessage`, `SessionLoadRequested`, etc. via closures.
    pub bridge: Bridge,
    /// DAO over `sessions.db` for thread↔session mapping persistence.
    pub thread_map: DiscordThreadMap,
    pub config: Arc<DiscordConfig>,
    /// Runtime services — the intent handler reads the slice registry and
    /// key route table from here.
    pub services: jinn_kernel::Services,
}

/// Runs the Discord gateway: starts poise, registers slash commands, and
/// spawns the bridge-event drain loop. Blocks the calling task until the
/// gateway shuts down.
///
/// `rx` is the receiving half of the channel fed by
/// [`DiscordBridgeSubscriber`](crate::bridge_subscriber::DiscordBridgeSubscriber);
/// the drain loop consumes [`BridgeEvent`]s and posts the bot's replies to
/// Discord.
///
/// # Errors
///
/// Returns [`SpawnError`] if the bot token is missing/empty or the poise
/// framework fails to start.
pub async fn run(
    data: BotData,
    token: String,
    rx: kanal::AsyncReceiver<BridgeEvent>,
    gw_rx: kanal::AsyncReceiver<GatewayRequest>,
    status_tx: kanal::Sender<DiscordStatusUpdate>,
    stop_signal: std::sync::Arc<tokio::sync::Notify>,
) -> Result<(), Report<SpawnError>> {
    if token.trim().is_empty() {
        tracing::error!("discord bot enabled but no token configured");
        let _ = status_tx.send(DiscordStatusUpdate::Error {
            message: "no token configured".to_owned(),
        });
        return Err(Report::new(SpawnError));
    }

    // Deny-by-default: an empty allow-list means every inbound interaction is
    // refused. Do not abort — running disabled-by-config is legal — but make
    // the consequence loud so a misconfigured owner recognizes the fix.
    if data.config.authorized_users.is_empty() {
        tracing::warn!(
            "[discord].authorized_users is empty — nobody will be able to \
             interact with the bot; add your numeric Discord user ID(s) to \
             jinn.toml and restart"
        );
    }

    let intents =
        serenity::GatewayIntents::non_privileged() | serenity::GatewayIntents::MESSAGE_CONTENT;

    // Pre-parse the configured guild id so the setup callback can register
    // slash commands to that guild (instant propagation) rather than globally
    // (up to 1h). poise does not auto-register; without this /new is unreachable.
    let guild_id = data
        .config
        .guild_id
        .as_deref()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(serenity::GuildId::new);

    let _ = status_tx.send(DiscordStatusUpdate::Connecting);

    let build_status_tx = status_tx.clone();

    let drain_data = data.clone();
    // The supervisor's stop signal, cloned per consumer: the loops
    // spawned inside setup take one, the run select below another.
    let stop_for_loops = std::sync::Arc::clone(&stop_signal);
    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: vec![
                commands::new(),
                commands::prompts(),
                commands::teardown(),
                commands::archive(),
            ],
            // Plain-message handler: route an inbound Discord message to the
            // jinn session bound to its thread (resuming/un-archiving first),
            // then enqueue or steer depending on the current phase.
            event_handler: |ctx, event, _framework, data| Box::pin(on_event(ctx, event, data)),
            on_error: |error| {
                Box::pin(async move {
                    tracing::error!(?error, "poise framework error");
                })
            },
            ..Default::default()
        })
        .setup(move |ctx, _ready, framework| {
            let data = data.clone();
            Box::pin(async move {
                // Spawn the bridge-event drain loop. It owns its own BotData clone
                // plus the poise http handle so it can post replies back to Discord
                // without touching the command path.
                let http = ctx.http.clone();
                let status_tx = status_tx.clone();
                let drain_data = drain_data.clone();
                tokio::spawn(drain_loop(
                    drain_data,
                    http.clone(),
                    rx,
                    std::sync::Arc::clone(&stop_for_loops),
                ));
                // Drain the request channel: domain → gateway do-something
                // requests (currently only CreateThreadForSession). The loop
                // creates the Discord thread, records the mapping, and reports
                // the result back onto the bus.
                tokio::spawn(request_loop(
                    data.clone(),
                    http.clone(),
                    gw_rx,
                    std::sync::Arc::clone(&stop_for_loops),
                ));

                // The bot is online — the setup callback fires after the
                // gateway's `ready` event, so this is the right place to
                // signal that the connection succeeded.
                let _ = status_tx.send(DiscordStatusUpdate::Connected);

                // Register slash commands so /new is reachable in the client.
                let commands = &framework.options().commands;
                let register_result = match guild_id {
                    Some(gid) => poise::builtins::register_in_guild(&http, commands, gid).await,
                    None => poise::builtins::register_globally(&http, commands).await,
                };
                if let Err(e) = register_result {
                    tracing::warn!(error = ?e, "failed to register slash commands");
                }
                Ok(data)
            })
        })
        .build();

    let mut client = serenity::ClientBuilder::new(token, intents)
        .framework(framework)
        .await
        .map_err(|e| {
            tracing::error!(error = ?e, "failed to build discord client");
            let _ = build_status_tx.send(DiscordStatusUpdate::Error {
                message: format!("client build failed: {e}"),
            });
            Report::new(SpawnError)
        })?;

    // The websocket runs until either it exits on its own or the
    // supervisor's stop signal fires (a deferred disable landing at the
    // idle boundary). The shard manager is cloned up front so the stop
    // arm doesn't touch `client`: the `start` future borrows it for the
    // whole select, and dropping that future on the stop path is what
    // unwinds the poise framework tasks.
    let stop_for_run = std::sync::Arc::clone(&stop_signal);
    let shard_manager = std::sync::Arc::clone(&client.shard_manager);
    let outcome = tokio::select! {
        outcome = client.start() => {
            outcome.map_err(|e| {
                tracing::error!(error = ?e, "discord gateway exited with error");
                let _ = build_status_tx.send(DiscordStatusUpdate::Disconnected);
                Report::new(SpawnError)
            })
        }
        () = stop_for_run.notified() => {
            tracing::info!("discord gateway stopping (config change)");
            shard_manager.shutdown_all().await;
            let _ = build_status_tx.send(DiscordStatusUpdate::Disconnected);
            Ok(())
        }
    };
    outcome
}

/// Consume [`GatewayRequest`]s from the bridge actor and execute them.
///
/// Reverse direction of [`drain_loop`]: where the drain loop reacts to
/// domain events by posting to Discord, this loop reacts to domain *requests*
/// by *creating* things on Discord (forum threads, for now) and reporting
/// the result back onto the bus.
async fn request_loop(
    data: BotData,
    http: Arc<serenity::Http>,
    gw_rx: kanal::AsyncReceiver<GatewayRequest>,
    stop_signal: std::sync::Arc<tokio::sync::Notify>,
) {
    loop {
        // The stop signal and the next request race: whichever arrives
        // first wins. `notify_waiters` does not buffer, so a signal that
        // fired before this task started does not strand it — the
        // spawner starts this task only after `run` has registered its
        // own watcher, and `client.start()` keeps the websocket alive
        // until every loop has run down.
        let received = tokio::select! {
            () = stop_signal.notified() => {
                tracing::info!("discord gateway request loop stopping (config change)");
                break;
            }
            received = gw_rx.recv() => received,
        };
        match received {
            Ok(GatewayRequest::CreateThreadForSession { session_id, title }) => {
                handle_create_thread(&data, &http, session_id, title).await;
            }
            Err(_) => {
                tracing::debug!("discord gateway request channel closed; exiting");
                break;
            }
        }
    }
    tracing::info!("discord gateway request loop exiting");
}

/// Handle a single `CreateThreadForSession` request end to end.
///
/// Pure decision (already-bound?) is delegated to [`to_thread_decision`] so it
/// stays unit-testable; this fn is the thin serenity/DB adapter that maps the
/// decision + side effects to bus result events.
async fn handle_create_thread(
    data: &BotData,
    http: &serenity::Http,
    session_id: jinn_core_types::SessionId,
    title: String,
) {
    // 1. Already-bound guard: refuse to rebind, never orphan an existing thread.
    let existing_binding = match data.thread_map.get_thread_by_session(&session_id).await {
        Ok(Some(_)) => true,
        Ok(None) => false,
        Err(e) => {
            tracing::warn!(error = ?e, %session_id, "thread-map lookup failed; reporting failure");
            report_failure(
                &data.bridge,
                session_id,
                CreateThreadReason::CreateFailed("thread-map lookup failed".to_owned()),
            );
            return;
        }
    };
    if let Some(reason) = refusal_reason(&to_thread_decision(existing_binding)) {
        tracing::info!(%session_id, ?reason, "to-thread rejected");
        report_failure(&data.bridge, session_id, reason);
        return;
    }

    // 2. Resolve the configured forum channel into a Discord `ChannelId`. The
    //    gateway is the sole judge of whether `forum_channel` is usable, so
    //    both the missing and invalid cases are surfaced here with distinct
    //    reasons (never at the intent handler).
    let forum_channel_raw = data.config.forum_channel.as_deref().map(str::trim);
    let forum_channel_id = match forum_channel_raw {
        None | Some("") => {
            tracing::warn!(%session_id, "forum_channel unset");
            report_failure(
                &data.bridge,
                session_id,
                CreateThreadReason::ForumChannel(ForumChannelError::Missing),
            );
            return;
        }
        Some(s) => match s.parse::<u64>() {
            Ok(id) => serenity::ChannelId::new(id),
            Err(_) => {
                tracing::warn!(%session_id, raw = s, "forum_channel unparseable as a snowflake");
                report_failure(
                    &data.bridge,
                    session_id,
                    CreateThreadReason::ForumChannel(ForumChannelError::Invalid {
                        value: s.to_owned(),
                    }),
                );
                return;
            }
        },
    };

    // 3. Create the forum post. Forum threads require an initial message.
    let created = forum_channel_id
        .create_forum_post(
            http,
            serenity::builder::CreateForumPost::new(
                title.clone(),
                serenity::builder::CreateMessage::new().content("Continuing a jinn session here."),
            ),
        )
        .await;
    let channel = match created {
        Ok(channel) => channel,
        Err(e) => {
            tracing::warn!(error = ?e, %session_id, "discord forum thread creation failed");
            report_failure(
                &data.bridge,
                session_id,
                CreateThreadReason::CreateFailed(e.to_string()),
            );
            return;
        }
    };

    // 4. Record the thread→session mapping. guild_id is unknown on the reverse
    //    path (we never saw a poise ctx), matching the /new fallback of None.
    let thread_id = crate::thread_map::ThreadId::new(channel.id.get().to_string());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    if let Err(e) = data
        .thread_map
        .set(&thread_id, &session_id, None, now)
        .await
    {
        tracing::warn!(error = ?e, %session_id, "mapping write failed (thread exists but unbound)");
        report_failure(
            &data.bridge,
            session_id,
            CreateThreadReason::MappingWriteFailed,
        );
        return;
    }

    // 5. Success — tell the feedback actor to confirm in chat.
    tracing::info!(%session_id, thread_id = %thread_id, "created discord thread for session");
    publish(&data.bridge, DiscordThreadCreated { session_id, title });
}

/// Publish a `DiscordThreadCreateFailed` event for the session.
fn report_failure(
    bridge: &Bridge,
    session_id: jinn_core_types::SessionId,
    reason: CreateThreadReason,
) {
    publish(bridge, DiscordThreadCreateFailed { session_id, reason });
}

/// Consume [`BridgeEvent`]s from the bridge actor and post replies to Discord.
///
/// Two cases:
/// - [`BridgeEvent::SetupCompleted`] — format the result message and post to the
///   thread bound to the session (reverse lookup).
/// - [`BridgeEvent::TurnFinished`] — read the final assistant/error reply from
///   shared state, split it into ≤2000-char chunks, and post each to the thread.
async fn drain_loop(
    data: BotData,
    http: Arc<serenity::Http>,
    rx: kanal::AsyncReceiver<BridgeEvent>,
    stop_signal: std::sync::Arc<tokio::sync::Notify>,
) {
    loop {
        let received = tokio::select! {
            () = stop_signal.notified() => {
                tracing::info!("discord bridge-event drain loop stopping (config change)");
                break;
            }
            received = rx.recv() => received,
        };
        match received {
            Ok(event) => match event {
                BridgeEvent::SetupCompleted {
                    session_id,
                    cwd,
                    error,
                } => match resolve_thread(&data, &session_id).await {
                    Ok(Some(channel_id)) => {
                        let msg = match &error {
                            Some(e) => format!("❌ Setup failed: {e}"),
                            None => format!("✅ Setup complete ({cwd})"),
                        };
                        if let Err(e) = post_message(&http, channel_id, &msg).await {
                            tracing::warn!(error = ?e, "failed to post setup result");
                        }
                    }
                    Ok(None) => {
                        tracing::debug!(%session_id, "no thread bound to session; dropping setup result");
                    }
                    Err(e) => tracing::warn!(error = %e, "thread lookup failed for setup result"),
                },
                BridgeEvent::TurnFinished { session_id } => {
                    match resolve_thread(&data, &session_id).await {
                        Ok(Some(channel_id)) => {
                            let reply = read_reply(&data, &session_id);
                            tracing::info!(
                                %session_id,
                                reply_found = reply.is_some(),
                                "drain: TurnFinished"
                            );
                            match reply {
                                Some(reply) => {
                                    if let Err(e) = post_reply(&http, channel_id, &reply).await {
                                        tracing::warn!(error = ?e, "failed to post final reply");
                                    }
                                }
                                None => {
                                    tracing::debug!(%session_id, "turn finished but no final reply found");
                                }
                            }
                        }
                        Ok(None) => {
                            tracing::info!(%session_id, reply_found = false, "drain: TurnFinished (no thread bound)");
                            tracing::debug!(%session_id, "no thread bound to session; dropping reply");
                        }
                        Err(e) => {
                            tracing::info!(%session_id, reply_found = false, "drain: TurnFinished (thread lookup failed)");
                            tracing::warn!(error = %e, "thread lookup failed for final reply");
                        }
                    }
                }
                BridgeEvent::TeardownFinished { session_id, error } => {
                    match resolve_thread(&data, &session_id).await {
                        Ok(Some(channel_id)) => {
                            let msg = match &error {
                                Some(e) => format!("❌ Teardown failed: {e}"),
                                None => "✅ Teardown complete".to_owned(),
                            };
                            if let Err(e) = post_message(&http, channel_id, &msg).await {
                                tracing::warn!(error = ?e, "failed to post teardown result");
                            }
                        }
                        Ok(None) => {
                            tracing::debug!(%session_id, "no thread bound to session; dropping teardown result");
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "thread lookup failed for teardown result");
                        }
                    }
                }
                BridgeEvent::Archived { session_id } => {
                    match resolve_thread(&data, &session_id).await {
                        Ok(Some(channel_id)) => {
                            if let Err(e) = post_message(&http, channel_id, "✅ Archived").await {
                                tracing::warn!(error = ?e, "failed to post archive result");
                            }
                        }
                        Ok(None) => {
                            tracing::debug!(%session_id, "no thread bound to session; dropping archive result");
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "thread lookup failed for archive result");
                        }
                    }
                }
            },
            Err(_) => {
                tracing::debug!("discord bridge channel closed; drain loop exiting");
                break;
            }
        }
    }
    tracing::info!("discord bridge-event drain loop exiting");
}

/// Read the final assistant/error reply for a session from shared state.
fn read_reply(data: &BotData, session_id: &SessionId) -> Option<FinalReply> {
    let state = data.state.read();
    // Fallible lookup: the session may have been concurrently closed/archived
    // between the TurnFinished event and this read. Returning None makes the
    // drain loop skip the reply rather than panic and tear down the bot.
    let projection = state
        .try_session(session_id)
        .map(SessionReadProjection::from)?;
    read_final_reply(projection.entries())
}

/// Why a thread could not be resolved from a jinn session id.
///
/// The lookup variant carries its rendered message: the map's error type
/// is a unit struct whose only payload is its `Debug` text.
#[derive(Debug, wherror::Error)]
enum ThreadResolveError {
    #[error("invalid thread id {thread_id}: {source}")]
    InvalidThreadId {
        thread_id: String,
        #[source]
        source: std::num::ParseIntError,
    },
    #[error("thread map lookup failed: {detail}")]
    Lookup { detail: String },
}

/// Look up the Discord thread bound to a jinn session (reverse mapping).
///
/// # Errors
///
/// Returns an error when the mapping read fails or a stored thread id no
/// longer parses as a Discord snowflake.
async fn resolve_thread(
    data: &BotData,
    session_id: &SessionId,
) -> Result<Option<serenity::ChannelId>, Report<ThreadResolveError>> {
    match data.thread_map.get_thread_by_session(session_id).await {
        Ok(Some(mapping)) => mapping.thread_id.as_str().parse::<u64>().map_or_else(
            |source| {
                Err(Report::new(ThreadResolveError::InvalidThreadId {
                    thread_id: mapping.thread_id.as_str().to_owned(),
                    source,
                }))
            },
            |id| Ok(Some(serenity::ChannelId::new(id))),
        ),
        Ok(None) => Ok(None),
        Err(e) => Err(Report::new(ThreadResolveError::Lookup {
            detail: format!("{:?}", e.current_context()),
        })),
    }
}

/// Post a single plain message to a channel.
#[expect(
    clippy::result_large_err,
    reason = "serenity::Error is the crate-wide error type; boxing it would ripple through every call site for no gain"
)]
async fn post_message(
    http: &serenity::Http,
    channel: serenity::ChannelId,
    body: &str,
) -> Result<(), serenity::Error> {
    channel.say(http, body).await.map(|_| ())
}

/// Split a final reply into ≤2000-char chunks and post each to a channel.
#[expect(
    clippy::result_large_err,
    reason = "serenity::Error is the crate-wide error type; boxing it would ripple through every call site for no gain"
)]
async fn post_reply(
    http: &serenity::Http,
    channel: serenity::ChannelId,
    reply: &FinalReply,
) -> Result<(), serenity::Error> {
    let body = match reply {
        FinalReply::Assistant(t) | FinalReply::Error(t) => t.as_str(),
    };
    for chunk in split_message(body) {
        channel.say(http, chunk).await?;
    }
    Ok(())
}

/// Poise event handler — dispatches plain messages to the bound session.
///
/// Slash commands are routed by poise separately; this only handles the
/// `Message` event for non-bot authors.
async fn on_event(
    ctx: &serenity::Context,
    event: &serenity::FullEvent,
    data: &BotData,
) -> Result<(), BotError> {
    let serenity::FullEvent::Message { new_message } = event else {
        return Ok(());
    };
    // Ignore our own messages.
    if new_message.author.id == ctx.cache.current_user().id {
        return Ok(());
    }
    // Ignore non-user message types (system indicators like pins, joins,
    // thread-created) and other bots. Only genuine human text/reply messages
    // route into sessions — system messages carry empty content and would
    // produce empty user turns that LLM providers reject (e.g. ZAI error 1213).
    if !is_forwardable_message_type(new_message.kind) || new_message.author.bot {
        return Ok(());
    }
    // Deny-by-default: unauthorized authors are dropped silently — no reply,
    // no publish — so probing strangers get no feedback. Ordering matters:
    // this sits after the self/bot/type filters so system noise never even
    // reaches the check.
    if !authorize::is_authorized(&data.config.authorized_users, new_message.author.id.get()) {
        tracing::debug!(
            author_id = new_message.author.id.get(),
            channel_id = new_message.channel_id.get(),
            "unauthorized discord message dropped"
        );
        return Ok(());
    }
    handle_inbound_message(ctx, new_message, data).await?;
    Ok(())
}

/// Route an inbound Discord message to the jinn session bound to its channel.
///
/// The routing decision is delegated to [`classify_inbound`] so it is unit-
/// testable without Discord types. This function is the thin adapter that maps
/// the decision to bus publishes and Discord replies.
///
/// Decision summary:
/// - No thread→session mapping → silent no-op (no reply, no publish). `/new`
///   is the only entry point, and the wizard reads its replies from its own
///   `MessageCollector`, so silence here avoids racing the wizard.
/// - Bound + session present → enqueue (idle) or steer (mid-turn) immediately.
/// - Bound + session missing → publish `SessionLoadRequested` and ask the user
///   to resend. We never enqueue against a missing session: the enqueue
///   handler's `session_mut_or_create` would create a throwaway that the
///   subsequent load overwrites, dropping the message.
async fn handle_inbound_message(
    ctx: &serenity::Context,
    msg: &serenity::Message,
    data: &BotData,
) -> Result<(), BotError> {
    let thread_id = crate::thread_map::ThreadId::new(msg.channel_id.get().to_string());
    let Some(session_id) = data
        .thread_map
        .get_session_by_thread(&thread_id)
        .await
        .map_err(|e| format!("thread map lookup: {e:?}"))?
    else {
        // Unbound channel: silent no-op. See `classify_inbound::UnboundNoOp`.
        return Ok(());
    };

    // Read the phase under a short-lived read lock, then drop the lock before
    // any publish. Holding the lock across a `Bridge::send` can deadlock if the
    // bus dispatch path re-enters `State`.
    let phase = {
        let state = data.state.read();
        state
            .try_session(&session_id)
            .map(SessionReadProjection::from)
            .map(|projection| projection.phase())
    };

    match classify_inbound(true, phase) {
        InboundOutcome::UnboundNoOp => Ok(()),
        InboundOutcome::Enqueue => {
            publish(
                &data.bridge,
                EnqueueUserMessage {
                    session_id: session_id.clone(),
                    entry: ChatEntry::user(msg.content.clone()),
                },
            );
            Ok(())
        }
        InboundOutcome::Steer => {
            publish(
                &data.bridge,
                SubmitSteeringMessage {
                    session_id: session_id.clone(),
                    text: msg.content.clone(),
                },
            );
            Ok(())
        }
        InboundOutcome::LoadMissing => {
            publish(
                &data.bridge,
                SessionLoadRequested {
                    session_id: session_id.clone(),
                    // Discord has no view of the terminal's layout, so it
                    // offers no width; the store actor derives one.
                    content_width: None,
                },
            );
            msg.reply(ctx, "Session restoring — please resend your message.")
                .await?;
            Ok(())
        }
    }
}

/// Publish a typed bus message via a bridge closure.
pub(crate) fn publish<M>(bridge: &Bridge, msg: M)
where
    M: jinn_slices::PublishableMessage,
{
    if let Err(e) = bridge.send(Bridge::publish_closure(msg)) {
        tracing::error!(error = %e, "bridge send failed");
    }
}

#[cfg(test)]
mod tests;
