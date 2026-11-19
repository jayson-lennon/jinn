//! The compiled-in epoch seed CSVs.
//!
//! The three files are byte-for-byte members of Epoch AI's
//! `benchmark_data.zip` (CC BY 4.0 — attribution in
//! `THIRD_PARTY_LICENSES`), committed verbatim and embedded by
//! [`crate::eci`]'s `include_bytes!` constants: `eci_scores.csv` (the
//! Epoch Capabilities Index the ECI column joins on),
//! `benchmark_metadata.csv` (per-benchmark normalization rows), and
//! `model_metadata.csv` (the per-version roster). This module exists to
//! keep the data files in one documented place — the parser and the
//! cache layer live in their own modules; nothing here is code.
