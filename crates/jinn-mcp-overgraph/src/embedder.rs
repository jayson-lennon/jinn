//! Local embedding models behind the server's vector tools.
//!
//! The overgraph server embeds text on the local CPU via candle:
//! [`graph_upsert_node`](crate::server) accepts an `embed_text` argument and
//! [`graph_vector_search`](crate::server) embeds its query server-side, so an
//! agent never touches a vector.
//!
//! # Shape
//!
//! - [`EmbedderModel`] — the catalog of known models (dimension, metric, name),
//!   parsed from the `--embed-model` CLI arg.
//! - [`Embedder`] — the backend trait; production runs [`CandleEmbedder`],
//!   tests inject a deterministic fake. [`embed_async`] hops any backend onto
//!   a blocking thread.
//! - [`EmbedError`] — why an embed (or the model download behind it) failed.
//!
//! # Model lifecycle
//!
//! The model loads lazily at the first embed and stays resident until the
//! process exits — there is no unload path. Weights, config, and tokenizer
//! download from HuggingFace on first use into [`AppPaths::embed_models_dir`]
//! (pre-fetchable headlessly via `jinn mcp overgraph --fetch-model`). Every
//! init is bracketed by explicit stderr lines (see [`announce`]); hf-hub's
//! own download progress is disabled so the inspector log stream stays
//! legible. This crate installs no tracing subscriber, so stderr is its only
//! user-visible channel.
//!
//! A failed init is not cached: the next embed retries the download, so a
//! transient network failure heals without a restart.
//!
//! # Embedding convention
//!
//! bge-small-en-v1.5 produces one vector per sequence position; the single
//! document vector is the `[CLS]` position (sequence index 0), L2-normalized.
//! That convention lives in [`pool_cls_normalized`] and is pinned by the
//! feature-gated parity test (`--features embed-parity`) against reference
//! vectors captured from the model itself.

use marked_path::{Absolute, MarkedPathAccess, MarkedPathBuf};
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::Instant;

use candle_core::{Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config as BertConfig};
use error_stack::{Report, ResultExt};
use hf_hub::api::sync::ApiBuilder;
use tokenizers::Tokenizer;
use tokenizers::TruncationParams;
use wherror::Error;

use jinn_common::app_paths::AppPaths;

/// The longest token sequence the model sees; longer inputs are truncated,
/// never split. bge-small-en-v1.5's `max_position_embeddings` is 512, and the
/// database contract freezes embeddings at this context window.
const MAX_SEQUENCE_TOKENS: usize = 512;

/// Why an embed failed: the model is unavailable (cold cache + failed
/// download) or the backend rejected the text.
///
/// Callers render this as tool-level error content (`<tool> failed: ...`),
/// so the message must name the failed model download by name.
#[derive(Debug, Clone, Error)]
#[error(debug)]
pub struct EmbedError {
    /// What went wrong, in tool-error prose.
    reason: String,
}

impl EmbedError {
    /// Wraps a backend failure with the reason the embed could not run.
    fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }

    /// The failure text as it should appear in tool content.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.reason
    }
}

/// The catalog of embedding models the server can run.
///
/// The default is the deliberate choice: small enough to download and run on
/// any CPU, strong enough to beat the older hosted APIs. The dimension is a
/// permanent property of the database (locked at open), so it lives here and
/// nowhere else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbedderModel {
    /// BAAI/bge-small-en-v1.5 — 384-dim, cosine, ~35MB, 512-token context.
    BgeSmallEnV15,
}

impl EmbedderModel {
    /// The catalog default, used when no `--embed-model` is given.
    #[must_use]
    pub const fn default_model() -> Self {
        Self::BgeSmallEnV15
    }

    /// Parses a `--embed-model` value; unknown names yield `None` so the CLI
    /// can render a legible error naming the valid choices.
    #[must_use]
    pub fn from_arg(arg: &str) -> Option<Self> {
        match arg {
            "bge-small-en-v1.5" => Some(Self::BgeSmallEnV15),
            _ => None,
        }
    }

    /// The `--embed-model` spelling.
    #[must_use]
    pub const fn arg_name(&self) -> &'static str {
        match self {
            Self::BgeSmallEnV15 => "bge-small-en-v1.5",
        }
    }

    /// The model's dense-vector dimension; the database locks to this at open.
    #[must_use]
    pub const fn dimension(&self) -> u32 {
        match self {
            Self::BgeSmallEnV15 => 384,
        }
    }

    /// The similarity metric the model was trained for.
    #[must_use]
    pub const fn metric(&self) -> overgraph::types::DenseMetric {
        match self {
            Self::BgeSmallEnV15 => overgraph::types::DenseMetric::Cosine,
        }
    }

    /// The HuggingFace repo id holding this model's safetensors, config, and
    /// tokenizer files.
    #[must_use]
    pub const fn hf_repo(self) -> &'static str {
        match self {
            Self::BgeSmallEnV15 => "BAAI/bge-small-en-v1.5",
        }
    }

    /// Approximate download size, for the stderr announcement.
    #[must_use]
    pub const fn approximate_download(self) -> &'static str {
        match self {
            Self::BgeSmallEnV15 => "~35MB",
        }
    }
}

/// The embedding backend: text in, dense vector out.
///
/// Production runs [`CandleEmbedder`]; tests inject a deterministic fake
/// so the suite never downloads a model. Not async — the model is
/// synchronous and potentially slow (cold init downloads ~35MB), so async
/// callers go through [`embed_async`], which hops onto a blocking thread.
pub trait Embedder: Send + Sync {
    /// The dimension of every vector this backend produces; always the
    /// configured model's [`EmbedderModel::dimension`].
    fn dimension(&self) -> u32;

    /// Embeds one text.
    ///
    /// # Errors
    ///
    /// Returns an error when the model is unavailable — most commonly the
    /// cold-cache + failed-download case, which the message must name.
    fn embed(&self, text: &str) -> Result<Vec<f32>, Report<EmbedError>>;
}

/// Embeds one text off the async worker, via [`spawn_blocking`].
///
/// # Errors
///
/// Propagates [`Embedder::embed`]'s errors; a panic in the blocking task
/// (engine-level fault) also lands here as an error.
pub async fn embed_async(
    embedder: &Arc<dyn Embedder>,
    text: &str,
) -> Result<Vec<f32>, Report<EmbedError>> {
    let embedder = Arc::clone(embedder);
    let text = text.to_owned();
    tokio::task::spawn_blocking(move || embedder.embed(&text))
        .await
        .change_context(EmbedError::new("embedding task failed unexpectedly"))?
}

/// One resident model: bert weights, tokenizer, and the device they live on.
struct LoadedModel {
    model: BertModel,
    tokenizer: Tokenizer,
    device: Device,
    /// The model's hidden width (`config.hidden_size`); candle's `BertModel`
    /// does not retain its config, and the pooling step needs the row width.
    hidden_size: usize,
}

impl LoadedModel {
    /// Runs tokenizer + bert forward over one text, returning the flattened
    /// `[1, seq, hidden]` hidden states row-major (the `[CLS]` position is
    /// the first `hidden` floats).
    fn last_hidden_state(&self, text: &str) -> candle_core::Result<Vec<f32>> {
        let encoding = self
            .tokenizer
            .encode(text, true)
            .map_err(|err| candle_core::Error::Msg(format!("tokenization failed: {err}")))?;
        let input_ids = Tensor::new(encoding.get_ids().to_vec(), &self.device)?.unsqueeze(0)?;
        let token_type_ids =
            Tensor::new(encoding.get_type_ids().to_vec(), &self.device)?.unsqueeze(0)?;
        let hidden = self.model.forward(&input_ids, &token_type_ids, None)?;
        hidden.flatten_all()?.to_vec1::<f32>()
    }
}

/// Why pooling a hidden-state batch into one document vector failed.
#[derive(Debug, Error)]
#[error(debug)]
enum PoolError {
    /// The hidden width was zero — a model config that cannot hold a row.
    ZeroHiddenSize,
    /// The flat buffer was too short to hold even the `[CLS]` row.
    TooShort {
        /// Hidden width the model was configured with.
        hidden_size: usize,
        /// Floats actually present in the flat buffer.
        len: usize,
    },
    /// The `[CLS]` row's L2 norm was zero or non-finite; normalizing it
    /// would poison the vector index with NaNs.
    DegenerateNorm(f32),
}

/// Squashes per-position hidden states into the one document vector bge
/// semantics require: the `[CLS]` position (sequence index 0) of the
/// flattened `[1, seq, hidden]` batch, L2-normalized.
///
/// The database's cosine metric assumes unit-length vectors; this is where
/// that invariant is produced. Pinned by the feature-gated parity test.
fn pool_cls_normalized(hidden: &[f32], hidden_size: usize) -> Result<Vec<f32>, PoolError> {
    if hidden_size == 0 {
        return Err(PoolError::ZeroHiddenSize);
    }
    let Some(cls) = hidden.get(..hidden_size) else {
        return Err(PoolError::TooShort {
            hidden_size,
            len: hidden.len(),
        });
    };
    let norm = l2_norm(cls);
    if !norm.is_finite() || norm <= f32::EPSILON {
        return Err(PoolError::DegenerateNorm(norm));
    }
    Ok(cls.iter().map(|value| value / norm).collect())
}

/// The Euclidean length of one vector.
fn l2_norm(values: &[f32]) -> f32 {
    values.iter().map(|value| value * value).sum::<f32>().sqrt()
}

/// The production embedder: candle running one local bge transformer on CPU.
///
/// The model initializes lazily inside the first [`Embedder::embed`] call —
/// jinn owns the lifecycle decision that there is no idle-timeout or explicit
/// unload; the weights stay resident until the process exits. Init is
/// serialized under a mutex so concurrent first calls download exactly once,
/// and the stderr download announcement happens inside that critical section.
pub struct CandleEmbedder {
    model: EmbedderModel,
    cache_dir: MarkedPathBuf<Absolute>,
    /// `None` until the first successful init; a failed init stays `None` so
    /// the next embed retries the download. The loaded model is shared behind
    /// an `Arc` because bert forward and tokenization both take `&self` and
    /// are thread-safe — no per-embed lock is needed.
    model_cell: Mutex<Option<Arc<LoadedModel>>>,
}

impl CandleEmbedder {
    /// Builds an embedder for `model`, caching weights under `cache_dir`.
    ///
    /// No I/O happens here; the model loads (and downloads, if the cache is
    /// cold) at the first [`Embedder::embed`].
    #[must_use]
    pub fn new(model: EmbedderModel, cache_dir: MarkedPathBuf<Absolute>) -> Self {
        Self {
            model,
            cache_dir,
            model_cell: Mutex::new(None),
        }
    }

    /// Builds an embedder for `model` at the jinn-managed default cache dir.
    #[must_use]
    pub fn with_default_cache(model: EmbedderModel) -> Self {
        Self::new(model, AppPaths::default().embed_models_dir())
    }

    /// The model this embedder runs.
    #[must_use]
    pub const fn model(&self) -> EmbedderModel {
        self.model
    }

    /// Force-initializes the model now: `--fetch-model`'s whole job.
    ///
    /// Idempotent; after the first success this is cheap.
    ///
    /// # Errors
    ///
    /// Propagates the init failure (cold cache + failed download, most
    /// commonly). Failures are not cached — call again to retry.
    pub fn init(&self) -> Result<(), Report<EmbedError>> {
        self.ensure_initialized().map(|_| ())
    }

    /// Loads the model if not yet loaded, announcing the download on stderr.
    fn ensure_initialized(&self) -> Result<Arc<LoadedModel>, Report<EmbedError>> {
        // The lock is held across the (potentially long) init: every embed
        // needs the same model, so waiting callers are the correct behavior.
        let mut slot = self.model_cell.lock();
        if let Some(loaded) = slot.as_ref() {
            return Ok(Arc::clone(loaded));
        }
        let initialized = Arc::new(self.initialize()?);
        *slot = Some(Arc::clone(&initialized));
        Ok(initialized)
    }

    /// Runs the actual init, announcing the download on stderr.
    fn initialize(&self) -> Result<LoadedModel, Report<EmbedError>> {
        announce_download_start(self.model, &self.cache_dir);
        let started = Instant::now();
        match load_candle_model(self.model, &self.cache_dir) {
            Ok(loaded) => {
                announce_download_finished(self.model, started.elapsed());
                Ok(loaded)
            }
            Err(err) => {
                announce_download_failed(self.model, &err);
                Err(Report::new(EmbedError::new(format!(
                    "embedding model {} could not be loaded (download failed? \
                     pre-fetch with: jinn mcp overgraph --fetch-model): {err}",
                    self.model.arg_name()
                ))))
            }
        }
    }
}

impl Embedder for CandleEmbedder {
    fn dimension(&self) -> u32 {
        self.model.dimension()
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, Report<EmbedError>> {
        let loaded = self.ensure_initialized()?;
        let hidden = loaded.last_hidden_state(text).map_err(|err| {
            Report::new(EmbedError::new(format!(
                "embedding model {} rejected the text: {err}",
                self.model.arg_name()
            )))
        })?;
        pool_cls_normalized(&hidden, loaded.hidden_size).map_err(|err| {
            Report::new(EmbedError::new(format!(
                "embedding model {} produced an unusable vector: {err}",
                self.model.arg_name()
            )))
        })
    }
}

/// Fetches (or reuses from cache) the model's files and loads a resident
/// [`LoadedModel`] on CPU.
fn load_candle_model(
    model: EmbedderModel,
    cache_dir: &MarkedPathBuf<Absolute>,
) -> Result<LoadedModel, Box<dyn std::error::Error + Send + Sync>> {
    let device = Device::Cpu;
    let api = {
        // The progress bar stays off: the download announcement on stderr is
        // jinn's user-visible record, and the inspector log must stay legible.
        let builder = ApiBuilder::new()
            .with_cache_dir(cache_dir.as_path().to_path_buf())
            .with_progress(false);
        builder.build()?
    };
    let repo = api.model(model.hf_repo().to_owned());
    let weights_path = repo.get("model.safetensors")?;
    let config_path = repo.get("config.json")?;
    let tokenizer_path = repo.get("tokenizer.json")?;

    let config = {
        let raw = std::fs::read_to_string(&config_path)?;
        serde_json::from_str::<BertConfig>(&raw)?
    };
    let mut tokenizer = Tokenizer::from_file(&tokenizer_path)?;
    tokenizer
        .with_truncation(Some(TruncationParams {
            max_length: MAX_SEQUENCE_TOKENS,
            ..TruncationParams::default()
        }))
        .map_err(|err| format!("invalid truncation settings: {err}"))?;

    // Buffered, not mmap'd: the weights are ~33MB, and reading them once
    // keeps this crate free of `unsafe` on every platform.
    let weights = std::fs::read(&weights_path)?;
    let vb = VarBuilder::from_buffered_safetensors(weights, candle_core::DType::F32, &device)?;
    let bert = BertModel::load(vb, &config)?;

    Ok(LoadedModel {
        model: bert,
        tokenizer,
        device,
        hidden_size: config.hidden_size,
    })
}

/// A deterministic test embedder: hash-derived vectors, no model.
///
/// The suite injects this so no test ever downloads weights. Vectors are
/// derived from a stable hash of the input: same text, same vector — which is
/// all the search tests need to assert ordering and identity.
#[derive(Clone)]
pub struct FakeEmbedder {
    dimension: u32,
    /// `true` = model available; `false` = cold + unfetchable (simulates the
    /// offline failure path).
    available: bool,
}

impl FakeEmbedder {
    /// An available fake at `dimension` dimensions.
    #[must_use]
    pub fn available(dimension: u32) -> Self {
        Self {
            dimension,
            available: true,
        }
    }

    /// A fake that always fails, simulating the cold-cache offline state.
    #[must_use]
    pub fn unavailable() -> Self {
        Self {
            dimension: 384,
            available: false,
        }
    }

    /// Deterministic pseudo-vector: FNV-1a seeded per lane from the text.
    /// Magnitude carries no meaning; determinism and distinctness do.
    fn hash_vector(&self, text: &str) -> Vec<f32> {
        let mut out = Vec::with_capacity(self.dimension as usize);
        for lane in 0..self.dimension {
            let mut seed: u64 = 0xcbf2_9ce4_8422_2325;
            for byte in text.as_bytes() {
                seed ^= u64::from(*byte);
                seed = seed.wrapping_mul(0x0000_0100_0000_01b3);
            }
            seed ^= u64::from(lane).rotate_left(17);
            seed = seed.wrapping_mul(0x0000_0100_0000_01b3);
            let unit = f32::from((seed >> 48) as u16) / f32::from(u16::MAX);
            out.push(unit * 2.0 - 1.0);
        }
        out
    }
}

impl Embedder for FakeEmbedder {
    fn dimension(&self) -> u32 {
        self.dimension
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, Report<EmbedError>> {
        if !self.available {
            return Err(Report::new(EmbedError::new(
                "embedding model bge-small-en-v1.5 could not be loaded \
                 (download failed? pre-fetch with: jinn mcp overgraph --fetch-model)",
            )));
        }
        Ok(self.hash_vector(text))
    }
}

/// Announces a model download start on stderr.
///
/// jinn pipes the child's stderr into the MCP inspector log, so these lines
/// are the user-visible record of the download; the model's own progress bar
/// is disabled to keep the stream legible.
fn announce_download_start(model: EmbedderModel, cache_dir: &MarkedPathBuf<Absolute>) {
    announce(&format!(
        "overgraph: loading embedding model {} ({}) into {} ...",
        model.arg_name(),
        model.approximate_download(),
        cache_dir
    ));
}

/// Announces a finished model init on stderr (download or cache hit).
fn announce_download_finished(model: EmbedderModel, init_took: std::time::Duration) {
    announce(&format!(
        "overgraph: embedding model {} ready (init took {:.1}s)",
        model.arg_name(),
        init_took.as_secs_f32()
    ));
}

/// Announces a failed model init on stderr.
fn announce_download_failed(model: EmbedderModel, err: &impl std::fmt::Display) {
    announce(&format!(
        "overgraph: embedding model {} download failed: {err}",
        model.arg_name()
    ));
}

/// Writes one line to stderr: this crate's only user-visible channel when run
/// as a jinn-managed child (no tracing subscriber is installed).
///
/// Exposed so sibling modules (the ingest pipeline's progress lines) share
/// the same channel and lint carve-out instead of duplicating both.
#[expect(
    clippy::print_stderr,
    reason = "the MCP child's stderr is piped to jinn's inspector log; no tracing subscriber exists in this process"
)]
pub fn announce(message: &str) {
    eprintln!("{message}");
}

/// Tests for the pure parts of the embedder module.
#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "test code asserts with expect for clear failures"
)]
mod tests {
    use super::*;

    #[rstest::rstest]
    #[case("bge-small-en-v1.5", EmbedderModel::BgeSmallEnV15)]
    fn from_arg_parses_known_names(#[case] arg: &str, #[case] expected: EmbedderModel) {
        // Given a known --embed-model spelling.
        // When parsing it.
        let parsed = EmbedderModel::from_arg(arg);
        // Then it lands on the catalog entry.
        assert_eq!(parsed, Some(expected));
    }

    #[rstest::rstest]
    fn from_arg_rejects_unknown_names() {
        // Given a name nothing in the catalog carries.
        // When parsing it.
        let parsed = EmbedderModel::from_arg("nope");
        // Then it is rejected.
        assert_eq!(parsed, None);
    }

    #[rstest::rstest]
    fn default_model_is_bge_small() {
        // Given no --embed-model argument.
        // When picking the default.
        let model = EmbedderModel::default_model();
        // Then it is bge-small-en-v1.5.
        assert_eq!(model, EmbedderModel::BgeSmallEnV15);
    }

    #[rstest::rstest]
    fn bge_small_dimension_is_384_cosine() {
        // Given the default model.
        let model = EmbedderModel::default_model();

        // When its dimension and metric are read.
        let dimension = model.dimension();
        let metric = model.metric();

        // Then its dimension and metric match the documented catalog entry.
        assert_eq!(dimension, 384);
        assert_eq!(metric, overgraph::types::DenseMetric::Cosine);
    }

    #[rstest::rstest]
    fn hf_repo_names_the_baai_mirror() {
        // Given the default model.
        let model = EmbedderModel::default_model();

        // When its HuggingFace repo is read.
        let repo = model.hf_repo();

        // Then it is the BAAI bge-small mirror.
        assert_eq!(repo, "BAAI/bge-small-en-v1.5");
    }

    #[rstest::rstest]
    fn cls_pool_extracts_first_row_and_normalizes() {
        // Given a flattened [1, 2, 3] hidden batch whose first row is the
        // CLS position.
        let hidden = [3.0, 0.0, 0.0, 9.0, 9.0, 9.0];

        // When pooling at hidden width 3.
        let pooled = pool_cls_normalized(&hidden, 3).expect("pools");

        // Then the CLS row comes back L2-normalized, and nothing after it.
        assert_eq!(pooled, vec![1.0, 0.0, 0.0]);
    }

    #[rstest::rstest]
    fn cls_pool_rejects_a_degenerate_zero_norm() {
        // Given a hidden batch whose CLS row is all zeros.
        let hidden = [0.0_f32; 6];

        // When pooling.
        let result = pool_cls_normalized(&hidden, 3);

        // Then the zero norm is rejected instead of emitting NaNs.
        assert!(
            matches!(result, Err(PoolError::DegenerateNorm(_))),
            "zero norm must be rejected: {result:?}"
        );
    }

    #[rstest::rstest]
    fn cls_pool_rejects_a_buffer_shorter_than_one_row() {
        // Given a flat buffer too short to hold one row at width 3.
        let hidden = [1.0_f32, 2.0];

        // When pooling.
        let result = pool_cls_normalized(&hidden, 3);

        // Then the shape mismatch is rejected.
        assert!(matches!(result, Err(PoolError::TooShort { .. })));
    }

    #[rstest::rstest]
    fn fake_embedder_is_deterministic_per_text() {
        // Given an available fake embedder.
        let fake = FakeEmbedder::available(8);

        // When embedding the same text twice.
        let first = fake.embed("hello world").expect("available fake embeds");
        let second = fake.embed("hello world").expect("available fake embeds");

        // Then both vectors are identical, at the fake's dimension.
        assert_eq!(first, second);
        assert_eq!(first.len(), 8);
    }

    #[rstest::rstest]
    fn fake_embedder_distinct_texts_get_distinct_vectors() {
        // Given an available fake embedder.
        let fake = FakeEmbedder::available(8);

        // When embedding two different texts.
        let a = fake.embed("alpha").expect("embeds");
        let b = fake.embed("beta").expect("embeds");

        // Then the vectors differ.
        assert_ne!(a, b);
    }

    #[rstest::rstest]
    fn unavailable_fake_embedder_errors_naming_the_download() {
        // Given an unavailable fake embedder.
        let fake = FakeEmbedder::unavailable();

        // When embedding.
        let result = fake.embed("anything");

        // Then the error names the failed model download.
        let err = result.expect_err("unavailable fake must fail");
        assert!(
            err.current_context().message().contains("download failed"),
            "error must name the download: {}",
            err.current_context().message()
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn embed_async_runs_through_the_blocking_pool() {
        // Given an available fake embedder behind the trait object.
        let embedder: Arc<dyn Embedder> = Arc::new(FakeEmbedder::available(4));

        // When embedding through the async surface.
        let vector = embed_async(&embedder, "async text").await.expect("embeds");

        // Then the vector matches the sync path — same backend, off-thread.
        assert_eq!(vector, embedder.embed("async text").expect("sync embed"));
    }
}
