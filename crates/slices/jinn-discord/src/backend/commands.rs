//! Slash commands for the Discord bot.
//!
//! [`new`] starts a session via the configured lifecycle wizard; [`prompts`]
//! lists the prompt templates available to the current thread's session;
//! [`teardown`] and [`archive`] operate on the session bound to the invoking
//! thread. Every command is deny-by-default — the `ensure_authorized` helper
//! gates on `[discord].authorized_users` before any command body runs. Plain
//! messages are handled (and gated) in [`crate::backend::gateway`] via the poise event
//! handler, not here.

use std::time::Duration;

use crate::authorize;
use jinn_domain::feat::context::prompt_template::PromptTemplateStore;
use jinn_domain::feat::session::protocol::archive_session::ArchiveSession;
use jinn_domain::protocol::Intent;
use jinn_domain::{Bridge, SessionId};
use jinn_preferences_config::schemas::SessionLifecycle;
use poise::serenity_prelude as serenity;

use crate::backend::gateway::{BotContext, BotData, BotError};

/// Deny-by-default gate shared by every slash command.
///
/// Returns `true` when the invoker may proceed; otherwise sends the single
/// generic ephemeral refusal — identical for every command so a denial
/// reveals nothing about bot state — and returns `false`.
///
/// # Errors
///
/// Returns [`BotError`] if sending the refusal fails.
async fn ensure_authorized(ctx: &BotContext<'_>) -> Result<bool, BotError> {
    let data = ctx.data();
    if authorize::is_authorized(&data.config.authorized_users, ctx.author().id.get()) {
        return Ok(true);
    }
    tracing::debug!(
        author_id = ctx.author().id.get(),
        "unauthorized slash command refused"
    );
    ctx.send(ephemeral("You are not authorized to use this bot."))
        .await?;
    Ok(false)
}

/// Start a new jinn session in this thread.
///
/// The user first picks a project (sets the starting CWD), then picks a
/// lifecycle from the `[[session_lifecycle]]` entries in preferences (plus an
/// implicit "blank" option). The picked lifecycle's setup runs with the args
/// that lifecycle declares (any number of positional params via
/// `$1`/`<name>`/`$@`); the bot prompts once for them, space-delimited, and
/// re-prompts on a count mismatch. The setup result is posted by the drain
/// loop when the `SessionSetupCompleted` event arrives.
#[poise::command(slash_command)]
pub async fn new(ctx: BotContext<'_>) -> Result<(), BotError> {
    if !ensure_authorized(&ctx).await? {
        return Ok(());
    }
    let data = ctx.data();

    // 1. Gather projects from preferences.
    let projects = {
        let state = data.state.read();
        state.frontend.preferences.projects.clone()
    };
    if projects.is_empty() {
        ctx.say("No projects configured. Add `[[project]]` entries to jinn.toml first.")
            .await?;
        return Ok(());
    }

    // 2. Present numbered list.
    let mut list = String::from("Available projects:\n");
    for (i, p) in projects.iter().enumerate() {
        use std::fmt::Write as _;
        let _ = writeln!(list, "{}. {}", i + 1, p.path.display());
    }
    list.push_str("\nReply with a number.");
    ctx.say(list).await?;

    // 3. Wait for the user's number.
    let channel = ctx.channel_id();
    let author = ctx.author().id;
    let sctx = ctx.serenity_context();
    let pick = collect_reply(sctx, channel, author).await;
    let Some(pick) = pick else {
        ctx.say("Timed out waiting for project selection.").await?;
        return Ok(());
    };

    let idx: usize = match pick.content.trim().parse::<usize>() {
        Ok(n) if (1..=projects.len()).contains(&n) => n - 1,
        _ => {
            ctx.say("Invalid selection. Run `/new` again.").await?;
            return Ok(());
        }
    };
    let Some(chosen) = projects.get(idx) else {
        ctx.say("Invalid selection. Run `/new` again.").await?;
        return Ok(());
    };
    let Some((lifecycle, args)) = pick_lifecycle(ctx, sctx, channel, author, data).await? else {
        return Ok(());
    };

    let new_session_id = {
        let mut state = data.state.write(&data.intent_handler_cap);
        // Stash the chosen project so the lifecycle handler consumes it as the
        // new session's starting CWD and project stamp (same convention as the
        // project picker). Without this the handler falls back to inheriting
        // the currently-active session's CWD, which is unrelated to the pick.
        state.frontend.pending_creation = Some(
            jinn_domain::feat::ui::frontend_state::PendingSessionCreation {
                project_dir: chosen.path.clone(),
                starting_cwd: chosen.path.clone(),
            },
        );
        let result = jinn_domain::feat::intent::IntentHandler::handle(
            &Intent::SessionLifecycleSetup {
                lifecycle_name: lifecycle.clone(),
                args: args.clone(),
            },
            &mut state,
            &data.services.slices,
            &data.services.key_routes,
            &data.services.picker_registry,
        );
        for closure in result.messages {
            let _ = data.bridge.send(closure);
        }
        state.session.active_session_id().clone()
    };

    // 7. Record the thread→session mapping so future messages + the drain loop
    //    can find the session.
    let thread_id = channel.get().to_string();
    let guild_id = ctx.guild_id().map(|g| g.get().to_string());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    if let Err(e) = data
        .thread_map
        .set(
            &thread_id,
            &new_session_id.to_string(),
            guild_id.as_deref(),
            now,
        )
        .await
    {
        tracing::warn!(error = ?e, "failed to record thread→session mapping");
    }

    // 8. The actual "Setup complete" message is posted by the drain loop when
    //    the SessionSetupCompleted event arrives. Acknowledge here only.
    // Render a friendly label for the blank pick (empty name) so the ack
    // doesn't read `lifecycle `` `. Explicit picks show their real name.
    let lifecycle_label = if lifecycle.is_empty() {
        "blank"
    } else {
        &lifecycle
    };
    ctx.say(format!(
        "Setting up session `{new_session_id}` (lifecycle `{lifecycle_label}`)..."
    ))
    .await?;
    Ok(())
}

/// Re-run the lifecycle teardown script for the session bound to this thread.
///
/// Looks up the thread→session mapping, renders the bound session's teardown
/// command (replaying its stored lifecycle args), and publishes
/// `RunSessionTeardown`. The ✅/❌ result is posted by the drain loop when the
/// `SessionTeardownFinished` event arrives.
///
/// Replies with an error message when no session is bound to the thread, or
/// when the session has no teardown command configured.
#[poise::command(slash_command)]
pub async fn teardown(ctx: BotContext<'_>) -> Result<(), BotError> {
    if !ensure_authorized(&ctx).await? {
        return Ok(());
    }
    let data = ctx.data().clone();
    let thread_id = ctx.channel_id().get().to_string();

    let Some(session_id_str) = resolve_bound_session(&data, &thread_id).await? else {
        ctx.say("No session bound to this thread. Use `/new` first.")
            .await?;
        return Ok(());
    };
    let session_id = SessionId::from(session_id_str);

    // Resolve + render under a short-lived read guard so it never spans an await.
    let Some(publish) = build_teardown_publish(&data.state.read(), &session_id) else {
        ctx.say("This session has no teardown command configured.")
            .await?;
        return Ok(());
    };

    data.bridge.send(publish)?;
    ctx.say(format!("Running teardown for session `{session_id}`…"))
        .await?;
    Ok(())
}

/// Resolve + render the teardown command for `session_id` and wrap it as a
/// publish closure. Returns `None` when the session has no teardown command.
fn build_teardown_publish(
    state: &jinn_domain::StateReadGuard<'_>,
    session_id: &SessionId,
) -> Option<jinn_domain::BridgeClosure> {
    let msg = jinn_domain::feat::session_lifecycle::intent::build_run_session_teardown(
        state, session_id,
    )?;
    Some(Bridge::publish_closure(msg))
}

/// Archive the session bound to this thread without running teardown.
///
/// Looks up the thread→session mapping and publishes `ArchiveSession`, which
/// marks the session archived in SQLite and removes it from memory. The ✅
/// confirmation is posted by the drain loop when the `SessionArchived` event
/// arrives.
///
/// Replies with an error message when no session is bound to the thread.
#[poise::command(slash_command)]
pub async fn archive(ctx: BotContext<'_>) -> Result<(), BotError> {
    if !ensure_authorized(&ctx).await? {
        return Ok(());
    }
    let data = ctx.data().clone();
    let thread_id = ctx.channel_id().get().to_string();

    let Some(session_id_str) = resolve_bound_session(&data, &thread_id).await? else {
        ctx.say("No session bound to this thread. Use `/new` first.")
            .await?;
        return Ok(());
    };
    let session_id = SessionId::from(session_id_str);

    data.bridge.send(Bridge::publish_closure(ArchiveSession {
        session_id: session_id.clone(),
    }))?;
    ctx.say(format!("Archiving session `{session_id}`…"))
        .await?;
    Ok(())
}

/// Resolve the jinn session bound to a Discord thread, if any.
///
/// Returns `Ok(None)` when the thread has no bound session (the caller replies
/// with a helpful message). Errors only on a DAO read failure.
///
/// # Errors
///
/// Returns [`BotError`] if the thread-map lookup fails.
async fn resolve_bound_session(
    data: &crate::backend::gateway::BotData,
    thread_id: &str,
) -> Result<Option<String>, BotError> {
    data.thread_map
        .get_session_by_thread(thread_id)
        .await
        .map_err(|e| Box::from(format!("thread lookup failed: {e:?}")) as BotError)
}

/// Outcome of a single locked read of a session's prompt store.
///
/// Captured under one read lock so the three outcomes are decided from a
/// consistent state snapshot — never split across two lock passes.
enum Lookup {
    /// The session has a non-empty store; here is the rendered cheat-sheet.
    List(String),
    /// The session exists but its prompt store is empty.
    Empty,
    /// The session is absent from state (mid-restore race).
    Missing,
}

/// List the prompt templates available to this thread's session.
///
/// Resolves the channel → session mapping via the thread map, reads the
/// session's already-merged prompt-template store, and replies privately
/// (ephemerally) with each prompt's `#name` and description. Composition is
/// unchanged — the user still types `#name` into a normal message. No rescan
/// is triggered; the store is whatever the session loaded at startup.
#[poise::command(slash_command)]
pub async fn prompts(ctx: BotContext<'_>) -> Result<(), BotError> {
    if !ensure_authorized(&ctx).await? {
        return Ok(());
    }
    let data = ctx.data();
    let channel_id = ctx.channel_id();

    // 1. Resolve channel → session via the thread map (same lookup as the
    //    inbound message handler). Unbound channels get an ephemeral hint
    //    rather than the silent no-op used on the message path.
    let thread_id = channel_id.get().to_string();
    let reply = match data.thread_map.get_session_by_thread(&thread_id).await {
        Ok(Some(id)) => {
            let session_id: jinn_domain::SessionId = id.into();
            // 2. Read the session's prompt store under a single short-lived
            //    read lock. The decision (list / empty / missing) is captured
            //    as a `Lookup` so the guard is dropped before the await on
            //    ctx.send below — and so the three outcomes are decided from
            //    one consistent state snapshot rather than two lock passes.
            let lookup = {
                let state = data.state.read();
                match state.try_session(&session_id) {
                    Some(session) => {
                        match render_prompts_list(session.discovered_prompt_templates()) {
                            Some(text) => Lookup::List(text),
                            None => Lookup::Empty,
                        }
                    }
                    None => Lookup::Missing,
                }
            };
            match lookup {
                Lookup::List(text) => ephemeral(text),
                Lookup::Empty => ephemeral("No prompts configured for this session."),
                Lookup::Missing => ephemeral("Session restoring — please try again shortly."),
            }
        }
        Ok(None) => ephemeral("No active session in this thread — run `/new` first."),
        Err(e) => ephemeral(format!("Failed to look up session: {e:?}")),
    };

    ctx.send(reply).await?;
    Ok(())
}

/// Build an ephemeral `CreateReply` carrying the given content.
fn ephemeral(content: impl Into<String>) -> poise::CreateReply {
    poise::CreateReply::default()
        .content(content)
        .ephemeral(true)
}

/// Build the numbered lifecycle list for the `/new` picker.
///
/// Always begins with the implicit "blank" entry (mirrors the TUI picker),
/// so the list is never empty even when `lifecycles` is empty. Each entry is
/// `N. {name}` and appends ` - {description}` when the lifecycle declares
/// one. Returns the full prompt text (header + entries + reply instruction).
fn format_lifecycle_list(lifecycles: &[SessionLifecycle]) -> String {
    use std::fmt::Write as _;
    let mut out = String::from("Available lifecycles:\n");
    let _ = writeln!(out, "1. blank - New empty session");
    for (i, l) in lifecycles.iter().enumerate() {
        let _ = match &l.description {
            Some(d) => writeln!(out, "{}. {} - {}", i + 2, l.name, d),
            None => writeln!(out, "{}. {}", i + 2, l.name),
        };
    }
    out.push_str("\nReply with a number.");
    out
}

/// Present the numbered lifecycle list and resolve the user's pick.
///
/// Returns `Ok(Some((lifecycle_name, args)))` to proceed, `Ok(None)` after
/// responding with a reason (timeout, invalid selection), or `Err` on a
/// Discord failure. The list always begins with the implicit "blank" entry
/// (option 1), followed by every `[[session_lifecycle]]` from preferences.
///
/// Selecting "blank" short-circuits arg collection (no setup command); any
/// other pick flows into [`collect_lifecycle_args`] for positional params.
async fn pick_lifecycle(
    ctx: BotContext<'_>,
    sctx: &serenity::Context,
    channel: serenity::ChannelId,
    author: serenity::UserId,
    data: &BotData,
) -> Result<Option<(String, Vec<String>)>, BotError> {
    let lifecycles = {
        data.state
            .read()
            .frontend
            .preferences
            .session_lifecycles
            .clone()
    };

    ctx.say(format_lifecycle_list(&lifecycles)).await?;

    let Some(pick) = collect_reply(sctx, channel, author).await else {
        ctx.say("Timed out waiting for lifecycle selection.")
            .await?;
        return Ok(None);
    };

    // blank = 1; explicit entries start at 2 (index + 2).
    let n: usize = match pick.content.trim().parse::<usize>() {
        Ok(n) => n,
        _ => {
            ctx.say("Invalid selection. Run `/new` again.").await?;
            return Ok(None);
        }
    };

    // blank: empty name, no args — skip collection entirely.
    if n == 1 {
        return Ok(Some((String::new(), vec![])));
    }

    // n >= 2: map to an index without underflowing and validate it against
    // the lifecycle slice in one step.
    let Some(lifecycle) = n.checked_sub(2).and_then(|i| lifecycles.get(i)) else {
        ctx.say("Invalid selection. Run `/new` again.").await?;
        return Ok(None);
    };
    let lifecycle_name = lifecycle.name.clone();
    collect_lifecycle_args(ctx, data, sctx, channel, author, lifecycle_name).await
}

/// Collect the named lifecycle's positional args from the user.
///
/// Returns `Ok(Some((lifecycle, args)))` to proceed, `Ok(None)` after
/// responding with a reason (missing lifecycle, timeout), or `Err` on a
/// Discord failure. The `lifecycle` name is supplied by the caller (resolved
/// via [`pick_lifecycle`]); zero-param lifecycles skip collection entirely.
/// On a count mismatch the bot responds and re-prompts; each attempt is
/// bounded by [`collect_reply`]'s 2-minute timeout.
async fn collect_lifecycle_args(
    ctx: BotContext<'_>,
    data: &BotData,
    sctx: &serenity::Context,
    channel: serenity::ChannelId,
    author: serenity::UserId,
    lifecycle: String,
) -> Result<Option<(String, Vec<String>)>, BotError> {
    use crate::backend::feat::discord::lifecycle_inputs::resolve_lifecycle_inputs;
    use jinn_domain::feat::session_lifecycle::command_template::parse_quoted_args;

    // Resolve how many positional args the lifecycle needs and the prompt text
    // to show for them. Reading preferences under a short-lived read guard so it
    // never spans an await.
    let spec = {
        let lifecycles = data
            .state
            .read()
            .frontend
            .preferences
            .session_lifecycles
            .clone();
        match resolve_lifecycle_inputs(&lifecycles, &lifecycle) {
            Some(s) => s,
            None => {
                ctx.say(format!(
                    "No session lifecycle named `{lifecycle}` is configured."
                ))
                .await?;
                return Ok(None);
            }
        }
    };

    if spec.param_count == 0 {
        return Ok(Some((lifecycle, vec![])));
    }
    ctx.say(&spec.prompt).await?;
    loop {
        let Some(reply) = collect_reply(sctx, channel, author).await else {
            ctx.say("Timed out waiting for input. Run `/new` again.")
                .await?;
            return Ok(None);
        };
        let parsed = parse_quoted_args(reply.content.trim());
        if parsed.len() >= spec.param_count {
            return Ok(Some((lifecycle, parsed)));
        }
        ctx.say(format!(
            "Expected {} arguments, got {}. {}",
            spec.param_count,
            parsed.len(),
            spec.prompt
        ))
        .await?;
    }
}

/// Collect one message from the user who invoked the command, within a
/// 2-minute window.
#[expect(
    clippy::duration_suboptimal_units,
    reason = "from_mins is unstable; from_secs is the stable alternative"
)]
async fn collect_reply(
    sctx: &serenity::Context,
    channel: serenity::ChannelId,
    author: serenity::UserId,
) -> Option<serenity::Message> {
    serenity::MessageCollector::new(sctx)
        .channel_id(channel)
        .author_id(author)
        .timeout(Duration::from_secs(2 * 60))
        .await
}

/// Soft cap on the rendered cheat-sheet length, leaving headroom under
/// Discord's 2000-character message body limit. Prompts beyond this are
/// truncated with a count rather than causing `ctx.send` to error.
const PROMPT_LIST_SOFT_LIMIT: usize = 1900;

/// Renders a prompt-template store as a human-readable cheat-sheet of
/// `#name` tokens and their descriptions.
///
/// Returns `None` when the store is empty so the caller can emit a distinct
/// "no prompts configured" message. The store's slice order is preserved
/// so the listing is stable and deterministic. If the full list would
/// approach Discord's message-length limit, rendering stops early and a
/// trailing count of omitted prompts is appended.
fn render_prompts_list(store: &PromptTemplateStore) -> Option<String> {
    let templates = store.templates();
    if templates.is_empty() {
        return None;
    }
    let mut out = String::from("Available prompts (use `#name` in a message):\n");
    let mut shown = 0;
    for t in templates {
        let line = format!("`#{}` — {}\n", t.name, t.description);
        if out.len() + line.len() > PROMPT_LIST_SOFT_LIMIT {
            break;
        }
        out.push_str(&line);
        shown += 1;
    }
    let remaining = templates.len() - shown;
    if remaining > 0 {
        use std::fmt::Write as _;
        let _ = write!(
            out,
            "\n… and {remaining} more not shown (message length limit)."
        );
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::{format_lifecycle_list, render_prompts_list};
    use jinn_domain::feat::context::prompt_template::PromptTemplateStore;
    use jinn_domain::protocol::PromptTemplate;
    use jinn_preferences_config::schemas::LifecycleCommand;
    use jinn_preferences_config::schemas::SessionLifecycle;

    fn lifecycle(name: &str, description: Option<&str>) -> SessionLifecycle {
        SessionLifecycle {
            name: name.to_owned(),
            description: description.map(str::to_owned),
            setup: Some(LifecycleCommand::Shell("true".to_owned())),
            teardown: None,
        }
    }

    fn template(name: &str, description: &str) -> PromptTemplate {
        PromptTemplate {
            name: name.to_owned(),
            description: description.to_owned(),
            body: String::new(),
        }
    }

    #[rstest::rstest]
    #[test]
    #[expect(clippy::expect_used, reason = "non-empty store is constructed above")]
    fn render_prompts_list_formats_name_and_description() {
        // Given a store with two prompts.
        let store = PromptTemplateStore::from_vec(vec![
            template("code-review", "Perform a thorough code review"),
            template("summarize", "Summarize the conversation"),
        ]);

        // When rendering.
        let out = render_prompts_list(&store).expect("non-empty store renders");

        // Then each prompt appears as `#name` — description.
        assert!(
            out.contains("`#code-review` — Perform a thorough code review"),
            "missing code-review line: {out:?}"
        );
        assert!(
            out.contains("`#summarize` — Summarize the conversation"),
            "missing summarize line: {out:?}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn render_prompts_list_empty_store_returns_none() {
        // Given an empty store.
        let store = PromptTemplateStore::from_vec(vec![]);

        // When rendering.
        let out = render_prompts_list(&store);

        // Then no text is produced.
        assert!(out.is_none());
    }

    #[rstest::rstest]
    #[test]
    #[expect(
        clippy::expect_used,
        reason = "store order and presence are the behavior under test"
    )]
    fn render_prompts_list_preserves_store_order() {
        // Given a store with a known insertion order.
        let store = PromptTemplateStore::from_vec(vec![
            template("first", "alpha"),
            template("second", "beta"),
            template("third", "gamma"),
        ]);

        // When rendering.
        let out = render_prompts_list(&store).expect("non-empty store renders");

        // Then the prompt lines appear in input order.
        let first = out.find("#first").expect("first present");
        let second = out.find("#second").expect("second present");
        let third = out.find("#third").expect("third present");
        assert!(first < second && second < third, "order wrong: {out:?}");
    }

    #[rstest::rstest]
    #[test]
    #[expect(clippy::expect_used, reason = "non-empty store is constructed above")]
    fn render_prompts_list_truncates_when_exceeding_soft_limit() {
        // Given a store whose rendered output would exceed Discord's
        // 2000-char limit — many prompts with long descriptions.
        let mut templates = Vec::new();
        for i in 0..200 {
            templates.push(template(
                &format!("prompt-{i:03}"),
                "lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do",
            ));
        }
        let store = PromptTemplateStore::from_vec(templates);

        // When rendering.
        let out = render_prompts_list(&store).expect("non-empty store renders");

        // Then the output stays under the hard limit.
        assert!(
            out.len() <= 2000,
            "rendered output exceeded Discord limit: {} bytes",
            out.len()
        );
        // And a truncation notice with a non-zero omitted count appears.
        assert!(
            out.contains("more not shown"),
            "missing truncation notice: {out:?}"
        );
        // And the very first prompt is still rendered (truncation is from
        // the tail, not the head).
        assert!(
            out.contains("`#prompt-000`"),
            "head prompt was truncated: {out:?}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn format_lifecycle_list_renders_blank_first() {
        // Given an empty lifecycle list.
        let lifecycles: Vec<SessionLifecycle> = vec![];

        // When formatting.
        let out = format_lifecycle_list(&lifecycles);

        // Then blank is rendered as option 1 with its description.
        assert!(
            out.starts_with("Available lifecycles:\n1. blank - New empty session"),
            "blank-first header wrong: {out:?}"
        );
        // And the reply instruction is appended.
        assert!(
            out.ends_with("\nReply with a number."),
            "missing reply instruction: {out:?}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn format_lifecycle_list_enumerates_explicit_entries() {
        // Given two explicit lifecycles.
        let lifecycles = vec![lifecycle("alpha", None), lifecycle("beta", None)];

        // When formatting.
        let out = format_lifecycle_list(&lifecycles);

        // Then blank stays option 1 and the two entries are numbered 2 and 3.
        assert!(
            out.contains("1. blank - New empty session"),
            "blank missing: {out:?}"
        );
        assert!(out.contains("2. alpha"), "alpha line wrong: {out:?}");
        assert!(out.contains("3. beta"), "beta line wrong: {out:?}");
    }

    #[rstest::rstest]
    #[test]
    fn format_lifecycle_list_appends_description_when_present() {
        // Given a lifecycle that declares a description.
        let lifecycles = vec![lifecycle("branch", Some("opens a fossil branch"))];

        // When formatting.
        let out = format_lifecycle_list(&lifecycles);

        // Then the entry line carries the description after the name.
        assert!(
            out.contains("2. branch - opens a fossil branch"),
            "description line wrong: {out:?}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn format_lifecycle_list_omits_description_when_absent() {
        // Given a lifecycle with no description.
        let lifecycles = vec![lifecycle("plain", None)];

        // When formatting.
        let out = format_lifecycle_list(&lifecycles);

        // Then the entry line is just the number and name, no trailing dash.
        assert!(
            out.contains("2. plain\n"),
            "expected bare `2. plain` line, got: {out:?}"
        );
        assert!(
            !out.contains("2. plain -"),
            "unexpected description separator on description-less entry: {out:?}"
        );
    }
}
