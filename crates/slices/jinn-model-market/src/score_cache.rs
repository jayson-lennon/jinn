//! The scores cache: the ECI zip extraction and the vals JSON write,
//! both under one scores dir with per-source subdirectories.
//!
//! The ECI half downloads `benchmark_data.zip`, extracts exactly the
//! three CSVs into `eci/`, and decides cache-vs-seed by TTL with an
//! injected clock. The vals half stores the downloaded Index JSON into
//! `vals/`. Both subdirectories sit under the resolved scores dir
//! ([`jinn_common::app_paths::AppPaths::scores_cache_dir`]).
//!
//! Layout of the ECI subdir: three files, `eci_scores.csv`,
//! `benchmark_metadata.csv`, `model_metadata.csv` — never the zip
//! itself, never anything else (locked decision B). A directory holding
//! all three is *complete*; anything less falls back to the compiled
//! seed rather than serving a partial index. Layout of the vals subdir:
//! one `vals_index.json` plus the same `fetched_at` stamp.
//!
//! Time never enters here except through the `now_ms` closure: TTL
//! freshness and the download timestamp ride one injectable clock, which
//! is what makes the expiry tests honest. The seed bytes are **never**
//! written to disk as a stand-in for a download — a directory on disk
//! means a real fetch happened.

// The `expect`s below join a bare cache filename onto a directory already
// typed `Absolute`; a bare-name join cannot introduce a relative segment, so
// they cannot fail at runtime — they guard programmer error, not state.
#![allow(
    clippy::expect_used,
    clippy::missing_panics_doc,
    reason = "bare-name joins onto a typed-absolute cache dir are infallible by construction"
)]

use error_stack::ResultExt;
use marked_path::{Absolute, MarkedPathAccess, MarkedPathBuf};
use std::fs;
use std::io::Read;

/// The upstream archive (`research/openrouter-api-model-selection-data.md`
/// §8 — ~2.4 MB, 91 benchmark CSVs plus the three files this slice
/// extracts).
pub const ECI_ZIP_URL: &str = "https://epoch.ai/data/benchmark_data.zip";

/// The upstream Vals Index page the data URL is re-derived from at
/// refresh time. The page's HTML names the current content-hashed
/// `_astro` JSON path; a persisted path would be a dead URL after their
/// next deploy, so it is never stored.
pub const VALS_PAGE_URL: &str = "https://www.vals.ai/benchmarks/vals_index";

/// Cache freshness window: one week, matching the issue's "weekly-ish"
/// TTLs, shared by both score sources — they refresh together.
pub const SCORE_TTL_MS: u64 = 7 * 24 * 60 * 60 * 1000;

/// The scores dir's per-source subdirectory names.
pub const ECI_CACHE_SUBDIR: &str = "eci";
/// The vals half of the scores cache layout.
pub const VALS_CACHE_SUBDIR: &str = "vals";

/// The zip member the ECI scores live in (a directory inside the
/// archive).
pub const ZIP_ECI_SCORES: &str = "epoch_capabilities_index/eci_scores.csv";
/// The zip member the benchmark normalization metadata lives in.
pub const ZIP_BENCHMARK_METADATA: &str = "benchmark_metadata.csv";
/// The zip member the model roster lives in.
pub const ZIP_MODEL_METADATA: &str = "model_metadata.csv";

/// The three extracted files, with their zip-member origins.
pub const EXTRACTED_FILES: [(&str, &str); 3] = [
    ("eci_scores.csv", ZIP_ECI_SCORES),
    ("benchmark_metadata.csv", ZIP_BENCHMARK_METADATA),
    ("model_metadata.csv", ZIP_MODEL_METADATA),
];

/// The stamp file recording when a source's cache was written. Both
/// subdirectories carry one.
pub const FETCHED_AT_FILE: &str = "fetched_at";

/// The vals subdir's data file: the downloaded Index JSON, stored
/// verbatim.
pub const VALS_DATA_FILE: &str = "vals_index.json";

/// Why a score cache could not be consulted, downloaded, or written.
///
/// Variants name what a user can do about the failure; the actor
/// forwards the `Display` into the popup's degraded-mode line.
#[derive(Debug, wherror::Error)]
#[error(debug)]
pub enum ScoreCacheError {
    /// The ECI download failed (offline, DNS, non-200).
    #[error("ECI data download failed: {status}")]
    EciDownload {
        /// The HTTP status line or transport error, ready to render.
        status: String,
    },
    /// The response body could not be read as a zip or an extraction
    /// target was missing from the archive.
    #[error("ECI zip unusable: {detail}")]
    EciZip {
        /// What the archive layer rejected, ready to render.
        detail: String,
    },
    /// The extracted CSVs could not be written into the cache dir.
    #[error("writing ECI cache dir {dir}: {detail}")]
    EciWrite {
        /// The cache dir the write targeted.
        dir: String,
        /// The filesystem's complaint.
        detail: String,
    },
    /// The vals page fetch failed, or the page named no data file.
    #[error("Vals Index fetch failed: {detail}")]
    ValsFetch {
        /// What the fetch or page scan failed on, ready to render.
        detail: String,
    },
    /// The vals JSON could not be written into the cache dir.
    #[error("writing vals cache file {dir}: {detail}")]
    ValsWrite {
        /// The cache file the write targeted.
        dir: String,
        /// The filesystem's complaint.
        detail: String,
    },
}

/// The complete parsed content of one ECI cache read, with its freshness
/// stamp.
#[derive(Debug, Clone, PartialEq)]
pub struct CachedEci {
    /// The three CSVs' bytes, in the [`EXTRACTED_FILES`] order.
    pub files: Vec<Vec<u8>>,
    /// When the cache was written (ms since the epoch, from the
    /// injected clock).
    pub fetched_at_ms: u64,
}

/// The complete content of one vals cache read, with its freshness
/// stamp.
#[derive(Debug, Clone, PartialEq)]
pub struct CachedVals {
    /// The downloaded Index JSON, verbatim.
    pub bytes: Vec<u8>,
    /// When the cache was written (ms since the epoch, from the
    /// injected clock).
    pub fetched_at_ms: u64,
}

/// The scores dir resolved from the cache root: one subdirectory per
/// source.
///
/// The actor takes the resolved subdirs at spawn so it cannot re-derive
/// a divergent path; tests point them at a tempdir under the fixture
/// root.
#[derive(Debug, Clone)]
pub struct ScoreCacheDirs {
    /// The ECI subdir — the three extracted CSVs plus the stamp.
    pub eci: MarkedPathBuf<Absolute>,
    /// The vals subdir — the Index JSON plus the stamp.
    pub vals: MarkedPathBuf<Absolute>,
}

impl ScoreCacheDirs {
    /// Splits a resolved scores dir into its per-source subdirectories.
    #[must_use]
    pub fn new(scores_dir: &MarkedPathBuf<Absolute>) -> Self {
        Self {
            eci: scores_dir
                .join(ECI_CACHE_SUBDIR)
                .expect("absolute parent joins a bare name"),
            vals: scores_dir
                .join(VALS_CACHE_SUBDIR)
                .expect("absolute parent joins a bare name"),
        }
    }
}

/// Reads the ECI subdir when it is complete and fresh.
///
/// Completeness: all three CSVs present. Freshness: the `fetched_at`
/// stamp within `ttl_ms` of `now_ms()`. Either failing returns `None`
/// (the caller falls back to the seed or re-downloads).
pub fn load_eci_fresh<N>(dir: &MarkedPathBuf<Absolute>, ttl_ms: u64, now_ms: N) -> Option<CachedEci>
where
    N: Fn() -> u64,
{
    let stamp = read_stamp(dir)?;
    if now_ms().saturating_sub(stamp) >= ttl_ms {
        return None;
    }
    let mut files = Vec::with_capacity(EXTRACTED_FILES.len());
    for (name, _) in EXTRACTED_FILES {
        files.push(
            fs::read(
                dir.join(name)
                    .expect("absolute parent joins a bare filename"),
            )
            .ok()?,
        );
    }
    Some(CachedEci {
        files,
        fetched_at_ms: stamp,
    })
}

/// Reads the ECI subdir when it is complete, regardless of freshness.
///
/// `<c-r>` uses this after dropping the cache: the drop deletes the dir,
/// so anything still standing is a completed extraction from *this*
/// refresh's download.
pub fn load_eci_any(dir: &MarkedPathBuf<Absolute>) -> Option<CachedEci> {
    let stamp = read_stamp(dir)?;
    let mut files = Vec::with_capacity(EXTRACTED_FILES.len());
    for (name, _) in EXTRACTED_FILES {
        files.push(
            fs::read(
                dir.join(name)
                    .expect("absolute parent joins a bare filename"),
            )
            .ok()?,
        );
    }
    Some(CachedEci {
        files,
        fetched_at_ms: stamp,
    })
}

/// Reads the vals subdir when the JSON and its stamp are present and
/// fresh. Incomplete or stale returns `None` (the caller re-downloads).
pub fn load_vals_fresh<N>(
    dir: &MarkedPathBuf<Absolute>,
    ttl_ms: u64,
    now_ms: N,
) -> Option<CachedVals>
where
    N: Fn() -> u64,
{
    let stamp = read_stamp(dir)?;
    if now_ms().saturating_sub(stamp) >= ttl_ms {
        return None;
    }
    let bytes = fs::read(
        dir.join(VALS_DATA_FILE)
            .expect("absolute parent joins a bare filename"),
    )
    .ok()?;
    Some(CachedVals {
        bytes,
        fetched_at_ms: stamp,
    })
}

/// Reads the vals subdir when the JSON and its stamp are present,
/// regardless of freshness — the post-drop read the forced refresh makes.
pub fn load_vals_any(dir: &MarkedPathBuf<Absolute>) -> Option<CachedVals> {
    let stamp = read_stamp(dir)?;
    let bytes = fs::read(
        dir.join(VALS_DATA_FILE)
            .expect("absolute parent joins a bare filename"),
    )
    .ok()?;
    Some(CachedVals {
        bytes,
        fetched_at_ms: stamp,
    })
}

/// Reads and parses the dir's `fetched_at` stamp, or nothing.
fn read_stamp(dir: &MarkedPathBuf<Absolute>) -> Option<u64> {
    fs::read_to_string(
        dir.join(FETCHED_AT_FILE)
            .expect("absolute parent joins a bare filename"),
    )
    .ok()?
    .trim()
    .parse::<u64>()
    .ok()
}

/// Deletes a source's cache directory if it exists.
///
/// `<c-r>`'s drop half. Missing dir is success (nothing to drop); a
/// failed delete is a real error — refresh must not pretend it
/// invalidated what it could not remove. A failed *vals* drop is
/// reported to the vals path alone: the ECI half still refreshes.
///
/// # Errors
///
/// Returns an error when the directory exists but cannot be removed.
pub fn drop_cache_dir(
    dir: &MarkedPathBuf<Absolute>,
) -> Result<(), error_stack::Report<ScoreCacheError>> {
    if !dir.as_path().exists() {
        return Ok(());
    }
    fs::remove_dir_all(dir).map_err(|error| {
        error_stack::Report::new(ScoreCacheError::EciWrite {
            dir: dir.to_string(),
            detail: error.to_string(),
        })
        .attach("failed to drop a score cache dir")
    })
}

/// Downloads the zip, extracts exactly the three CSVs into `dir`, and
/// stamps the extraction time — all through the injected pieces so tests
/// serve a local mock and a fake clock.
///
/// The zip is downloaded whole into memory (≤ a few MB), never written
/// to disk; only the three extracted files land in the cache dir.
///
/// # Errors
///
/// Returns an error when the download fails, the archive is unreadable
/// or missing a member, or the extraction write fails.
pub fn download_and_extract(
    dir: &MarkedPathBuf<Absolute>,
    zip_bytes: &[u8],
    now_ms: u64,
) -> Result<(), error_stack::Report<ScoreCacheError>> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes)).map_err(|error| {
        error_stack::Report::new(ScoreCacheError::EciZip {
            detail: error.to_string(),
        })
        .attach("failed to open the downloaded benchmark_data.zip")
    })?;

    fs::create_dir_all(dir).map_err(|error| {
        error_stack::Report::new(ScoreCacheError::EciWrite {
            dir: dir.to_string(),
            detail: error.to_string(),
        })
        .attach("failed to create the ECI cache dir")
    })?;

    for (name, member) in EXTRACTED_FILES {
        let mut entry = archive.by_name(member).map_err(|error| {
            error_stack::Report::new(ScoreCacheError::EciZip {
                detail: format!("{member}: {error}"),
            })
            .attach("the archive is missing an extraction target")
        })?;
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut bytes).map_err(|error| {
            error_stack::Report::new(ScoreCacheError::EciZip {
                detail: format!("{member}: {error}"),
            })
            .attach("failed to read the archive member")
        })?;
        let target = dir.join(name).change_context(ScoreCacheError::EciZip {
            detail: format!("cannot join {name} under the cache dir"),
        })?;
        write_file(&target, &bytes)?;
    }

    let fetched_at = dir
        .join(FETCHED_AT_FILE)
        .change_context(ScoreCacheError::EciZip {
            detail: "cannot join the fetched-at filename under the cache dir".to_owned(),
        })?;
    write_file(&fetched_at, now_ms.to_string().as_bytes())?;
    Ok(())
}

/// Writes the downloaded vals JSON into the vals subdir and stamps the
/// write time.
///
/// # Errors
///
/// Returns an error when the directory cannot be created or the write
/// fails.
pub fn store_vals(
    dir: &MarkedPathBuf<Absolute>,
    json_bytes: &[u8],
    now_ms: u64,
) -> Result<(), error_stack::Report<ScoreCacheError>> {
    fs::create_dir_all(dir).map_err(|error| {
        error_stack::Report::new(ScoreCacheError::ValsWrite {
            dir: dir.to_string(),
            detail: error.to_string(),
        })
        .attach("failed to create the vals cache dir")
    })?;
    let target = dir
        .join(VALS_DATA_FILE)
        .change_context(ScoreCacheError::ValsWrite {
            dir: dir.to_string(),
            detail: "cannot join the data filename under the cache dir".to_owned(),
        })?;
    write_file(&target, json_bytes)?;
    let fetched_at = dir
        .join(FETCHED_AT_FILE)
        .change_context(ScoreCacheError::ValsWrite {
            dir: dir.to_string(),
            detail: "cannot join the fetched-at filename under the cache dir".to_owned(),
        })?;
    write_file(&fetched_at, now_ms.to_string().as_bytes())?;
    Ok(())
}

/// Downloads `url` over HTTPS.
///
/// Split from the extract/store half so tests can serve the zip and the
/// vals page from a local mock without a TLS stack, and so a future
/// ETag/conditional-get has one seam to live in.
///
/// # Errors
///
/// Returns an error on transport failure or a non-2xx status.
pub async fn fetch_bytes(
    client: &reqwest::Client,
    url: &str,
) -> Result<Vec<u8>, error_stack::Report<ScoreCacheError>> {
    let response = client.get(url).send().await.map_err(|error| {
        error_stack::Report::new(ScoreCacheError::EciDownload {
            status: error.to_string(),
        })
        .attach("requesting a score data file")
    })?;
    if !response.status().is_success() {
        return Err(error_stack::Report::new(ScoreCacheError::EciDownload {
            status: response.status().to_string(),
        })
        .attach("a score data file download returned an error status"));
    }
    response
        .bytes()
        .await
        .map(|bytes| bytes.to_vec())
        .map_err(|error| {
            error_stack::Report::new(ScoreCacheError::EciDownload {
                status: error.to_string(),
            })
            .attach("reading a score data file body")
        })
}

fn write_file(
    path: &MarkedPathBuf<Absolute>,
    bytes: &[u8],
) -> Result<(), error_stack::Report<ScoreCacheError>> {
    fs::write(path, bytes).map_err(|error| {
        error_stack::Report::new(ScoreCacheError::EciWrite {
            dir: path.to_string(),
            detail: error.to_string(),
        })
        .attach("failed to write a score cache file")
    })
}
