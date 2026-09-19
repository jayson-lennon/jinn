#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "test code"
)]

use std::path::PathBuf;

use kameo::prelude::Spawn;

use crate::common::actor_deps::ActorDeps;
use crate::common::app_paths::AppPaths;
use crate::common::app_state::AppState;
use crate::common::bus::test_harness::TestHarness;
use crate::common::services::test_services::TestServices;
use crate::common::state::State;
use crate::common::tcaps::mint::mint_frontend_cap;

use super::directory_lister_actor::{
    DirectoryListerActor, DirectoryListerActorDeps, ListDirectory,
};
use super::file_picker_state::{FileEntry, FilePickerState, resolve_list_dir};

/// Test helper: a non-directory entry.
fn file_entry(name: &str) -> FileEntry {
    FileEntry {
        name: name.into(),
        is_dir: false,
    }
}

fn mint_cap() -> crate::common::tcaps::frontend::FrontendCap {
    mint_frontend_cap()
}

// ── resolve_list_dir ───────────────────────────────────────────────────────

#[rstest::rstest]
#[test]
fn resolve_empty_filter_returns_cwd() {
    // Given a cwd and home.
    let cwd = PathBuf::from("/proj");
    let home = PathBuf::from("/home/u");

    // When resolving an empty filter.
    let dir = resolve_list_dir("", &cwd, &home);

    // Then the result is the cwd.
    assert_eq!(dir, cwd);
}

#[rstest::rstest]
#[test]
fn resolve_absolute_filter_returns_path_as_is() {
    // Given a cwd, home, and an absolute filter.
    let cwd = PathBuf::from("/proj");
    let home = PathBuf::from("/home/u");

    // When resolving an absolute path.
    let dir = resolve_list_dir("/etc", &cwd, &home);

    // Then the result is the absolute path.
    assert_eq!(dir, PathBuf::from("/etc"));
}

#[rstest::rstest]
#[test]
fn resolve_tilde_filter_returns_home() {
    // Given a cwd and home.
    let cwd = PathBuf::from("/proj");
    let home = PathBuf::from("/home/u");

    // When resolving a bare tilde.
    let dir = resolve_list_dir("~", &cwd, &home);

    // Then the result is the home dir.
    assert_eq!(dir, home);
}

#[rstest::rstest]
#[test]
fn resolve_tilde_slash_filter_returns_home_subpath() {
    // Given a cwd and home.
    let cwd = PathBuf::from("/proj");
    let home = PathBuf::from("/home/u");

    // When resolving ~/sub.
    let dir = resolve_list_dir("~/sub", &cwd, &home);

    // Then the result is home/sub.
    assert_eq!(dir, PathBuf::from("/home/u/sub"));
}

#[rstest::rstest]
#[test]
fn resolve_relative_filter_joins_cwd() {
    // Given a cwd and home.
    let cwd = PathBuf::from("/proj");
    let home = PathBuf::from("/home/u");

    // When resolving a relative path `foo/bar`.
    let dir = resolve_list_dir("foo/bar", &cwd, &home);

    // Then the result is cwd/foo/bar.
    assert_eq!(dir, PathBuf::from("/proj/foo/bar"));
}

// ── FileEntry / FilePickerState ────────────────────────────────────────────

#[rstest::rstest]
#[test]
fn visible_entries_returns_all_when_filter_empty() {
    // Given a picker with two entries.
    let picker = FilePickerState::with_entries(vec![
        FileEntry {
            name: "a".into(),
            is_dir: false,
        },
        FileEntry {
            name: "b".into(),
            is_dir: true,
        },
    ]);

    // When filtering with an empty segment.
    let visible = picker.visible_entries("");

    // Then both entries are returned, in stored order.
    assert_eq!(visible.len(), 2);
    assert_eq!(visible[0].name, "a");
    assert_eq!(visible[1].name, "b");
}

#[rstest::rstest]
#[test]
fn visible_entries_narrows_by_last_segment() {
    // Given a picker with several entries.
    let picker = FilePickerState::with_entries(vec![
        file_entry("src"),
        file_entry("srv"),
        file_entry("static"),
        file_entry("img.png"),
    ]);

    // When filtering with the prefix "sr".
    let visible = picker.visible_entries("sr");

    // Then only entries starting with "sr" are returned.
    assert_eq!(visible.len(), 2);
    assert_eq!(visible[0].name, "src");
    assert_eq!(visible[1].name, "srv");
}

#[rstest::rstest]
#[test]
fn visible_entries_is_case_insensitive() {
    // Given a picker with mixed-case entries.
    let picker = FilePickerState::with_entries(vec![
        file_entry("README.md"),
        file_entry("readme.txt"),
        file_entry("src"),
    ]);

    // When filtering with a lowercase prefix.
    let visible = picker.visible_entries("read");

    // Then both case variants match.
    assert_eq!(visible.len(), 2);
    assert_eq!(visible[0].name, "README.md");
    assert_eq!(visible[1].name, "readme.txt");
}

#[rstest::rstest]
#[test]
fn visible_entries_uses_segment_after_last_slash() {
    // Given a picker and a filter that has already descended.
    let picker = FilePickerState::with_entries(vec![
        file_entry("src"),
        file_entry("srv"),
        file_entry("img.png"),
    ]);

    // When filtering with "foo/s" (already inside a deeper directory).
    let visible = picker.visible_entries("foo/s");

    // Then only the segment after the last `/` ("s") narrows the list.
    assert_eq!(visible.len(), 2);
    assert_eq!(visible[0].name, "src");
    assert_eq!(visible[1].name, "srv");
}

#[rstest::rstest]
#[test]
fn visible_entries_all_after_trailing_slash() {
    // Given a picker.
    let picker = FilePickerState::with_entries(vec![file_entry("src"), file_entry("img.png")]);

    // When filtering with "foo/" (trailing slash → empty segment).
    let visible = picker.visible_entries("foo/");

    // Then all entries are shown (empty segment matches everything).
    assert_eq!(visible.len(), 2);
}

// ── DirectoryListerActor (spawned harness) ─────────────────────────────────

async fn create_harness() -> (TestHarness, State, ActorDeps) {
    let harness = TestHarness::new().await;
    let state = State::new(AppState::default());
    let mut services = TestServices::builder()
        .paths(AppPaths::new_in(std::path::Path::new("")))
        .build();
    services.bus = harness.bus();
    services.trouper_system = harness.system().clone();
    let deps = ActorDeps { services };
    (harness, state, deps)
}

fn make_temp_dir(entries: &[(&str, bool)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("jinn-file-lister-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    for (name, is_dir) in entries {
        let path = dir.join(name);
        if *is_dir {
            std::fs::create_dir_all(&path).expect("create subdir");
        } else {
            std::fs::write(&path, b"x").expect("create file");
        }
    }
    dir
}

async fn wait_for_list_complete(state: &State) {
    // The actor clears `loading` when it finishes (success or error).
    // Poll until that happens, with a timeout.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while state.read().frontend.file_picker.loading {
        assert!(
            std::time::Instant::now() <= deadline,
            "timed out waiting for DirectoryListerActor to finish"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

async fn spawn_actor(deps: &ActorDeps, state: &State) -> trouper::actor::ActorPath {
    // The path is a placeholder; the actor self-subscribes to the domain
    // topic at its static path. Tests drive it via bus publishes only.
    DirectoryListerActor::spawn(
        &deps.services.trouper_system,
        DirectoryListerActorDeps {
            deps: deps.clone(),
            state: state.clone(),
            frontend_cap: mint_frontend_cap(),
        },
    )
}

#[rstest::rstest]
#[tokio::test]
async fn actor_reads_directory_entries_into_file_picker() {
    // Given a temp dir with a file and a subdirectory.
    let dir = make_temp_dir(&[("alpha.txt", false), ("subdir", true)]);
    let (harness, state, deps) = create_harness().await;
    let _actor = spawn_actor(&deps, &state).await;

    // Set the expected request id and mark loading.
    state.with_file_picker(&mint_cap(), |ops| {
        ops.file_picker().expected_request_id = 1;
        ops.file_picker().loading = true;
    });

    // When the actor lists the directory.
    harness
        .publish(ListDirectory {
            session_id: crate::SessionId::new(),
            path: dir.clone(),
            request_id: 1,
        })
        .await;

    wait_for_list_complete(&state).await;

    // Then the file picker is populated and loading is cleared.
    let entries = state.read().frontend.file_picker.entries.clone();
    let loading = state.read().frontend.file_picker.loading;
    assert!(
        !loading,
        "loading should be cleared after a successful read"
    );
    assert!(
        entries.iter().any(|e| e.name == "alpha.txt" && !e.is_dir),
        "file entry should be present: {entries:?}"
    );
    assert!(
        entries.iter().any(|e| e.name == "subdir" && e.is_dir),
        "dir entry should be present: {entries:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn actor_drops_stale_reply_when_request_id_mismatches() {
    // Given a temp dir with one file.
    let dir = make_temp_dir(&[("stale.txt", false)]);
    let (harness, state, deps) = create_harness().await;
    let _actor = spawn_actor(&deps, &state).await;

    // The expected id is 5, but we send a request with id 1 (stale).
    state.with_file_picker(&mint_cap(), |ops| {
        ops.file_picker().expected_request_id = 5;
        ops.file_picker().loading = true;
    });

    // When the actor processes a request whose id does not match.
    harness
        .publish(ListDirectory {
            session_id: crate::SessionId::new(),
            path: dir,
            request_id: 1, // stale
        })
        .await;

    // The actor processes the stale request but does NOT clear loading.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Then the stale reply is dropped: entries stay empty, loading unchanged.
    let entries = state.read().frontend.file_picker.entries.clone();
    let loading = state.read().frontend.file_picker.loading;
    assert!(entries.is_empty(), "stale reply must not populate entries");
    assert!(loading, "stale reply must not clear loading");
}

#[rstest::rstest]
#[tokio::test]
async fn actor_returns_empty_for_nonexistent_directory() {
    // Given an actor and a path that does not exist.
    let (harness, state, deps) = create_harness().await;
    let _actor = spawn_actor(&deps, &state).await;
    state.with_file_picker(&mint_cap(), |ops| {
        ops.file_picker().expected_request_id = 1;
        ops.file_picker().loading = true;
    });
    let bogus = PathBuf::from("/this/path/does/not/exist/jinn-test");

    // When the actor lists the nonexistent directory.
    harness
        .publish(ListDirectory {
            session_id: crate::SessionId::new(),
            path: bogus,
            request_id: 1,
        })
        .await;

    wait_for_list_complete(&state).await;

    // Then the entries are empty (not an error), loading cleared.
    let entries = state.read().frontend.file_picker.entries.clone();
    let loading = state.read().frontend.file_picker.loading;
    assert!(entries.is_empty(), "nonexistent dir yields empty entries");
    assert!(!loading, "loading should be cleared even on read error");
}

#[rstest::rstest]
#[tokio::test]
async fn actor_lists_hidden_files() {
    // Given a temp dir with a dotfile and a regular file.
    let dir = make_temp_dir(&[(".hidden", false), ("visible.txt", false)]);
    let (harness, state, deps) = create_harness().await;
    let _actor = spawn_actor(&deps, &state).await;
    state.with_file_picker(&mint_cap(), |ops| {
        ops.file_picker().expected_request_id = 1;
        ops.file_picker().loading = true;
    });

    // When the actor lists the directory.
    harness
        .publish(ListDirectory {
            session_id: crate::SessionId::new(),
            path: dir,
            request_id: 1,
        })
        .await;

    wait_for_list_complete(&state).await;

    // Then the dotfile appears in the listing (hidden files shown).
    let entries = state.read().frontend.file_picker.entries.clone();
    assert!(
        entries.iter().any(|e| e.name == ".hidden"),
        "hidden files should be listed: {entries:?}"
    );
}
