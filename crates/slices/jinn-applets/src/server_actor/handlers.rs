//! Message handlers for [`AppletsServerActor`] — the lazy-bind transition.
//!
//! The flow is the actor's whole job:
//!
//! 1. Dormant phase: canonicalize the applets root, bind the tokio
//!    listener (port `0` resolved here), start the axum task.
//! 2. Live phase: read back the resolved bind — the listener is never
//!    restarted, so every open of any app lands on the same URL base.
//!
//! Both the store actor's ensure ask and the picker's wake command land
//! on the same [`establish`] — the only two paths that may create the
//! listener — and every transition out of dormant announces the
//! serving fact, so the picker's footer tracks the live address.
//!
//! A bind failure travels as a refused outcome, not an actor crash: the
//! malformed-bind case already aborted launch at activation, so reaching
//! a runtime bind failure means a taken port or an OS refusal — worth
//! reporting on the ask, not worth tearing down the slice.

use std::net::SocketAddr;
use std::path::Path;

use marked_path::{Absolute, MarkedPathBuf};
use trouper::context::MsgCtx;

use crate::server_actor::{EnsureServing, EnsureServingOutcome, ServingState};

/// Handles an ensure ask: bind on the first ask, read back on the rest.
///
/// The first bind is a serving transition and announces the fact on the
/// bus — an applet opened before any picker open must still leave the
/// picker's footer correct when it next opens.
pub(crate) async fn handle_ensure_serving(
    actor: &mut crate::server_actor::AppletsServerActor,
    msg: &EnsureServing,
    ctx: &mut MsgCtx<'_>,
) {
    if let Some(state) = actor.serving.as_ref() {
        // Already serving: the resolved bind is the answer. The
        // apps_root argument is advisory — the server never re-targets
        // through this path (a bind change goes through the reload
        // handler's rebind instead). No fact is published: the phase
        // did not change.
        ctx.reply(EnsureServingOutcome::serving(state.bind));
        return;
    }

    match establish(actor.bind, Path::new(msg.apps_root.as_str())).await {
        Ok((state, resolved)) => {
            actor.serving = Some(state);
            actor.publish_serving(Some(resolved)).await;
            ctx.reply(EnsureServingOutcome::serving(resolved));
        }
        Err(error) => {
            actor.publish_serving(None).await;
            ctx.reply(EnsureServingOutcome::refused(error));
        }
    }
}

/// Establishes the serving state at `bind` over `root`: canonicalize the
/// root, bind the listener, start the serve task.
///
/// Port 0 asks the OS here, at the first bind, so the URL an opener
/// builds is the truth. The refusal sentences are the open path's
/// observable failure text — keep them stable.
///
/// # Errors
///
/// Returns the failure sentence on an unresolvable root, a bind
/// failure, or an unreadable OS-assigned port.
pub(crate) async fn establish(
    bind: SocketAddr,
    root: &Path,
) -> Result<(ServingState, SocketAddr), String> {
    // Canonicalize the root before serving from it: every handler read
    // resolves under this path, and the canonical, classic-spelled form
    // is the one the containment check anchors to.
    let root = canonical_root(root)
        .map_err(|error| format!("failed to resolve the applets directory: {error}"))?;

    let (listener, resolved) = bind_listener(bind)
        .await
        .map_err(|e| format!("failed to bind {bind}: {e}"))?;

    Ok((serve(listener, &root, resolved), resolved))
}

/// Binds a listener at `bind`, resolving the OS-assigned port.
///
/// # Errors
///
/// Returns the bind or local-address failure sentence.
pub(crate) async fn bind_listener(
    bind: SocketAddr,
) -> Result<(tokio::net::TcpListener, SocketAddr), String> {
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|e| format!("{e}"))?;
    let resolved: SocketAddr = listener
        .local_addr()
        .map_err(|e| format!("bound {bind} but the OS-assigned port was unreadable: {e}"))?;
    Ok((listener, resolved))
}

/// Starts the serve task for `listener` over `root`, held on the
/// returned state.
///
/// The task lives until the state is dropped (jinn exits → listener
/// closes); a rebind aborts it explicitly so the old port frees — a
/// dropped listener alone does not end an axum serve task.
pub(crate) fn serve(
    listener: tokio::net::TcpListener,
    root: &MarkedPathBuf<Absolute>,
    resolved: SocketAddr,
) -> ServingState {
    let shared = crate::server_actor::shared_for(root.clone());
    let serve_task = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, crate::server::router_arc(shared)).await {
            tracing::error!("applets server task failed: {e}");
        }
    });
    ServingState {
        bind: resolved,
        serve_task,
    }
}

/// Canonicalizes `path` into the typed, classic-spelled form every
/// consumer reads — `CanonicalPath` canonicalizes, proves existence, and
/// strips the Windows verbatim prefix, and `into_marked` hands the
/// pipeline a `MarkedPathBuf<Absolute>`.
///
/// # Errors
///
/// Propagates the [`marked_path::PathError`] — `Io` when the path does
/// not exist or cannot be read.
fn canonical_root(path: &Path) -> Result<MarkedPathBuf<Absolute>, marked_path::PathError> {
    marked_path::CanonicalPath::from_path(path).map(marked_path::CanonicalPath::into_marked)
}
