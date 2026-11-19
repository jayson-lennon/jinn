//! Parsing the epoch benchmark CSVs into the data cell's [`EciIndex`].
//!
//! The three seed files ship verbatim from `benchmark_data.zip`
//! (`crates/slices/jinn-model-market/src/seed/`): `eci_scores.csv` is the
//! one the ECI column reads (display name → score + bootstrap CI),
//! `benchmark_metadata.csv` carries the per-benchmark normalization rows
//! the help card's data-sources section renders, and `model_metadata.csv`
//! the per-version roster rows. Parse is pure: bytes in, typed data out,
//! no filesystem and no clock — the cache layer decides *which* bytes.

use std::collections::HashMap;

use jinn_model_market_msg::EciIndex;
use jinn_model_market_msg::EciScore;

/// The seed's `eci_scores.csv`, compiled in so a first run with no cache
/// dir and no network still shows ECI values (`eci_source = Seed`).
pub const SEED_ECI_SCORES: &[u8] = include_bytes!("seed/eci_scores.csv");

/// The seed's `benchmark_metadata.csv` — per-benchmark normalization
/// metadata (scale, baselines, ceiling, supersession).
pub const SEED_BENCHMARK_METADATA: &[u8] = include_bytes!("seed/benchmark_metadata.csv");

/// The seed's `model_metadata.csv` — the per-version roster
/// (organization, accessibility, date).
pub const SEED_MODEL_METADATA: &[u8] = include_bytes!("seed/model_metadata.csv");

/// Why ECI data could not be parsed.
///
/// Every variant carries the file it came from plus the row or field that
/// failed; the actor renders the message into the popup's degraded-mode
/// line, so the strings are written for a user reading a status line.
#[derive(Debug, wherror::Error)]
#[error(debug)]
pub enum EciParseError {
    /// The CSV reader rejected the file's structure.
    #[error("parsing {file}: {detail}")]
    Csv {
        /// Which seed or extracted file failed.
        file: &'static str,
        /// The CSV reader's complaint.
        detail: String,
    },
    /// A score row was missing a required field or carried a value that
    /// did not parse as a number.
    #[error("{file} row {row}: field {field} is not a usable number ({value})")]
    BadNumber {
        /// Which seed or extracted file failed.
        file: &'static str,
        /// The 1-based CSV record number.
        row: usize,
        /// The field that would not parse.
        field: &'static str,
        /// The raw value.
        value: String,
    },
}

impl EciParseError {
    fn bad_number(
        file: &'static str,
        row: usize,
        field: &'static str,
        value: &str,
    ) -> error_stack::Report<Self> {
        error_stack::Report::new(Self::BadNumber {
            file,
            row,
            field,
            value: value.to_owned(),
        })
        .attach(format!("{file} row {row}: failed to parse field {field}"))
    }
}

/// One `benchmark_metadata.csv` row: how a benchmark's raw scores are
/// normalized into the ECI.
#[derive(Debug, Clone, PartialEq)]
pub struct BenchmarkNormalization {
    /// The benchmark's display name (`GPQA diamond`).
    pub benchmark: String,
    /// Whether the benchmark feeds the published ECI (62 of 91 do).
    pub in_eci: bool,
    /// The zip member the per-question scores live in.
    pub source_file: String,
    /// The column the score is read from.
    pub score_column: String,
    /// The score's scale ceiling divisor (1.0, 100.0, …).
    pub scale: f64,
    /// A no-skill attempt's score, subtracted before normalization.
    pub random_baseline: f64,
    /// The maximum attainable score.
    pub score_ceiling: f64,
    /// The benchmark that replaces this one, when superseded.
    pub superseded_by: Option<String>,
}

/// The parsed shape of one ECI refresh: the index plus the two
/// metadata tables the help card renders.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EciData {
    /// Parsed ECI scores keyed by display name — the data cell's `eci`
    /// payload.
    pub index: EciIndex,
    /// How many rows `eci_scores.csv` carried, including any dropped by
    /// `parse_eci_scores` — the `EciLoaded` event's `total`.
    pub total_rows: u32,
    /// Per-benchmark normalization metadata rows.
    pub benchmark_metadata: Vec<BenchmarkNormalization>,
    /// The display names the roster carries (model versions the join can
    /// name even when the score table does not).
    pub roster_names: Vec<String>,
}

/// Canonical form for the ECI name join: lowercase, `.`/`-`/`_`/`:`
/// become spaces, whitespace collapsed.
///
/// Dots fold to spaces so `gpt-4o-mini` and `GPT-4o mini` collide (the
/// dominant case); version dots ride along harmlessly because both
/// sides of the join fold the same way. Parentheses survive so dated
/// snapshots stay distinct.
#[must_use]
pub fn canonical_name(name: &str) -> String {
    let folded = name.trim().to_lowercase();
    let mapped: String = folded
        .chars()
        .map(|c| match c {
            '-' | '_' | '.' | ':' => ' ',
            other => other,
        })
        .collect();
    mapped.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Parses `eci_scores.csv` bytes into an [`EciIndex`].
///
/// Rows missing any of the three numeric fields are skipped (Epoch marks
/// CI-less versions with empty cells — the seed currently carries two,
/// e.g. `GPT-5`); the row count still counts them, so the `EciLoaded`
/// event can report the join's coverage honestly.
///
/// # Errors
///
/// Returns an error when the CSV structure is unreadable or a numeric
/// field carries a non-numeric value.
pub fn parse_eci_scores(
    bytes: &[u8],
) -> Result<(EciIndex, u32), error_stack::Report<EciParseError>> {
    const FILE: &str = "eci_scores.csv";
    let mut reader = csv::Reader::from_reader(bytes);
    let mut scores = HashMap::new();
    let mut total: u32 = 0;
    for result in reader.records() {
        total = total.saturating_add(1);
        let record = read_record(result, FILE, "eci_scores.csv record")?;
        let row = total as usize;
        let name = record.get(1).unwrap_or_default().trim().to_owned();
        if name.is_empty() {
            continue;
        }
        let (Some(score), Some(ci_low), Some(ci_high)) = (
            optional_score(FILE, row, "eci", record.get(2).unwrap_or_default())?,
            optional_score(FILE, row, "eci_ci_low", record.get(3).unwrap_or_default())?,
            optional_score(FILE, row, "eci_ci_high", record.get(4).unwrap_or_default())?,
        ) else {
            continue;
        };
        scores.insert(
            name,
            EciScore {
                eci: score,
                ci_low,
                ci_high,
            },
        );
    }
    Ok((EciIndex { scores }, total))
}

/// Parses `benchmark_metadata.csv` bytes into normalization rows.
///
/// # Errors
///
/// Returns an error when the CSV structure is unreadable or a numeric
/// field carries a non-numeric value.
pub fn parse_benchmark_metadata(
    bytes: &[u8],
) -> Result<Vec<BenchmarkNormalization>, error_stack::Report<EciParseError>> {
    const FILE: &str = "benchmark_metadata.csv";
    let mut reader = csv::Reader::from_reader(bytes);
    let mut rows = Vec::new();
    for result in reader.records() {
        let record = read_record(result, FILE, "benchmark_metadata.csv record")?;
        let row = rows.len() + 1;
        let name = record.get(0).unwrap_or_default().trim();
        if name.is_empty() {
            continue;
        }
        rows.push(BenchmarkNormalization {
            benchmark: name.to_owned(),
            in_eci: record.get(1).unwrap_or_default().trim() == "True",
            source_file: record.get(2).unwrap_or_default().trim().to_owned(),
            score_column: record.get(3).unwrap_or_default().trim().to_owned(),
            scale: required_score(FILE, row, "scale", record.get(4).unwrap_or_default())?,
            random_baseline: required_score(
                FILE,
                row,
                "random_baseline",
                record.get(5).unwrap_or_default(),
            )?,
            score_ceiling: required_score(
                FILE,
                row,
                "score_ceiling",
                record.get(6).unwrap_or_default(),
            )?,
            superseded_by: {
                let value = record.get(8).unwrap_or_default().trim();
                (!value.is_empty()).then(|| value.to_owned())
            },
        });
    }
    Ok(rows)
}

/// Parses `model_metadata.csv` bytes into the roster's display names.
///
/// The roster's rows are ragged (the zip ships several layout variants
/// across its history), so this reads only the columns the join and the
/// help card need: the first non-empty of the name-bearing cells names
/// the version. Empty rows are skipped.
///
/// # Errors
///
/// Returns an error when the CSV structure is unreadable.
pub fn parse_roster_names(bytes: &[u8]) -> Result<Vec<String>, error_stack::Report<EciParseError>> {
    const FILE: &str = "model_metadata.csv";
    let mut reader = csv::Reader::from_reader(bytes);
    let mut names = Vec::new();
    for result in reader.records() {
        let record = read_record(result, FILE, "model_metadata.csv record")?;
        let name = record.iter().map(str::trim).find(|cell| !cell.is_empty());
        if let Some(name) = name {
            names.push(name.to_owned());
        }
    }
    Ok(names)
}

/// Parses all three seed tables in one call — the compile-time fixture
/// the seed-fallback path serves and the actor test-fixture shape.
///
/// `total` on the returned [`EciData`] is `eci_scores.csv`'s row count
/// including rows skipped for missing numbers.
///
/// # Errors
///
/// Returns an error when any table fails to parse; the caller treats the
/// whole refresh as failed (partial ECI data would mislead the join).
pub fn parse_all(
    eci_scores: &[u8],
    benchmark_metadata: &[u8],
    model_metadata: &[u8],
) -> Result<EciData, error_stack::Report<EciParseError>> {
    let (index, total_rows) = parse_eci_scores(eci_scores)?;
    let benchmark_metadata = parse_benchmark_metadata(benchmark_metadata)?;
    let roster_names = parse_roster_names(model_metadata)?;
    Ok(EciData {
        index,
        total_rows,
        benchmark_metadata,
        roster_names,
    })
}

/// The seed's fully-parsed data — the first-run fallback content.
///
/// # Errors
///
/// Propagates [`parse_all`]'s errors; the seed is a compile-time fixture,
/// so an error here is a bug in the shipped bytes, not a runtime case.
pub fn seed_data() -> Result<EciData, error_stack::Report<EciParseError>> {
    parse_all(
        SEED_ECI_SCORES,
        SEED_BENCHMARK_METADATA,
        SEED_MODEL_METADATA,
    )
}

/// Parses one required score cell, mapping the failure into a
/// row-addressable error.
fn required_score(
    file: &'static str,
    row: usize,
    field: &'static str,
    raw: &str,
) -> Result<f64, error_stack::Report<EciParseError>> {
    optional_score(file, row, field, raw)?
        .ok_or_else(|| EciParseError::bad_number(file, row, field, raw))
}

/// Parses one score cell: `None` for an empty cell (the row is then
/// skipped), the parsed value otherwise, a row-addressable error for a
/// non-numeric non-empty value. The source error is dropped by design —
/// `f64::parse`'s error adds nothing over naming the raw string — and
/// the discarded binding keeps the lint honest about that choice.
fn optional_score(
    file: &'static str,
    row: usize,
    field: &'static str,
    raw: &str,
) -> Result<Option<f64>, error_stack::Report<EciParseError>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    trimmed
        .parse::<f64>()
        .map(Some)
        .map_err(|_dropped| EciParseError::bad_number(file, row, field, raw))
}

/// Reads one CSV record, converting the reader error into the parse
/// error with the file named.
fn read_record(
    result: Result<csv::StringRecord, csv::Error>,
    file: &'static str,
    what: &'static str,
) -> Result<csv::StringRecord, error_stack::Report<EciParseError>> {
    result.map_err(|error| {
        error_stack::Report::new(EciParseError::Csv {
            file,
            detail: error.to_string(),
        })
        .attach(format!("failed to read {what}"))
    })
}
