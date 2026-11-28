//! The market's pure projection — the filter, the sort, the wire row,
//! and the model page URL — shared by every surface that lists the
//! market.
//!
//! Both consumers need the same rows for the same ask: the terminal
//! popup filters and sorts its table through these functions, and the
//! web UI's server builds its rows through them for the frame it
//! answers. A `-msg` crate never depends on an implementation crate, so
//! placing the projection here is the only arrangement where the two
//! surfaces cannot drift — the provider-picker precedent.
//!
//! Nothing here reads a cell, an actor, or a theme: every function is a
//! pure transformation over [`MarketRow`]s and the view the caller
//! asks for. The caller owns the resolved mix; [`resolve_mix`] (the UI
//! cell's own, re-exported here beside the projection that consumes it)
//! derives it from the mix source and the session tree's measured usage.

use std::ops::Range;

use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;
use unicode_segmentation::UnicodeSegmentation as _;

use crate::row::MarketRow;
use crate::ui_state::SortColumn;
pub use crate::ui_state::resolve_mix;

/// The placeholder an empty cell renders as — an unpublished price, an
/// unmatched ECI, a missing latency. One constant so the glyph is a
/// one-line change.
pub const EMPTY_CELL: &str = "-";

/// The projected cost of one turn under the given mix, in cents.
///
/// `proj = I·(hit·cache_read_or_prompt + (1−hit)·prompt) + O·completion`
/// — a listing that publishes no cache price (decision F) prices its
/// cache reads at the prompt price, so the projection is always fully
/// costed.
#[must_use]
pub fn projected_cents(row: &MarketRow, mix: (u32, f64, u32)) -> f64 {
    projected_cents_for(
        row.prompt_price,
        row.completion_price,
        row.cache_read_price,
        mix,
    )
}

/// [`projected_cents`] over raw prices — the shared body both the
/// listing rows and the detail rows (whose sentinel prices come from a
/// `MarketRow`) project through.
#[must_use]
pub fn projected_cents_for(
    prompt: f64,
    completion: f64,
    cache: Option<f64>,
    mix: (u32, f64, u32),
) -> f64 {
    let (input_tokens, cache_hit, output_tokens) = mix;
    let cache_read = cache.unwrap_or(prompt);
    let input = f64::from(input_tokens) * (cache_hit * cache_read + (1.0 - cache_hit) * prompt);
    let output = f64::from(output_tokens) * completion;
    (input + output) * 100.0
}

/// The row's display name the filter matches against:
/// `author/slug[:variant]`.
#[must_use]
pub fn row_name(row: &MarketRow) -> String {
    match &row.variant {
        Some(variant) => format!("{}/{}:{variant}", row.author, row.slug),
        None => format!("{}/{}", row.author, row.slug),
    }
}

/// The filter split into its AND terms: whitespace-separated, each of
/// which must fuzzy-match a row's name for the row to stay visible.
/// A whitespace-only filter yields no terms — unfiltered.
#[must_use]
pub fn filter_terms(filter: &str) -> Vec<&str> {
    filter.split_whitespace().collect()
}

/// Whether every filter term fuzzy-matches the row name.
///
/// Terms are order-insensitive and match anywhere — plain per-term
/// `SkimMatcherV2` subsequence semantics, no positional logic.
#[must_use]
pub fn all_terms_match(matcher: &SkimMatcherV2, name: &str, terms: &[&str]) -> bool {
    terms
        .iter()
        .all(|term| matcher.fuzzy_match(name, term).is_some())
}

/// The visible projection: rows filtered by the fuzzy filter, then
/// stably sorted by the view's column and direction under `mix`.
///
/// The filter runs over `author/slug[:variant]` with `SkimMatcherV2`:
/// whitespace-separated terms AND-combine, so a row stays only when
/// every term matches; a whitespace-only filter keeps everything. The
/// sort is stable so equal-cost rows keep their fetch order.
#[must_use]
pub fn filter_and_sort(
    rows: &[MarketRow],
    filter: &str,
    sort: SortColumn,
    sort_desc: bool,
    mix: (u32, f64, u32),
) -> Vec<MarketRow> {
    let matcher = SkimMatcherV2::default();
    let terms = filter_terms(filter);
    let mut rows: Vec<MarketRow> = rows
        .iter()
        .filter(|row| terms.is_empty() || all_terms_match(&matcher, &row_name(row), &terms))
        .cloned()
        .collect();
    rows.sort_by(|a, b| compare_rows(a, b, sort, sort_desc, mix));
    rows
}

/// The fuzzy match offsets of the filter against one row's display name
/// — the byte indices the filter highlighted.
///
/// `None` means "no filter active" (nothing to highlight). An active
/// multi-term filter highlights the UNION of every term's matched
/// glyphs: the row reaching here matched every term, so each term
/// contributes its indices, merged and ascending. An active filter that
/// misses a row cannot reach here, because the projection has already
/// dropped the row.
#[must_use]
pub fn match_ranges(row: &MarketRow, filter: &str) -> Option<Vec<Range<usize>>> {
    let terms = filter_terms(filter);
    if terms.is_empty() {
        return None;
    }
    let matcher = SkimMatcherV2::default();
    let name = row_name(row);
    let mut indices: Vec<usize> = terms
        .iter()
        .filter_map(|term| matcher.fuzzy_indices(&name, term))
        .flat_map(|(_, matched)| matched)
        .collect();
    indices.sort_unstable();
    indices.dedup();
    Some(indices.into_iter().map(|i| i..i + 1).collect())
}

/// The sort key for one row.
///
/// `None` sorts after every value in either direction — an unmatched ECI
/// or an unpriced cache must not jump to the top when the user flips to
/// descending.
#[must_use]
pub fn sort_key(row: &MarketRow, col: SortColumn, mix: (u32, f64, u32)) -> Option<f64> {
    match col {
        SortColumn::Proj => Some(projected_cents(row, mix)),
        SortColumn::In => Some(row.prompt_price),
        SortColumn::Out => Some(row.completion_price),
        SortColumn::Cache => row.cache_read_price,
        SortColumn::Eci => row.eci,
        SortColumn::Vals => row.vals,
        SortColumn::Model => None,
    }
}

/// Compares two rows under a sort column and direction.
///
/// `None` keys sort after values regardless of direction, so a flip
/// never surfaces the unmatched rows first. The name tiebreak — the
/// whole comparison for the name sort, and the equal-key tiebreak for
/// every other — follows the direction: a header tap on the name column
/// reverses the names like any other column.
#[must_use]
pub fn compare_rows(
    a: &MarketRow,
    b: &MarketRow,
    sort: SortColumn,
    sort_desc: bool,
    mix: (u32, f64, u32),
) -> std::cmp::Ordering {
    match (sort_key(a, sort, mix), sort_key(b, sort, mix)) {
        (Some(av), Some(bv)) => {
            let ord = av.partial_cmp(&bv).unwrap_or(std::cmp::Ordering::Equal);
            if sort_desc { ord.reverse() } else { ord }
        }
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => {
            let ord = row_name(a).cmp(&row_name(b));
            if sort_desc { ord.reverse() } else { ord }
        }
    }
}

/// A run of a row's display name: a stretch the filter matched, or did
/// not. The browser renders the matched runs highlighted and the rest
/// plain; the split is cut on grapheme boundaries.
///
/// A local type rather than a dependency on the web UI's own run type —
/// a message crate never depends on another message crate, so each
/// surface maps it at its own boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameRun {
    /// The run's text.
    pub text: String,
    /// Whether the filter matched this run.
    pub matched: bool,
}

/// The name split into matched and plain runs for the filter. One plain
/// run when no filter is active.
#[must_use]
pub fn name_runs(row: &MarketRow, filter: &str) -> Vec<NameRun> {
    let Some(ranges) = match_ranges(row, filter) else {
        return vec![NameRun {
            text: row_name(row),
            matched: false,
        }];
    };
    let name = row_name(row);
    let mut runs: Vec<NameRun> = Vec::new();
    let mut current = String::new();
    let mut in_match = false;
    for (byte_off, grapheme) in name.grapheme_indices(true) {
        let is_matched = ranges.iter().any(|r| r.contains(&byte_off));
        if is_matched != in_match && !current.is_empty() {
            runs.push(NameRun {
                text: std::mem::take(&mut current),
                matched: in_match,
            });
        }
        in_match = is_matched;
        current.push_str(grapheme);
    }
    if !current.is_empty() {
        runs.push(NameRun {
            text: current,
            matched: in_match,
        });
    }
    runs
}

// ── Formatting ────────────────────────────────────────────────────────

/// Formats a USD/token price as a four-digit `$X.XXXX/M` column value —
/// every price renders the same width, so a column of prices scans as
/// one shape (e.g. `$3.0000/M`, `$0.3021/M`).
#[must_use]
pub fn price_per_million(usd_per_token: f64) -> String {
    let per_million = usd_per_token * 1_000_000.0;
    format!("${per_million:.4}/M")
}

/// Formats a USD/token price as [`EMPTY_CELL`] when it is unpublished
/// (decision F: the row is still fully costed at the prompt price).
#[must_use]
pub fn cache_column(usd_per_token: Option<f64>) -> String {
    usd_per_token.map_or_else(|| EMPTY_CELL.to_owned(), price_per_million)
}

/// Formats a projected cost in dollars with four decimals
/// (e.g. `$0.5100`).
#[must_use]
pub fn dollars(cents: f64) -> String {
    format!("${:.4}", cents / 100.0)
}

/// Formats an ECI value, [`EMPTY_CELL`] when unmatched.
#[must_use]
pub fn eci_column(eci: Option<f64>) -> String {
    eci.map_or_else(|| EMPTY_CELL.to_owned(), |v| format!("{v:.1}"))
}

/// Formats a Vals Index accuracy, [`EMPTY_CELL`] when unmatched — two
/// decimals, the precision Vals publishes.
#[must_use]
pub fn vals_column(vals: Option<f64>) -> String {
    vals.map_or_else(|| EMPTY_CELL.to_owned(), |v| format!("{v:.2}"))
}

/// Formats a latency in milliseconds, [`EMPTY_CELL`] when unpublished.
#[must_use]
pub fn ms_column(ms: Option<f64>) -> String {
    ms.map_or_else(|| EMPTY_CELL.to_owned(), |v| format!("{v:.0}ms"))
}

/// Formats a throughput in tok/s, [`EMPTY_CELL`] when unpublished.
#[must_use]
pub fn throughput_column(tokens_per_second: Option<f64>) -> String {
    tokens_per_second.map_or_else(|| EMPTY_CELL.to_owned(), |t| format!("{t:.0} tok/s"))
}

/// Formats a fraction as a percentage with one decimal, [`EMPTY_CELL`]
/// when unpublished.
#[must_use]
pub fn percent_column(fraction: Option<f64>) -> String {
    fraction.map_or_else(
        || EMPTY_CELL.to_owned(),
        |frac| format!("{:.1}%", frac * 100.0),
    )
}

/// The elapsed time since `stamp` as a compact human string
/// (`42s ago`, `7m ago`, `3h ago`) — the status line's "updated" stamp,
/// rendered on the server so the page computes nothing.
#[must_use]
pub fn age_stamp(fetched_at: jiff::Timestamp) -> String {
    let elapsed = jiff::Timestamp::now() - fetched_at;
    let secs = elapsed.total(jiff::Unit::Second).unwrap_or(0.0).max(0.0) as u64;
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 60 * 60 {
        format!("{}m ago", secs / 60)
    } else {
        format!("{}h ago", secs / 3600)
    }
}

/// The openrouter.ai page for a listing id: the id minus its
/// `openrouter/` prefix, appended to the site root. The remainder *is*
/// OpenRouter's page path verbatim, variant suffix (`:free`,
/// `:thinking`) included; a prefix-miss falls back to the raw id —
/// defensive only, `MarketRow.id` is prefixed by construction.
///
/// Both surfaces open the page through this one builder: the terminal's
/// `O` key hands the URL to the platform opener, and the web UI's
/// detail page is a link to it.
#[must_use]
pub fn model_page_url(model_id: &str) -> String {
    let rest = model_id.strip_prefix("openrouter/").unwrap_or(model_id);
    format!("https://openrouter.ai/{rest}")
}

#[cfg(test)]
#[path = "projection_tests.rs"]
mod tests;
