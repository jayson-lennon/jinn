//! The builtin smolvm MCP server — one sandboxed Linux microVM per
//! enabling session.
//!
//! `jinn mcp smolvm` boots one ephemeral machine through the
//! [`smolmachines`] SDK, mounts the enabling session's working directory
//! read-write at `/workspace` inside the guest, then speaks JSON-RPC over
//! stdio so jinn's MCP subsystem drives it as a child process exactly like
//! any user-configured server. Two tools:
//!
//! - `vm_run` — run a shell command in the guest, capture
//!   stdout/stderr/exit code.
//! - `vm_info` — the machine manifest plus live guest facts.
//!
//! # Shape
//!
//! - [`serve`] — the entry point: resolves the manifest, gates on host
//!   availability, boots the machine, serves until the client disconnects.
//! - `manifest` — the on-disk manifest profile and its resolution.
//! - `backend` — the smolmachines-backed machine behind a trait seam.
//! - `server` — the rmcp `ServerHandler` exposing the two tools.
//!
//! # Manifests live on disk
//!
//! The machine is never launched from in-memory config: every boot reads
//! a manifest file from disk — the `--smolfile` override, else the
//! reconciler-seeded `~/.config/jinn/smolvm/alpine.toml`. A missing file
//! is a fatal startup error naming the path.
//!
//! # Stdout discipline
//!
//! Stdout is the MCP transport. Nothing in this crate may print to stdout;
//! diagnostics go through `tracing` (captured to jinn's headless log when
//! run as a child) or stderr for fatal startup errors and engine-fetch
//! progress.
//!
//! The `--cwd` and `--smolfile` flag values are the argv boundary: they
//! arrive raw (relative values rejected loudly in [`resolve_cwd`] and the
//! manifest resolver) and are typed into `MarkedPathBuf<Absolute>` before
//! anything consumes them.
#![expect(
    clippy::disallowed_types,
    reason = "argv boundary: raw --cwd/--smolfile flag values are typed in resolve_cwd and resolve_manifest before any jinn logic consumes them"
)]
#![warn(missing_docs)]

pub mod backend;
pub mod boot_log;
pub mod manifest;
pub mod server;
pub mod tools;

use std::path::PathBuf;
use std::sync::Arc;

use error_stack::{Report, ResultExt};
use marked_path::{Absolute, MarkedPathAccess, MarkedPathBuf};
use wherror::Error;

/// Fatal errors while bringing the smolvm server up.
#[derive(Debug, Error)]
#[error(debug)]
pub struct ServeError;

/// Arguments for [`serve`].
#[derive(Debug, Default)]
pub struct SmolvmServeArgs {
    /// The host directory mounted read-write at `/workspace` in the guest.
    /// Required: the shipped MCP entry expands `<session_cwd>` into it.
    pub cwd: Option<PathBuf>,
    /// Manifest override; `None` uses the jinn-managed seed
    /// (`~/.config/jinn/smolvm/alpine.toml`).
    pub smolfile: Option<PathBuf>,
}

/// Resolves the session working directory: the `--cwd` value, typed at the
/// argv boundary.
///
/// # Panics
///
/// Panics when `--cwd` is missing or names a relative path: the server is
/// a spawned child whose only feedback channel at startup is process death
/// plus stderr, so a malformed flag aborts the launch loudly rather than
/// silently mounting nothing (a `None` cwd would boot a machine with no
/// workspace at all — never the configured contract).
#[expect(
    clippy::panic,
    reason = "a missing or relative --cwd is user input at process startup, where aborting the spawn is the only feedback channel (the resolve_db_dir precedent)"
)]
#[must_use]
pub fn resolve_cwd(cwd: Option<PathBuf>) -> MarkedPathBuf<Absolute> {
    let raw = cwd.unwrap_or_else(|| {
        panic!("--cwd is required: the smolvm server mounts the session working directory")
    });
    MarkedPathBuf::<Absolute>::new(&raw).unwrap_or_else(|error| {
        panic!(
            "--cwd must be an absolute path: {}: {error}",
            raw.to_string_lossy()
        )
    })
}

/// Serves the smolvm MCP server over stdio until the client disconnects.
///
/// # Errors
///
/// Returns an error if the manifest cannot be resolved or parsed, the host
/// cannot run machines, the engine cannot be resolved or fetched, the
/// machine fails to boot or run its init list, or the stdio transport
/// fails. The machine is deleted on the way out (best-effort).
pub async fn serve(args: SmolvmServeArgs) -> Result<(), Report<ServeError>> {
    let cwd = resolve_cwd(args.cwd);

    // The session working directory must exist before the boot sequence
    // mounts it: a deleted worktree should die here, naming the path,
    // not inside the engine's mount handling.
    if !cwd.as_marked_path().as_path().exists() {
        let report = Report::new(ServeError)
            .attach(format!(
                "session working directory does not exist: {}",
                cwd.as_marked_path().as_path().display()
            ))
            .attach(
                "the smolvm server mounts this directory at /workspace; it cannot boot without it",
            );
        emit_fatal(&report);
        return Err(report);
    }

    // Manifest resolution: the file must exist on disk — the machine is
    // never launched from in-memory config. The engine-fetch progress
    // note goes to stderr first, so a cold machine explains its delay.
    let (manifest_path, manifest, manifest_contents) =
        manifest::resolve_manifest(args.smolfile.as_deref()).map_err(|report| {
            let wrapped = report.change_context(ServeError);
            emit_fatal(&wrapped);
            wrapped
        })?;

    // The boot progress lines go to stderr from here on; the picker's
    // stderr debounce picks them up live while the connection is coming up.
    boot_log::phase(&format!(
        "mounting {} at /workspace (manifest {})",
        cwd.as_marked_path().as_path().display(),
        manifest_path.as_marked_path().as_path().display()
    ));

    tracing::info!(
        manifest = %manifest_path.as_marked_path().as_path().display(),
        image = %manifest.image,
        "booting the session machine"
    );

    // Boot on the blocking pool: every SDK call blocks, and the async
    // server must not stall the runtime during a cold engine fetch.
    let boot_cwd = cwd.clone();
    let boot_manifest = manifest.clone();
    let boot = tokio::task::spawn_blocking(move || {
        backend::borrow_engine_assets()?;
        backend::SmolMachineBackend::boot(&boot_cwd.as_marked_path(), &boot_manifest)
    })
    .await
    .change_context(ServeError)
    .attach("machine boot task panicked")
    .and_then(|inner| inner.change_context(ServeError));

    let backend = match boot {
        Ok(backend) => backend,
        Err(report) => {
            let wrapped = report.change_context(ServeError);
            emit_fatal(&wrapped);
            return Err(wrapped);
        }
    };

    let server = server::SmolvmMcpServer::new(
        Arc::new(backend),
        manifest_path
            .as_marked_path()
            .as_path()
            .to_string_lossy()
            .into_owned(),
        manifest_contents,
    );

    tracing::info!("smolvm MCP server listening on stdio");

    let transport = rmcp::transport::io::stdio();
    let running = rmcp::service::ServiceExt::serve(server, transport)
        .await
        .change_context(ServeError)
        .attach("MCP stdio handshake failed")?;

    // Completes when the transport closes (client disconnect / stdin EOF).
    let _ = running.waiting().await;

    tracing::info!("smolvm MCP server shutting down");

    // The machine dies here — the ephemeral lifetime. `serve` consumed the
    // server, so the backend drops when the rmcp task tree does; its Drop
    // deletes the machine on a detached blocking thread. A SIGKILLed jinn
    // leaves the stopped machine record to the engine's own bookkeeping
    // (no orphan reaper — see the spec's anti-goals).
    Ok(())
}

/// Renders a fatal startup failure to stderr — the only feedback channel
/// of a spawned child before the transport exists.
fn emit_fatal(report: &Report<ServeError>) {
    // Rendered via format! into boot_log::fatal: the debug render carries
    // the full attachment chain — the only place the cause names itself.
    boot_log::fatal(&format!("{report:?}"));
}
