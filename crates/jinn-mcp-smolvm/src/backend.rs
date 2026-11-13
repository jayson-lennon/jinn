//! The smolmachines-backed backend: one ephemeral machine, booted at
//! server startup, served until the client disconnects.
//!
//! Boot sequence (each step is the fatal-startup channel — stderr + a
//! non-zero exit — when it fails):
//!
//! 1. availability check: the host hypervisor and a resolvable engine
//!    platform, named by [`smolmachines::local_availability`]
//! 2. engine assets: borrow an installed smolvm of this SDK's engine
//!    generation when one is on `PATH`; without one, the SDK fetches its
//!    pinned release into its cache on first create (progress on stderr)
//! 3. machine: ephemeral (`persistent` left off — the disk dies with the
//!    machine), resources and image from the manifest, the enabling
//!    session's working directory mounted read-write at `/workspace`
//! 4. first-start init: the manifest's command list, run once as root in
//!    the guest; a failure here aborts startup (the toolchain the tools
//!    document would otherwise be a lie)
//!
//! Every SDK call blocks, so the async server drives it through
//! `spawn_blocking`.

use std::time::Instant;

use error_stack::{Report, ResultExt};
use marked_path::{Absolute, MarkedPath, MarkedPathAccess};
use smolmachines::{Machine, Mount, RuntimeAssets, configure_runtime_assets, local_availability};
use wherror::Error;

use crate::boot_log;
use crate::manifest::MachineManifest;

/// The fixed guest path the enabling session's working directory mounts at.
pub const GUEST_WORKSPACE: &str = "/workspace";

/// Error while booting or driving the machine.
#[derive(Debug, Error)]
#[error(debug)]
pub struct BackendError;

/// Exit code returned when the timeout killed the guest command — the GNU
/// `timeout` convention. Lives beside the backend because the engine's
/// `--timeout` kill is what produces it.
pub const TIMEOUT_EXIT_CODE_LABEL: i32 = 124;

/// The guest-facing command surface one booted machine serves.
///
/// The seam the tool handlers depend on: tests drive a fake, production
/// drives the real machine. Blocking calls, run on the blocking pool by
/// the caller.
pub trait SmolvmBackend: Send + Sync {
    /// Runs `argv` in the guest and captures the result.
    ///
    /// # Errors
    ///
    /// Returns [`Report<BackendError>`] when the guest could not run the
    /// command at all (machine gone, agent unreachable); the command's own
    /// non-zero exit is a *result*, not an error.
    fn exec(
        &self,
        argv: Vec<String>,
        options: ExecOptions,
    ) -> Result<ExecOutcome, Report<BackendError>>;

    /// The guest-facing description of this machine, for tool-level text.
    fn guest_image(&self) -> String;
}

/// Per-exec options the tools control.
#[derive(Debug, Clone, Default)]
pub struct ExecOptions {
    /// Working directory inside the guest.
    pub workdir: Option<String>,
    /// Give up after this long; the engine kills the command.
    pub timeout: Option<std::time::Duration>,
}

/// What one guest command produced.
#[derive(Debug, Clone)]
pub struct ExecOutcome {
    /// Everything the command wrote to stdout.
    pub stdout: String,
    /// Everything the command wrote to stderr.
    pub stderr: String,
    /// The process exit code (124 when the timeout killed it, the engine's
    /// GNU-timeout convention).
    pub exit_code: i32,
}

/// A booted [`Machine`], behind the backend seam.
pub struct SmolMachineBackend {
    machine: Machine,
    image: String,
}

impl SmolvmBackend for SmolMachineBackend {
    fn exec(
        &self,
        argv: Vec<String>,
        options: ExecOptions,
    ) -> Result<ExecOutcome, Report<BackendError>> {
        let mut exec_options = smolmachines::ExecOptions::new();
        if let Some(workdir) = options.workdir {
            exec_options = exec_options.workdir(workdir);
        }
        if let Some(timeout) = options.timeout {
            exec_options = exec_options.timeout(timeout);
        }

        let result = self
            .machine
            .exec_with(argv, exec_options)
            .change_context(BackendError)
            .attach("guest exec failed")?;

        Ok(ExecOutcome {
            stdout: result.stdout_utf8().into_owned(),
            stderr: result.stderr_utf8().into_owned(),
            exit_code: result.exit_code,
        })
    }

    fn guest_image(&self) -> String {
        self.image.clone()
    }
}

impl SmolMachineBackend {
    /// Runs the boot sequence and returns the serving backend.
    ///
    /// # Errors
    ///
    /// Returns [`Report<BackendError>`] naming the failing step when the
    /// host lacks a hypervisor, the engine cannot be resolved or fetched,
    /// the session working directory has vanished, the machine fails to
    /// boot, or the manifest's init list fails in the guest.
    pub fn boot(
        cwd: &MarkedPath<Absolute>,
        manifest: &MachineManifest,
    ) -> Result<Self, Report<BackendError>> {
        let boot_started = Instant::now();
        availability_gate()?;
        resolve_engine()?;

        let machine = {
            // Per-session, collision-free: the server process id makes the
            // name unique across concurrent sessions and across a restart
            // of the same session.
            let name = format!("jinn-smolvm-{}", std::process::id());
            let mount = Mount::new(cwd.as_path().to_string_lossy(), GUEST_WORKSPACE);

            boot_log::machine_creating(&manifest.image);
            let created = Instant::now();
            let machine = Machine::builder(name)
                .image(&manifest.image)
                .cpus(manifest.cpus)
                .memory_mib(manifest.memory)
                .network(manifest.net)
                .mount(mount)
                .create()
                .change_context(BackendError)
                .attach("failed to create the machine")?;
            boot_log::machine_created(created);
            machine
        };

        boot_log::machine_starting();
        let start_started = Instant::now();
        machine
            .start()
            .change_context(BackendError)
            .attach("failed to start the machine")?;
        boot_log::machine_started(start_started);

        run_first_boot_init(&machine, manifest)?;

        boot_log::ready(boot_started);
        Ok(Self {
            machine,
            image: manifest.image.clone(),
        })
    }
}

impl Drop for SmolMachineBackend {
    /// Deletes the machine when the backend is dropped — the ephemeral
    /// lifetime: the VM lives and dies with the server process.
    ///
    /// The SDK calls block, so the delete runs on a detached blocking
    /// thread; the drop itself never stalls the async runtime. A failure
    /// (engine already gone, host mid-shutdown) is logged, not panicked:
    /// a leaked stopped machine record is harmless, and panicking in a
    /// drop during process teardown would be worse.
    fn drop(&mut self) {
        let machine = self.machine.clone();
        match std::thread::Builder::new()
            .name("smolvm-delete".into())
            .spawn(move || {
                if let Err(error) = machine.delete() {
                    tracing::warn!(%error, "failed to delete the ephemeral machine on shutdown");
                }
            }) {
            // The handle is dropped: the delete thread runs to completion
            // on its own while the process tears down.
            Ok(_handle) => {}
            Err(error) => {
                tracing::warn!(%error, "could not spawn the machine-delete thread");
            }
        }
    }
}

/// The pre-boot availability gate: names the missing hypervisor piece
/// before any engine download starts.
///
/// # Errors
///
/// Returns [`Report<BackendError>`] carrying the SDK's own message when
/// the host cannot run local machines at all.
fn availability_gate() -> Result<(), Report<BackendError>> {
    local_availability().change_context(BackendError).attach(
        "this host cannot run local machines (no hypervisor or engine platform); \
             the smolvm server needs hardware virtualization",
    )
}

/// Runs the manifest's init list in the guest, once, as root.
///
/// A failing init aborts startup: the tools' description promises bash,
/// git, and python3, and a machine that could not install them must not
/// serve those tools as if it had them.
///
/// # Errors
///
/// Returns [`Report<BackendError>`] naming the failing command and its
/// captured output when any init command exits non-zero.
fn run_first_boot_init(
    machine: &Machine,
    manifest: &MachineManifest,
) -> Result<(), Report<BackendError>> {
    boot_log::init_starting(manifest.init.len());
    for (index, command) in manifest.init.iter().enumerate() {
        boot_log::init_command_starting(command, index + 1, manifest.init.len());
        let command_started = Instant::now();
        let result = machine
            .exec(["sh", "-c", command.as_str()])
            .change_context(BackendError)
            .attach(format!("init command failed: {command}"))?;
        boot_log::init_command_done(command, index + 1, manifest.init.len(), command_started);
        if result.exit_code != 0 {
            return Err(Report::new(BackendError)
                .attach(format!(
                    "init command `{}` exited {}",
                    command, result.exit_code
                ))
                .attach(format!("stdout: {}", result.stdout_utf8()))
                .attach(format!("stderr: {}", result.stderr_utf8())));
        }
    }
    Ok(())
}

/// Borrows an installed smolvm of this SDK's engine generation when one is
/// on `PATH`, pointing the engine at its boot helper and libraries.
///
/// Without this, a boot in an embedded process re-executes *this* binary —
/// which does not know how to be a VM — and fails. With no compatible
/// install on `PATH`, nothing is configured here and the SDK fetches its
/// pinned engine release into its cache on first create.
///
/// # Errors
///
/// Returns [`Report<BackendError>`] when the asset configuration itself
/// fails (unresolvable or wrong-shaped paths).
pub fn borrow_engine_assets() -> Result<bool, Report<BackendError>> {
    match RuntimeAssets::from_path_lookup() {
        Some(assets) => {
            configure_runtime_assets(assets)
                .change_context(BackendError)
                .attach("failed to configure the engine's runtime assets")?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Resolves the engine binary before the machine is created: a compatible
/// install on `PATH` wins; otherwise the SDK's pinned release is fetched
/// into its cache now — inside an announced phase, with a heartbeat, so
/// the picker preview shows the download rather than a silent hang.
///
/// # Errors
///
/// Returns [`Report<BackendError>`] when the SDK's engine resolution or
/// fetch fails (network unreachable, release missing for this platform).
fn resolve_engine() -> Result<(), Report<BackendError>> {
    if borrow_engine_assets()? {
        boot_log::engine_on_path();
        return Ok(());
    }

    // Not on PATH: the SDK will fetch its pinned release. The SDK caches
    // after one fetch, so this phase runs once per machine; the heartbeat
    // keeps the picker alive across the ~37 MB download.
    boot_log::engine_fetching();
    let started = Instant::now();
    let mut heartbeat = boot_log::heartbeat(format!(
        "downloading VM engine smolvm {} (~37 MB, first run only)",
        smolmachines::VERSION
    ));
    let fetched = smolmachines::engine_cli()
        .change_context(BackendError)
        .attach("failed to resolve the VM engine (fetch or lookup)");
    // Stop the heartbeat before announcing the outcome, so the completion
    // line is the tail the picker shows.
    heartbeat.stop_and_take_ticker();
    fetched?;
    boot_log::engine_fetched(started);
    Ok(())
}
