//! ECI-sourced vocabulary shared by both cells: the parsed ECI index,
//! and where the ECI data on display came from.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Parsed epoch capability scores, keyed by ECI display name.
///
/// The join key is the *name*: `eci_scores.csv` carries no OpenRouter
/// ids, so the models actor resolves each listing to an ECI name through
/// strict display-name matching plus the compiled-in alias table, then
/// reads the score here. Normalization metadata rows ride along for the
/// help card's data-sources section.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EciIndex {
    /// ECI display name → `(score, ci_low, ci_high)`.
    pub scores: HashMap<String, EciScore>,
}

/// One model version's capability score with its bootstrap interval.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EciScore {
    /// The Epoch Capabilities Index value.
    pub eci: f64,
    /// Lower bound of the bootstrap confidence interval.
    pub ci_low: f64,
    /// Upper bound of the bootstrap confidence interval.
    pub ci_high: f64,
}

impl EciIndex {
    /// The score for an ECI display name, if the index carries it.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&EciScore> {
        self.scores.get(name)
    }

    /// How many model versions the index holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.scores.len()
    }

    /// Whether the index is empty (no seed, no download, nothing parsed).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.scores.is_empty()
    }
}

/// Where the ECI data currently in the data cell came from.
///
/// Actors record the provenance on every load; nothing renders it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum EciSource {
    /// The CSVs compiled into the slice (`include_bytes!`): first run
    /// with no cache dir and no network still shows ECI values.
    #[default]
    Seed,
    /// Extracted from a downloaded `benchmark_data.zip` into the cache
    /// dir.
    Download,
    /// Nothing loaded — the ECI column is `—` everywhere.
    None,
}

/// Parsed Vals Index scores, keyed by Vals' `provider/model` slug.
///
/// The join key is the *slug*: the index keys its records the same way
/// Vals publishes them (`anthropic/claude-sonnet-5-5`), and the models
/// actor resolves each OpenRouter listing to one through strict
/// canonical matching plus the compiled-in alias table, then reads the
/// overall accuracy here. Only the `overall` task bucket is consumed —
/// per-sector buckets stay unparsed (vendor-etiquette bound).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ValsIndex {
    /// Vals `provider/model` slug → overall accuracy.
    pub scores: HashMap<String, ValsScore>,
    /// The index version Vals published (`2.2`), for diagnostics.
    pub version: Option<String>,
    /// The publish date Vals stamped (`2026-11-18`).
    pub updated: Option<String>,
}

/// One model's Vals Index overall accuracy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ValsScore {
    /// Overall accuracy, percent (e.g. `71.309`).
    pub accuracy: f64,
}

impl ValsIndex {
    /// The score for a Vals slug, if the index carries it.
    #[must_use]
    pub fn get(&self, slug: &str) -> Option<&ValsScore> {
        self.scores.get(slug)
    }

    /// How many model slugs the index holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.scores.len()
    }

    /// Whether the index is empty (no cache, no download, nothing
    /// parsed).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.scores.is_empty()
    }
}

/// Where the vals data currently in the data cell came from.
///
/// There is no `Seed` variant: no vals snapshot ships in the repo (the
/// user directed no in-repo copy), so the column starts empty and only
/// a download ever fills it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValsSource {
    /// Extracted from a downloaded Vals Index JSON into the cache dir.
    Download,
    /// Nothing loaded — the VALS column is empty everywhere.
    #[default]
    None,
}
