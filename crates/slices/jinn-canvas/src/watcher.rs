//! The notify-based directory watcher — what turns an edit into a reload.
//!
//! Watches the served file's parent directory (artifacts reference
//! sibling assets, so any change out there counts), debounces bursts
//! (editors write temp-file + rename, which lands as several events),
//! and pushes one reload signal per settled change onto the broadcast
//! channel the WebSocket route relays.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use notify::Watcher as _;
use tokio::sync::broadcast;

/// Events inside this window coalesce into one reload.
pub const DEBOUNCE: Duration = Duration::from_millis(250);

/// The settled-change signal: one unit per debounce window.
pub type ReloadSignal = broadcast::Sender<()>;

/// Errors the watcher can hit at spawn. Mid-watch failures end the loop
/// with a log line instead: a dead watcher means no live reload, never a
/// wrong render, so the server keeps serving.
#[derive(Debug, wherror::Error)]
#[error(debug)]
pub enum WatcherError {
    /// The watch could not be established.
    Establish,
}

/// Establishes the watch and spawns the debounce loop: every settled
/// change in `dir` sends one `()` on `reload_tx`. The server calls this
/// once at startup; re-targets go through [`establish`] so the previous
/// watcher is dropped with the swap.
///
/// # Errors
///
/// Returns [`WatcherError::Establish`] when the directory cannot be
/// watched (it was deleted, permissions changed).
pub fn spawn(
    dir: &Path,
    reload_tx: broadcast::Sender<()>,
) -> Result<(), error_stack::Report<WatcherError>> {
    // The loop task owns its handle clone, so dropping this one does not
    // unregister the watch — the watch dies only when the loop dies.
    establish(dir, reload_tx).map(|_watcher| ())
}

/// Establishes the watch, spawns the debounce loop, and returns the
/// watcher handle: the caller must keep it alive (dropping it
/// unregisters the watch), which is what makes the re-target's
/// swap-and-drop correct.
///
/// # Errors
///
/// Returns [`WatcherError::Establish`] when the directory cannot be
/// watched (it was deleted, permissions changed).
pub fn establish(
    dir: &Path,
    reload_tx: broadcast::Sender<()>,
) -> Result<Arc<notify::RecommendedWatcher>, error_stack::Report<WatcherError>> {
    use error_stack::ResultExt;

    // The notify callback runs on watcher threads; an unbounded tokio
    // channel hands events to the async loop without either side
    // blocking (`try_send` never parks, the loop always awaits).
    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    let mut watcher =
        notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
            // A dropped loop reads as shutdown; dropping the event is
            // then correct — nobody is left to reload.
            let _ = event_tx.send(res.map_err_into_stream_failure());
        })
        .change_context(WatcherError::Establish)
        .attach(format!("watching {}", dir.display()))
        .attach("failed to establish canvas directory watcher")?;

    watcher
        .watch(dir, notify::RecursiveMode::NonRecursive)
        .change_context(WatcherError::Establish)
        .attach(format!("watching {}", dir.display()))
        .attach("failed to bind canvas directory watcher")?;

    // Keep the watcher alive for the loop's lifetime; dropping it would
    // unregister the watch. The handle is returned to the caller for
    // exactly that reason.
    let watcher = Arc::new(watcher);
    tokio::spawn(watch_loop(Arc::clone(&watcher), event_rx, reload_tx));
    Ok(watcher)
}

/// One watcher event: what arrived, or that the stream failed.
type Event = Result<notify::Event, StreamFailure>;

/// Marks an event-read failure so the loop can tell it from a real event.
struct StreamFailure;

/// Whether this event means "content changed". Only write-shaped kinds
/// count: Modify/Create/Remove. Reads (Access) are noise the server
/// itself generates — `serve_artifact` reads the artifact on every
/// `GET /`, so counting Access events makes the server reload every
/// browser on every page load, forever. Renames of interest surface as
/// Modify(Name)/Create (the atomic-save pattern lands as both).
#[must_use]
fn is_write_event(event: &notify::Event) -> bool {
    use notify::EventKind;
    matches!(
        event.kind,
        EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
    )
}

/// The loop: debounce event bursts, signal once per settled window.
async fn watch_loop(
    // Owned here purely to keep the watch registered; never read.
    _watcher: Arc<notify::RecommendedWatcher>,
    mut event_rx: tokio::sync::mpsc::UnboundedReceiver<Event>,
    reload_tx: broadcast::Sender<()>,
) {
    loop {
        // Wait for the first write-shaped event of a burst; reads and
        // other access noise are dropped without opening a window.
        loop {
            match event_rx.recv().await {
                // The channel closed: the watcher is gone, no reloads.
                None => {
                    tracing::warn!("canvas watcher event stream ended; live reload is off");
                    return;
                }
                // A watcher-stream failure ends live reload: a dead
                // watcher means no live reload, never a wrong render.
                Some(Err(_)) => {
                    tracing::warn!("canvas watcher event stream failed; live reload is off");
                    return;
                }
                Some(Ok(event)) => {
                    if is_write_event(&event) {
                        break;
                    }
                }
            }
        }
        // Settle: drain whatever else lands inside the debounce window.
        let deadline = tokio::time::Instant::now() + DEBOUNCE;
        loop {
            match tokio::time::timeout_at(deadline, event_rx.recv()).await {
                // Another event arrived inside the window; keep draining.
                // Reads inside the window are skipped, same as outside.
                Ok(Some(Ok(event))) if !is_write_event(&event) => {}
                // A write inside the window keeps draining: the settle
                // deadline governs when the window closes.
                Ok(Some(Ok(_))) => {}
                // The channel closed (mid-window or not): fire what
                // settled — the content did change — and let the next
                // outer recv see the closure and exit cleanly.
                Ok(Some(Err(_)) | None) => break,
                // The window closed with no further events: fire.
                Err(_elapsed) => break,
            }
        }
        // A lost broadcast receiver is fine: no browser is connected.
        let _ = reload_tx.send(());
    }
}

/// Lifts a notify result into the loop's event type.
trait MapErrIntoStreamFailure {
    fn map_err_into_stream_failure(self) -> Event;
}
impl MapErrIntoStreamFailure for Result<notify::Event, notify::Error> {
    fn map_err_into_stream_failure(self) -> Event {
        self.map_err(|_source| StreamFailure)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test assertions need expect")]

    use std::time::Duration;

    use marked_path::MarkedPathAccess as _;
    use tokio::sync::broadcast;

    use super::{DEBOUNCE, spawn};

    /// Writes `path` via the temp-file + rename pattern editors use, so
    /// the watcher sees what a real save produces.
    fn atomic_write(path: &std::path::Path, contents: &str) {
        let tmp = path.with_extension("html.tmp");
        std::fs::write(&tmp, contents).expect("write temp file");
        std::fs::rename(&tmp, path).expect("rename into place");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn atomic_save_settles_into_one_reload() {
        // Given a watched directory holding the artifact.
        let dir = jinn_testutil::temp::tempdir();
        let artifact = dir
            .path
            .join("chart.html")
            .expect("name join keeps the root absolute");
        std::fs::write(artifact.as_path(), "<html></html>").expect("seed artifact");
        let (reload_tx, mut reload_rx) = broadcast::channel(16);
        spawn(artifact.as_path().parent().expect("has parent"), reload_tx)
            .expect("watcher establishes");

        // When saving the artifact atomically.
        atomic_write(artifact.as_path(), "<html><body>v2</body></html>");

        // Then exactly one reload signal arrives (the temp+rename burst
        // coalesces inside the debounce window).
        let received = tokio::time::timeout(DEBOUNCE * 3, reload_rx.recv())
            .await
            .expect("reload arrives");
        received.expect("broadcast live");
        tokio::time::sleep(DEBOUNCE).await;
        assert!(
            reload_rx.try_recv().is_err(),
            "one atomic save must produce exactly one reload"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn file_reads_produce_no_reload() {
        // Given a watched directory holding the artifact.
        let dir = jinn_testutil::temp::tempdir();
        let artifact = dir
            .path
            .join("chart.html")
            .expect("name join keeps the root absolute");
        std::fs::write(artifact.as_path(), "<html></html>").expect("seed artifact");
        let (reload_tx, mut reload_rx) = broadcast::channel(16);
        spawn(artifact.as_path().parent().expect("has parent"), reload_tx)
            .expect("watcher establishes");

        // When the file is read repeatedly — what the server itself does
        // on every `GET /`, and what `ls`/tab-completion do directory-wide.
        for _ in 0..5 {
            let bytes = std::fs::read(artifact.as_path()).expect("read artifact");
            std::thread::sleep(Duration::from_millis(50));
            let _ = bytes;
        }

        // Then no reload signal arrives: reads are not writes, and
        // counting them loops every connected browser on every page load.
        let no_reload = tokio::time::timeout(DEBOUNCE * 4, reload_rx.recv()).await;
        assert!(
            no_reload.is_err(),
            "a plain file read must not trigger a reload"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn sibling_asset_change_triggers_reload_too() {
        // Given a watched directory whose artifact references a sibling.
        let dir = jinn_testutil::temp::tempdir();
        let artifact = dir
            .path
            .join("chart.html")
            .expect("name join keeps the root absolute");
        std::fs::write(artifact.as_path(), "<html></html>").expect("seed artifact");
        let (reload_tx, mut reload_rx) = broadcast::channel(16);
        spawn(artifact.as_path().parent().expect("has parent"), reload_tx)
            .expect("watcher establishes");

        // When the sibling changes.
        let sibling = dir.path.join("data.json").expect("name join");
        std::fs::write(sibling.as_path(), "{}").expect("write sibling");

        // Then a reload signal arrives.
        tokio::time::timeout(Duration::from_millis(1500), async {
            loop {
                reload_rx.recv().await.expect("broadcast live");
            }
        })
        .await
        .expect_err("reload arrived");
    }
}
