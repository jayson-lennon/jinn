//! The compiled-in Vals ↔ OpenRouter slug alias table.
//!
//! The Vals Index keys its records on `provider/model` slugs it
//! publishes (`z-ai/glm-5.3`-style provider namespaces frequently
//! differ from OpenRouter's, and dated/edited suffixes accumulate).
//! The join is: strip the listing's `:variant`, canonicalize both
//! sides (lowercase, separators to spaces), strict-match against the
//! index keys — first the full `author/slug` form, then the post-slash
//! slug alone — then consult this table for the residual spellings.
//!
//! **No fuzzy matching, ever**: attributing the wrong model's
//! performance is worse than a blank cell, so a listing that resolves
//! neither strictly nor through this table shows `—` and wears the `#`
//! flag. The table is Rust source, **never** a cache-dir file:
//! `<c-r>` wipes and re-downloads everything under the cache dir, and
//! the alias map must survive that.
//!
//! There is no vals counterpart to the ECI tripwire test (which proves
//! every alias's target exists in the shipped seed): no vals snapshot
//! ships in the repo, so tests here run against synthetic fixtures only.

use jinn_model_market_msg::ValsIndex;

/// One curated alias: a Vals `provider/model` slug and the OpenRouter
/// listing id that names the same model.
///
/// `vals_slug` is verbatim Vals form (`deepseek/deepseek-v4.1-flash-1017`).
/// `or_id` is the OpenRouter listing id without the `openrouter/`
/// selection prefix (`deepseek/deepseek-v4.1-flash-1017`) — the join
/// also tries the post-slash slug, but rows whose provider namespaces
/// differ need the full form to disambiguate.
#[derive(Debug)]
pub struct ValsAlias {
    /// The Vals slug, verbatim from the Index.
    pub vals_slug: &'static str,
    /// The OpenRouter listing id (author/slug, no variant) naming the
    /// same model.
    pub or_id: &'static str,
}

/// The curated alias table. Order matters only for readability; lookups
/// build one canonical-id map.
pub const VALS_ALIASES: &[ValsAlias] = &[
    // Provider-namespace differences: Vals names the model owner,
    // OpenRouter names the listing publisher.
    ValsAlias {
        vals_slug: "zai/glm-5.3",
        or_id: "z-ai/glm-5.3",
    },
    ValsAlias {
        vals_slug: "zai/glm-5.3-flash",
        or_id: "z-ai/glm-5.3-flash",
    },
    ValsAlias {
        vals_slug: "grok/grok-4.7",
        or_id: "x-ai/grok-4.7",
    },
    ValsAlias {
        vals_slug: "kimi/kimi-k3",
        or_id: "moonshotai/kimi-k3",
    },
    ValsAlias {
        vals_slug: "kimi/kimi-k3.1-pro",
        or_id: "moonshotai/kimi-k3.1-pro",
    },
    ValsAlias {
        vals_slug: "alibaba/qwen4-max",
        or_id: "qwen/qwen4-max",
    },
    // Edition and suffix residuals: Vals carries a variant word the
    // OpenRouter slug drops.
    ValsAlias {
        vals_slug: "baseten/nemotron-3.5-super-vl-120b-a12b",
        or_id: "baseten/nemotron-3.5-super-vl",
    },
];

/// Resolves an OpenRouter `author/slug[:variant]` listing id to a Vals
/// slug: canonicalized strict match against the index's keys — the full
/// id first, the post-slash slug second — then the alias table.
///
/// The variant suffix is stripped before lookup, so a match for the base
/// listing serves its `:free`/`:thinking` twins.
#[must_use]
pub fn resolve_vals<'a>(or_id: &str, index: &'a ValsIndex) -> Option<&'a str> {
    // The listing id is OpenRouter-namespaced (`openrouter/author/model`);
    // the vals slug is `provider/model`. Drop jinn's own prefix first so
    // the two namespaces line up.
    let without_ns = or_id.strip_prefix("openrouter/").unwrap_or(or_id);
    let listing = strip_variant(without_ns);
    let full = crate::eci::canonical_name(listing);
    let lookup = |probe: &str| {
        index
            .scores
            .keys()
            .find(|slug| crate::eci::canonical_name(slug) == probe)
            .map(String::as_str)
    };
    if let Some(found) = lookup(&full) {
        return Some(found);
    }
    let bare = crate::eci::canonical_name(listing.rsplit('/').next().unwrap_or(listing));
    if bare != full
        && let Some(found) = lookup(&bare)
    {
        return Some(found);
    }
    VALS_ALIASES
        .iter()
        .find(|alias| crate::eci::canonical_name(alias.or_id) == full)
        .filter(|alias| index.scores.keys().any(|slug| slug == alias.vals_slug))
        .map(|alias| alias.vals_slug)
}

/// Strips a trailing `:variant` suffix (`:free`, `:thinking`).
fn strip_variant(or_id: &str) -> &str {
    match or_id.split_once(':') {
        Some((base, _)) => base,
        None => or_id,
    }
}

#[cfg(test)]
#[path = "vals_aliases_tests.rs"]
mod vals_aliases_tests;
