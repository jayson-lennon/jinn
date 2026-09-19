//! [`DirectoryListerActor`] — async directory listing for the `@path` popup.

use std::path::PathBuf;

use kameo::prelude::{Actor, ActorRef, Context, Message};
use serde::{Deserialize, Serialize};

use crate::common::actor_deps::{ActorDeps, BusPublish};
use crate::common::services::bus_service::BusService;
use crate::common::state::State;

use super::file_picker_state::FileEntry;

/// Command: list the directory at `path` (already resolved absolute) for the
/// active session's `@path` popup.
///
/// `request_id` is the staleness token. The actor writes its result only when
/// this matches `frontend.file_picker.expected_request_id`, so an earlier,
/// slow read cannot overwrite a newer one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListDirectory {
    /// The session whose popup this listing is for.
    pub session_id: crate::SessionId,
    /// Resolved absolute directory to list.
    pub path: PathBuf,
    /// Monotonic id tying this request to the expected reply slot.
    pub request_id: u64,
}

impl crate::common::bus::BusMessage for ListDirectory {}

jinn_slices::crossing_schema!(ListDirectory, "ListDirectory",
trouper::schema::SchemaKind::Command,
description: "List a directory for the file picker popup.",
fields: ["session_id" => trouper::schema::FieldTy::Uuid,
"request_id" => trouper::schema::FieldTy::Int,]);

/// Dependencies for [`DirectoryListerActor`].
#[derive(Clone)]
pub struct DirectoryListerActorDeps {
    /// Runtime services and bus access.
    pub deps: ActorDeps,
    /// Shared application state.
    pub state: State,
    /// Authority to write `frontend.file_picker`.
    pub frontend_cap: crate::common::tcaps::frontend::FrontendCap,
}

/// Lists directories on `ListDirectory` commands and writes results to
/// `frontend.file_picker`.
pub struct DirectoryListerActor {
    /// Bus service.
    bus: BusService,
    /// Shared application state.
    state: State,
    /// Authority to write `frontend.file_picker`.
    frontend_cap: crate::common::tcaps::frontend::FrontendCap,
}

impl BusPublish for DirectoryListerActor {
    fn bus(&self) -> &BusService {
        &self.bus
    }
}

impl Actor for DirectoryListerActor {
    type Args = DirectoryListerActorDeps;
    type Error = std::convert::Infallible;

    async fn on_start(args: Self::Args, actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
        let bus = args.deps.services.bus.clone();
        bus.subscribe::<ListDirectory, _>(&actor_ref).await;
        Ok(Self {
            bus,
            state: args.state,
            frontend_cap: args.frontend_cap,
        })
    }
}

impl Message<ListDirectory> for DirectoryListerActor {
    type Reply = ();

    async fn handle(&mut self, msg: ListDirectory, _ctx: &mut Context<Self, Self::Reply>) {
        let path = msg.path.clone();
        let request_id = msg.request_id;
        let result = tokio::task::spawn_blocking(move || list_dir_blocking(&path)).await;
        let entries = result.unwrap_or_default();

        // Staleness guard: write only if this reply is still the expected one.
        self.state.with_file_picker(&self.frontend_cap, |ops| {
            let picker = ops.file_picker();
            if picker.expected_request_id == request_id {
                picker.entries = entries;
                picker.loading = false;
            }
        });
    }
}

/// Reads a directory on a blocking thread. On any error (missing dir,
/// permission denied), returns an empty list — the popup shows `<empty>`.
fn list_dir_blocking(path: &std::path::Path) -> Vec<FileEntry> {
    let read = std::fs::read_dir(path);
    let Ok(read) = read else {
        return Vec::new();
    };
    let mut entries: Vec<FileEntry> = read
        .filter_map(std::result::Result::ok)
        .map(|entry| {
            let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
            FileEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                is_dir,
            }
        })
        .collect();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries
}
