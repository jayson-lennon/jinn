//! Parity guard for the embedding convention (CLS pooling + L2 normalize).
//!
//! `tests/fixtures/embedding-parity.json` holds reference vectors captured
//! from the embedder this backend replaced, for five fixed sentences. The
//! candle backend must land within cosine 0.99 of every reference: correct
//! implementations score ≈0.999 (different float pipelines), while a wrong
//! pooling convention drops far below. The suite stays green and offline
//! without this feature; run it explicitly, weights cached or downloadable
//! (`--release`: the first case pays the model load, and a debug bert
//! forward is far slower than production):
//!
//! ```text
//! cargo test --release -p jinn-mcp-overgraph --features embed-parity --test embedding_parity
//! ```
#![cfg(feature = "embed-parity")]

use std::sync::Arc;
use std::time::Duration;

use jinn_common::app_paths::AppPaths;
use jinn_mcp_overgraph::embedder::{CandleEmbedder, Embedder, EmbedderModel};
use marked_path::{Absolute, MarkedPathBuf};

/// The fixture sentences, in fixture order. The fixture's row `i` is the
/// reference vector for `SENTENCES[i]`.
const SENTENCES: [&str; 5] = [
    "The quick brown fox jumps over the lazy dog.",
    "jinn is a terminal agent harness written in Rust.",
    "The overgraph server embeds text locally on the CPU.",
    "Vector search finds documents by meaning rather than exact keywords.",
    "Completely unrelated: the valve pressure rating is 300 psi.",
];

/// The cosine the convention must clear against the reference vectors:
/// correct pooling lands ≈0.999; wrong pooling falls far below 0.99.
const PARITY_COSINE: f32 = 0.99;

/// Loads the reference vectors captured from the replaced backend.
fn fixture_vectors() -> Vec<Vec<f32>> {
    let path = MarkedPathBuf::<Absolute>::new(env!("CARGO_MANIFEST_DIR"))
        .expect("CARGO_MANIFEST_DIR is absolute")
        .join("tests/fixtures/embedding-parity.json")
        .expect("repo-relative join keeps the manifest dir absolute");
    let raw = std::fs::read_to_string(&path).expect("parity fixture is committed");
    serde_json::from_str(&raw).expect("parity fixture parses as [5][384] f32")
}

/// Cosine similarity of two equal-length vectors.
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm = |v: &[f32]| v.iter().map(|x| x * x).sum::<f32>().sqrt();
    dot / (norm(a) * norm(b))
}

/// The production embedder pointed at the real jinn cache, loaded once for
/// the whole test binary: each case shares the resident model.
fn production_embedder() -> Arc<dyn Embedder> {
    static EMBEDDER: std::sync::OnceLock<Arc<dyn Embedder>> = std::sync::OnceLock::new();
    Arc::clone(EMBEDDER.get_or_init(|| {
        let cache_dir = AppPaths::default().embed_models_dir();
        Arc::new(CandleEmbedder::new(EmbedderModel::BgeSmallEnV15, cache_dir))
    }))
}

#[rstest::rstest]
#[case(0, SENTENCES[0])]
#[case(1, SENTENCES[1])]
#[case(2, SENTENCES[2])]
#[case(3, SENTENCES[3])]
#[case(4, SENTENCES[4])]
#[timeout(Duration::from_secs(600))]
fn parity_matches_reference_vectors(#[case] index: usize, #[case] sentence: &str) {
    // Given the production embedder and the reference vector for one fixture
    // sentence.
    let embedder = production_embedder();
    let reference = &fixture_vectors()[index];

    // When embedding the sentence through the candle backend.
    let got = embedder.embed(sentence).expect("embeds with cached model");

    // Then the vector is 384-wide, unit-norm, and within the parity cosine
    // of the reference.
    let dim = EmbedderModel::BgeSmallEnV15.dimension() as usize;
    assert_eq!(got.len(), dim, "dimension must match the catalog entry");
    let norm = got.iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!(
        (norm - 1.0).abs() < 1e-3,
        "vector must be L2-normalized, norm was {norm}"
    );
    let similarity = cosine(reference, &got);
    assert!(
        similarity >= PARITY_COSINE,
        "cosine {similarity} below parity {PARITY_COSINE} for sentence {index}"
    );
}

#[rstest::rstest]
fn unavailable_model_error_names_the_prefetch_hint() {
    // Given an embedder whose cache dir cannot hold a model: a file where
    // hf-hub needs a directory, under a tempdir root.
    let dir = jinn_testutil::tempdir();
    let blocker = dir
        .path
        .join("blocker")
        .expect("temp root joins a bare name");
    std::fs::write(&blocker, b"not a directory").expect("writes the blocker file");
    let embedder: Arc<dyn Embedder> = Arc::new(CandleEmbedder::new(
        EmbedderModel::BgeSmallEnV15,
        blocker.clone(),
    ));

    // When embedding before any successful init.
    let result = embedder.embed("anything");

    // Then the failure names the model and the --fetch-model hint, and the
    // failed init is not cached (the blocker dies with the tempdir).
    let err = result.expect_err("blocked cache must fail");
    let message = err.current_context().message();
    assert!(
        message.contains("--fetch-model"),
        "error must carry the pre-fetch hint: {message}"
    );
}
