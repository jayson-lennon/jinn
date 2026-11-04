//! The axum listener — everything a running applets server does.
//!
//! Surfaces, and nothing else:
//!
//! - `/applets/<id>/` serves the applet's entry HTML, read fresh from disk on
//!   every request, **byte for byte** — no reload snippet, no telemetry
//!   snippet, no injection of any kind.
//! - `/applets/<id>/<path>` serves sibling files under the same app
//!   directory, same read-fresh-verbatim rule.
//! - `/vendor/<path>` serves the embedded vendor set, byte-identical to
//!   canvas's (the same manifest-defined files, the same headers).
//! - `/healthz` is the liveness marker.
//!
//! There is no `/ws`, no `/metrics`, no watcher, no broadcast: the applets
//! server is static-only by contract.
//!
//! Traversal is impossible: the applet id is validated
//! ([`jinn_applets_msg::is_valid_app_id`]) and the requested sub-path is
//! resolved against the applet directory and rejected when it escapes the
//! applets root — a crafted id or path answers 404, never a directory
//! listing or a redirect.
//!
//! The module has no serve orchestration (that is
//! [`super::server_actor`]) and no catalog logic (that is
//! [`super::store_actor`]); it is the request/response half plus the
//! [`ServerShared`] the spawned task holds.

use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use jinn_applets_msg::is_valid_app_id;
use marked_path::{Absolute, MarkedPathAccess as _, MarkedPathBuf};

use crate::vendor::VENDOR_ASSETS;

/// The health body prefix a live applets server answers with.
pub const HEALTH_MARKER: &str = "jinn-applets";

/// The URL path prefix every app is served under.
pub const APPS_ROUTE_PREFIX: &str = "/applets/";

/// State shared across the server's request handlers.
#[derive(Clone)]
pub struct ServerShared {
    /// The applets root — every `/applets/<id>` resolves under it. Set once at
    /// bind time; the server never re-targets.
    pub apps_root: MarkedPathBuf<Absolute>,
}

impl ServerShared {
    /// Builds the shared state for a resolved bind over `apps_root`.
    #[must_use]
    pub fn new(apps_root: MarkedPathBuf<Absolute>) -> Self {
        Self { apps_root }
    }
}

/// Builds the full router for an applets server.
#[must_use = "the router carries every applets route; building it without serving it is a wiring bug"]
pub fn router(shared: ServerShared) -> Router {
    router_arc(Arc::new(shared))
}

/// The `Arc`-state form of [`router`], for the actor's spawned task.
pub fn router_arc(shared: Arc<ServerShared>) -> Router {
    Router::new()
        .route("/healthz", get(health_handler))
        .route("/vendor/{*path}", get(serve_vendor))
        .fallback(serve_apps_path)
        .with_state(shared)
}

/// `GET /healthz` — the applets-specific liveness marker. Any 200 whose
/// body starts with [`HEALTH_MARKER`] identifies a live applets server.
async fn health_handler() -> impl IntoResponse {
    (StatusCode::OK, format!("{HEALTH_MARKER}\n"))
}

/// `GET /vendor/<path>` — one embedded asset, forwarded with the same
/// headers canvas's server sends for the same path (the generated table
/// pins bytes, encoding, and content type together).
async fn serve_vendor(axum::extract::Path(path): axum::extract::Path<String>) -> Response {
    let Some(asset) = VENDOR_ASSETS.iter().find(|a| a.path == path) else {
        return (
            StatusCode::NOT_FOUND,
            format!("no vendor asset at /{path}\n"),
        )
            .into_response();
    };
    (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                header::HeaderValue::from_static(asset.content_type),
            ),
            (
                header::CONTENT_ENCODING,
                header::HeaderValue::from_static(asset.encoding),
            ),
            // Vendored libraries are plain scripts and fonts, but ES
            // module imports and font loads from a foreign origin need
            // the CORS blessing; `*` is safe on a loopback server with
            // no credentials. Same header canvas sends.
            (
                header::ACCESS_CONTROL_ALLOW_ORIGIN,
                header::HeaderValue::from_static("*"),
            ),
        ],
        asset.bytes.to_vec(),
    )
        .into_response()
}

/// The fallback handler: every request that is not `/healthz` or
/// `/vendor/…` is an applet path. Routes `/applets/<id>/` (and bare `/applets/<id>`)
/// to the applet's entry HTML, and `/applets/<id>/<sub>` to a sibling file.
async fn serve_apps_path(State(shared): State<Arc<ServerShared>>, uri: Uri) -> Response {
    let raw = uri.path();
    let Some(rest) = raw.strip_prefix(APPS_ROUTE_PREFIX) else {
        return not_found(raw);
    };
    // Split off the applet id, then the sub-path. Both arrive percent-decoded
    // by the URI parse; the id gate and the traversal check below reject
    // anything that could reinterpret the boundary.
    let (id, sub) = match rest.split_once('/') {
        Some((id, sub)) => (id, sub),
        None => (rest, ""),
    };
    if !is_valid_app_id(id) {
        return not_found(raw);
    }

    // A directory-style request (`/applets/<id>/` or `/applets/<id>`) serves
    // the entry HTML; anything after the id serves the named sibling.
    let entry_html = sub.is_empty() || sub == "index.html";
    let requested = if entry_html {
        vec!["index.html"]
    } else {
        // Reject any segment that could climb: the resolution below is
        // the authority, but a `..` segment never even reaches it.
        let segments: Vec<&str> = sub.split('/').collect();
        if segments.iter().any(|seg| seg.is_empty() || *seg == ".") {
            // Normalize-friendly spellings collapse to their canonical
            // form: `a//b` and `a/./b` are `a/b`.
            let collapsed: Vec<&str> = segments
                .iter()
                .copied()
                .filter(|seg| !seg.is_empty() && *seg != ".")
                .collect();
            if collapsed.contains(&"..") {
                return not_found(raw);
            }
            collapsed
        } else {
            if segments.contains(&"..") {
                return not_found(raw);
            }
            segments
        }
    };

    // Resolve under the applet directory and prove containment: the typed
    // path system rejects a join that leaves the absolute base, which is
    // the traversal authority.
    let Some(app_dir) = shared.apps_root.join_all([id]).ok() else {
        return not_found(raw);
    };
    let Some(target) = app_dir.join_all(requested.iter().copied()).ok() else {
        return not_found(raw);
    };
    if !target.starts_with(&app_dir) {
        return not_found(raw);
    }

    match std::fs::read(target.as_path()) {
        Ok(bytes) => {
            let content_type = content_type_for(target.as_path());
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, content_type)],
                bytes,
            )
                .into_response()
        }
        Err(_) => not_found(raw),
    }
}

/// A 404 body naming the path, with no redirect and no listing.
fn not_found(path: &str) -> Response {
    (StatusCode::NOT_FOUND, format!("not found: {path}\n")).into_response()
}

/// MIME type for a served app file, by extension. Covers every extension
/// an HTML artifact directory produces; unknown types serve as octet
/// streams. Non-file paths cannot reach this (the caller reads a file).
fn content_type_for(path: &Path) -> &'static str {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return "application/octet-stream";
    };
    let ext = name.rsplit('.').next().unwrap_or(name);
    if ext == name {
        return "application/octet-stream";
    }
    match ext {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "txt" | "md" => "text/plain; charset=utf-8",
        "csv" => "text/csv",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::unwrap_used,
        clippy::panic,
        reason = "test assertions need expect/unwrap"
    )]

    use super::{ServerShared, serve_apps_path};
    use axum::body::to_bytes;
    use axum::extract::State;
    use axum::http::{StatusCode, Uri};
    use axum::response::Response;
    use marked_path::MarkedPathAccess as _;
    use std::sync::Arc;

    /// Seeds an applet directory with an entry page and a sibling, and
    /// builds the shared state over the temp applets root.
    fn shared_with_app(
        id: &str,
        entry_html: &str,
        sibling: Option<(&str, &str)>,
    ) -> (Arc<ServerShared>, jinn_testutil::JinnTempDir) {
        let dir = jinn_testutil::retained_tempdir();
        let app_dir = dir.path.join_all(["applets", id]).expect("name joins");
        std::fs::create_dir_all(app_dir.as_path()).expect("mkdir app");
        std::fs::write(
            app_dir.join("index.html").expect("join").as_path(),
            entry_html,
        )
        .expect("seed entry");
        if let Some((name, body)) = sibling {
            std::fs::write(app_dir.join(name).expect("join").as_path(), body)
                .expect("seed sibling");
        }
        let root = dir.path.join("applets").expect("join");
        let canonical =
            marked_path::CanonicalPath::from_path(root.as_path()).expect("canonical root");
        (Arc::new(ServerShared::new(canonical.into_marked())), dir)
    }

    async fn get(shared: &Arc<ServerShared>, path: &str) -> Response {
        let uri = Uri::builder()
            .path_and_query(path)
            .build()
            .expect("uri builds");
        serve_apps_path(State(shared.clone()), uri).await
    }

    async fn body(response: Response) -> Vec<u8> {
        to_bytes(response.into_body(), 10 * 1024 * 1024)
            .await
            .expect("body reads")
            .to_vec()
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn entry_request_serves_disk_bytes_verbatim() {
        // Given an applet whose entry file holds exact bytes on disk.
        let entry = "<html><head><title>t</title></head><body>verbatim</body></html>";
        let (shared, _apps_root_guard) = shared_with_app("sales-dashboard", entry, None);

        // When requesting the applet root.
        let response = get(&shared, "/applets/sales-dashboard/").await;

        // Then the response body is byte-identical to the disk file —
        // no reload snippet, no tracker, nothing injected.
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body(response).await, entry.as_bytes());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn entry_request_without_trailing_slash_serves_the_entry() {
        // Given an applet with an entry file.
        let entry = "<html><body>x</body></html>";
        let (shared, _apps_root_guard) = shared_with_app("my-app", entry, None);

        // When requesting the applet path without the trailing slash.
        let response = get(&shared, "/applets/my-app").await;

        // Then the entry is served the same way.
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body(response).await, entry.as_bytes());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn sibling_request_serves_the_named_file() {
        // Given an applet with a sibling data file.
        let (shared, _dir) =
            shared_with_app("my-app", "<html></html>", Some(("data.json", "[1,2,3]")));

        // When requesting the sibling.
        let response = get(&shared, "/applets/my-app/data.json").await;

        // Then its bytes come back verbatim.
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body(response).await, b"[1,2,3]");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn missing_app_id_is_a_404() {
        // Given an applets root holding one applet.
        let (shared, _apps_root_guard) = shared_with_app("real", "<html></html>", None);

        // When requesting an id that was never saved.
        let response = get(&shared, "/applets/never-saved/").await;

        // Then the answer is 404 — no listing, no redirect.
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn missing_sibling_is_a_404() {
        // Given an applet.
        let (shared, _apps_root_guard) = shared_with_app("real", "<html></html>", None);

        // When requesting a sibling that does not exist.
        let response = get(&shared, "/applets/real/nope.txt").await;

        // Then the answer is 404.
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[rstest::rstest]
    #[case("/applets/../etc/passwd")]
    #[case("/applets/%2e%2e/etc/passwd")]
    #[case("/applets/real/../../etc/passwd")]
    #[case("/applets/real/..%2f..%2fetc%2fpasswd")]
    #[case("/applets/./real")]
    #[case("/applets/.hidden/")]
    #[case("/applets/")]
    #[case("/not-applets/real")]
    #[tokio::test]
    async fn hostile_paths_are_404_never_served(#[case] path: &str) {
        // Given an applets root holding one applet.
        let (shared, _apps_root_guard) = shared_with_app("real", "<html></html>", None);

        // When requesting a hostile path (traversal, dot segments,
        // hidden ids, or a non-applets prefix).
        let response = get(&shared, path).await;

        // Then the answer is 404 — the request cannot escape the applets
        // root, and no sibling content is disclosed.
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "path: {path}");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn catalog_file_is_never_served_as_an_app() {
        // Given an applets root whose catalog file sits beside the applets.
        let (shared, _apps_root_guard) = shared_with_app("real", "<html></html>", None);
        // (the catalog lives at the applets root; `applets.json` as an id is
        // rejected by the id gate)
        let response = get(&shared, "/applets/applets.json").await;

        // Then it is not servable as an applet id.
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn reads_reflect_later_disk_writes() {
        // Given a served app.
        let (shared, dir_guard) = shared_with_app("live", "<html>v1</html>", None);
        let first = get(&shared, "/applets/live/").await;
        assert_eq!(body(first).await, b"<html>v1</html>");

        // When the file on disk changes.
        let app_dir = dir_guard.path.join_all(["applets", "live"]).expect("join");
        std::fs::write(
            app_dir.join("index.html").expect("join").as_path(),
            "<html>v2</html>",
        )
        .expect("rewrite");

        // Then the next request reads the new bytes — the file is read
        // fresh on every request.
        let second = get(&shared, "/applets/live/").await;
        assert_eq!(body(second).await, b"<html>v2</html>");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn html_entry_carries_the_html_content_type() {
        // Given a served applet entry.
        let (shared, _apps_root_guard) = shared_with_app("typed", "<html></html>", None);

        // When requesting it.
        let response = get(&shared, "/applets/typed/").await;

        // Then the response declares text/html.
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("text/html; charset=utf-8")
        );
    }
}
