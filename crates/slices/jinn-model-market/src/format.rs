//! Display formatting shared by the view and the tests: price columns
//! and the "updated X ago" stamps.

use jiff::Timestamp;

/// The placeholder an empty cell renders as — an unpublished price, an
/// unmatched ECI, a missing latency. One constant so the glyph is a
/// one-line change.
pub const EMPTY_CELL: &str = "-";

/// Formats a USD/token price as a four-digit `$X.XXXX/M` column value —
/// every price renders the same width, so a column of prices scans as
/// one shape (e.g. `$3.0000/M`, `$0.3021/M`).
#[must_use]
pub fn price_per_million(usd_per_token: f64) -> String {
    let per_million = usd_per_token * 1_000_000.0;
    format!("${per_million:.4}/M")
}

/// Formats a USD/token price as [`EMPTY_CELL`] when it is unpublished
/// (locked decision F: the row is still fully costed at the prompt
/// price).
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

/// The elapsed time since `stamp` as a compact human string
/// (`42s ago`, `7m ago`, `3h ago`), the models picker's form.
#[must_use]
pub fn age_stamp(fetched_at: Timestamp) -> String {
    let elapsed = Timestamp::now() - fetched_at;
    let secs = elapsed.total(jiff::Unit::Second).unwrap_or(0.0).max(0.0) as u64;
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 60 * 60 {
        format!("{}m ago", secs / 60)
    } else {
        format!("{}h ago", secs / 3600)
    }
}
