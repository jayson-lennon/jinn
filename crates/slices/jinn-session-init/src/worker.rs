//! The keyed discovery worker — one ServiceActor per session, activated
//! on demand by the trouper partition set.
//!
//! The worker owns a session's whole discovery boundary: the three
//! resource scans (skills, prompt templates, context files), the state
//! writes for each result, the per-resource `*Loaded` publications, and
//! the settle coalescing the kameo `DiscoveryCoordinatorActor` used to
//! perform across four separate actors. Keying by session id makes
//! per-session settlement native: the coordinator existed only because
//! kameo has no keyed actors; here the latch is a local join.
//!
//! Settle semantics (recorded behavior, preserved):
//! - a [`RunDiscovery`] arms a settle waiter joining the three scans
//!   under a fixed budget ([`SETTLE_BUDGET`]);
//! - settling within the budget publishes [`SessionDiscoverySettled`]
//!   with `delayed: None`; the budget firing publishes the
//!   coordinator's `"discovery delayed by <resources>"` reason naming
//!   the resources still missing, and the snapshot counts only the
//!   scans that finished in time;
//! - scans that finish after a timed settle still write state and
//!   publish their event (the waiter never cancels them);
//! - manual rescans run one resource only, with the other two
//!   pre-`Skipped`, so they settle and post a summary too (the
//!   coordinator's safety-net behavior for `RescanPromptTemplates`);
//! - a second `RunDiscovery` supersedes the running one: the stale
//!   waiter no-ops (the coordinator's `started_at` check, renamed to a
//!   run counter).
//!
//! Resource events publish onto each schema-named trouper topic — the
//! topic the kernel's reverse relay subscribes — and return to the
//! kameo bus as the exact kernel types kernel consumers already
//! subscribe to.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use trouper::actor::ActorPath;
use trouper::actor::{MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

use jinn_core_types::SessionId;
use jinn_domain::common::state::State;
use jinn_domain::common::tcaps::frontend::FrontendCap;
use jinn_domain::common::tcaps::session::SessionCap;
use jinn_domain::feat::context::env_context::ContextFile;
use jinn_domain::feat::context::prompt_template::PromptTemplateStore;
use jinn_domain::feat::skills::Skill;

use crate::commands::{RescanContext, RescanPrompts, RescanSkills, RunDiscovery};
use crate::contracts::{DiscoverySnapshot, SessionDiscoverySettled};
use crate::scans;

/// The settle budget: how long a discovery run waits for all three
/// scans before settling with a delayed reason. Production value;
/// tests inject a shorter one via the entity args (`settle_budget_ms`)
/// so a timed settle is exercisable in wall-clock-friendly time.
pub const SETTLE_BUDGET: std::time::Duration = std::time::Duration::from_secs(3);

/// The genesis-arg key carrying an override settle budget (ms).
pub const SETTLE_BUDGET_ARG: &str = "settle_budget_ms";

/// How long an idle discovery worker lives before the runtime
/// passivates it: the settle budget plus margin, so a run always
/// finishes (or times out and settles) before its actor can die. The
/// next trigger to the partition set transparently re-activates the
/// entity with fresh state.
pub const IDLE_LIFETIME: std::time::Duration = std::time::Duration::from_secs(5);

/// The resources a discovery run collects, in the order the delayed
/// reason names them (the coordinator's `missing_names` order).
const RESOURCES: [(&Resource, &str); 3] = [
    (&Resource::Skills, "skills"),
    (&Resource::Prompts, "prompts"),
    (&Resource::Context, "context"),
];

/// One of the three discovery resources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Resource {
    Skills,
    Prompts,
    Context,
}

/// The per-session discovery worker.
///
/// Holds the write authorities (caps), the trouper system handle for
/// out-of-task publishes, and the launch-wide scan inputs (home + the
/// four resource dirs). The session id arrives as the entity's `"key"`
/// genesis arg (the partition set merges it into the args template).
pub struct SessionDiscoveryWorker {
    /// The session this entity serves (the shard key).
    session_id: SessionId,
    /// The trouper system — resource tasks publish through it after
    /// this actor's handle has returned.
    system: ActorSystem,
    /// Shared application state.
    state: State,
    /// Authority to write discovered sets into sessions.
    session_cap: SessionCap,
    /// Authority to reload the skills picker in the frontend.
    frontend_cap: FrontendCap,
    /// Monotonic run counter: a waiter observing a smaller value than
    /// the current run has been superseded and no-ops.
    run: Arc<AtomicU64>,
    /// The user's home dir.
    home: PathBuf,
    /// The system-installed skills dir.
    system_skills_dir: PathBuf,
    /// The user-global skills dir.
    global_skills_dir: PathBuf,
    /// The user prompts dir.
    user_prompts_dir: PathBuf,
    /// The system-installed prompts dir.
    system_prompts_dir: PathBuf,
    /// How long a discovery run waits for all three scans. The
    /// production default ([`SETTLE_BUDGET`]); tests may shorten it via
    /// the entity's genesis args.
    settle_budget: std::time::Duration,
}

/// The authorities and inputs one worker entity is granted at genesis.
#[derive(Clone)]
pub struct WorkerDeps {
    /// The session this entity serves (the partition key).
    pub session_id: SessionId,
    /// The trouper system handle.
    pub system: ActorSystem,
    /// Shared application state.
    pub state: State,
    /// Authority to write discovered sets into sessions.
    pub session_cap: SessionCap,
    /// Authority to reload the skills picker in the frontend.
    pub frontend_cap: FrontendCap,
    /// The user's home dir.
    pub home: PathBuf,
    /// The system-installed skills dir.
    pub system_skills_dir: PathBuf,
    /// The user-global skills dir.
    pub global_skills_dir: PathBuf,
    /// The user prompts dir.
    pub user_prompts_dir: PathBuf,
    /// The system-installed prompts dir.
    pub system_prompts_dir: PathBuf,
    /// The settle budget (production default unless overridden).
    pub settle_budget: std::time::Duration,
}

impl WorkerDeps {
    /// Mints the per-entity authorities for one session: shared state
    /// and system handle, fresh caps, launch-wide scan inputs from
    /// the launch's `AppPaths`.
    ///
    /// `settle_budget` overrides the default wait (tests inject a
    /// shorter one); `None` uses [`SETTLE_BUDGET`].
    #[must_use]
    pub fn for_session_with_budget(
        system: &ActorSystem,
        state: &State,
        paths: &jinn_domain::common::app_paths::AppPaths,
        session_id: SessionId,
        settle_budget: Option<std::time::Duration>,
    ) -> Self {
        Self {
            session_id,
            system: system.clone(),
            state: state.clone(),
            session_cap: jinn_domain::common::tcaps::mint::mint_session_cap(),
            frontend_cap: jinn_domain::common::tcaps::mint::mint_frontend_cap(),
            home: paths.home_dir().to_path_buf(),
            system_skills_dir: paths.system_skills_dir(),
            global_skills_dir: paths.skills_dir(),
            user_prompts_dir: paths.prompts_dir(),
            system_prompts_dir: paths.system_prompts_dir(),
            settle_budget: settle_budget.unwrap_or(SETTLE_BUDGET),
        }
    }

    /// Mints the per-entity authorities with the default settle budget.
    #[must_use]
    pub fn for_session(
        system: &ActorSystem,
        state: &State,
        paths: &jinn_domain::common::app_paths::AppPaths,
        session_id: SessionId,
    ) -> Self {
        Self::for_session_with_budget(system, state, paths, session_id, None)
    }
}

impl ServiceActor for SessionDiscoveryWorker {
    async fn start(_args: &serde_json::Value) -> Result<Self, error_stack::Report<RegistryError>> {
        // The caps and State cannot ride JSON args; entities spawn via
        // the partition factory's `start_with` closure (see `spawn`).
        Err(
            error_stack::IntoReport::into_report(RegistryError::InvalidSpec).attach(
                "SessionDiscoveryWorker spawns via the partition factory; start requires caps",
            ),
        )
    }
}

impl SessionDiscoveryWorker {
    /// Spawns the worker entity at `path` with the injected handles.
    ///
    /// Called by the partition factory on activation; the factory
    /// closure captures the deps so every activation of this partition
    /// set grants its entity the same authorities.
    pub fn spawn(system: &ActorSystem, path: ActorPath, deps: WorkerDeps) -> ActorPath {
        let system_handle = system.clone();
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(path)
            .mailbox(1024, trouper::inbox::OverloadPolicy::Block)
            .passivate_after(IDLE_LIFETIME)
            .start_with(move || {
                Box::pin(async move {
                    let SessionDiscoveryWorkerDepsBuilder {
                        session_id,
                        state,
                        session_cap,
                        frontend_cap,
                        home,
                        system_skills_dir,
                        global_skills_dir,
                        user_prompts_dir,
                        system_prompts_dir,
                        system,
                        settle_budget,
                    } = deps.to_builder(&system_handle);
                    Ok(Self {
                        session_id,
                        system,
                        state,
                        session_cap,
                        frontend_cap,
                        run: Arc::new(AtomicU64::new(0)),
                        home,
                        system_skills_dir,
                        global_skills_dir,
                        user_prompts_dir,
                        system_prompts_dir,
                        settle_budget,
                    })
                })
            })
            .handles::<RunDiscovery>()
            .handles::<RescanSkills>()
            .handles::<RescanPrompts>()
            .handles::<RescanContext>()
            .start()
    }

    /// Begins a full discovery run: all three resource scans
    /// concurrently, each writing state + publishing as it completes,
    /// plus the settle waiter for this run.
    fn run_discovery(&mut self, cwd: PathBuf) {
        let run = self.run.fetch_add(1, Ordering::AcqRel) + 1;
        let skills = self.spawn_skills_task(&cwd);
        let prompts = self.spawn_prompts_task(&cwd);
        let context = self.spawn_context_task(&cwd);
        self.spawn_settle_waiter(run, skills, prompts, context);
    }

    /// Re-runs a single resource (manual rescan) and still settles:
    /// the two unscanned resources start pre-`Skipped` so the settle
    /// waiter fires as soon as the scanned resource completes — or at
    /// the settle budget, naming it as delayed — and the notifier
    /// posts the summary entry (the old coordinator's safety-net
    /// behavior for `RescanPromptTemplates` and friends).
    fn rescan_one(&mut self, scanned: Resource, cwd: PathBuf) {
        let run = self.run.fetch_add(1, Ordering::AcqRel) + 1;
        // Positional: skills, prompts, context — exactly what
        // `spawn_settle_waiter` expects.
        let (skills, prompts, context) = match scanned {
            Resource::Skills => (self.spawn_skills_task(&cwd), skipped(), skipped()),
            Resource::Prompts => (skipped(), self.spawn_prompts_task(&cwd), skipped()),
            Resource::Context => (skipped(), skipped(), self.spawn_context_task(&cwd)),
        };
        self.spawn_settle_waiter(run, skills, prompts, context);
    }

    /// The defensive cwd gate: the supervisor already suppressed the
    /// `.`, but a direct send (test, future caller) must not scan the
    /// sentinel either. `false` → no scan, no event.
    fn cwd_gate_open(cwd: &std::path::Path) -> bool {
        cwd != std::path::Path::new(".")
    }

    /// The skills resource scan (blocking), with the state write +
    /// picker reload + `SkillsLoaded` publication on completion.
    fn spawn_skills_task(&self, cwd: &std::path::Path) -> tokio::task::JoinHandle<ResourceOutcome> {
        if !Self::cwd_gate_open(cwd) {
            return skipped();
        }
        let project_dirs = scans::project_skills_dirs(cwd, &self.home);
        let system = self.system.clone();
        let state = self.state.clone();
        let session_cap = self.session_cap;
        let frontend_cap = self.frontend_cap;
        let session_id = self.session_id.clone();
        let system_dir = self.system_skills_dir.clone();
        let global_dir = self.global_skills_dir.clone();
        tokio::spawn(async move {
            let joined = tokio::task::spawn_blocking(move || {
                scans::scan_skills_merged(&system_dir, &global_dir, &project_dirs)
            })
            .await;
            match joined {
                Ok(skills) => {
                    tracing::info!(count = skills.len(), "scanned agent skills");
                    write_skills(&state, &session_cap, &frontend_cap, &session_id, &skills);
                    publish(
                        &system,
                        jinn_domain::feat::skills::SkillsLoaded {
                            session_id: session_id.clone(),
                            skills: skills.clone(),
                            error: None,
                        },
                    )
                    .await;
                    ResourceOutcome::Done(skills.len(), None)
                }
                Err(join_error) => {
                    tracing::error!("skills scan task panicked: {join_error}");
                    let error = format!("skills scan task failed: {join_error}");
                    publish(
                        &system,
                        jinn_domain::feat::skills::SkillsLoaded {
                            session_id: session_id.clone(),
                            skills: vec![],
                            error: Some(error.clone()),
                        },
                    )
                    .await;
                    ResourceOutcome::Done(0, Some(error))
                }
            }
        })
    }

    /// The prompt-templates resource scan.
    fn spawn_prompts_task(
        &self,
        cwd: &std::path::Path,
    ) -> tokio::task::JoinHandle<ResourceOutcome> {
        if !Self::cwd_gate_open(cwd) {
            return skipped();
        }
        let project_dirs = scans::project_prompts_dirs(cwd, &self.home);
        let system = self.system.clone();
        let state = self.state.clone();
        let session_cap = self.session_cap;
        let session_id = self.session_id.clone();
        let user_dir = self.user_prompts_dir.clone();
        let system_dir = self.system_prompts_dir.clone();
        tokio::spawn(async move {
            let joined = tokio::task::spawn_blocking(move || {
                scans::load_prompts(&user_dir, &system_dir, &project_dirs)
            })
            .await;
            match joined {
                Ok(Ok(store)) => {
                    tracing::info!(count = store.len(), "rescanned prompt templates");
                    let count = store.len();
                    write_prompts(&state, &session_cap, &session_id, &store);
                    publish(
                        &system,
                        jinn_domain::feat::provider::protocol::event::PromptTemplatesLoaded {
                            session_id: session_id.clone(),
                            templates: store.templates().to_vec(),
                            error: None,
                        },
                    )
                    .await;
                    ResourceOutcome::Done(count, None)
                }
                Ok(Err(error)) => {
                    tracing::warn!("failed to rescan prompt templates: {error:?}");
                    publish(
                        &system,
                        jinn_domain::feat::provider::protocol::event::PromptTemplatesLoaded {
                            session_id: session_id.clone(),
                            templates: vec![],
                            error: Some(error.clone()),
                        },
                    )
                    .await;
                    ResourceOutcome::Done(0, Some(error))
                }
                Err(join_error) => {
                    tracing::error!("rescan task panicked: {join_error}");
                    let error = format!("rescan task failed: {join_error}");
                    publish(
                        &system,
                        jinn_domain::feat::provider::protocol::event::PromptTemplatesLoaded {
                            session_id: session_id.clone(),
                            templates: vec![],
                            error: Some(error.clone()),
                        },
                    )
                    .await;
                    ResourceOutcome::Done(0, Some(error))
                }
            }
        })
    }

    /// The context-files resource scan.
    fn spawn_context_task(
        &self,
        cwd: &std::path::Path,
    ) -> tokio::task::JoinHandle<ResourceOutcome> {
        if !Self::cwd_gate_open(cwd) {
            return skipped();
        }
        let system = self.system.clone();
        let state = self.state.clone();
        let session_cap = self.session_cap;
        let session_id = self.session_id.clone();
        let home = self.home.clone();
        let cwd = cwd.to_path_buf();
        tokio::spawn(async move {
            let joined =
                tokio::task::spawn_blocking(move || scans::read_context_files(&cwd, &home)).await;
            match joined {
                Ok(files) => {
                    tracing::info!(count = files.len(), "scanned project context files");
                    write_context(&state, &session_cap, &session_id, &files);
                    publish(
                        &system,
                        jinn_domain::feat::context::protocol::event::ContextFilesLoaded {
                            session_id: session_id.clone(),
                            files: files.clone(),
                            error: None,
                        },
                    )
                    .await;
                    ResourceOutcome::Done(files.len(), None)
                }
                Err(join_error) => {
                    tracing::error!("context-files scan task panicked: {join_error}");
                    let error = format!("context-files scan task failed: {join_error}");
                    publish(
                        &system,
                        jinn_domain::feat::context::protocol::event::ContextFilesLoaded {
                            session_id: session_id.clone(),
                            files: vec![],
                            error: Some(error.clone()),
                        },
                    )
                    .await;
                    ResourceOutcome::Done(0, Some(error))
                }
            }
        })
    }

    /// Arms the settle waiter: joins the three resource tasks under the
    /// budget, then publishes the settled event for this run.
    ///
    /// A waiter whose run has been superseded no-ops. The resource
    /// tasks are never cancelled — late scans still write state and
    /// publish their events after the settle.
    fn spawn_settle_waiter(
        &self,
        run: u64,
        skills: tokio::task::JoinHandle<ResourceOutcome>,
        prompts: tokio::task::JoinHandle<ResourceOutcome>,
        context: tokio::task::JoinHandle<ResourceOutcome>,
    ) {
        let current_run = self.run.clone();
        let system = self.system.clone();
        let session_id = self.session_id.clone();
        let settle_budget = self.settle_budget;
        tokio::spawn(async move {
            // Shared so the timed-out settle can still read the
            // resources that finished within the budget.
            let finished: Arc<Mutex<Vec<(Resource, ResourceOutcome)>>> =
                Arc::new(Mutex::new(Vec::new()));
            let waiter = SettleWaiter {
                finished: Arc::clone(&finished),
            };
            let waited = tokio::time::timeout(settle_budget, async {
                tokio::join!(
                    waiter.join(Resource::Skills, skills),
                    waiter.join(Resource::Prompts, prompts),
                    waiter.join(Resource::Context, context),
                )
            })
            .await;

            // Stale-run guard: a newer run superseded this one.
            if current_run.load(Ordering::Acquire) != run {
                return;
            }

            let (snapshot, delayed) = match waited {
                Ok((skills, prompts, context)) => (snapshot_of(skills, prompts, context), None),
                Err(_elapsed) => {
                    // The budget fired: settle now with the outcomes
                    // already recorded; the still-running resources are
                    // missing. Their tasks keep going (the waiter never
                    // cancels), so late resources still land.
                    let missing = {
                        let done = finished.lock().expect("settle outcome lock");
                        RESOURCES
                            .iter()
                            .filter(|(resource, _)| !done.iter().any(|(done, _)| done == *resource))
                            .map(|(_, name)| (*name).to_owned())
                            .collect::<Vec<_>>()
                    };
                    (snapshot_of_finished(&finished), delayed_reason(&missing))
                }
            };
            publish(
                &system,
                SessionDiscoverySettled {
                    session_id,
                    snapshot,
                    delayed,
                },
            )
            .await;
        });
    }
}

/// The `start_with` closure cannot move fields out of a captured
/// `WorkerDeps` more than once (the factory may spawn many entities);
/// this builder receives cloned fields instead.
struct SessionDiscoveryWorkerDepsBuilder {
    session_id: SessionId,
    state: State,
    session_cap: SessionCap,
    frontend_cap: FrontendCap,
    home: PathBuf,
    system_skills_dir: PathBuf,
    global_skills_dir: PathBuf,
    user_prompts_dir: PathBuf,
    system_prompts_dir: PathBuf,
    system: ActorSystem,
    settle_budget: std::time::Duration,
}

impl WorkerDeps {
    /// Clones the (all cheap) captured handles into the builder shape.
    fn to_builder(&self, system: &ActorSystem) -> SessionDiscoveryWorkerDepsBuilder {
        SessionDiscoveryWorkerDepsBuilder {
            session_id: self.session_id.clone(),
            state: self.state.clone(),
            session_cap: self.session_cap,
            frontend_cap: self.frontend_cap,
            home: self.home.clone(),
            system_skills_dir: self.system_skills_dir.clone(),
            global_skills_dir: self.global_skills_dir.clone(),
            user_prompts_dir: self.user_prompts_dir.clone(),
            system_prompts_dir: self.system_prompts_dir.clone(),
            system: system.clone(),
            settle_budget: self.settle_budget,
        }
    }
}

/// A gated-out resource: zero counts, no error, no settle contribution.
fn skipped() -> tokio::task::JoinHandle<ResourceOutcome> {
    tokio::spawn(async { ResourceOutcome::Skipped })
}

/// The settle waiter's per-resource join: awaits one resource task and
/// records its outcome in the shared finished map, so a budget-timeout
/// settle can still count the resources that made it in time.
struct SettleWaiter {
    finished: Arc<Mutex<Vec<(Resource, ResourceOutcome)>>>,
}

impl SettleWaiter {
    async fn join(
        &self,
        resource: Resource,
        handle: tokio::task::JoinHandle<ResourceOutcome>,
    ) -> ResourceOutcome {
        let outcome = join_outcome(handle).await;
        self.finished
            .lock()
            .expect("settle outcome lock")
            .push((resource, outcome.clone()));
        outcome
    }
}

/// Folds the three outcomes into the settle snapshot.
fn snapshot_of(
    skills: ResourceOutcome,
    prompts: ResourceOutcome,
    context: ResourceOutcome,
) -> DiscoverySnapshot {
    DiscoverySnapshot {
        skill_count: skills.count(),
        skill_error: skills.error(),
        prompt_count: prompts.count(),
        prompt_error: prompts.error(),
        context_file_count: context.count(),
        context_error: context.error(),
    }
}

/// Folds only the finished outcomes into a partial settle snapshot —
/// the coordinator's behavior on a timed settle, where missing
/// resources contributed zero counts and no error.
fn snapshot_of_finished(finished: &Mutex<Vec<(Resource, ResourceOutcome)>>) -> DiscoverySnapshot {
    let pull = |resource: Resource| {
        finished
            .lock()
            .expect("settle outcome lock")
            .iter()
            .find(|(done, _)| *done == resource)
            .map(|(_, outcome)| outcome.clone())
            .unwrap_or(ResourceOutcome::Skipped)
    };
    snapshot_of(
        pull(Resource::Skills),
        pull(Resource::Prompts),
        pull(Resource::Context),
    )
}

/// The coordinator's delayed-reason format, naming the missing
/// resources in its `missing_names` order: `"discovery delayed by
/// skills, prompts"`.
fn delayed_reason(missing: &[String]) -> Option<String> {
    (!missing.is_empty()).then(|| format!("discovery delayed by {}", missing.join(", ")))
}

/// One resource's outcome, as the settle waiter reads it.
#[derive(Debug, Clone)]
enum ResourceOutcome {
    /// The scan completed: count and optional error description.
    Done(usize, Option<String>),
    /// The gated cwd suppressed the scan.
    Skipped,
}

impl ResourceOutcome {
    fn count(&self) -> usize {
        match self {
            ResourceOutcome::Done(count, _) => *count,
            ResourceOutcome::Skipped => 0,
        }
    }

    fn error(&self) -> Option<String> {
        match self {
            ResourceOutcome::Done(_, error) => error.clone(),
            ResourceOutcome::Skipped => None,
        }
    }
}

/// Joins a resource task; a panicked task folds into an error outcome.
async fn join_outcome(handle: tokio::task::JoinHandle<ResourceOutcome>) -> ResourceOutcome {
    handle.await.unwrap_or(ResourceOutcome::Done(
        0,
        Some("resource task failed".to_owned()),
    ))
}

/// Writes the discovered skills into the session and reloads the picker.
fn write_skills(
    state: &State,
    session_cap: &SessionCap,
    frontend_cap: &FrontendCap,
    session_id: &SessionId,
    skills: &[Skill],
) {
    state.with_session(session_cap, |view| {
        if let Some(session) = view.session.map().get_mut(session_id) {
            session.set_discovered_skills(skills.to_vec());
        }
    });

    // Reload the picker from the now-updated session data — the kameo
    // actor's post-scan sequence, verbatim.
    let (discovered, disabled, sample_theme) = {
        let r = state.read();
        let session = r.session.get(session_id);
        (
            session
                .map(|s| s.discovered_skills().to_vec())
                .unwrap_or_default(),
            session
                .map(|s| s.disabled_skills().clone())
                .unwrap_or_default(),
            r.frontend.theme.clone(),
        )
    };
    state.with_skills_frontend(frontend_cap, |ops| {
        ops.reload_picker(&discovered, &disabled, &sample_theme);
    });
}

/// Writes the discovered prompt templates into the session.
fn write_prompts(
    state: &State,
    session_cap: &SessionCap,
    session_id: &SessionId,
    store: &PromptTemplateStore,
) {
    state.with_session(session_cap, |view| {
        if let Some(session) = view.session.map().get_mut(session_id) {
            session.set_discovered_prompt_templates(store.clone());
        }
    });
}

/// Writes the discovered context files into the session.
fn write_context(
    state: &State,
    session_cap: &SessionCap,
    session_id: &SessionId,
    files: &[ContextFile],
) {
    state.with_session(session_cap, |view| {
        if let Some(session) = view.session.map().get_mut(session_id) {
            session.set_discovered_context_files(files.to_vec());
        }
    });
}

/// Publishes `msg` onto its schema-named trouper topic — the topic the
/// kernel's reverse relay subscribes. Called from resource tasks (after
/// the actor's handle has moved on), so it goes through the captured
/// system handle rather than an actor context.
async fn publish<M>(system: &ActorSystem, msg: M)
where
    M: trouper::schema::Schema + serde::Serialize,
{
    let topic = trouper::topics::Topic::new(M::schema_def().name.as_str());
    let payload = serde_json::to_value(&msg).unwrap_or(serde_json::Value::Null);
    let event = trouper::envelope::Event::new(M::schema_id(), payload);
    let envelope = system.envelope_to_topic(event, topic);
    let _ = system.send(envelope).await;
}

impl MsgHandler<RunDiscovery> for SessionDiscoveryWorker {
    async fn handle(&mut self, msg: RunDiscovery, _ctx: &mut MsgCtx<'_>) {
        self.run_discovery(msg.cwd);
    }
}

impl MsgHandler<RescanSkills> for SessionDiscoveryWorker {
    async fn handle(&mut self, msg: RescanSkills, _ctx: &mut MsgCtx<'_>) {
        self.rescan_one(Resource::Skills, msg.cwd);
    }
}

impl MsgHandler<RescanPrompts> for SessionDiscoveryWorker {
    async fn handle(&mut self, msg: RescanPrompts, _ctx: &mut MsgCtx<'_>) {
        self.rescan_one(Resource::Prompts, msg.cwd);
    }
}

impl MsgHandler<RescanContext> for SessionDiscoveryWorker {
    async fn handle(&mut self, msg: RescanContext, _ctx: &mut MsgCtx<'_>) {
        self.rescan_one(Resource::Context, msg.cwd);
    }
}
