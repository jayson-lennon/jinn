//! Message handlers for [`CanvasActor`] — the serve transitions.
//!
//! The serve flow is the actor's whole job:
//!
//! 1. Validate the artifact (exists, canonicalize) and the bind
//!    (parses as a host:port).
//! 2. First serve: bind the tokio listener (port `0` resolved here),
//!    arm the directory watcher, start the axum task.
//! 3. Every serve: swap the artifact and the watcher on the live
//!    server, broadcast reload, publish the serving status.
//!
//! Domain refusals travel as `ok: false` outcomes — the actor answers
//! every command it receives — so the tool caller never confuses a
//! validation failure with a transport one.

use std::net::SocketAddr;

use jinn_canvas_msg::CanvasCommand;
use jinn_canvas_msg::CanvasMetricsDrainOutcome;
use jinn_canvas_msg::CanvasServeOutcome;
use marked_path::{Absolute, MarkedPathAccess, MarkedPathBuf};
use trouper::context::MsgCtx;

use crate::canvas_actor::CanvasActor;
use crate::canvas_actor::ServingState;
use crate::errors::CanvasServeError;
use crate::server;
use crate::watcher;

/// Why a serve was refused: the rendered sentence the reply and the
/// dashboard note both carry.
pub(crate) struct ServeRefusal {
    /// The rendered cause-plus-fix sentence.
    pub message: String,
}

impl ServeRefusal {
    fn new(error: CanvasServeError, detail: String) -> Self {
        Self {
            message: render_failure(error, detail),
        }
    }
}

/// Handles a serve ask: validate, act on the server state, then publish
/// the row status from the actor (the acting halves touch only that
/// state, so the publication borrows the actor alone).
pub(crate) async fn handle_serve(
    actor: &mut CanvasActor,
    msg: &CanvasCommand,
    ctx: &mut MsgCtx<'_>,
) {
    let request = match validate(actor.bind, std::path::Path::new(msg.path.as_str())) {
        Ok(request) => request,
        Err(refusal) => {
            actor.publish_serve_failed(&refusal.message).await;
            ctx.reply(CanvasServeOutcome::refused(refusal.message));
            return;
        }
    };

    // Route on the phase without holding a borrow of `actor.server`:
    // the live branch's swap touches the state, the fresh branch's bind
    // touches nothing, and the publication below borrows the actor
    // alone. `ServingOutcome` carries the new state when one was born.
    // The canonical artifact binding outlives both branches so the
    // dashboard status publishes the same spelling the outcome carries.
    let ServeRequest { bind, artifact } = request;
    let live = actor.server.is_some();
    let outcome = if live {
        let Some(state) = actor.server.as_mut() else {
            // `live` was read one statement above.
            return;
        };
        Ok((retarget(state, &artifact), None))
    } else {
        establish(bind, &artifact)
            .await
            .map(|(outcome, state)| (outcome, Some(state)))
    };

    match outcome {
        Ok((outcome, new_state)) => {
            if let Some(state) = new_state {
                actor.server = Some(state);
            }
            // The status rides the outcome's own path binding — the same
            // typed path `CanvasServeOutcome::served` formatted — so the
            // row and the tool result can never disagree, spelling
            // included.
            actor
                .publish_serving(&outcome.url, artifact.as_path())
                .await;
            ctx.reply(outcome);
        }
        Err(refusal) => {
            actor.publish_serve_failed(&refusal.message).await;
            ctx.reply(CanvasServeOutcome::refused(refusal.message));
        }
    }
}

/// Handles a drain ask: every accumulated metric event, oldest first.
///
/// The dormant phase (nothing ever served) answers with an empty
/// outcome — "no activity" is a normal answer, never an error, so the
/// asking message goes out unmodified rather than being treated as a
/// failure.
///
/// Also the actor's heartbeat (the drain rides every user message), so
/// this is where a pending bind whose pageholders have all gone
/// applies.
pub(crate) async fn handle_drain_metrics(actor: &mut CanvasActor, ctx: &mut MsgCtx<'_>) {
    let events = match actor.server.as_ref() {
        Some(state) => state
            .shared
            .drain_metrics()
            .into_iter()
            .map(|entry| jinn_canvas_msg::CanvasMetricEvent {
                event: entry.event,
                received_at: entry.received_at.to_string(),
                artifact: entry.artifact,
            })
            .collect(),
        None => Vec::new(),
    };
    ctx.reply(CanvasMetricsDrainOutcome { events });
    apply_pending_bind_when_quiet(actor).await;
}

/// A validated serve request.
struct ServeRequest {
    /// The configured bind, taken as configured — the operator chooses
    /// the interface and owns the endpoint's security.
    bind: SocketAddr,
    /// The canonical artifact path — absolute, symlink-resolved, spelled
    /// with the platform's classic separators.
    artifact: MarkedPathBuf<Absolute>,
}

/// Validates the artifact (exists, canonicalizes) and the bind
/// (parses). Pure checks — no actor state touched, no server started.
fn validate(bind: SocketAddr, path: &std::path::Path) -> Result<ServeRequest, ServeRefusal> {
    // Artifact: must be an existing file, canonicalized before storing —
    // the watcher re-arm, the injection, and the status line all read the
    // canonical form, and the typed path strips the Windows verbatim
    // prefix canonicalization produces, so every consumer sees the
    // classic spelling a user recognizes.
    if !path.is_file() {
        return Err(ServeRefusal::new(
            CanvasServeError::ArtifactMissing,
            format!(
                "the artifact must be an existing HTML file: {}",
                path.display()
            ),
        ));
    }
    // Canonicalize through the shared helper — the watcher parent, the
    // file read, the status line, and the outcome all read the classic
    // spelling a user recognizes.
    let artifact = match server::canonical_artifact(path) {
        Ok(artifact) => artifact,
        Err(e) => {
            return Err(ServeRefusal::new(
                CanvasServeError::ArtifactRead,
                format!(
                    "failed to resolve the artifact path: {} ({e})",
                    path.display()
                ),
            ));
        }
    };

    Ok(ServeRequest { bind, artifact })
}

/// First serve: bind the listener, arm the watcher, start the server
/// task. Returns the outcome plus the serving state to store — the
/// caller owns the actor, so the state lands there.
async fn establish(
    bind: SocketAddr,
    artifact: &MarkedPathBuf<Absolute>,
) -> Result<(CanvasServeOutcome, ServingState), ServeRefusal> {
    // Port 0 asks the OS here, at serve time, so the URL in the reply is
    // the truth.
    let listener = match tokio::net::TcpListener::bind(bind).await {
        Ok(listener) => listener,
        Err(e) => {
            return Err(ServeRefusal::new(
                CanvasServeError::Bind,
                format!("failed to bind {bind}: {e}"),
            ));
        }
    };
    let resolved = match listener.local_addr() {
        Ok(resolved) => resolved,
        Err(e) => {
            return Err(ServeRefusal::new(
                CanvasServeError::Bind,
                format!("bound {bind} but the OS-assigned port was unreadable: {e}"),
            ));
        }
    };

    let shared = server::shared_for(artifact);

    // Arm the watcher before the listener accepts: a browser that loads
    // the page before the watch exists would miss the first edit's
    // reload. Failure degrades to no-live-reload — the artifact still
    // serves — surfaced as a warning, not a refusal.
    arm_watcher(&shared, artifact);

    let shared_for_task = shared.clone();
    // The task is held on the state so a pending-bind rebind can end it
    // precisely: aborting the task closes the listener. Detached, the
    // old listener would keep the old port open forever and a rebind
    // could never reclaim a fixed port.
    let serve_task = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, server::router(shared_for_task)).await {
            tracing::error!("canvas server task failed: {e}");
        }
    });

    let url = format!("http://{resolved}");
    let state = ServingState {
        bind: resolved,
        shared,
        serve_task,
    };
    Ok((
        CanvasServeOutcome::served(url, resolved, artifact.as_path()),
        state,
    ))
}

/// Later serves: swap the artifact and re-arm the watcher on the live
/// server; the bind never changes, so the URL a user already has keeps
/// working. Synchronous: the swap touches locks and broadcasts only.
fn retarget(state: &mut ServingState, artifact: &MarkedPathBuf<Absolute>) -> CanvasServeOutcome {
    let url = format!("http://{}", state.bind);

    // Swap the artifact first so a reload arriving mid-swap serves the
    // new file; then re-arm the watcher onto the new directory. The old
    // watcher drops with the swap: watchers must be replaced, never
    // accumulated, or every edit broadcasts once per accumulated
    // watcher.
    state.shared.set_artifact(artifact.clone());
    arm_watcher(&state.shared, artifact);

    // Every connected browser follows the swap; a lost receiver means
    // no browser is connected.
    let _ = state.shared.reload_tx.send(());

    CanvasServeOutcome::served(url, state.bind, artifact.as_path())
}

/// Establishes the directory watch for `artifact`'s parent, storing the
/// handle in `shared`'s swap slot.
fn arm_watcher(shared: &server::ServerShared, artifact: &MarkedPathBuf<Absolute>) {
    let Some(dir) = artifact.parent() else {
        return;
    };
    match watcher::establish(dir.as_path(), shared.reload_tx.clone()) {
        Ok(handle) => {
            shared.swap_watcher(handle);
        }
        Err(report) => {
            tracing::warn!("canvas watcher establishment failed: {report}");
        }
    }
}

/// Live-config landing: re-read `[canvas] bind` from the layer the
/// actor carries.
///
/// Dormant (no listener yet): the new bind is adopted and the next
/// serve uses it. A re-read that fails to parse keeps the current bind
/// and notes the refusal on the row — the same keep-old behavior a
/// refused config reload gives every other reader.
/// Serving: the listener keeps running — a browser holds the page — and
/// a changed bind parks as pending. The actor's drain tick applies it
/// once no browser holds a page (zero ws clients), so no served URL a
/// user holds ever breaks.
pub(crate) async fn handle_reload(actor: &mut CanvasActor) {
    let parsed = actor
        .config
        .get::<jinn_preferences_config::schemas::CanvasConfig>()
        .ok()
        .and_then(|config| config.parse_bind().ok());
    let Some(bind) = parsed else {
        actor
            .publish_status(
                Some("canvas bind re-read refused; the previous bind stays live".to_owned()),
                Some(jinn_slices::NoteTone::Error),
            )
            .await;
        return;
    };
    if actor.server.is_some() {
        // Live listener: keep serving; park a changed bind for the
        // quiet boundary. A pending bind is superseded wholesale — the
        // newest edit wins.
        if bind != actor.bind && actor.pending_bind.as_ref() != Some(&bind) {
            actor.pending_bind = Some(bind);
            actor
                .publish_status(
                    Some(format!(
                        "canvas bind changed to {bind} — pending; applies when the current page closes"
                    )),
                    Some(jinn_slices::NoteTone::Warning),
                )
                .await;
        }
        return;
    }
    if bind != actor.bind {
        actor.bind = bind;
        actor.publish_offline().await;
    }
}

/// Applies a pending bind once nothing holds the old URL.
///
/// The drain tick calls this: zero ws clients means no browser has the
/// old page open, so rebinding breaks nobody. A rebind that fails to
/// bind (port taken) refuses keep-old — the old listener dies with the
/// server it was attached to, so this tears the whole serving phase
/// down to dormant with an error note rather than half-applying.
async fn apply_pending_bind_when_quiet(actor: &mut CanvasActor) {
    let Some(pending) = actor.pending_bind else {
        return;
    };
    // Snapshot what the rebind needs before mutating the actor: the
    // artifact path outlives the serving state it came from.
    let artifact = match actor.server.as_ref() {
        Some(state) => {
            if state.shared.client_count() > 0 {
                return;
            }
            state.shared.artifact()
        }
        // The serving phase ended some other way; the pending bind is
        // simply the next bind.
        None => {
            actor.bind = pending;
            actor.pending_bind = None;
            return;
        }
    };
    // Quiet: end the old serve task — aborting it closes the old
    // listener and frees the old port — then bind the new one.
    let old_task = actor.server.take().map(|state| {
        state.serve_task.abort();
        state.shared
    });
    drop(old_task);
    match establish(pending, &artifact).await {
        Ok((_outcome, new_state)) => {
            actor.bind = new_state.bind;
            actor.pending_bind = None;
            actor.server = Some(new_state);
            actor
                .publish_status(
                    Some(format!(
                        "canvas rebound to {} — the pending bind change applied",
                        actor.bind
                    )),
                    None,
                )
                .await;
        }
        Err(refusal) => {
            // Keep-old is impossible (the old listener is gone), so
            // surface the failure and go dormant; the next serve tries
            // the new bind fresh.
            actor.bind = pending;
            actor.pending_bind = None;
            actor.publish_serve_failed(&refusal.message).await;
        }
    }
}

/// Renders a refusal sentence: the cause, then the variant's fix.
///
/// Consumes its arguments: the rendered sentence is the whole payload
/// the refusal carries.
#[expect(
    clippy::needless_pass_by_value,
    reason = "consumes the parts it renders; callers have nothing left to do with them"
)]
fn render_failure(error: CanvasServeError, detail: String) -> String {
    format!("canvas serve failed: {detail} — fix: {}", error.fix())
}
