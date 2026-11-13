//! The keyed discovery worker's settle semantics: the budget firing
//! settles with the coordinator's delayed-reason format naming the
//! missing resources, the snapshot counts only what finished in time,
//! late scans still land after the settle, and the per-resource events
//! cross the schema-named topics the reverse relays subscribe.

#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "test code asserts with expect/panic for clear failures"
)]

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jinn_core_types::SessionId;
use jinn_kernel::common::app_paths::AppPaths;
use jinn_kernel::common::app_state::AppState;
use jinn_kernel::common::state::State;
use marked_path::MarkedPathAccess as _;
use marked_path::{Absolute, MarkedPathBuf};

use jinn_session_init::commands::{RescanPrompts, RunDiscovery};
use jinn_session_init::worker::SETTLE_BUDGET_ARG;

/// The short budget tests inject so the timeout fires fast.
const TEST_BUDGET_MS: u64 = 300;

/// A partition set installed over a real temp home with a short
/// injected settle budget. The cwd the worker scans is
/// `<home>/proj` — a VCS-rooted project dir the command payloads
/// point at (home is exclusive to the bounded walk; a nested project
/// gives prompts + context files a home). No cwd is ever seeded into
/// shared state: the worker must read it from the command.
struct Wired {
    fabric: jinn_testutil::TestFabric,
    state: State,
    session_id: SessionId,
    /// Schema names of every `Sent` broadcast observed on the system.
    sent: Arc<Mutex<Vec<String>>>,
    /// The VCS-rooted project dir the command payloads point at.
    project: MarkedPathBuf<Absolute>,
    _dir: Box<jinn_testutil::JinnTempDir>,
}

impl Wired {
    fn wire() -> Self {
        Self::wire_with_args(trouper::json!({}))
    }

    /// Wires with a custom args template (merged with the entity key),
    /// letting tests shorten the settle budget. Spawns the notifier so
    /// settles surface as summary entries.
    fn wire_with_args(args_template: trouper::json::Json) -> Self {
        let dir = Box::new(jinn_testutil::tempdir());
        let home = dir.path.clone();
        let project = home.join("proj").expect("temp root joins a bare name");
        std::fs::create_dir_all(
            project
                .join(".git")
                .expect("project root joins a bare name")
                .as_path(),
        )
        .expect("project dir");
        let paths = AppPaths::new_in(&home.as_marked_path());
        let state = State::new(AppState::default());
        let session_id = state.read().session.active_session_id().clone();
        let fabric = jinn_testutil::TestFabric::new();
        let sent: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&sent);
        fabric
            .system()
            .set_observation(Arc::new(move |obs: &trouper::observe::Observation| {
                if let trouper::observe::ObservationKind::Sent {
                    dest: trouper::envelope::Address::Schema(_),
                    schema,
                    ..
                } = &obs.kind
                {
                    sink.lock()
                        .expect("observation sink")
                        .push(schema.name().to_owned());
                }
            }));
        jinn_session_init::install_partition_set_with_args(
            fabric.system(),
            &paths,
            &state,
            args_template,
        )
        .expect("install partition set");
        jinn_session_init::notifier::DiscoveryNotifier::spawn(fabric.system(), state.clone());
        Self {
            fabric,
            state,
            session_id,
            sent,
            project,
            _dir: dir,
        }
    }

    /// Sends `RunDiscovery` through the partition's public path, with
    /// the project dir as the command's cwd.
    async fn run_discovery(&self) {
        let sent = self
            .fabric
            .system()
            .send(self.fabric.system().envelope(
                <RunDiscovery as trouper::schema::Schema>::schema_id(),
                trouper::actor::ActorPath::new(jinn_session_init::DISCOVERY_PATH),
                serde_json::json!({
                    "session_id": self.session_id.to_string(),
                    "cwd": self.project.to_string(),
                }),
            ))
            .await;
        assert!(sent.is_ok(), "partition send must resolve: {sent:?}");
    }

    /// Sends `RescanPrompts` through the partition's public path, with
    /// the project dir as the command's cwd.
    async fn rescan_prompts(&self) {
        let sent = self
            .fabric
            .system()
            .send(self.fabric.system().envelope(
                <RescanPrompts as trouper::schema::Schema>::schema_id(),
                trouper::actor::ActorPath::new(jinn_session_init::DISCOVERY_PATH),
                serde_json::json!({
                    "session_id": self.session_id.to_string(),
                    "cwd": self.project.to_string(),
                }),
            ))
            .await;
        assert!(sent.is_ok(), "partition send must resolve: {sent:?}");
    }

    /// The newest summary entry text, if any (the notifier's
    /// observable side effect).
    fn summary_text(&self) -> Option<String> {
        let guard = self.state.read();
        let s = guard.session.get(&self.session_id)?;
        s.history().iter().rev().find_map(|e| match &e.kind {
            jinn_kernel::protocol::ChatEntryKind::Transient(text) => Some(text.clone()),
            _ => None,
        })
    }

    /// Whether a broadcast of `schema_name` crossed the fabric (the
    /// worker's per-resource publishes are schema broadcasts; the
    /// observation handler records each as a `Sent` addressed to the
    /// schema).
    fn published_schema(&self, schema_name: &str) -> bool {
        self.sent
            .lock()
            .expect("observation sink")
            .iter()
            .any(|name| name == schema_name)
    }
}

/// Writes one skill under `<base>/.agents/skills/<name>`.
fn write_skill(base: &MarkedPathBuf<Absolute>, name: &str) {
    let dir = base
        .join_all([".agents", "skills", name])
        .expect("project root joins bare names");
    std::fs::create_dir_all(dir.as_path()).expect("create skill dir");
    std::fs::write(
        dir.join("SKILL.md").expect("skill dir joins a bare name"),
        format!("---\nname: {name}\ndescription: {name} skill\n---\n\nbody"),
    )
    .expect("write SKILL.md");
}

/// Blocks the prompts scan past the injected budget: a `stuck.md` in
/// the project's prompts dir is a named pipe whose writer only exits
/// (releasing the scanner's blocked read) when the scan is released.
/// The prompt scanner reads every `*.md` it finds — no type gate — so
/// the fifo parks its `read` call.
///
/// The release must never leave the writer parked on its barrier: a
/// parked writer keeps the scanner's blocking read alive, and tokio
/// waits for in-flight blocking tasks at teardown — which would wedge
/// the test binary past any timeout. [`Drop`] therefore releases the
/// writer on a detached thread on every exit path, including a timeout
/// panic unwinding past `release`.
///
/// Unix-only: the stall needs a fifo.
#[cfg(unix)]
struct StalledScan {
    fifo: MarkedPathBuf<Absolute>,
    release: std::sync::Arc<std::sync::Barrier>,
    /// Set by `release` so `Drop` becomes a no-op after an explicit
    /// release (the payload has already been consumed by the scanner).
    released: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(unix)]
impl StalledScan {
    fn new(project: &MarkedPathBuf<Absolute>) -> Self {
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;

        let prompts_dir = project
            .join_all([".agents", "prompts"])
            .expect("project root joins bare names");
        std::fs::create_dir_all(prompts_dir.as_path()).expect("create prompts dir");
        let fifo = prompts_dir
            .join("stuck.md")
            .expect("prompts dir joins a bare name");
        let release = Arc::new(std::sync::Barrier::new(2));
        let waiter = Arc::clone(&release);
        mkfifo(&fifo);
        let writer_end = fifo.clone();
        std::thread::spawn(move || {
            // Holding the write end open without writing parks the
            // scanner's read on an empty pipe until this thread exits.
            let file = std::fs::File::create(writer_end.as_path()).expect("open fifo write end");
            waiter.wait();
            drop(file);
        });
        Self {
            fifo,
            release,
            released: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Releases the stalled scan: writes the late prompt into the pipe
    /// (the blocked scanner read consumes it) and closes the write end,
    /// so the late prompts scan parses one prompt. Runs off the runtime
    /// thread — a sync-blocked runtime thread cannot poll the rstest
    /// timeout future, so this inline would turn a stall into an
    /// infinite hang instead of a timeout failure.
    async fn release(self) {
        let fifo = self.fifo.clone();
        let release = Arc::clone(&self.release);
        let released = Arc::clone(&self.released);
        tokio::task::spawn_blocking(move || {
            Self::write_and_close(&fifo, &release, LATE_PROMPT);
            released.store(true, std::sync::atomic::Ordering::SeqCst);
        })
        .await
        .expect("release task");
    }

    /// The unblocking write, shared by `release` and `Drop`.
    fn write_and_close(
        fifo: &MarkedPathBuf<Absolute>,
        release: &std::sync::Arc<std::sync::Barrier>,
        content: &[u8],
    ) {
        use std::io::Write as _;

        // Open the replacement writer before releasing the original. This
        // guarantees at least one write end stays open across the handoff,
        // so the scanner's blocked read cannot observe a transient EOF.
        if let Ok(mut writer) = std::fs::OpenOptions::new().write(true).open(fifo) {
            release.wait();
            let _ = writer.write_all(content);
        }
    }
}

/// Content `release` feeds the scanner so the late prompts scan parses
/// exactly one prompt.
#[cfg(unix)]
const LATE_PROMPT: &[u8] = b"+++\nname = \"late\"\ndescription = \"Late\"\n+++\nLate!";

#[cfg(unix)]
impl Drop for StalledScan {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering;

        if self.released.load(Ordering::SeqCst) {
            return;
        }
        // Detached thread: drop cannot block (a timeout panic unwind
        // must land fast) and cannot fail the test. Filler bytes let
        // the blocked scanner read finish so tokio teardown can join
        // its blocking task instead of waiting forever.
        let fifo = self.fifo.clone();
        let release = Arc::clone(&self.release);
        std::thread::spawn(move || {
            Self::write_and_close(
                &fifo,
                &release,
                b"+++\nname = \"d\"\ndescription = \"d\"\n+++\nd",
            );
        });
    }
}

/// `mkfifo(3)` via the libc binding the platform links.
#[cfg(unix)]
fn mkfifo(path: &MarkedPathBuf<Absolute>) {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;

    unsafe extern "C" {
        fn mkfifo(path: *const std::ffi::c_char, mode: u32) -> i32;
    }

    let c = CString::new(path.as_os_str().as_bytes()).expect("path has no NUL");
    // mkfifo(3)'s mode is the permissions; the fifo type is implied.
    // SAFETY: `c` is a live, NUL-terminated CString matching the
    // `*const c_char` mkfifo(3) expects, and 0o644 is a valid mode_t;
    // mkfifo only touches the filesystem, writing nothing through the pointer.
    let result = unsafe { mkfifo(c.as_ptr(), 0o644) };
    assert_eq!(result, 0, "mkfifo failed");
}

#[rstest::rstest]
#[timeout(Duration::from_secs(30))]
#[tokio::test]
#[cfg(unix)]
async fn budget_timeout_settles_with_delayed_reason_naming_missing_resources() {
    // Given a partition set whose prompts scan stalls past the
    // injected budget (stuck.md is a blocked fifo; skills + context
    // have nothing to scan, so they finish fast).
    let wired = Wired::wire_with_args(trouper::json!({
        SETTLE_BUDGET_ARG: TEST_BUDGET_MS,
    }));
    let stalled = StalledScan::new(&wired.project);

    // When a full discovery runs and the budget elapses.
    wired.run_discovery().await;
    wait_for_summary(&wired).await;

    // Then the settle fired early with the coordinator's delayed-reason
    // format naming the still-missing resource.
    let text = wired.summary_text().expect("summary entry");
    assert!(
        text.contains("discovery delayed by prompts"),
        "delayed reason must name the missing resources: {text}"
    );

    // And releasing the stall lets the late prompts scan still land —
    // state written and event published after the settle.
    stalled.release().await;
    wait_for(|| wired.published_schema("PromptTemplatesLoaded")).await;
    wait_for(|| {
        let guard = wired.state.read();
        guard
            .session
            .get(&wired.session_id)
            .is_some_and(|session| !session.discovered_prompt_templates().templates().is_empty())
    })
    .await;
}

#[rstest::rstest]
#[tokio::test]
#[cfg(unix)]
async fn timed_settle_snapshot_counts_finished_resources() {
    // Given a stalled prompts scan and real content for the other two
    // resources, so skills + context finish within the budget.
    let wired = Wired::wire_with_args(trouper::json!({
        SETTLE_BUDGET_ARG: TEST_BUDGET_MS,
    }));
    let stalled = StalledScan::new(&wired.project);
    write_skill(&wired.project, "quick-skill");
    std::fs::write(
        wired
            .project
            .join("AGENTS.md")
            .expect("project root joins a bare name"),
        "context body",
    )
    .expect("write AGENTS.md");

    // When a discovery runs and the budget fires.
    wired.run_discovery().await;
    wait_for_summary(&wired).await;

    // Then the timed settle's snapshot counted the finished resources:
    // the summary lists the skill and context counts (finished in
    // time) with no prompt count (still missing).
    let text = wired.summary_text().expect("summary entry");
    assert!(text.contains("1 skill(s)"), "{text}");
    assert!(text.contains("1 AGENTS.md / context file(s)"), "{text}");
    assert!(!text.contains("prompt(s)"), "{text}");
    stalled.release().await;
}

#[rstest::rstest]
#[tokio::test]
async fn full_discovery_settles_without_a_delay_note() {
    // Given a wired partition set whose project tree has one skill, one
    // prompt, and one AGENTS.md — all scans finish inside the budget.
    let wired = Wired::wire_with_args(trouper::json!({
        SETTLE_BUDGET_ARG: TEST_BUDGET_MS,
    }));
    write_skill(&wired.project, "quick-skill");
    let prompts_dir = wired
        .project
        .join_all([".agents", "prompts"])
        .expect("project root joins bare names");
    std::fs::create_dir_all(prompts_dir.as_path()).expect("prompts dir");
    std::fs::write(
        prompts_dir
            .join("hello.md")
            .expect("prompts dir joins a bare name"),
        "+++\nname = \"hello\"\ndescription = \"Say hello\"\n+++\nHello!",
    )
    .expect("write prompt");
    std::fs::write(
        wired
            .project
            .join("AGENTS.md")
            .expect("project root joins a bare name"),
        "context body",
    )
    .expect("write AGENTS.md");

    // When a full discovery runs.
    wired.run_discovery().await;

    // Then the settle lists all three discovered counts and no delay
    // note: the waiter joined every scan within the budget.
    wait_for_summary(&wired).await;
    let text = wired.summary_text().expect("summary entry");
    assert!(text.contains("1 skill(s)"), "{text}");
    assert!(text.contains("1 prompt(s)"), "{text}");
    assert!(text.contains("1 AGENTS.md / context file(s)"), "{text}");
    assert!(!text.contains("discovery delayed"), "{text}");
}

#[rstest::rstest]
#[tokio::test]
async fn worker_publishes_onto_schema_named_topics() {
    // Given a wired partition set with one skill on disk.
    let wired = Wired::wire();
    write_skill(&wired.project, "topic-skill");

    // When a full discovery runs.
    wired.run_discovery().await;

    // Then a SkillsLoaded broadcast crossed the fabric.
    wait_for(|| wired.published_schema("SkillsLoaded")).await;
    // And the other two resources broadcast as well.
    wait_for(|| wired.published_schema("PromptTemplatesLoaded")).await;
    wait_for(|| wired.published_schema("ContextFilesLoaded")).await;
    // And the flush gate dead-lettered nothing: the supervisor's
    // `.emits` surface covers every command its handlers send.
    let dead = wired.fabric.system().drain_dead_letters();
    assert!(
        dead.is_empty(),
        "discovery must not dead-letter: {:?}",
        dead.iter()
            .map(|l| l.schema.to_string())
            .collect::<Vec<_>>()
    );
}

#[rstest::rstest]
#[tokio::test]
async fn worker_scans_command_cwd_not_state() {
    // Given a wired partition set (no cwd seeded into state) with one
    // skill under the project dir.
    let wired = Wired::wire();
    write_skill(&wired.project, "command-cwd-skill");

    // When a RunDiscovery whose command cwd points at that project dir
    // crosses the partition path.
    wired.run_discovery().await;

    // Then the scan followed the command's cwd: the skill was
    // discovered into the session despite state never carrying a cwd.
    wait_for(|| {
        let guard = wired.state.read();
        guard
            .session
            .get(&wired.session_id)
            .is_some_and(|s| !s.discovered_skills().is_empty())
    })
    .await;
}

#[rstest::rstest]
#[tokio::test]
async fn manual_prompt_rescan_settles_with_a_summary() {
    // Given a wired partition set with one prompt under the project
    // dir (the unscanned resources have nothing on disk).
    let wired = Wired::wire();
    let prompts_dir = wired
        .project
        .join_all([".agents", "prompts"])
        .expect("project root joins bare names");
    std::fs::create_dir_all(prompts_dir.as_path()).expect("prompts dir");
    std::fs::write(
        prompts_dir
            .join("rescanned.md")
            .expect("prompts dir joins a bare name"),
        "+++\nname = \"rescanned\"\ndescription = \"Found by rescan\"\n+++\nFound!",
    )
    .expect("write prompt");

    // When a manual prompt rescan runs.
    wired.rescan_prompts().await;

    // Then it settles: the summary lands naming the scanned resource
    // with no delay note — the pre-skipped resources don't stall it.
    wait_for_summary(&wired).await;
    let text = wired.summary_text().expect("summary entry");
    assert!(text.contains("1 prompt(s)"), "{text}");
    assert!(!text.contains("discovery delayed"), "{text}");
}

#[rstest::rstest]
#[tokio::test]
#[cfg(unix)]
async fn stalled_manual_rescan_settles_at_the_budget() {
    // Given a wired partition set whose prompts scan stalls past the
    // injected budget.
    let wired = Wired::wire_with_args(trouper::json!({
        SETTLE_BUDGET_ARG: TEST_BUDGET_MS,
    }));
    let stalled = StalledScan::new(&wired.project);

    // When a manual prompt rescan runs and the budget elapses.
    wired.rescan_prompts().await;
    wait_for_summary(&wired).await;

    // Then the settle fired anyway, naming prompts as delayed — a
    // manual rescan always surfaces its status.
    let text = wired.summary_text().expect("summary entry");
    assert!(
        text.contains("discovery delayed by prompts"),
        "delayed reason must name the scanned-but-stalled resource: {text}"
    );
    stalled.release().await;
}

/// Waits until the notifier has written its summary entry.
async fn wait_for_summary(wired: &Wired) {
    wait_for(|| wired.summary_text().is_some()).await;
}

/// Polls `check` until it passes or the retry budget runs out.
async fn wait_for(check: impl Fn() -> bool) {
    for _ in 0..1200 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition never held within the retry budget");
}

/// Whether a worker entity is live for the session key (the export's
/// actor list is the observable "is this entity spawned" surface).
async fn worker_live(fabric: &jinn_testutil::TestFabric, session_id: &SessionId) -> bool {
    let entity_path = trouper::actor::ActorPath::new(format!(
        "{}/{}",
        jinn_session_init::DISCOVERY_PATH,
        session_id
    ));
    fabric
        .system()
        .export()
        .await
        .actors
        .iter()
        .any(|actor| actor.path == entity_path)
}

/// Passivation clears the entity from the export's actor list, and the
/// next trigger re-activates it: the scan completes and the summary
/// posts again (the notifier's observable side effect).
// 30s, not the 10s default: the test waits out the worker's 5s idle
// passivation window (plus scans) and its own 20s retry budget only
// fits under 30.
#[rstest::rstest]
#[timeout(Duration::from_secs(30))]
#[tokio::test]
async fn idle_worker_passivates_and_reactivates_on_next_trigger() {
    // Given a wired partition set with one skill on disk and a worker
    // that has already run one discovery.
    let wired = Wired::wire();
    write_skill(&wired.project, "passivate-skill");
    wired.run_discovery().await;
    wait_for_summary(&wired).await;
    assert!(
        worker_live(&wired.fabric, &wired.session_id).await,
        "worker entity live after its run"
    );

    // When the idle window (5s) elapses with no messages.
    wait_for_async(|| async { !worker_live(&wired.fabric, &wired.session_id).await }).await;

    // And a new trigger addresses the same session key.
    let sent = wired
        .fabric
        .system()
        .send(wired.fabric.system().envelope(
            <RunDiscovery as trouper::schema::Schema>::schema_id(),
            trouper::actor::ActorPath::new(jinn_session_init::DISCOVERY_PATH),
            serde_json::json!({
                "session_id": wired.session_id.to_string(),
                "cwd": wired.project.to_string(),
            }),
        ))
        .await;
    assert!(sent.is_ok(), "post-passivation send must resolve: {sent:?}");

    // Then the entity re-activated and ran the scan again (a second
    // summary entry lands; the first settled before passivation).
    wait_for_async(|| async { worker_live(&wired.fabric, &wired.session_id).await }).await;
    wait_for(|| wired.published_schema("SkillsLoaded")).await;
}

/// Polls an async `check` until it passes or the retry budget runs
/// out. The budget must exceed the 5s idle lifetime passivation waits
/// for, so 20s of retries here.
async fn wait_for_async<F>(check: impl Fn() -> F)
where
    F: Future<Output = bool>,
{
    for _ in 0..2000 {
        if check().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition never held within the retry budget");
}
