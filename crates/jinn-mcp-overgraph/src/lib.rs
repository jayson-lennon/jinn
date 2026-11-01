//! The builtin overgraph MCP server — jinn's bundled graph database.
//!
//! This crate turns the [`overgraph`] embedded graph engine into a standalone
//! MCP server: `jinn mcp overgraph` opens a database directory, then
//! speaks JSON-RPC over stdio so jinn's MCP subsystem can drive it as a child
//! process exactly like any user-configured server.
//!
//! # Shape
//!
//! - [`serve`] — the entry point: resolves the database directory, opens the
//!   engine with the embedder's dense-vector config, serves until the client
//!   disconnects.
//! - [`fetch_model`] — the `--fetch-model` entry point: warm the embedding
//!   model's cache and exit, without opening a database or a transport.
//! - `server` — the rmcp `ServerHandler` exposing the six graph tools
//!   (`graph_query`, `graph_upsert_node`, `graph_upsert_edge`,
//!   `graph_vector_search`, `graph_ingest_file`, `graph_docs`).
//! - `embedder` — the local embedding model behind the vector tools.
//! - `ingest` — the JSONL batch-file pipeline behind `graph_ingest_file`.
//! - `docs` — the vendored GQL reference served by `graph_docs`.
//! - `convert` — pure JSON ⇄ engine value converters.
//!
//! # Stdout discipline
//!
//! Stdout is the MCP transport. Nothing in this crate may print to stdout;
//! diagnostics go through `tracing` (captured to jinn's headless log when run
//! as a child) or stderr for fatal startup errors and embedding-model
//! announcements.
//!
//! The `--db` flag value is the argv boundary: it arrives raw (relative values
//! rejected loudly in `resolve_db_dir`) and is typed into
//! `MarkedPathBuf<Absolute>` before anything consumes it.
#![expect(
    clippy::disallowed_types,
    reason = "argv boundary: the raw --db flag value is typed in resolve_db_dir before any jinn logic consumes it"
)]
#![warn(missing_docs)]

pub mod convert;
pub mod docs;
pub mod embedder;
pub mod ingest;
pub mod server;

use std::path::PathBuf;
use std::sync::Arc;

use error_stack::{Report, ResultExt};
use jinn_common::app_paths::AppPaths;
use marked_path::{Absolute, MarkedPathAccess, MarkedPathBuf};
use overgraph::DatabaseEngine;
use wherror::Error;

/// Fatal errors while bringing the overgraph server up.
#[derive(Debug, Error)]
#[error(debug)]
pub struct ServeError;

/// Arguments for [`serve`].
#[derive(Debug, Default)]
pub struct OvergraphServeArgs {
    /// Database directory override; `None` uses the jinn-managed default.
    pub db: Option<PathBuf>,
    /// Embedding-model override; `None` uses the catalog default.
    ///
    /// The model's dimension is locked into the database at open, so a
    /// non-default value only works on a database created with (or never
    /// opened by) the same model.
    pub embed_model: Option<embedder::EmbedderModel>,
    /// Warm the embedding model's cache and exit without serving.
    pub fetch_model: bool,
}

/// Resolves the database directory: the explicit `--db` override, or the
/// jinn-managed default under `AppPaths`.
///
/// # Panics
///
/// Panics when `--db` names a relative path: the MCP server is spawned as
/// a child process whose only feedback channel at startup is process
/// death plus stderr, so a malformed flag value aborts the launch loudly
/// rather than silently opening the wrong directory.
#[expect(
    clippy::expect_used,
    reason = "bare-name `default` join on the typed AppPaths base is infallible by construction"
)]
#[must_use]
pub fn resolve_db_dir(db: Option<PathBuf>) -> MarkedPathBuf<Absolute> {
    // A relative `--db` is user input at process startup: the server is a
    // spawned child whose only startup feedback is process death plus
    // stderr, so a malformed flag aborts the launch loudly.
    #![allow(
        clippy::panic,
        clippy::panic_in_result_fn,
        reason = "a relative --db is user input at process startup, where aborting the spawn is the only feedback channel"
    )]
    if let Some(db) = db {
        let typed = MarkedPathBuf::<Absolute>::new(&db).unwrap_or_else(|error| {
            panic!(
                "--db must be an absolute path: {}: {error}",
                db.to_string_lossy()
            )
        });
        typed
            .join("default")
            .unwrap_or_else(|error| panic!("joining `default` keeps --db absolute: {error}"))
    } else {
        AppPaths::default()
            .graph_db_dir()
            .join("default")
            .expect("name join keeps the base absolute")
    }
}

/// Resolves the embedding model: the explicit `--embed-model` override, or
/// the catalog default.
#[must_use]
pub fn resolve_embed_model(model: Option<embedder::EmbedderModel>) -> embedder::EmbedderModel {
    model.unwrap_or_else(embedder::EmbedderModel::default_model)
}

/// Warms the embedding model's cache and exits: `--fetch-model`'s handler.
///
/// Prints the load progress to stderr, never touches stdout, and never opens
/// a database — a fetch must work even where the db directory is unusable.
///
/// # Errors
///
/// Returns an error if the model could not be loaded (cold cache + failed
/// download, most commonly); the failure message names the download.
pub fn fetch_model(args: &OvergraphServeArgs) -> Result<(), Report<ServeError>> {
    let model = resolve_embed_model(args.embed_model);
    let embedder = embedder::CandleEmbedder::with_default_cache(model);
    embedder
        .init()
        .change_context(ServeError)
        .attach("embedding model pre-fetch failed")
}

/// Builds the database options for this server: dense-vector configuration
/// derived from the configured embedding model, so vector writes and searches
/// are available for the life of the database, plus edge uniqueness, so a
/// repeated `(from, to, label)` upsert updates the existing edge instead of
/// creating a parallel one — which is what makes `graph_upsert_edge` and
/// `graph_ingest_file` re-runs idempotent.
///
/// The engine reconciles dense config only at open; opening with the config
/// on every start upgrades an existing graph-only database in place, and a
/// persisted mismatch with the configured model's dimension is a hard open
/// error surfaced on stderr.
fn db_options(model: embedder::EmbedderModel) -> overgraph::types::DbOptions {
    overgraph::types::DbOptions {
        dense_vector: Some(overgraph::types::DenseVectorConfig {
            dimension: model.dimension(),
            metric: model.metric(),
            hnsw: overgraph::types::HnswConfig::default(),
        }),
        edge_uniqueness: true,
        ..overgraph::types::DbOptions::default()
    }
}

/// Serves the overgraph MCP server over stdio until the client disconnects.
///
/// # Errors
///
/// Returns an error if the database cannot be opened or the stdio transport
/// fails. The engine is closed on the way out (best-effort).
pub async fn serve(args: OvergraphServeArgs) -> Result<(), Report<ServeError>> {
    let db_dir = resolve_db_dir(args.db);
    // The engine's own create-if-missing covers the leaf; this covers parents.
    if let Err(err) = std::fs::create_dir_all(&db_dir) {
        let report = Report::new(ServeError)
            .attach(format!("failed to create database directory {db_dir}"))
            .attach(format!("os error: {err}"));
        return Err(report);
    }

    let model = resolve_embed_model(args.embed_model);
    let server = {
        // Dense config from the embedder at every open: the engine
        // reconciles it only here, and None -> Some upgrades an existing
        // graph-only database in place. A dimension mismatch with a
        // persisted database is a hard open error (below).
        match DatabaseEngine::open(db_dir.as_path(), &db_options(model)) {
            Ok(engine) => {
                let embedder: Arc<dyn embedder::Embedder> =
                    Arc::new(embedder::CandleEmbedder::with_default_cache(model));
                server::OvergraphMcpServer::new(Arc::new(engine), embedder)
            }
            Err(err) => {
                // stderr, not stdout: stdout is the MCP transport, and this
                // is the last message a human sees before the process dies.
                #[expect(
                    clippy::print_stderr,
                    reason = "fatal startup failure before the transport starts"
                )]
                let () = {
                    eprintln!("overgraph: failed to open database at {db_dir}: {err}");
                };
                let report = Report::new(ServeError)
                    .attach(format!("failed to open database at {db_dir}"))
                    .attach(format!("engine error: {err}"));
                return Err(report);
            }
        }
    };

    tracing::info!(db = %db_dir, "overgraph MCP server listening on stdio");

    let transport = rmcp::transport::io::stdio();
    let running = rmcp::service::ServiceExt::serve(server, transport)
        .await
        .change_context(ServeError)
        .attach("MCP stdio handshake failed")?;

    // Completes when the transport closes (client disconnect / stdin EOF).
    let _ = running.waiting().await;

    tracing::info!("overgraph MCP server shutting down");
    Ok(())
}

/// Tests for the pure parts of the crate root.
#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "test code asserts with expect for clear failures"
)]
mod tests {
    use super::*;

    #[rstest::rstest]
    fn db_options_carry_the_models_dimension_and_metric() {
        // Given the default model.
        let model = resolve_embed_model(None);

        // When building the database options.
        let options = db_options(model);

        // Then the dense config carries the model's dimension and metric.
        let dense = options.dense_vector.expect("vector config present");
        assert_eq!(dense.dimension, model.dimension());
        assert_eq!(dense.metric, model.metric());
    }

    #[rstest::rstest]
    fn resolve_embed_model_prefers_the_override() {
        // Given an explicit model override.
        // When resolving.
        let model = resolve_embed_model(Some(embedder::EmbedderModel::BgeSmallEnV15));
        // Then the override wins.
        assert_eq!(model, embedder::EmbedderModel::BgeSmallEnV15);
    }
}
