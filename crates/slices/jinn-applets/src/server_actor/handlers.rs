//! Message handlers for [`AppletsServerActor`] — the lazy-bind transition.
//!
//! The flow is the actor's whole job:
//!
//! 1. Dormant phase: canonicalize the applets root, bind the tokio
//!    listener (port `0` resolved here), start the axum task.
//! 2. Live phase: read back the resolved bind — the listener is never
//!    restarted, so every open of any app lands on the same URL base.
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
pub(crate) async fn handle_ensure_serving(
    actor: &mut crate::server_actor::AppletsServerActor,
    msg: &EnsureServing,
    ctx: &mut MsgCtx<'_>,
) {
    if let Some(state) = actor.serving.as_ref() {
        // Already serving: the resolved bind is the answer. The
        // apps_root argument is advisory — the server never re-targets,
        // by contract (one bind per process).
        ctx.reply(EnsureServingOutcome::serving(state.bind));
        return;
    }

    // Canonicalize the root before serving from it: every handler read
    // resolves under this path, and the canonical, classic-spelled form
    // is the one the containment check anchors to.
    let root = match canonical_root(Path::new(msg.apps_root.as_str())) {
        Ok(root) => root,
        Err(error) => {
            ctx.reply(EnsureServingOutcome::refused(format!(
                "failed to resolve the applets directory: {error}"
            )));
            return;
        }
    };

    // Port 0 asks the OS here, at the first open, so the URL an opener
    // builds is the truth.
    let listener = match tokio::net::TcpListener::bind(actor.bind).await {
        Ok(listener) => listener,
        Err(e) => {
            ctx.reply(EnsureServingOutcome::refused(format!(
                "failed to bind {}: {e}",
                actor.bind
            )));
            return;
        }
    };
    let resolved: SocketAddr = match listener.local_addr() {
        Ok(resolved) => resolved,
        Err(e) => {
            ctx.reply(EnsureServingOutcome::refused(format!(
                "bound {} but the OS-assigned port was unreadable: {e}",
                actor.bind
            )));
            return;
        }
    };

    let shared = crate::server_actor::shared_for(root);

    // The task is detached deliberately: it lives as long as the
    // process, which is the server-lifetime contract (jinn exits →
    // listener closes). Dropping the JoinHandle detaches, not aborts.
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, crate::server::router_arc(shared)).await {
            tracing::error!("applets server task failed: {e}");
        }
    });

    actor.serving = Some(ServingState { bind: resolved });
    ctx.reply(EnsureServingOutcome::serving(resolved));
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
