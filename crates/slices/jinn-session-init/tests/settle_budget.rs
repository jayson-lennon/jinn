//! The keyed discovery worker's settle semantics: the budget firing
//! settles with the coordinator's delayed-reason format naming the
//! missing resources, the snapshot counts only what finished in time,
//! late scans still land after the settle, and the per-resource events
//! cross the schema-named topics the reverse relays subscribe.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "test code"
)]

use std::time::Duration;

use jinn_domain::common::app_paths::AppPaths;
use jinn_domain::common::app_state::AppState;
use jinn_domain::common::state::State;
use jinn_domain::protocol::SessionId;
use trouper::tap::FactKind;

use jinn_session_init::commands::RunDiscovery;
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
    /// The VCS-rooted project dir the command payloads point at.
    project: std::path::PathBuf,
    _dir: Box<tempfile::TempDir>,
}

impl Wired {
    async fn wire() -> Self {
        Self::wire_with_args(serde_json::json!({})).await
    }

    /// Wires with a custom args template (merged with the entity key),
    /// letting tests shorten the settle budget. Spawns the notifier so
    /// settles surface as summary entries.
    async fn wire_with_args(args_template: serde_json::Value) -> Self {
        let dir = Box::new(tempfile::tempdir().expect("temp dir"));
        let home = dir.path().to_path_buf();
        let project = home.join("proj");
        std::fs::create_dir_all(project.join(".git")).expect("project dir");
        let paths = AppPaths::new_in(&home);
        let state = State::new(AppState::default());
        let session_id = state.read().session.active_session_id().clone();
        let fabric = jinn_testutil::TestFabric::new();
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
                    "cwd": self.project.to_string_lossy(),
                }),
            ))
            .await;
        assert!(sent.is_ok(), "partition send must resolve: {sent:?}");
    }

    /// The newest summary entry text, if any (the notifier's
    /// observable side effect).
    fn summary_text(&self) -> Option<String> {
        let guard = self.state.read();
        guard.session.get(&self.session_id).and_then(|s| {
            s.history().iter().rev().find_map(|e| match &e.kind {
                jinn_domain::protocol::ChatEntryKind::Transient(text) => Some(text.clone()),
                _ => None,
            })
        })
    }

    /// Whether a publish crossed a topic for `schema_name` (the
    /// worker's per-resource publishes are topic publishes; the tap
    /// records each as `TopicPublished`).
    fn published_schema(&self, schema_name: &str) -> bool {
        self.fabric.system().tap_facts().iter().any(|fact| {
            matches!(&fact.kind, FactKind::TopicPublished { schema, .. } if schema.name() == schema_name)
        })
    }
}

/// Writes one skill under `<base>/.agents/skills/<name>`.
fn write_skill(base: &std::path::Path, name: &str) {
    let dir = base.join(".agents").join("skills").join(name);
    std::fs::create_dir_all(&dir).expect("create skill dir");
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {name} skill\n---\n\nbody"),
    )
    .expect("write SKILL.md");
}

/// Blocks the prompts scan past the injected budget: a `stuck.md` in
/// the project's prompts dir is a named pipe whose writer only exits
/// (releasing the scanner's blocked read) when [`StalledScan::release`]
/// runs. The prompt scanner reads every `*.md` it finds — no type gate
/// — so the fifo parks its `read` call.
///
/// Unix-only: the stall needs a fifo.
#[cfg(unix)]
struct StalledScan {
    fifo: std::path::PathBuf,
    release: std::sync::Arc<std::sync::Barrier>,
}

#[cfg(unix)]
impl StalledScan {
    fn new(project: &std::path::Path) -> Self {
        use std::sync::Arc;

        let prompts_dir = project.join(".agents").join("prompts");
        std::fs::create_dir_all(&prompts_dir).expect("create prompts dir");
        let fifo = prompts_dir.join("stuck.md");
        let release = Arc::new(std::sync::Barrier::new(2));
        let waiter = Arc::clone(&release);
        mkfifo(&fifo);
        let writer_end = fifo.clone();
        std::thread::spawn(move || {
            // Holding the write end open without writing parks the
            // scanner's read on an empty pipe until this thread exits.
            let file = std::fs::File::create(&writer_end).expect("open fifo write end");
            waiter.wait();
            drop(file);
        });
        Self { fifo, release }
    }

    /// Releases the stalled scan: writes the template content into the
    /// pipe (the blocked scanner read consumes it) and closes the
    /// write end, so the late prompts scan parses one prompt.
    fn release(self) {
        use std::io::Write as _;

        self.release.wait();
        // Writing through a fresh handle keeps the pipe open until the
        // content is flushed; dropping it then yields EOF.
        let mut writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&self.fifo)
            .expect("reopen fifo write end");
        writer
            .write_all(b"+++\nname = \"late\"\ndescription = \"Late\"\n+++\nLate!")
            .expect("write template content");
        drop(writer);
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// `mkfifo(3)` via the libc binding the platform links.
#[cfg(unix)]
fn mkfifo(path: &std::path::Path) {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;

    let c = CString::new(path.as_os_str().as_bytes()).expect("path has no NUL");
    // mkfifo(3)'s mode is the permissions; the fifo type is implied.
    unsafe extern "C" {
        fn mkfifo(path: *const std::ffi::c_char, mode: u32) -> i32;
    }
    let result = unsafe { mkfifo(c.as_ptr(), 0o644) };
    assert_eq!(result, 0, "mkfifo failed");
}

#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(15))]
#[cfg(unix)]
async fn budget_timeout_settles_with_delayed_reason_naming_missing_resources() {
    // Given a partition set whose prompts scan stalls past the
    // injected budget (stuck.md is a blocked fifo; skills + context
    // have nothing to scan, so they finish fast).
    let wired = Wired::wire_with_args(serde_json::json!({
        SETTLE_BUDGET_ARG: TEST_BUDGET_MS,
    }))
    .await;
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
    stalled.release();
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
#[timeout(Duration::from_secs(15))]
#[cfg(unix)]
async fn timed_settle_snapshot_counts_finished_resources() {
    // Given a stalled prompts scan and real content for the other two
    // resources, so skills + context finish within the budget.
    let wired = Wired::wire_with_args(serde_json::json!({
        SETTLE_BUDGET_ARG: TEST_BUDGET_MS,
    }))
    .await;
    let stalled = StalledScan::new(&wired.project);
    write_skill(&wired.project, "quick-skill");
    std::fs::write(wired.project.join("AGENTS.md"), "context body").expect("write AGENTS.md");

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
    stalled.release();
}

#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(10))]
async fn full_discovery_settles_without_a_delay_note() {
    // Given a wired partition set whose project tree has one skill, one
    // prompt, and one AGENTS.md — all scans finish inside the budget.
    let wired = Wired::wire_with_args(serde_json::json!({
        SETTLE_BUDGET_ARG: TEST_BUDGET_MS,
    }))
    .await;
    write_skill(&wired.project, "quick-skill");
    let prompts_dir = wired.project.join(".agents").join("prompts");
    std::fs::create_dir_all(&prompts_dir).expect("prompts dir");
    std::fs::write(
        prompts_dir.join("hello.md"),
        "+++\nname = \"hello\"\ndescription = \"Say hello\"\n+++\nHello!",
    )
    .expect("write prompt");
    std::fs::write(wired.project.join("AGENTS.md"), "context body").expect("write AGENTS.md");

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
    let wired = Wired::wire().await;
    write_skill(&wired.project, "topic-skill");

    // When a full discovery runs.
    wired.run_discovery().await;

    // Then a SkillsLoaded event was delivered on its schema-named
    // topic — the exact topic the kernel's reverse relay subscribes.
    wait_for(|| wired.published_schema("SkillsLoaded")).await;
    // And the other two resources crossed their own topics.
    wait_for(|| wired.published_schema("PromptTemplatesLoaded")).await;
    wait_for(|| wired.published_schema("ContextFilesLoaded")).await;
}

#[rstest::rstest]
#[tokio::test]
async fn worker_scans_command_cwd_not_state() {
    // Given a wired partition set (no cwd seeded into state — its
    // sentinel stays) with one skill under the project dir.
    let wired = Wired::wire().await;
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

/// Waits until the notifier has written its summary entry.
async fn wait_for_summary(wired: &Wired) {
    wait_for(|| wired.summary_text().is_some()).await;
}

/// Polls `check` until it passes or the retry budget runs out.
async fn wait_for(check: impl Fn() -> bool) {
    for _ in 0..300 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition never held within the retry budget");
}
