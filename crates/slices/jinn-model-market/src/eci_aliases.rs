//! The compiled-in ECI ↔ OpenRouter alias table.
//!
//! `eci_scores.csv` keys on Epoch's display names and carries no
//! OpenRouter ids, so the join is: canonicalize both sides (lowercase,
//! separators to spaces), strict-match, then consult this table for the
//! residual spellings strict matching cannot close (`gpt-4o-2024-11-20`
//! → `GPT-4o (Nov 2024)`, `kimi-k2-thinking` → `Kimi K2 Thinking`).
//!
//! The table is Rust source, **never** a cache-dir file: `<c-r>` wipes
//! and re-downloads everything under the cache dir, and the alias map
//! must survive that (locked decision B / contract trap). A unit test in
//! `eci_aliases_tests.rs` asserts every `eci_name` here matches a row in
//! the shipped seed CSV — the drift tripwire that keeps this table
//! honest when the seed updates.

use jinn_model_market_msg::EciIndex;

/// One curated alias: an ECI display name and the OpenRouter slug that
/// resolves to it.
///
/// `eci_name` must appear verbatim in the shipped `eci_scores.csv`'s
/// `Display name` column (the tripwire test enforces it). `or_slug` is
/// the OpenRouter listing slug without the author namespace
/// (`openai/gpt-4o-2024-11-20` → `gpt-4o-2024-11-20`) — the join
/// compares against the full `author/slug` too, but the author prefix
/// never helps Epoch's names, so the table stores the slug alone.
#[derive(Debug)]
pub struct EciAlias {
    /// The ECI display name, verbatim from the seed CSV.
    pub eci_name: &'static str,
    /// The OpenRouter slug (without author) that names the same version.
    pub or_slug: &'static str,
}

/// The curated alias table. Order matters only for readability; lookups
/// build one canonical-slug map.
pub const ECI_ALIASES: &[EciAlias] = &[
    // Dated GPT snapshots: OpenRouter carries the date in the slug,
    // Epoch appends a parenthesized month.
    EciAlias {
        eci_name: "GPT-4o (Nov 2024)",
        or_slug: "gpt-4o-2024-11-20",
    },
    EciAlias {
        eci_name: "GPT-4o (Aug 2024)",
        or_slug: "gpt-4o-2024-08-06",
    },
    EciAlias {
        eci_name: "GPT-4o (May 2024)",
        or_slug: "gpt-4o-2024-05-13",
    },
    EciAlias {
        eci_name: "GPT-4 Turbo (Nov 2023)",
        or_slug: "gpt-4-1106-preview",
    },
    EciAlias {
        eci_name: "GPT-4 Turbo (Apr 2024)",
        or_slug: "gpt-4-turbo-2024-04-09",
    },
    EciAlias {
        eci_name: "GPT-4 (Mar 2023)",
        or_slug: "gpt-4",
    },
    EciAlias {
        eci_name: "GPT-4 (Jun 2023)",
        or_slug: "gpt-4-0613",
    },
    EciAlias {
        eci_name: "GPT-3.5 Turbo (Jan 2024)",
        or_slug: "gpt-3.5-turbo-0125",
    },
    EciAlias {
        eci_name: "GPT-3.5 Turbo (Nov 2023)",
        or_slug: "gpt-3.5-turbo-1106",
    },
    EciAlias {
        eci_name: "GPT-3.5 Turbo (Jun 2023)",
        or_slug: "gpt-3.5-turbo-0613",
    },
    // Dated Gemini snapshots: OpenRouter embeds the date, Epoch
    // parenthesizes a month.
    EciAlias {
        eci_name: "Gemini 2.0 Flash (Feb 2025)",
        or_slug: "gemini-2.0-flash-001",
    },
    EciAlias {
        eci_name: "Gemini 2.5 Pro (Mar 2025)",
        or_slug: "gemini-2.5-pro",
    },
    EciAlias {
        eci_name: "Gemini 2.5 Pro (May 2025)",
        or_slug: "gemini-2.5-pro-05-06",
    },
    EciAlias {
        eci_name: "Gemini 2.5 Pro (Jun 2025)",
        or_slug: "gemini-2.5-pro-06-17",
    },
    EciAlias {
        eci_name: "Gemini 2.5 Flash (Apr 2025)",
        or_slug: "gemini-2.5-flash",
    },
    EciAlias {
        eci_name: "Gemini 2.5 Flash (May 2025)",
        or_slug: "gemini-2.5-flash-05-20",
    },
    EciAlias {
        eci_name: "Gemini 2.5 Flash (Jun 2025)",
        or_slug: "gemini-2.5-flash-06-17",
    },
    EciAlias {
        eci_name: "Gemini 2.5 Flash-Lite (Jun 2025)",
        or_slug: "gemini-2.5-flash-lite-06-17",
    },
    EciAlias {
        eci_name: "Gemini 2.0 Flash Thinking (Jan 2025)",
        or_slug: "gemini-2.0-flash-thinking-exp-01-21",
    },
    EciAlias {
        eci_name: "Gemini 1.5 Pro (May 2024)",
        or_slug: "gemini-1.5-pro",
    },
    EciAlias {
        eci_name: "Gemini 1.5 Flash (May 2024)",
        or_slug: "gemini-1.5-flash",
    },
    // Dated snapshots of other families.
    EciAlias {
        eci_name: "Claude 3.5 Sonnet (October 2024)",
        or_slug: "claude-3.5-sonnet-20241022",
    },
    EciAlias {
        eci_name: "DeepSeek-V3 (Mar 2025)",
        or_slug: "deepseek-v3-0324",
    },
    EciAlias {
        eci_name: "DeepSeek-R1 (May 2025)",
        or_slug: "deepseek-r1-0528",
    },
    EciAlias {
        eci_name: "Kimi K2 (Jul 2025)",
        or_slug: "kimi-k2",
    },
    EciAlias {
        eci_name: "Qwen3-235B-A22B-Instruct (Jul 2025)",
        or_slug: "qwen3-235b-a22b",
    },
    EciAlias {
        eci_name: "Qwen3-235B-A22B-Thinking (Jul 2025)",
        or_slug: "qwen3-235b-a22b-thinking",
    },
    EciAlias {
        eci_name: "Qwen3-30B-A3B-Instruct (Jul 2025)",
        or_slug: "qwen3-30b-a3b",
    },
    EciAlias {
        eci_name: "Qwen3-30B-A3B-Thinking (Jul 2025)",
        or_slug: "qwen3-30b-a3b-thinking",
    },
    EciAlias {
        eci_name: "Mistral Large 2 (Jul 2024)",
        or_slug: "mistral-large-2407",
    },
    EciAlias {
        eci_name: "Mistral Large 2 (Nov 2024)",
        or_slug: "mistral-large-2411",
    },
    EciAlias {
        eci_name: "Grok-2 (Dec 2024)",
        or_slug: "grok-2-1212",
    },
    // Hyphenation and intra-family word-order differences strict
    // canonicalization cannot close.
    EciAlias {
        eci_name: "Grok 4 Fast",
        or_slug: "grok-4-fast",
    },
    EciAlias {
        eci_name: "Grok-3 mini",
        or_slug: "grok-3-mini",
    },
    EciAlias {
        eci_name: "o3-pro",
        or_slug: "o3-pro",
    },
    EciAlias {
        eci_name: "Command R+",
        or_slug: "command-r-plus",
    },
    EciAlias {
        eci_name: "DeepSeek-V3.2-Exp",
        or_slug: "deepseek-v3.2-exp",
    },
    EciAlias {
        eci_name: "DeepSeek-V3.1",
        or_slug: "deepseek-v3.1",
    },
    EciAlias {
        eci_name: "DeepSeek-R1-Distill-Qwen-14B",
        or_slug: "deepseek-r1-distill-qwen-14b",
    },
    EciAlias {
        eci_name: "DeepSeek-R1-Distill-Qwen-32B",
        or_slug: "deepseek-r1-distill-qwen-32b",
    },
    // Suffix remnant rows: names that differ by more than separators.
    EciAlias {
        eci_name: "DeepSeek V4.1 Flash",
        or_slug: "deepseek-v4.1-flash",
    },
    EciAlias {
        eci_name: "Kimi K2 Thinking",
        or_slug: "kimi-k2-thinking",
    },
    EciAlias {
        eci_name: "GLM-5.3-Flash",
        or_slug: "glm-5.3-flash",
    },
];

/// Resolves an OpenRouter `author/slug[:variant]` listing id to an ECI
/// display name: canonicalized strict match against the index's keys
/// first, then the alias table.
///
/// The variant suffix is stripped before lookup, so a match for the base
/// listing serves its `:free`/`:thinking` twins; the `author/` prefix is
/// accepted but never helps (Epoch's names do not carry one).
#[must_use]
pub fn resolve_eci<'a>(or_id: &str, index: &'a EciIndex) -> Option<&'a str> {
    let base = strip_variant(or_id);
    let slug = base.rsplit('/').next().unwrap_or(base);
    let key = crate::eci::canonical_name(slug);
    let strict = index
        .scores
        .keys()
        .find(|name| crate::eci::canonical_name(name) == key);
    if let Some(name) = strict {
        return Some(name.as_str());
    }
    ECI_ALIASES
        .iter()
        .find(|alias| crate::eci::canonical_name(alias.or_slug) == key)
        .map(|alias| alias.eci_name)
}

/// Strips a trailing `:variant` suffix (`:free`, `:thinking`).
fn strip_variant(or_id: &str) -> &str {
    match or_id.split_once(':') {
        Some((base, _)) => base,
        None => or_id,
    }
}

#[cfg(test)]
#[path = "eci_aliases_tests.rs"]
mod eci_aliases_tests;
