//! Fetching and tolerantly parsing the Vals Index.
//!
//! The data URL is **derived at refresh time**: the leaderboard page is
//! server-rendered HTML that names the current content-hashed
//! `_astro` JSON path, the hash changes per deploy, and old hashes 404 —
//! so a persisted path would be a dead URL and is never stored.
//!
//! Parse is tolerant by contract: unknown JSON fields are ignored (the
//! payload carries token totals, sampling params, harness names — none
//! are modeled), the task map is read as data (an added or renamed task
//! bucket parses unchanged), and only the `overall` bucket's `accuracy`
//! is consumed. Records are located by walking whatever map-shaped
//! `tasks` object the payload carries and keeping only entries whose
//! values are objects with a numeric `accuracy`.
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
/// Returns an error when the bytes are not JSON, or when no task map
/// with any object-valued entries exists anywhere (nothing to display).
pub fn parse_vals_index(bytes: &[u8]) -> Result<ValsIndex, error_stack::Report<ValsError>> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|error| {
        error_stack::Report::new(ValsError::Parse {
            detail: error.to_string(),
        })
        .attach("the fetched body is not JSON")
    })?;

    let mut scores: HashMap<String, ValsScore> = HashMap::new();
    for tasks in task_maps(&value) {
        collect_accuracies(tasks, &mut scores);
    }
    if scores.is_empty() {
        return Err(error_stack::Report::new(ValsError::Parse {
            detail: "the payload carried no model accuracy records".to_owned(),
        })
        .attach("no tasks map held any usable records"));
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

/// Every map-shaped `tasks` object in the payload, top level first,
/// `metadata.tasks` second. Extra hits are harmless: dedup happens by
/// key in the collector, and per-bucket maps merge into one slug→score
/// view where the `overall` bucket's value wins by insertion order.
fn task_maps(value: &serde_json::Value) -> Vec<&serde_json::Map<String, serde_json::Value>> {
    let mut maps = Vec::new();
    if let Some(top) = value.get("tasks").and_then(serde_json::Value::as_object) {
        maps.push(top);
    }
    if let Some(nested) = value
        .get("metadata")
        .and_then(|m| m.get("tasks"))
        .and_then(serde_json::Value::as_object)
    {
        maps.push(nested);
    }
    maps
}

/// Walks one tasks map: a value that is itself a map of object-shaped
/// records is a bucket — collect each record's `accuracy` under its
/// slug. A string value (a bucket display label) is skipped. The
/// `overall` bucket is collected like any other; it is simply the one
/// the join reads, and scanning every bucket keeps the parse data-driven.
fn collect_accuracies(
    tasks: &serde_json::Map<String, serde_json::Value>,
    scores: &mut HashMap<String, ValsScore>,
) {
    for (_bucket, records) in tasks {
        let Some(records) = records.as_object() else {
            continue;
        };
        for (slug, record) in records {
            let Some(accuracy) = record.get("accuracy").and_then(serde_json::Value::as_f64) else {
                continue;
            };
            scores.insert(slug.clone(), ValsScore { accuracy });
        }
    }
}

#[cfg(test)]
#[path = "vals_tests.rs"]
mod vals_tests;
