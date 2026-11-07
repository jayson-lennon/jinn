//! End-to-end tests for the in-process canvas actor, driven through the
//! same handle the `canvas` tool holds.
//!
//! Every test spawns a real [`CanvasActor`] on a test trouper system,
//! asks through the minted [`CanvasServerHandle`], and asserts at the
//! HTTP/WS surface with the hand-rolled loopback client the detached
//! e2e used — rehosted on the in-process path. The bind is port `0`
//! (the shipped default), so no test ever fights a real port.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::missing_panics_doc,
    reason = "test harness: a failed expect aborts the test loudly"
)]

use std::io::Read as _;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use jinn_canvas::canvas_actor::{CanvasActor, CanvasActorDeps, canvas_server_handle};
use jinn_canvas_msg::CanvasServeOutcome;
use jinn_canvas_msg::CanvasServerHandle;
use jinn_testutil::bus_harness::{TestHarness, await_recorded};
use marked_path::{Absolute, MarkedPathAccess as _, MarkedPathBuf};

/// A spawned canvas actor wired to the harness fabric, with its
/// tool-facing handle and the artifact directory it serves from.
struct ActorFixture {
    handle: Arc<dyn CanvasServerHandle>,
    /// Collects every `ServiceStatusUpdate` the actor publishes, via
    /// the harness's declarant recorder.
    statuses: jinn_testutil::bus_harness::Recorder<jinn_slices::ServiceStatusUpdate>,
    dir: jinn_testutil::JinnTempDir,
    /// The artifact path the serve helpers re-serve; set by each test
    /// after seeding.
    serve_path: String,
    /// The recorder's tap actor — the reload tests publish through its
    /// bus.
    harness: TestHarness,
}

/// Spawns the actor at a port-0 bind, the shipped default.
async fn spawn_actor() -> ActorFixture {
    spawn_actor_with(jinn_config::testutil::config_layer("")).await
}

/// Spawns the actor at a port-0 bind over `config` — the reload tests
/// point the layer at a `[canvas]` section and edit it behind the
/// actor's back, the way the config-watch slice's validated swap does.
async fn spawn_actor_with(config: jinn_config::ConfigLayer) -> ActorFixture {
    let harness = TestHarness::new().await;
    let statuses = harness
        .spawn_recorder::<jinn_slices::ServiceStatusUpdate>()
        .await;
    let handle = CanvasActor::spawn(
        harness.system(),
        &CanvasActorDeps {
            bind: "127.0.0.1:0".parse::<SocketAddr>().expect("bind parses"),
            config,
            bus: harness.bus(),
        },
    );
    ActorFixture {
        handle: canvas_server_handle(harness.system().clone(), handle),
        statuses,
        dir: jinn_testutil::retained_tempdir(),
        serve_path: String::new(),
        harness,
    }
}

impl ActorFixture {
    /// Writes an artifact into the fixture's directory.
    fn seed(&self, name: &str, contents: &str) -> MarkedPathBuf<Absolute> {
        let path = self.dir.path.join(name).expect("name join keeps root");
        std::fs::write(path.as_path(), contents).expect("seed artifact");
        path
    }

    /// Waits for a status publication naming the canvas row that is not
    /// the spawn-time baseline. The spawn handshake publishes
    /// `server offline` before any ask can land, so the publication a
    /// serve produced is the first non-offline one.
    async fn next_serve_status(&self) -> jinn_slices::ServiceStatusUpdate {
        // Drain whatever has landed; retry until a non-offline status
        // shows up or the bound expires.
        let deadline = tokio::time::Instant::now() + STATUS_TIMEOUT;
        loop {
            let updates = await_recorded(&self.statuses, 1, STATUS_TIMEOUT).await;
            if let Some(found) = updates.iter().find(|u| {
                u.name == "canvas" && u.status_message.as_deref() != Some("server offline")
            }) {
                return found.clone();
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "no canvas serve status arrived; offline baseline only"
            );
        }
    }
}

/// How long any single blocking read may hold the connection.
const HTTP_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Runs a blocking closure on the blocking pool: the single-threaded
/// test runtime thread is the one that polls the trouper tasks and the
/// axum server task — no socket I/O may park it, or the response being
/// waited for can never be produced (and a blocked thread fires no
/// timers, so even the test timeout would hang).
async fn off_runtime<R>(io: impl FnOnce() -> R + Send + 'static) -> R
where
    R: Send + 'static,
{
    tokio::task::spawn_blocking(io)
        .await
        .expect("blocking task joins")
}

/// A loopback HTTP/1.0 GET returning the raw response bytes.
async fn http_get(bind: SocketAddr, path: &str) -> Vec<u8> {
    let request = format!("GET {path} HTTP/1.0\r\nHost: localhost\r\n\r\n");
    off_runtime(move || {
        use std::io::Write as _;
        let mut stream = std::net::TcpStream::connect_timeout(&bind, Duration::from_secs(5))
            .expect("canvas server reachable");
        stream
            .set_read_timeout(Some(HTTP_READ_TIMEOUT))
            .expect("read timeout applies");
        stream
            .write_all(request.as_bytes())
            .expect("request written");
        let mut body = Vec::new();
        stream.read_to_end(&mut body).expect("response read");
        body
    })
    .await
}

/// Extracts the body from a raw HTTP response (first `\r\n\r\n` split).
fn body_of(raw: &[u8]) -> String {
    let idx = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("response headers terminate");
    String::from_utf8_lossy(
        raw.get(idx + 4..)
            .expect("headers terminate within the response"),
    )
    .into_owned()
}

/// Writes `contents` to `path` via temp-file + rename — the atomic-save
/// pattern editors actually produce.
fn atomic_write(path: &std::path::Path, contents: &str) {
    let tmp = path.with_extension("html.tmp");
    std::fs::write(&tmp, contents).expect("temp file written");
    std::fs::rename(&tmp, path).expect("rename into place");
}

/// How long a status publication may take to cross the fabric.
const STATUS_TIMEOUT: Duration = Duration::from_secs(5);

/// The URL from a successful outcome, parsed back to its socket address.
fn bind_of_url(url: &str) -> SocketAddr {
    url.strip_prefix("http://")
        .expect("the URL carries the http scheme")
        .parse::<SocketAddr>()
        .expect("the URL's authority is a socket address")
}

#[rstest::rstest]
#[tokio::test]
async fn first_serve_answers_the_url_with_the_artifact_injected() {
    // Given a dormant canvas actor and an artifact on disk.
    let fixture = spawn_actor().await;
    let artifact = fixture.seed(
        "chart.html",
        "<html><head><title>v1</title></head><body>v1</body></html>",
    );

    // When asking the actor to serve it (the tool's ask).
    let outcome = fixture
        .handle
        .serve(artifact.to_string_lossy().into_owned())
        .await
        .expect("the round trip completes");

    // Then the outcome carries the http-schemed URL and the canonical path.
    assert!(outcome.ok, "serve refused: {}", outcome.error);
    assert!(
        outcome.url.starts_with("http://127.0.0.1:"),
        "{}",
        outcome.url
    );
    assert!(outcome.artifact.ends_with("chart.html"));

    // And the served page is the artifact with the reload snippet
    // injected — the content a browser at that URL renders.
    let bind = bind_of_url(&outcome.url);
    let body = body_of(&http_get(bind, "/").await);
    assert!(body.contains("<title>v1</title>"));
    assert!(body.contains("new WebSocket"));
    // And the file on disk is untouched — injection is response-only.
    let on_disk = std::fs::read(artifact.as_path()).expect("artifact readable");
    assert_eq!(
        String::from_utf8_lossy(&on_disk),
        "<html><head><title>v1</title></head><body>v1</body></html>"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn first_serve_publishes_the_serving_status_with_scheme_and_path() {
    // Given a dormant canvas actor and an artifact on disk.
    let fixture = spawn_actor().await;
    let artifact = fixture.seed("chart.html", "<html><head></head></html>");

    // When serving it and waiting for the actor's status publication.
    let outcome = fixture
        .handle
        .serve(artifact.to_string_lossy().into_owned())
        .await
        .expect("the round trip completes");
    let update = fixture.next_serve_status().await;

    // Then the row reads `http://<bind> (<path>)` — the link-shaped
    // contract string.
    assert_eq!(update.name, "canvas");
    let status = update.status_message.expect("serving status set");
    assert_eq!(
        status,
        format!("{} ({})", outcome.url, outcome.artifact),
        "status must be scheme'd URL, space, path in parens"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn atomic_save_edit_pushes_a_reload_to_a_connected_ws_client() {
    // Given a serving actor and a WebSocket client subscribed to /ws.
    let fixture = spawn_actor().await;
    let artifact = fixture.seed(
        "chart.html",
        "<html><head><title>v1</title></head><body>v1</body></html>",
    );
    let outcome = fixture
        .handle
        .serve(artifact.to_string_lossy().into_owned())
        .await
        .expect("serve succeeds");
    let bind = bind_of_url(&outcome.url);
    let ws = ws_connect(bind).await;
    ws_send_handshake_done(&ws).await;

    // When editing the artifact atomically.
    atomic_write(
        artifact.as_path(),
        "<html><head><title>v2</title></head><body>v2</body></html>",
    );

    // Then the client receives the reload message.
    let frame = ws_read_frame(&ws).await;
    assert_eq!(frame, "reload");

    // And a fresh GET now serves the edited content.
    let body = body_of(&http_get(bind, "/").await);
    assert!(body.contains("<title>v2</title>"));
}

#[rstest::rstest]
#[tokio::test]
async fn second_serve_retargets_the_same_bind_without_restart() {
    // Given a serving actor with a connected WS client.
    let fixture = spawn_actor().await;
    let first = fixture.seed("chart.html", "<html><head><title>v1</title></head></html>");
    let outcome = fixture
        .handle
        .serve(first.to_string_lossy().into_owned())
        .await
        .expect("serve succeeds");
    let bind = bind_of_url(&outcome.url);
    let ws = ws_connect(bind).await;
    ws_send_handshake_done(&ws).await;

    // When serving a second artifact through the same handle.
    let second = fixture.seed("second.html", "<html><head><title>r2</title></head></html>");
    let retarget = fixture
        .handle
        .serve(second.to_string_lossy().into_owned())
        .await
        .expect("the round trip completes");

    // Then the URL is the same bind — the listener never restarted.
    assert_eq!(retarget.url, outcome.url, "same bind across serves");
    assert!(retarget.artifact.ends_with("second.html"));

    // And the connected browser was told to reload, and the fresh GET
    // serves the second artifact.
    assert_eq!(ws_read_frame(&ws).await, "reload");
    let body = body_of(&http_get(bind, "/").await);
    assert!(body.contains("<title>r2</title>"));
}

#[rstest::rstest]
#[tokio::test]
async fn refused_serve_names_the_cause_and_publishes_the_error_note() {
    // Given a dormant canvas actor.
    let fixture = spawn_actor().await;
    let missing = fixture.dir.path.join("nope.html").expect("join");

    // When serving a path that does not exist.
    let outcome = fixture
        .handle
        .serve(missing.to_string_lossy().into_owned())
        .await
        .expect("the round trip completes (the refusal is a real reply)");

    // Then the outcome carries the cause and the fix, not a success.
    assert!(!outcome.ok);
    assert!(
        outcome.error.contains("existing HTML file"),
        "{}",
        outcome.error
    );
    assert!(outcome.error.contains("fix:"), "{}", outcome.error);

    // And the dashboard row carries the refusal as an error-toned note.
    let update = fixture.next_serve_status().await;
    assert_eq!(
        update.status_message.as_deref(),
        Some(outcome.error.as_str())
    );
    assert_eq!(update.note_tone, Some(jinn_slices::NoteTone::Error));
}

#[rstest::rstest]
#[tokio::test]
async fn sibling_file_write_pushes_a_reload_too() {
    // Given a serving actor with a connected WS client.
    let fixture = spawn_actor().await;
    let artifact = fixture.seed("chart.html", "<html><head></head></html>");
    let outcome = fixture
        .handle
        .serve(artifact.to_string_lossy().into_owned())
        .await
        .expect("serve succeeds");
    let bind = bind_of_url(&outcome.url);
    let ws = ws_connect(bind).await;
    ws_send_handshake_done(&ws).await;

    // When a sibling file in the same directory is written.
    let sibling = fixture.seed("data.json", "{}");
    atomic_write(sibling.as_path(), "{\"v\":2}");

    // Then the client still receives the reload — the watch is
    // directory-scoped.
    assert_eq!(ws_read_frame(&ws).await, "reload");
}

#[rstest::rstest]
#[tokio::test]
async fn vendor_route_serves_the_gzip_marked_stylesheet() {
    // Given a serving actor.
    let fixture = spawn_actor().await;
    let artifact = fixture.seed("chart.html", "<html><head></head></html>");
    let outcome = fixture
        .handle
        .serve(artifact.to_string_lossy().into_owned())
        .await
        .expect("serve succeeds");
    let bind = bind_of_url(&outcome.url);

    // When fetching the authored stylesheet through the vendor route.
    let raw = http_get(bind, "/vendor/canvas.css").await;
    let head = String::from_utf8_lossy(&raw).into_owned();

    // Then the response carries the stylesheet's pinned type.
    assert!(
        head.contains("text/css"),
        "{}",
        head.lines().next().unwrap_or("")
    );
}

#[rstest::rstest]
#[tokio::test]
async fn vendor_route_serves_three_core_with_gzip_marking() {
    // Given a serving actor.
    let fixture = spawn_actor().await;
    let artifact = fixture.seed("scene.html", "<html><head></head></html>");
    let outcome = fixture
        .handle
        .serve(artifact.to_string_lossy().into_owned())
        .await
        .expect("serve succeeds");
    let bind = bind_of_url(&outcome.url);

    // When fetching the three.js core module build through the vendor
    // route.
    let raw = http_get(bind, "/vendor/three/three.module.min.js").await;
    let head = String::from_utf8_lossy(&raw).into_owned();
    let status = head.lines().next().unwrap_or("");

    // Then it serves 200, typed as JavaScript, gzip-marked — the shape a
    // browser's module loader requires of an import-map target.
    assert!(status.contains("200"), "{status}");
    assert!(head.contains("text/javascript"), "{status}");
    assert!(head.to_ascii_lowercase().contains("content-encoding: gzip"));
}

#[rstest::rstest]
#[tokio::test]
async fn vendor_route_serves_three_addons_at_their_import_map_paths() {
    // Given a serving actor.
    let fixture = spawn_actor().await;
    let artifact = fixture.seed("scene.html", "<html><head></head></html>");
    let outcome = fixture
        .handle
        .serve(artifact.to_string_lossy().into_owned())
        .await
        .expect("serve succeeds");
    let bind = bind_of_url(&outcome.url);

    // When fetching each addon at the exact path the artifact import
    // map and the addons' own relative imports resolve to.
    for path in [
        "/vendor/three/three.core.min.js",
        "/vendor/three/addons/controls/OrbitControls.js",
        "/vendor/three/addons/loaders/GLTFLoader.js",
        "/vendor/three/addons/loaders/STLLoader.js",
        "/vendor/three/addons/loaders/OBJLoader.js",
        "/vendor/three/addons/controls/PointerLockControls.js",
        "/vendor/three/addons/geometries/ParametricGeometry.js",
        "/vendor/three/addons/geometries/ParametricFunctions.js",
        "/vendor/three/addons/utils/BufferGeometryUtils.js",
        "/vendor/three/addons/utils/SkeletonUtils.js",
    ] {
        let raw = http_get(bind, path).await;
        let head = String::from_utf8_lossy(&raw).into_owned();
        let status = head.lines().next().unwrap_or("");

        // Then each serves 200 with the JavaScript type — a miss here
        // means the import map points at nothing.
        assert!(status.contains("200"), "{path}: {status}");
        assert!(head.contains("text/javascript"), "{path}: {status}");
    }
}

// -- WebSocket test client (hand-rolled; the server accepts any WS
//    upgrade axum accepts, and the client below speaks just enough RFC
//    6455 for one text frame).

async fn ws_connect(bind: SocketAddr) -> std::net::TcpStream {
    // Fixed key per RFC 6455 example; the server only needs a well-formed upgrade.
    let key = "dGhlIHNhbXBsZSBub25jZQ==";
    let request = format!(
        "GET /ws HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
    );
    off_runtime(move || {
        use std::io::Write as _;
        let mut stream = std::net::TcpStream::connect_timeout(&bind, Duration::from_secs(5))
            .expect("ws connect");
        // Read deadlines on every blocking read below: without one, a
        // missing frame would fail with an opaque timeout error instead
        // of a clear one.
        stream
            .set_read_timeout(Some(WS_READ_TIMEOUT))
            .expect("read timeout applies");
        stream
            .write_all(request.as_bytes())
            .expect("handshake written");
        stream.flush().expect("handshake flushed");
        stream
    })
    .await
}

/// How long any single blocking read may hold the connection.
const WS_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Runs a blocking read on the blocking pool: the reads below must
/// never park the single-threaded test runtime, which is the very
/// thread that has to poll the trouper tasks producing the frames.
async fn blocking_read<R>(
    stream: &std::net::TcpStream,
    read: impl FnOnce(&mut std::net::TcpStream) -> std::io::Result<R> + Send + 'static,
) -> R
where
    R: Send + 'static,
{
    let mut owned = stream
        .try_clone()
        .expect("the ws socket clones for the blocking pool");
    off_runtime(move || read(&mut owned).expect("blocking read completes")).await
}

async fn ws_send_handshake_done(stream: &std::net::TcpStream) {
    let reply = blocking_read(stream, |s| {
        let mut buf = [0u8; 1024];
        let read = std::io::Read::read(s, &mut buf)?;
        String::from_utf8_lossy(buf.get(..read).expect("read fills within buffer")).into_owned();
        Ok(
            String::from_utf8_lossy(buf.get(..read).expect("read fills within buffer"))
                .into_owned(),
        )
    })
    .await;
    assert!(reply.contains("101"), "upgrade refused: {reply}");
}

/// Reads one server frame (server frames are unmasked per RFC 6455) and
/// returns its UTF-8 payload.
async fn ws_read_frame(stream: &std::net::TcpStream) -> String {
    let header = blocking_read(stream, |s| {
        let mut header = [0u8; 2];
        std::io::Read::read_exact(s, &mut header).map(|()| header)
    })
    .await;
    let len = (header[1] & 0x7f) as usize;
    let payload_bytes = blocking_read(stream, move |s| {
        let mut payload_bytes = vec![0u8; len];
        std::io::Read::read_exact(s, &mut payload_bytes).map(|()| payload_bytes)
    })
    .await;
    String::from_utf8(payload_bytes).expect("text frame")
}

/// Polls until `serve` answers at `port`, driving the actor's heartbeat
/// (the drain tick applies a pending bind when quiet).
async fn wait_for_serve_port(fixture: &ActorFixture, port: u16) -> CanvasServeOutcome {
    let deadline = tokio::time::Instant::now() + STATUS_TIMEOUT;
    loop {
        let outcome = fixture
            .handle
            .serve(fixture.serve_path.clone())
            .await
            .expect("the round trip completes");
        if outcome.ok && bind_of_url(&outcome.url).port() == port {
            return outcome;
        }
        fixture
            .handle
            .drain_metrics()
            .await
            .expect("drain completes");
        assert!(
            tokio::time::Instant::now() < deadline,
            "pending bind never applied; serve still at {}",
            outcome.url
        );
    }
}

#[rstest::rstest]
#[tokio::test]
async fn bind_change_while_serving_names_it_pending_and_applies_when_the_last_client_leaves() {
    // Given a serving canvas actor over a layer whose [canvas] bind
    // names a second (held, then released) port, and a browser holding
    // the served page.
    let probe = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("probe binds");
    let port_b = probe.local_addr().expect("probe addr").port();
    let config = jinn_config::testutil::config_layer("");
    let layer = config.clone();
    let mut fixture = spawn_actor_with(config).await;
    let artifact = fixture.seed("chart.html", "<html><body>a</body></html>");
    fixture.serve_path = artifact.to_string_lossy().into_owned();
    let first = fixture
        .handle
        .serve(fixture.serve_path.clone())
        .await
        .expect("the round trip completes");
    assert!(first.ok, "serve refused: {}", first.error);
    let url_a = first.url.clone();
    let port_a = bind_of_url(&url_a).port();
    let ws = ws_connect(bind_of_url(&url_a)).await;

    // When the bind changes behind the actor's back and a reload
    // re-reads the layer (the config-watch slice's own write path).
    layer
        .put(&jinn_preferences_config::schemas::CanvasConfig {
            bind: format!("127.0.0.1:{port_b}"),
        })
        .expect("layer put lands");
    fixture
        .harness
        .bus()
        .publish(jinn_boot_msg::JinnTomlReloaded)
        .await;

    // Then the live listener is unchanged — the same URL still serves —
    // and the row names the change pending with a warning tone.
    let again = fixture
        .handle
        .serve(fixture.serve_path.clone())
        .await
        .expect("the round trip completes");
    assert_eq!(
        bind_of_url(&again.url).port(),
        port_a,
        "live listener stays while a client holds the page"
    );
    let deadline = tokio::time::Instant::now() + STATUS_TIMEOUT;
    let pending = loop {
        if let Some(found) = fixture.statuses.drain().into_iter().find(|u| {
            u.name == "canvas"
                && u.status_message
                    .as_deref()
                    .is_some_and(|m| m.contains("pending"))
        }) {
            break found;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "pending-bind status never arrived; got {:?}",
            fixture.statuses.drain()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(
        pending
            .status_message
            .as_deref()
            .is_some_and(|m| m.contains(&format!("127.0.0.1:{port_b}"))),
        "note must name the new bind: {:?}",
        pending.status_message
    );
    assert_eq!(pending.note_tone, Some(jinn_slices::NoteTone::Warning));

    // When the last client leaves, the heartbeat applies the pending
    // bind: the fresh URL serves at port_b.
    drop(probe);
    drop(ws);
    let rebound = wait_for_serve_port(&fixture, port_b).await;

    // Then the new listener answers end to end, and the old one closed.
    let new_bind = bind_of_url(&rebound.url);
    let body = body_of(&http_get(new_bind, "/").await);
    assert!(body.contains("a</body>"), "new bind serves: {body}");
    let old_closed = off_runtime(move || {
        std::net::TcpStream::connect_timeout(
            &format!("127.0.0.1:{port_a}").parse().expect("bind parses"),
            Duration::from_secs(1),
        )
        .is_err()
    })
    .await;
    assert!(
        old_closed,
        "old listener must stop once the pending bind applied"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn bind_change_while_dormant_adopts_the_new_bind_at_the_next_serve() {
    // Given a dormant actor over a layer with a second bind.
    let probe = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("probe binds");
    let port_b = probe.local_addr().expect("probe addr").port();
    let config = jinn_config::testutil::config_layer("");
    let layer = config.clone();
    let mut fixture = spawn_actor_with(config).await;
    let artifact = fixture.seed("chart.html", "<html><body>b</body></html>");
    fixture.serve_path = artifact.to_string_lossy().into_owned();
    layer
        .put(&jinn_preferences_config::schemas::CanvasConfig {
            bind: format!("127.0.0.1:{port_b}"),
        })
        .expect("layer put lands");

    // When a reload re-reads the bind while nothing is serving.
    fixture
        .harness
        .bus()
        .publish(jinn_boot_msg::JinnTomlReloaded)
        .await;

    // Then the next serve answers at the new bind, not the default.
    drop(probe);
    let outcome = fixture
        .handle
        .serve(fixture.serve_path.clone())
        .await
        .expect("the round trip completes");
    assert!(outcome.ok, "serve refused: {}", outcome.error);
    let bind = bind_of_url(&outcome.url);
    assert_eq!(bind.port(), port_b, "dormant reload adopts the new bind");
    let body = body_of(&http_get(bind, "/").await);
    assert!(body.contains("b</body>"));
}

#[rstest::rstest]
#[tokio::test]
async fn a_failed_rebind_while_quiet_goes_dormant_and_surfaces_the_refusal() {
    // Given a serving actor whose pageholder has left, and a layer
    // whose bind names a port that is already taken by another
    // listener.
    let squatter = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("squatter binds");
    let port_taken = squatter.local_addr().expect("squatter addr").port();
    let config = jinn_config::testutil::config_layer("");
    let layer = config.clone();
    let mut fixture = spawn_actor_with(config).await;
    let artifact = fixture.seed("chart.html", "<html><body>x</body></html>");
    fixture.serve_path = artifact.to_string_lossy().into_owned();
    let first = fixture
        .handle
        .serve(fixture.serve_path.clone())
        .await
        .expect("the round trip completes");
    let url_a = first.url.clone();
    let ws = ws_connect(bind_of_url(&url_a)).await;
    ws_send_handshake_done(&ws).await;
    drop(ws);
    layer
        .put(&jinn_preferences_config::schemas::CanvasConfig {
            bind: format!("127.0.0.1:{port_taken}"),
        })
        .expect("layer put lands");

    // When a reload parks the pending bind and the quiet heartbeat
    // tries to apply it.
    fixture
        .harness
        .bus()
        .publish(jinn_boot_msg::JinnTomlReloaded)
        .await;
    let deadline = tokio::time::Instant::now() + STATUS_TIMEOUT;
    let refused = loop {
        if let Some(found) = fixture
            .statuses
            .drain()
            .into_iter()
            .find(|u| u.name == "canvas" && u.note_tone == Some(jinn_slices::NoteTone::Error))
        {
            break found;
        }
        fixture
            .handle
            .drain_metrics()
            .await
            .expect("drain completes");
        assert!(
            tokio::time::Instant::now() < deadline,
            "the rebind refusal never surfaced"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    // Then the row carries the refusal, and the next serve fails too —
    // the actor went dormant AT the pending bind (the taken port), so
    // serving stays refused until the port frees or the bind changes
    // again.
    // Anchor on strings jinn itself renders — the taken bind and the
    // fix hint — never on OS error text, which differs per platform
    // ("Address already in use" vs "Only one usage of each socket
    // address …" on Windows).
    let taken_bind = format!("failed to bind 127.0.0.1:{port_taken}");
    assert!(
        refused
            .status_message
            .as_deref()
            .is_some_and(|m| m.contains("serve failed")
                && m.contains(&taken_bind)
                && m.contains("fix: choose another [canvas] bind port")),
        "refusal must name the failure: {:?}",
        refused.status_message
    );
    let _ = squatter;
    let next = fixture
        .handle
        .serve(fixture.serve_path.clone())
        .await
        .expect("the round trip completes");
    assert!(
        !next.ok,
        "the actor is dormant at the taken bind: the next serve refuses rather than silently serving elsewhere"
    );
}
