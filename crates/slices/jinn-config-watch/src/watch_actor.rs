//! The config-watch actor — the loop that turns an edit into events.
//!
//! Notify events (the config dir, the `--config` override's dir when it
//! differs, and each resource dir) land on a dedicated channel; a spawned
//! loop debounces bursts and hands one settle per window to the actor. The
//! settle diffs content signatures against the last-seen ledger: only
//! content that actually changed is handled, which is what makes jinn's
//! own no-op writes publish nothing.
//!
//! The settle's handling order is the correctness story:
//!
//! 1. `jinn.toml` → [`ConfigLayer::reload_validated`] → only on success,
//!    publish [`JinnTomlReloaded`]. The swap lands *before* the publish,
//!    so every handler reads post-reload values.
//! 2. `providers.toml` → parse through the storage boundary → on success,
//!    publish [`ProvidersTomlReloaded`]; the handlers re-read and refuse
//!    on their own, and in-flight streams hold their own factories.
//! 3. `stream_rules.toml` → publish [`StreamRulesReloaded`]; the matcher
//!    actor's refuse-and-keep-old covers a bad file.
//! 4. Resource fingerprint change → publish [`ResourcesChanged`].
//!
//! A refused reload (malformed TOML, a section that fails validation, an
//! unparseable `providers.toml`) publishes a transient chat entry and an
//! error-toned dashboard note naming the failure; the previously installed
//! values stay live. That surfacing is deliberate: a silently ignored edit
//! reads exactly like a bug from the user's chair.

use std::sync::Arc;
use std::time::Duration;

use error_stack::{Report, ResultExt as _};
use jinn_boot_msg::JinnTomlReloaded;
use jinn_boot_msg::ProvidersTomlReloaded;
use jinn_boot_msg::ResourcesChanged;
use jinn_kernel::Services;
use jinn_kernel::common::actor_deps::BusPublish;
use jinn_kernel::common::state::State;
use jinn_session_history_msg::PushChatEntry;
use jinn_slices::NoteTone;
use jinn_slices::ServiceStatusUpdate;
use jinn_slices::bus::BusService;
use jinn_stream_rules_msg::StreamRulesReloaded;
use marked_path::Absolute;
use marked_path::MarkedPathAccess as _;
use marked_path::MarkedPathBuf;
use notify::Watcher as _;
use trouper::actor::ActorPath;
use trouper::actor::MsgHandler;
use trouper::actor::ServiceActor;
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

/// Why a watch could not be established.
#[derive(Debug, wherror::Error)]
#[error(debug)]
pub enum WatchError {
    /// A directory could not be watched (missing, permissions).
    Establish,
}

use crate::signature::SignatureLedger;

/// Events inside this window coalesce into one settle. Editors write
/// temp-file + rename, which lands as several events; the canvas
/// watcher's value is proven in-tree.
pub const DEBOUNCE: Duration = Duration::from_millis(250);

/// The config-watch actor's static trouper path.
pub const CONFIG_WATCH_PATH: &str = "config-watch";

/// The dashboard row this slice publishes under.
const ROW_NAME: &str = CONFIG_WATCH_PATH;

/// The files whose content is hashed per settle.
///
/// `jinn.toml` is wherever the run's `--config` resolved to (the default
/// preferences path when the flag is absent); `providers.toml` and
/// `stream_rules.toml` always sit in the standard app-config dir. When the
/// override lives elsewhere, its parent dir is watched in addition — the
/// hash ledger keyed by path makes the double coverage harmless.
#[derive(Debug, Clone)]
struct WatchedFiles {
    config_dir: MarkedPathBuf<Absolute>,
    jinn_toml: MarkedPathBuf<Absolute>,
    /// The override file's parent, watched only when it differs from
    /// `config_dir` — otherwise one save would arm two watch dirs.
    override_dir: Option<MarkedPathBuf<Absolute>>,
    providers_toml: MarkedPathBuf<Absolute>,
    stream_rules_toml: MarkedPathBuf<Absolute>,
}

impl WatchedFiles {
    /// Resolves the config files from the services.
    ///
    /// `jinn.toml` follows the layer: it is the run's resolved
    /// `--config` path carried on [`Services`], not a hardcoded
    /// `jinn.toml` under the app-config dir. The other two files stay
    /// standard — `--config` names a jinn.toml, nothing else.
    fn from_services(services: &Services) -> Self {
        let config_dir = services.paths.app_config_dir();
        let jinn_toml = services.resolved_config_path.clone();
        let stream_rules_toml = {
            #[expect(
                clippy::expect_used,
                reason = "a constant name join on an absolute base cannot fail"
            )]
            config_dir
                .join("stream_rules.toml")
                .expect("name join keeps the base absolute")
        };
        // The override dir matters only when it is not the standard dir:
        // inside it, one watch already covers the file and a second dir
        // watcher would deliver duplicate events for one save.
        let override_dir = jinn_toml
            .as_path()
            .parent()
            .map(|parent| {
                #[expect(
                    clippy::expect_used,
                    reason = "the parent of an absolute path is absolute"
                )]
                MarkedPathBuf::<Absolute>::new(parent)
                    .expect("the parent of an absolute path is absolute")
            })
            .filter(|parent| parent != &config_dir);
        Self {
            config_dir,
            jinn_toml,
            override_dir,
            providers_toml: services.paths.config_path(),
            stream_rules_toml,
        }
    }

    /// The watched parent directory (the three files are siblings).
    fn config_dir(&self) -> &MarkedPathBuf<Absolute> {
        &self.config_dir
    }

    /// The `--config` override's parent dir, when it is not the standard
    /// config dir.
    fn override_dir(&self) -> Option<&MarkedPathBuf<Absolute>> {
        self.override_dir.as_ref()
    }

    /// All three as one array, for ledger capture and the settle diff.
    fn all(&self) -> [MarkedPathBuf<Absolute>; 3] {
        [
            self.jinn_toml.clone(),
            self.providers_toml.clone(),
            self.stream_rules_toml.clone(),
        ]
    }
}

/// The config-watch actor.
///
/// The notify side runs on its own channel and loop (watcher threads can
/// never touch actor state); the actor receives one [`SettleTick`] per
/// settled window through its own mailbox, so all publication happens on
/// the actor — one publish path, one flush gate. The ledger lives behind
/// a mutex only because the spawn-time capture and the actor's diffs
/// are separate moments.
pub struct ConfigWatchActor {
    services: Services,
    /// Shared app state, for resolving where a refusal entry lands.
    state: State,
    ledger: parking_lot::Mutex<SignatureLedger>,
    files: WatchedFiles,
    resource_dirs: Vec<MarkedPathBuf<Absolute>>,
}

/// Self-addressed settle notification: the watcher loop completed one
/// debounce window with at least one write-shaped event. The diff inside
/// the handler decides whether anything actually changed.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, trouper::schema::Command)]
#[schema(description = "A debounced change settled; diff signatures and publish what changed.")]
pub struct SettleTick;

impl jinn_slices::BusMessage for SettleTick {}

impl ServiceActor for ConfigWatchActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "trait contract: init is never called (spawn uses init_with)"
    )]
    async fn init(_args: &trouper::json::Json) -> Result<Self, error_stack::Report<RegistryError>> {
        // Never called: the spawn helper injects the deps via `init_with`.
        Err(
            error_stack::IntoReport::into_report(RegistryError::InvalidSpec)
                .attach("ConfigWatchActor is spawned via init_with"),
        )
    }
}

impl BusPublish for ConfigWatchActor {
    fn bus(&self) -> &BusService {
        &self.services.bus
    }
}

/// Watch-establishment outcome, resolved inside `init_with` so the row's
/// baseline note reflects it.
enum WatchSetup {
    /// All watches live.
    Live,
    /// Establishment failed; live reload is off. Carries the first error.
    Failed(String),
}

impl ConfigWatchActor {
    /// Spawns the actor, establishes the notify watches, and publishes the
    /// watching row baseline.
    ///
    /// A watch that cannot be established is **not** fatal: a running
    /// jinn without live reload is the pre-slice status quo. The failure
    /// is logged, the row says so, and startup proceeds.
    ///
    /// # Errors
    ///
    /// Returns the trouper registration error if the actor itself cannot
    /// spawn. Watch failure degrades to a row note, never an error.
    pub fn spawn(
        system: &ActorSystem,
        services: &Services,
        state: &State,
    ) -> Result<ActorPath, error_stack::Report<RegistryError>> {
        let files = WatchedFiles::from_services(services);
        let resource_dirs = resource_dirs_from_services(services);
        let path = ActorPath::new(CONFIG_WATCH_PATH);
        let actor_path = path.clone();

        // The actor's own path is the settle sink: the loop publishes a
        // `SettleTick` addressed to it, so the tick rides the actor's
        // mailbox and its flush gate governs everything downstream.
        let watch = establish_watches(&files, &resource_dirs, actor_path, system.clone());
        if let Err(first_error) = &watch {
            tracing::warn!(
                error = %first_error,
                "config-watch could not establish the file watches; live reload is off"
            );
        }

        let spawned = trouper::builder::spawn_service_builder::<Self>(system)
            .at(path)
            .init_with({
                let services = services.clone();
                let files = files.clone();
                let resource_dirs = resource_dirs.clone();
                let state = state.clone();
                let watch = match &watch {
                    Ok(()) => WatchSetup::Live,
                    Err(first_error) => WatchSetup::Failed(first_error.to_string()),
                };
                move || {
                    Box::pin(async move {
                        let ledger = SignatureLedger::capture(&files.all(), &resource_dirs);
                        let actor = Self {
                            services,
                            state,
                            ledger: parking_lot::Mutex::new(ledger),
                            files,
                            resource_dirs,
                        };
                        actor.publish_baseline(&watch).await;
                        Ok(actor)
                    })
                }
            })
            .handles::<SettleTick>()
            .emits::<JinnTomlReloaded>()
            .emits::<ProvidersTomlReloaded>()
            .emits::<ResourcesChanged>()
            .emits::<StreamRulesReloaded>()
            .emits::<ServiceStatusUpdate>()
            .emits::<PushChatEntry>()
            .mailbox(16, trouper::inbox::OverloadPolicy::Block)
            .start();
        Ok(spawned)
    }

    /// Publishes the row's baseline: what is under watch, or the failure.
    async fn publish_baseline(&self, watch: &WatchSetup) {
        let (note, tone) = match watch {
            WatchSetup::Live => (None, None),
            WatchSetup::Failed(reason) => (
                Some(format!("watch setup failed ({reason}); live reload is off")),
                Some(NoteTone::Warning),
            ),
        };
        self.publish(ServiceStatusUpdate {
            name: ROW_NAME.to_owned(),
            description: Some(
                "watching jinn.toml, providers.toml, stream_rules.toml, and resource dirs"
                    .to_owned(),
            ),
            status_message: note,
            note_tone: tone,
        })
        .await;
    }

    /// One settle: diff signatures, handle what changed in order, publish.
    ///
    /// A settle whose diff is empty publishes nothing — the no-op-write
    /// dedupe. `jinn.toml` is handled first and its event only after the
    /// validated swap landed, so every handler in the settle (and every
    /// downstream handler of `JinnTomlReloaded`) observes the post-reload
    /// snapshot.
    pub async fn handle_settle(&self, ctx: &mut MsgCtx<'_>) {
        let (changed_jinn, changed_providers, changed_rules, resources_changed) = {
            let mut ledger = self.ledger.lock();
            let changed_jinn = ledger.file_changed(self.files.jinn_toml.clone());
            let changed_providers = ledger.file_changed(self.files.providers_toml.clone());
            let changed_rules = ledger.file_changed(self.files.stream_rules_toml.clone());
            let resources_changed = ledger.resources_changed(&self.resource_dirs);
            (
                changed_jinn,
                changed_providers,
                changed_rules,
                resources_changed,
            )
        };
        if !changed_jinn && !changed_providers && !changed_rules && !resources_changed {
            return;
        }

        let mut reloaded: Vec<&'static str> = Vec::new();
        let mut refusals: Vec<String> = Vec::new();

        if changed_jinn {
            match self.services.config.reload_validated() {
                Ok(()) => {
                    // Re-resolve the shells from the freshly swapped
                    // layer BEFORE publishing: a `[shell]` edit lands in
                    // the same settle as the event that announces it.
                    // A resolution that fails keeps the previous shells
                    // (a running jinn never loses a working shell) and
                    // adds a refusal, but the other `jinn.toml` changes
                    // still land — the shells are one section, not the
                    // file's validity.
                    match self.reresolve_shells() {
                        Ok(()) => {}
                        Err(refusal) => refusals.push(refusal),
                    }
                    ctx.publish(JinnTomlReloaded);
                    reloaded.push("jinn.toml");
                }
                Err(report) => refusals.push(format!("jinn.toml: {report:?}")),
            }
        }
        if changed_providers {
            // Parse now, not at handler time: the event must mean "the
            // file is at least readable", so a broken file is a refusal
            // here rather than a broadcast every handler must fend off.
            // A deleted providers.toml is also a refusal — the storage
            // `load()` materializes a default file on a miss, which
            // would turn a deletion into a silent reset to the default
            // provider set. Absence is only legitimate before launch.
            if self.files.providers_toml.as_path().exists() {
                match self.services.config_storage.load() {
                    Ok(_) => {
                        ctx.publish(ProvidersTomlReloaded);
                        reloaded.push("providers.toml");
                    }
                    Err(report) => refusals.push(format!("providers.toml: {report:?}")),
                }
            } else {
                refusals.push(
                    "providers.toml: file is gone; refusing to reload to defaults".to_owned(),
                );
            }
        }
        if changed_rules {
            // No pre-check: the matcher actor is the only reader and owns
            // its refusal (old matcher stays live).
            ctx.publish(StreamRulesReloaded);
            reloaded.push("stream_rules.toml");
        }
        if resources_changed {
            ctx.publish(ResourcesChanged);
            reloaded.push("resources");
        }

        if !reloaded.is_empty() {
            self.publish_status(format!("reloaded {}", reloaded.join(", ")), None)
                .await;
        }
        for refusal in refusals {
            self.surface_refusal(&refusal, ctx).await;
        }
    }

    /// Surfaces a refused reload: an error chat entry (excluded from
    /// LLM context, rendered in the error style) plus an error-toned
    /// dashboard note. The old values stay live — that is the point of
    /// refusing, and the entry says so.
    async fn surface_refusal(&self, refusal: &str, ctx: &mut MsgCtx<'_>) {
        tracing::warn!(
            refusal,
            "config reload refused; the previously installed values stay live"
        );
        let session_id = self.state.read().session.active_session_id().clone();
        let entry = jinn_core_types::ChatEntry::error(format!(
            "**Config reload refused**\n\n{refusal}\n\nThe previously installed values stay live."
        ));
        ctx.publish(PushChatEntry {
            session_id,
            entry,
            pin: None,
        });
        self.publish_status(format!("refused: {refusal}"), Some(NoteTone::Error))
            .await;
    }

    /// The single publication path for the dashboard row.
    /// Re-resolves the shells from the (just-swapped) config layer and
    /// swaps them into the shared [`jinn_common::shell::ShellsService`].
    ///
    /// Unlike the launch path this is not fatal: a configured path that
    /// names no file is a refusal to apply *that section*, and the
    /// previous resolution stays live. Only a malformed `[shell]` table
    /// cannot be judged at all — that cannot happen here, because the
    /// validated reload above already rejected a malformed file.
    fn reresolve_shells(&self) -> Result<(), String> {
        let shell_config = self
            .services
            .config
            .get::<jinn_preferences_config::schemas::ShellConfig>()
            .map_err(|error| format!("jinn.toml: [shell] is malformed: {error}"))?;
        let resolved = jinn_common::shell::Shells::resolve(
            shell_config.bash_path.as_deref(),
            shell_config.powershell_path.as_deref(),
        );
        let failures: Vec<String> = [resolved.bash().err(), resolved.powershell().err()]
            .into_iter()
            .flatten()
            .filter(|error| error.is_configuration_error())
            .map(ToString::to_string)
            .collect();
        if failures.is_empty() {
            self.services.shells.replace(resolved);
            Ok(())
        } else {
            // Keep the old resolution; report what was refused.
            Err(format!(
                "jinn.toml: [shell] refused — {}",
                failures.join("; ")
            ))
        }
    }

    async fn publish_status(&self, status_message: String, note_tone: Option<NoteTone>) {
        self.publish(ServiceStatusUpdate {
            name: ROW_NAME.to_owned(),
            description: None,
            status_message: Some(status_message),
            note_tone,
        })
        .await;
    }
}

impl MsgHandler<SettleTick> for ConfigWatchActor {
    async fn handle(&mut self, _msg: &SettleTick, ctx: &mut MsgCtx<'_>) {
        self.handle_settle(ctx).await;
    }
}

/// The resource directories under watch.
fn resource_dirs_from_services(services: &Services) -> Vec<MarkedPathBuf<Absolute>> {
    vec![
        services.paths.themes_dir(),
        services.paths.personas_dir(),
        services.paths.prompts_dir(),
        services.paths.skills_dir(),
        services.paths.workflows_dir(),
    ]
}

/// Establishes every notify watch and spawns the settle loop.
///
/// All watches share one channel; the loop debounces bursts and tells the
/// actor to settle. Any establishment failure fails the whole call — a
/// half-watched tree would silently miss edits, which is worse than no
/// live reload.
///
/// # Errors
///
/// Returns the first establishment failure. The caller degrades to
/// "no live reload" rather than failing startup.
fn establish_watches(
    files: &WatchedFiles,
    resource_dirs: &[MarkedPathBuf<Absolute>],
    settle_sink: ActorPath,
    system: ActorSystem,
) -> Result<(), Report<WatchError>> {
    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let watchers = watch_result(files, resource_dirs, &event_tx)?;
    // The loop owns every watcher handle; dropping any of them would
    // unregister that watch, so they all live in the loop's bundle.
    tokio::spawn(settle_loop(
        Arc::new(WatchKeep(watchers)),
        event_rx,
        settle_sink,
        system,
    ));
    Ok(())
}

/// Builds every watcher, feeding write-shaped events into `event_tx`.
fn watch_result(
    files: &WatchedFiles,
    resource_dirs: &[MarkedPathBuf<Absolute>],
    event_tx: &tokio::sync::mpsc::UnboundedSender<()>,
) -> Result<Vec<notify::RecommendedWatcher>, Report<WatchError>> {
    let mut watchers = Vec::new();

    let mut watch_dir =
        |path: &std::path::Path, mode: notify::RecursiveMode| -> Result<(), Report<WatchError>> {
            let tx = event_tx.clone();
            let mut watcher =
                notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
                    // Read-shaped noise is dropped on the watcher thread,
                    // before it can open a debounce window.
                    if let Ok(event) = res
                        && is_write_event(&event)
                    {
                        let _ = tx.send(());
                    }
                })
                .change_context(WatchError::Establish)
                .attach(format!("watching {}", path.display()))
                .attach("failed to establish config-watch watcher")?;
            watcher
                .watch(path, mode)
                .change_context(WatchError::Establish)
                .attach(format!("watching {}", path.display()))?;
            watchers.push(watcher);
            Ok(())
        };

    // The config directory covers providers.toml and stream_rules.toml —
    // non-recursive: nothing beneath it is ours to care about.
    let config_dir = files.config_dir();
    watch_dir(config_dir.as_path(), notify::RecursiveMode::NonRecursive)?;
    // When the `--config` override lives outside the standard config dir,
    // its parent needs its own watch or edits to the override file would
    // never be seen. The same-file case is filtered out at resolution
    // (no duplicate events for one save); the hash ledger is keyed by
    // path, so even overlapping coverage re-hashes each file once.
    if let Some(dir) = files.override_dir() {
        watch_dir(dir.as_path(), notify::RecursiveMode::NonRecursive)?;
    }
    // Resource dirs are recursive: subdirectories are part of the
    // resource (a theme pack, a skill folder). Each is created first —
    // jinn owns these directories, an absent one cannot be watched, and
    // a first-run tree simply has empty dirs until a resource lands.
    for dir in resource_dirs {
        std::fs::create_dir_all(dir.as_path()).change_context(WatchError::Establish)?;
        watch_dir(dir.as_path(), notify::RecursiveMode::Recursive)?;
    }
    Ok(watchers)
}

/// Whether this event means "content may have changed". Only write-shaped
/// kinds count: Modify/Create/Remove. Reads (Access) are noise — the
/// canvas watcher's finding, kept here for the same reason.
fn is_write_event(event: &notify::Event) -> bool {
    matches!(
        event.kind,
        notify::EventKind::Modify(_) | notify::EventKind::Create(_) | notify::EventKind::Remove(_)
    )
}

/// The live watcher handles, kept only so the watches stay registered.
/// A newtype because clippy rightly objects to `Arc<Vec<_>>` directly.
/// The field is never read — dropping it would unregister the watch.
struct WatchKeep(Vec<notify::RecommendedWatcher>);

impl Drop for WatchKeep {
    fn drop(&mut self) {
        // Dropping a watcher unregisters its watch; this drop is where
        // teardown belongs if the settle loop ever stops.
        for watcher in self.0.drain(..) {
            drop(watcher);
        }
    }
}

/// The settle loop: wait for the first write-shaped event of a burst,
/// drain the debounce window, then hand one settle to the actor.
async fn settle_loop(
    // Owned purely to keep the watches registered; never read.
    _watchers: Arc<WatchKeep>,
    mut event_rx: tokio::sync::mpsc::UnboundedReceiver<()>,
    settle_sink: ActorPath,
    system: ActorSystem,
) {
    loop {
        // First event of a burst opens the window. Channel close or
        // stream failure ends live reload: logging and exiting is the
        // degraded mode, never a wrong config. A tell to a dead actor
        // fails the tell, not the loop — the next window retries.
        if event_rx.recv().await.is_none() {
            tracing::warn!("config-watch event stream ended; live reload is off");
            return;
        }
        // Drain whatever else lands inside the window.
        let deadline = tokio::time::Instant::now() + DEBOUNCE;
        while tokio::time::timeout_at(deadline, event_rx.recv())
            .await
            .is_ok()
        {
            // Another event in the window; keep draining to the deadline.
        }
        // One settle per window, straight to the actor's path. A tell to
        // a dead path returns the envelope; nothing to do but log.
        if let Err(_dead_letter) = system.tell(settle_sink.clone(), SettleTick).await {
            tracing::warn!("config-watch settle was not deliverable; window skipped");
        }
    }
}
