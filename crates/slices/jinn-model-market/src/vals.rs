//! Fetching and tolerantly parsing the Vals Index.
//!
//! The data URL is **derived at refresh time**: the leaderboard page is
//! server-rendered HTML that names the current content-hashed
//! `_astro` JSON path, the hash changes per deploy, and old hashes 404 —
//! so a persisted path would be a dead URL and is never stored.
//!
//! Parse is tolerant by contract: unknown JSON fields are ignored (the
//! payload carries token totals, sampling params, harness names — none
//! are modeled), and a record with a missing or non-numeric `accuracy`
//! is skipped, not fatal. Exactly ONE bucket is consumed: the map named
//! `overall` — the Vals Index aggregate itself. Its sibling buckets
//! (`vibe_code_bench`, `finance_agent`, …) are the sector benchmarks
//! that feed the index; scanning them would overwrite the index value
//! with a sector score under the same slug key, so they are never read.
//! An *added* bucket parses unchanged precisely because only `overall`
//! is consulted.
//!
//! Vendor-etiquette bound: jinn consumes one number per model — the
//! overall accuracy. Cost, latency, rank, and per-sector buckets are
//! deliberately not parsed into typed structures.

use std::collections::HashMap;

use jinn_model_market_msg::ValsIndex;
use jinn_model_market_msg::ValsScore;

/// The data file's URL prefix: the leaderboard page serves its JSON from
/// the site's `_astro` bundle directory.
const ASTRO_DIR: &str = "/_astro/";

/// The data file's name stem: `benchmark_view_vals_index.<hash>.json`.
const DATA_STEM: &str = "benchmark_view_vals_index.";

/// The one task bucket the display consumes: the Vals Index aggregate.
const OVERALL_BUCKET: &str = "overall";

/// Why the Vals Index could not be fetched or parsed.
///
/// Variants render into the popup's degraded-mode line, so the strings
/// are written for a user reading a status line.
#[derive(Debug, wherror::Error)]
#[error(debug)]
pub enum ValsError {
    /// The leaderboard page fetch failed (offline, DNS, non-200), or
    /// the fetched page named no recognizable data file.
    #[error("fetching the Vals leaderboard page: {detail}")]
    Page {
        /// What the fetch or page scan failed on, ready to render.
        detail: String,
    },
    /// The fetched bytes were not JSON, or carried no usable records.
    #[error("parsing the Vals Index JSON: {detail}")]
    Parse {
        /// What the JSON layer rejected, ready to render.
        detail: String,
    },
}

/// Fetches the leaderboard page, extracts the current data-file path,
/// and downloads it.
///
/// The returned bytes are the raw Index JSON, ready for
/// [`parse_vals_index`] and verbatim disk storage.
///
/// # Errors
///
/// Returns an error when either request fails or the page names no
/// data file.
pub async fn fetch_index_json(
    client: &reqwest::Client,
    page_url: &str,
) -> Result<Vec<u8>, error_stack::Report<ValsError>> {
    let page = fetch_bytes(client, page_url).await?;
    let data_url = derive_data_url(page_url, &page)?;
    fetch_bytes(client, &data_url).await
}

/// Fetches a URL's body as bytes.
///
/// # Errors
///
/// Returns an error on transport failure or a non-2xx status.
async fn fetch_bytes(
    client: &reqwest::Client,
    url: &str,
) -> Result<Vec<u8>, error_stack::Report<ValsError>> {
    let response = client.get(url).send().await.map_err(|error| {
        error_stack::Report::new(ValsError::Page {
            detail: error.to_string(),
        })
        .attach("requesting a vals.ai resource")
    })?;
    if !response.status().is_success() {
        return Err(error_stack::Report::new(ValsError::Page {
            detail: format!("server returned {}", response.status()),
        })
        .attach("a vals.ai request returned an error status"));
    }
    response
        .bytes()
        .await
        .map(|bytes| bytes.to_vec())
        .map_err(|error| {
            error_stack::Report::new(ValsError::Page {
                detail: error.to_string(),
            })
            .attach("reading a vals.ai response body")
        })
}

/// Extracts the current data-file URL from the page's HTML.
///
/// The page embeds the hashed JSON path (`/_astro/benchmark_view_vals_index.<hash>.json`)
/// in its rendered markup. The first match wins: the page ships one
/// benchmark view per page, and the stem is specific enough that a
/// collision would be a different benchmark's view, which the parse
/// would then reject. The path is resolved against the *page's own
/// origin* — the site-relative path rides whatever host served the
/// page, which is also what lets tests point the whole flow at a local
/// mock.
fn derive_data_url(
    page_url: &str,
    page_html: &[u8],
) -> Result<String, error_stack::Report<ValsError>> {
    let (scheme, origin) = page_url
        .split_once("://")
        .map(|(scheme, rest)| {
            (
                scheme.to_owned(),
                rest.split('/').next().unwrap_or(rest).to_owned(),
            )
        })
        .ok_or_else(|| {
            error_stack::Report::new(ValsError::Page {
                detail: "the page URL has no origin".to_owned(),
            })
            .attach("resolving the data path against the page origin")
        })?;
    let html = String::from_utf8_lossy(page_html);
    let path = html
        .split(ASTRO_DIR)
        .skip(1)
        .find_map(|rest| {
            let candidate = rest.split(['"', '\'', ')', ' '].as_slice()).next()?;
            candidate
                .starts_with(DATA_STEM)
                .then(|| candidate.trim_end_matches('.').to_owned())
        })
        .filter(|name| name.to_ascii_lowercase().ends_with(".json"))
        .map(|name| format!("{scheme}://{origin}{ASTRO_DIR}{name}"))
        .ok_or_else(|| {
            error_stack::Report::new(ValsError::Page {
                detail: "the page named no Vals Index data file".to_owned(),
            })
            .attach("scanning the leaderboard page for its data-file path")
        })?;
    Ok(path)
}

/// Parses the Index JSON into a [`ValsIndex`], tolerating schema drift.
///
/// Tolerances, per contract:
///
/// - unknown fields anywhere are ignored (`serde_json::Value` walking —
///   the payload carries token totals, sampling params, harness names,
///   none modeled);
/// - the task map is data, not schema: every map-shaped `tasks` object
///   in the payload is scanned, and a record enters the index whenever
///   its value is an object carrying a numeric `accuracy`. An added,
///   renamed, or removed task bucket changes nothing;
/// - a record with a missing or non-numeric `accuracy` is skipped, not
///   fatal;
/// - the top-level `tasks` map is read first; `metadata.tasks` is the
///   fallback for a payload that nests records there instead.
///
/// # Errors
///
/// Returns an error when the bytes are not JSON, or when no `overall`
/// bucket carrying usable records exists anywhere in the payload
/// (nothing to display).
pub fn parse_vals_index(bytes: &[u8]) -> Result<ValsIndex, error_stack::Report<ValsError>> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|error| {
        error_stack::Report::new(ValsError::Parse {
            detail: error.to_string(),
        })
        .attach("the fetched body is not JSON")
    })?;

    let mut scores: HashMap<String, ValsScore> = HashMap::new();
    for tasks in overall_maps(&value) {
        collect_accuracies(tasks, &mut scores);
    }
    if scores.is_empty() {
        return Err(error_stack::Report::new(ValsError::Parse {
            detail: "the payload carried no Vals Index (overall) records".to_owned(),
        })
        .attach("no overall bucket held any usable records"));
    }

    let metadata = value.get("metadata");
    Ok(ValsIndex {
        scores,
        version: metadata
            .and_then(|m| m.get("version"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        updated: metadata
            .and_then(|m| m.get("updated"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
    })
}

/// The map-shaped objects that hold the index's model records: the
/// top-level `tasks.overall` map first, `metadata.tasks.overall` second
/// (the fallback for a payload that nests records there instead).
///
/// Only the bucket literally named `overall` qualifies — the Vals Index
/// aggregate. The other buckets are sector benchmarks whose records
/// share the same slug keys; reading them would put a sector score
/// where the index value belongs.
fn overall_maps(value: &serde_json::Value) -> Vec<&serde_json::Map<String, serde_json::Value>> {
    ["tasks", "metadata.tasks"]
        .iter()
        .find_map(|path| {
            let mut current = value;
            for segment in path.split('.') {
                current = current.get(segment)?;
            }
            current
                .as_object()
                .and_then(|tasks| tasks.get(OVERALL_BUCKET))
                .and_then(serde_json::Value::as_object)
        })
        .into_iter()
        .collect()
}

/// Walks the `overall` bucket's records: each slug whose record is an
/// object carrying a numeric `accuracy` enters the index. Missing or
/// non-numeric accuracies are skipped, not fatal.
fn collect_accuracies(
    tasks: &serde_json::Map<String, serde_json::Value>,
    scores: &mut HashMap<String, ValsScore>,
) {
    for (slug, record) in tasks {
        let Some(accuracy) = record.get("accuracy").and_then(serde_json::Value::as_f64) else {
            continue;
        };
        scores.insert(slug.clone(), ValsScore { accuracy });
    }
}

#[cfg(test)]
#[path = "vals_tests.rs"]
mod vals_tests;
