//! [`CwdSuggestActor`] — async directory listing behind the cwd popup's
//! ghost suggestion.

use error_stack::Report;
use jinn_cwd_msg::CwdInputState;
use jinn_cwd_msg::SuggestCwdCompletion;
use jinn_slices::SlotKey;
use jinn_slices::cell::TypedCell;
use trouper::actor::ActorPath;
use trouper::actor::MsgHandler;
use trouper::actor::ServiceActor;
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;

use jinn_kernel::common::actor_deps::ActorDeps;
use jinn_kernel::common::actor_deps::BusPublish;
use jinn_kernel::common::services::bus_service::BusService;

use crate::suggest::GhostCandidate;
use crate::suggest::suggest_ghost;

/// Dependencies for [`CwdSuggestActor`].
#[derive(Clone)]
pub struct CwdSuggestActorDeps {
    /// Runtime services and bus access.
    pub deps: ActorDeps,
    /// The cwd popup's cell — the actor writes the ghost here, guarded by
    /// the request id.
    pub cell: TypedCell<CwdInputState>,
    /// The cell's slot key (diagnostics; the handle above is the writer).
    pub slot: SlotKey,
}

/// Lists the requested directory on a blocking thread and writes the ranked
/// ghost into the cwd popup's cell — only when the reply's request id still
/// matches the cell's expected id.
pub struct CwdSuggestActor {
    /// Bus service.
    bus: BusService,
    /// The cwd popup's cell (the write handle).
    cell: TypedCell<CwdInputState>,
}

impl BusPublish for CwdSuggestActor {
    fn bus(&self) -> &BusService {
        &self.bus
    }
}

impl ServiceActor for CwdSuggestActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "ServiceActor::start is async by trait contract"
    )]
    async fn init(_args: &trouper::json::Json) -> Result<Self, Report<RegistryError>> {
        // Never called: spawned via `spawn`'s init_with (typed deps can't
        // ride the JSON args).
        Err(Report::new(RegistryError::InvalidSpec).attach("CwdSuggestActor spawns via init_with"))
    }
}

/// Static path the suggester spawns at (one instance per process).
pub const CWD_SUGGEST_PATH: &str = "jinn.cwd_suggest.actor";

impl CwdSuggestActor {
    /// Spawns the suggester onto the trouper system; its subscription is
    /// live when this returns.
    ///
    /// # Panics
    ///
    /// Panics if the actor's path is already taken or its topic
    /// subscription fails — both mean a wiring bug at composition.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "port convention: spawn takes owned deps and clones into init_with"
    )]
    pub fn spawn(system: &trouper::system::ActorSystem, deps: CwdSuggestActorDeps) -> ActorPath {
        let path = ActorPath::new(CWD_SUGGEST_PATH);
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(path.clone())
            .init_with({
                let deps = deps.clone();
                move || {
                    let deps = deps.clone();
                    Box::pin(async move {
                        Ok(Self {
                            bus: deps.deps.services.bus.clone(),
                            cell: deps.cell,
                        })
                    })
                }
            })
            .handles::<SuggestCwdCompletion>()
            .mailbox(64, trouper::inbox::OverloadPolicy::Block)
            .start();
        path
    }
}

impl MsgHandler<SuggestCwdCompletion> for CwdSuggestActor {
    async fn handle(&mut self, msg: &SuggestCwdCompletion, _ctx: &mut MsgCtx<'_>) {
        let parent_dir = msg.parent_dir.clone();
        let segment = msg.segment.clone();
        let request_id = msg.request_id;
        let entries = tokio::task::spawn_blocking(move || list_dir_blocking(&parent_dir)).await;

        // Staleness guard: write only if this reply is still the expected
        // one. A newer request has bumped the expected id, and this reply
        // is stale — dropping it leaves the newer request's ghost alone.
        let entries = entries.unwrap_or_default();
        self.cell.update(|s| {
            if s.expected_request_id == request_id {
                s.ghost = suggest_ghost(&segment, &entries);
            }
        });
    }
}

/// Reads one directory on a blocking thread. On any error (missing dir,
/// permission denied), returns an empty list — the ghost is simply absent.
fn list_dir_blocking(
    path: &marked_path::MarkedPathBuf<marked_path::Absolute>,
) -> Vec<GhostCandidate> {
    let read = std::fs::read_dir(path);
    let Ok(read) = read else {
        return Vec::new();
    };
    read.filter_map(std::result::Result::ok)
        .map(|entry| GhostCandidate {
            name: entry.file_name().to_string_lossy().into_owned(),
            is_dir: entry.file_type().is_ok_and(|t| t.is_dir()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::uninlined_format_args,
        reason = "test code"
    )]
    use super::*;
    use jinn_kernel::common::actor_deps::ActorDeps;
    use jinn_kernel::common::app_paths::AppPaths;
    use jinn_kernel::common::services::test_services::TestServices;
    use jinn_slices::Slices;
    use jinn_testutil::bus_harness::TestHarness;
    use jinn_testutil::wait::await_cell;
    use marked_path::MarkedPathAccess as _;
    use std::time::Duration;

    /// A test world: fresh harness + a registry with the popup cell, the
    /// actor spawned against them. The actor writes the very cell handle
    /// the test holds — production wires the same handle through the
    /// registry, so exercising the handle directly observes the same
    /// shared state.
    struct World {
        harness: TestHarness,
        cell: TypedCell<CwdInputState>,
    }

    async fn world_with_two_subdirs(dir_names: &[&str]) -> (World, jinn_testutil::JinnTempDir) {
        // Given a temp parent holding the named directories (plus a file
        // the ranker must ignore).
        let parent = jinn_testutil::tempdir();
        for name in dir_names {
            std::fs::create_dir_all(parent.path.join(name).expect("join")).expect("mkdir");
        }
        std::fs::write(
            parent
                .path
                .join("afile.txt")
                .expect("temp root joins a bare name")
                .as_marked_path()
                .as_path(),
            b"x",
        )
        .expect("touch file");

        let harness = TestHarness::new().await;
        let slices = Slices::new();
        let cell = slices
            .register(jinn_cwd_msg::cwds_slot(), CwdInputState::default())
            .expect("fresh registry has the cwd slot free");
        let paths_root = jinn_testutil::tempdir();
        let services = TestServices::builder()
            .paths(AppPaths::new_in(&paths_root.path.as_marked_path()))
            .build();
        CwdSuggestActor::spawn(
            harness.system(),
            CwdSuggestActorDeps {
                deps: ActorDeps {
                    services: {
                        let mut services = services;
                        services.bus = harness.bus();
                        services.trouper_system = harness.system().clone();
                        services
                    },
                },
                cell: cell.clone(),
                slot: jinn_cwd_msg::cwds_slot(),
            },
        );
        (World { harness, cell }, parent)
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn reply_with_the_current_request_id_writes_the_ghost() {
        // Given a parent with two directories and the cell expecting
        // request 1.
        let (world, parent) = world_with_two_subdirs(&["alpha", "beta"]).await;
        world.cell.update(|s| s.expected_request_id = 1);

        // When the actor handles a suggestion request carrying that id.
        world
            .harness
            .publish(SuggestCwdCompletion {
                parent_dir: parent.path.clone(),
                segment: "al".to_owned(),
                request_id: 1,
            })
            .await;
        await_cell(&world.cell, Duration::from_secs(5), |s| s.ghost.is_some()).await;

        // Then the ghost is the ranked winner (files excluded).
        assert_eq!(world.cell.read().ghost.as_deref(), Some("alpha"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn stale_reply_is_dropped_silently() {
        // Given a parent with a directory and a cell already expecting
        // request 2 (a newer edit bumped it).
        let (world, parent) = world_with_two_subdirs(&["alpha"]).await;
        world.cell.update(|s| {
            s.expected_request_id = 2;
            s.ghost = None;
        });

        // When the actor handles the stale request 1, followed by a
        // completion barrier proving the actor drained its inbox.
        world
            .harness
            .publish(SuggestCwdCompletion {
                parent_dir: parent.path.clone(),
                segment: "al".to_owned(),
                request_id: 1,
            })
            .await;
        // The newer request's answer is what the guard must leave alone;
        // drain first so absence is an observation, not a race.
        let drained = jinn_testutil::wait::await_completion_then_assert_absent(
            async {
                jinn_testutil::wait::await_inbox_drained(
                    world.harness.system(),
                    &ActorPath::new(CWD_SUGGEST_PATH),
                    Duration::from_secs(5),
                )
                .await;
                1
            },
            Duration::from_secs(5),
            || world.cell.read().ghost.is_none(),
        )
        .await;

        // Then the stale reply never wrote the ghost.
        assert!(drained, "a stale reply must not write the ghost");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn missing_directory_clears_the_ghost() {
        // Given a cell holding a ghost from an earlier suggestion.
        let (world, _parent) = world_with_two_subdirs(&["alpha"]).await;
        world.cell.update(|s| {
            s.expected_request_id = 1;
            s.ghost = Some("alpha".to_owned());
        });
        let missing = jinn_testutil::volume_root()
            .join("jinn-cwd-suggest-missing-dir")
            .expect("root joins a bare name");

        // When the actor handles a request for a directory that does not
        // exist (listing fails -> empty entries -> no winner).
        world
            .harness
            .publish(SuggestCwdCompletion {
                parent_dir: missing,
                segment: "al".to_owned(),
                request_id: 1,
            })
            .await;
        await_cell(&world.cell, Duration::from_secs(5), |s| s.ghost.is_none()).await;

        // Then the ghost is cleared — the popup ghosts nothing.
        assert_eq!(world.cell.read().ghost, None);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn empty_segment_writes_the_first_directory_for_the_walk() {
        // Given a parent whose first directory alphabetically is `alpha`.
        let (world, parent) = world_with_two_subdirs(&["beta", "alpha"]).await;
        world.cell.update(|s| s.expected_request_id = 1);

        // When the actor handles a request with an empty segment (the
        // input ends in a separator).
        world
            .harness
            .publish(SuggestCwdCompletion {
                parent_dir: parent.path.clone(),
                segment: String::new(),
                request_id: 1,
            })
            .await;
        await_cell(&world.cell, Duration::from_secs(5), |s| s.ghost.is_some()).await;

        // Then the ghost is the alphabetically-first directory.
        assert_eq!(world.cell.read().ghost.as_deref(), Some("alpha"));
    }
}
