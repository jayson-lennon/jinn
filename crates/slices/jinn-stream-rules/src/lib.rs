//! The stream-rules slice — regex rules tested against the live assistant stream.
//!
//! This slice contributes exactly two things at activation:
//!
//! - the matcher cell, registered by the catalog and resolved here, into
//!   which the compiled rule set is installed; and
//! - nothing else. No routes, no views, no overlays, no actors.
//!
//! The consumer is the inference actor's stream loop, which resolves the
//! matcher from the registry and asks it about each delta before publishing
//! that delta downstream. It never names this crate — it speaks
//! [`jinn_slices::StreamRuleSet`], which is why the vocabulary lives in
//! `jinn-slices` rather than here.
//!
//! # What the slice does *not* do
//!
//! It does not publish, subscribe, or intercept anything itself. Interruption
//! is the inference loop's job; this slice only answers the question "does
//! this delta trip a rule, and which one".

pub mod matcher;
pub mod rules;

use std::sync::Arc;

use jinn_slices::{RenderFacts, SliceHost, StreamRules};

use crate::matcher::CompiledSet;
use crate::rules::read_rules;

/// Activates the slice: reads `[[stream_rules.entry]]` from `jinn.toml`,
/// compiles the rules, and installs the matcher into its cell.
///
/// Every configured rule that fails to compile is warned and skipped; a
/// configuration with no usable rule installs an empty set, which the stream
/// loop treats as "no rules" and costs nothing per delta.
pub fn activate(host: &mut SliceHost<'_, RenderFacts>, config: &jinn_config::ConfigLayer) {
    // Read at the point of use rather than cached, matching the rules
    // themselves: a reload is observed by the next launch.
    let rules = read_rules(config);
    let set = Arc::new(CompiledSet::build(&rules));

    // The cell was minted by the catalog before any activation ran, so a
    // failure here means the boot list's ordering was violated.
    let Some(cell) = host
        .slices()
        .reader::<StreamRules>(&jinn_slices::stream_rules_slot())
    else {
        tracing::error!(
            "stream-rules cell is absent from the registry; \
             the cell catalog must run before this activation"
        );
        return;
    };

    cell.update(|payload| payload.install(set));
}
