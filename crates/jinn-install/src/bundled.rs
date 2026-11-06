//! The bundled-resource catalogue.
//!
//! The whole `res/` tree is embedded at compile time by [`include_dir!`], so
//! what `jinn install` ships is *derived* from the tree's shape rather than
//! hand-written. Dropping a file into `res/` ships it; nothing has to be
//! registered anywhere.
//!
//! A resource's **kind** — and therefore its destination root — comes from its
//! top-level directory under `res/` (`themes/`, `personas/`, `prompts/`,
//! `skills/`). Anything beneath that directory is preserved in the destination
//! path, so a nested structure like `skills/<name>/references/<file>.md`
//! installs as itself.
//!
//! A file whose top-level directory is none of the four is a hard error, not a
//! silent skip: silently skipping is the failure this module exists to
//! eliminate.

use std::path::Path;

use marked_path::{Absolute, MarkedPath, MarkedPathBuf, PathError, Relative};

use error_stack::{Report, ResultExt};
use include_dir::{Dir, DirEntry, File, include_dir};
use wherror::Error;

use crate::{Destinations, InstallError};

/// Every file under `res/`, embedded at compile time.
///
/// `include_dir!` expands to one `include_bytes!` per file, so *editing* a
/// resource re-triggers a rebuild on its own. *Adding*, moving, or deleting one
/// does not — `build.rs` declares `rerun-if-changed` for the whole tree to
/// close that gap.
static RESOURCES: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/../../res");

/// Where a bundled resource should be installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A theme, installed to the themes root.
    Theme,
    /// A persona, installed to the personas root.
    Persona,
    /// A prompt template, installed to the prompts root.
    Prompt,
    /// A workflow (one directory per workflow), installed to the workflows
    /// root — the directory workflow discovery scans.
    Workflow,
    /// A skill, installed to the skills root.
    Skill,
}

impl Kind {
    /// The top-level directory under `res/` that selects this kind.
    pub(crate) const fn dir_name(self) -> &'static str {
        match self {
            Kind::Theme => "themes",
            Kind::Persona => "personas",
            Kind::Prompt => "prompts",
            Kind::Workflow => "workflows",
            Kind::Skill => "skills",
        }
    }

    /// Resolves this kind to its destination root within `destinations`.
    pub(crate) fn root(self, destinations: &Destinations) -> MarkedPath<'_, Absolute> {
        match self {
            Kind::Theme => destinations.themes(),
            Kind::Persona => destinations.personas(),
            Kind::Prompt => destinations.prompts(),
            Kind::Workflow => destinations.workflows(),
            Kind::Skill => destinations.skills(),
        }
    }

    /// This kind's position in the install order: themes, then personas, then
    /// prompts, then workflows, then skills.
    const fn rank(self) -> u8 {
        match self {
            Kind::Theme => 0,
            Kind::Persona => 1,
            Kind::Prompt => 2,
            Kind::Workflow => 3,
            Kind::Skill => 4,
        }
    }
}

/// One bundled resource: its kind, its path relative to that kind's
/// destination root, and its compile-time-embedded payload.
#[derive(Debug)]
pub(crate) struct Bundled {
    /// Selects the destination root; see [`Kind`].
    kind: Kind,
    /// Path relative to the destination root, with any nesting beneath the
    /// kind directory preserved (e.g. `default.toml`,
    /// `jinn-usage/references/keybindings.md`).
    relative: MarkedPathBuf<Relative>,
    /// The embedded payload, written verbatim.
    contents: &'static [u8],
}

impl Bundled {
    /// The embedded payload, written verbatim.
    pub(crate) fn contents(&self) -> &'static [u8] {
        self.contents
    }

    /// The path relative to the `res/` root — the manifest's key form and
    /// the reconciler's expected-path form (e.g. `skills/canvas/SKILL.md`).
    pub(crate) fn res_relative(&self) -> Result<MarkedPathBuf<Relative>, PathError> {
        MarkedPathBuf::<Relative>::new(self.kind.dir_name())?.join(&self.relative)
    }
}

/// A resource's top-level directory under `res/` named no known kind, so it has
/// no destination to install into.
#[derive(Debug, Error)]
#[error(debug)]
struct UnknownKind;

/// Every embedded resource, in install order.
///
/// Ordering is by kind (themes, personas, prompts, skills) and then by
/// ascending path within a kind, so the sequence is deterministic and stable
/// across runs over the same tree.
///
/// # Errors
///
/// Returns [`Report<InstallError>`] if any embedded file sits under a
/// top-level directory that names no kind.
pub(crate) fn bundled_catalogue() -> Result<Vec<Bundled>, Report<InstallError>> {
    let mut catalogue = Vec::new();
    collect_files(&RESOURCES, &mut catalogue)?;

    catalogue.sort_by(|a, b| (a.kind.rank(), &a.relative).cmp(&(b.kind.rank(), &b.relative)));

    Ok(catalogue)
}

/// Walks `dir` recursively, appending one [`Bundled`] per embedded file.
///
/// Embedded paths are already relative to the root passed to `include_dir!` —
/// the root itself contributes an empty prefix — so a file's destination comes
/// straight from [`File::path`] and no prefix is accumulated along the way.
fn collect_files(
    dir: &'static Dir<'static>,
    catalogue: &mut Vec<Bundled>,
) -> Result<(), Report<InstallError>> {
    for entry in dir.entries() {
        match entry {
            DirEntry::Dir(nested) => collect_files(nested, catalogue)?,
            DirEntry::File(file) => catalogue.push(resource(file)?),
        }
    }

    Ok(())
}

/// Builds one [`Bundled`] from an embedded file, splitting its path into the
/// kind-selecting top-level directory and the destination-relative remainder.
///
/// [`File::path`] is already rooted at the directory handed to `include_dir!`
/// and the embedded bytes are `'static`, so the destination comes straight
/// from the file — no path prefix is accumulated along the walk.
fn resource(file: &'static File<'static>) -> Result<Bundled, Report<InstallError>> {
    let embedded = file.path();
    let (kind, relative) = split_kind(embedded)
        .change_context(InstallError)
        .attach("bundled resource has no destination")
        .attach(format!("path: {}", embedded.display()))?;

    let relative = MarkedPathBuf::<Relative>::new(relative)
        .change_context(InstallError)
        .attach("bundled resource path is not a valid relative path")
        .attach(format!("path: {}", embedded.display()))?;

    Ok(Bundled {
        kind,
        relative,
        contents: file.contents(),
    })
}

/// Splits a `res/`-relative path into its kind and the path relative to that
/// kind's destination root.
///
/// Matching is component-wise, so `themes-extra/x` does not match the `themes`
/// kind; neither does a file sitting directly in `res/`, which has no top-level
/// directory to name a kind.
fn split_kind(path: &Path) -> Result<(Kind, &Path), UnknownKind> {
    const KINDS: [Kind; 5] = [
        Kind::Theme,
        Kind::Persona,
        Kind::Prompt,
        Kind::Workflow,
        Kind::Skill,
    ];

    KINDS
        .iter()
        .find_map(|kind| {
            path.strip_prefix(kind.dir_name())
                .ok()
                .map(|rest| (*kind, rest))
        })
        .ok_or(UnknownKind)
}

/// Splits a `res/`-relative path into its kind and destination-relative
/// remainder, for callers that hold a plain path rather than an embedded
/// file ([`split_kind`] is the fallible form used by the catalogue walk).
pub(crate) fn res_relative_parts(path: &Path) -> Option<(Kind, &Path)> {
    split_kind(path).ok()
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unwrap_used,
        reason = "test code"
    )]

    use super::*;
    // `as_path()` on the catalogue's typed relative paths is test-only.
    use marked_path::MarkedPathAccess;

    #[rstest::rstest]
    #[test]
    fn embedded_tree_is_non_empty_and_kind_covered() {
        // Given the embedded catalogue.

        // When deriving the catalogue.
        let catalogue = bundled_catalogue().expect("derive catalogue");

        // Then it carries every kind with at least one file, so a missing
        // `res/` subtree at build time cannot ship a partial bundle.
        // (Membership against the repo's on-disk tree is deliberately not
        // asserted here: `res/` is content, not test fixture data.)
        for kind in [
            Kind::Theme,
            Kind::Persona,
            Kind::Prompt,
            Kind::Workflow,
            Kind::Skill,
        ] {
            let count = catalogue.iter().filter(|b| b.kind == kind).count();
            assert!(count > 0, "kind {kind:?} ships no files");
        }
    }

    #[rstest::rstest]
    #[test]
    fn catalogue_is_ordered_by_kind_then_path() {
        // Given the derived catalogue.

        // When deriving the catalogue.
        let catalogue = bundled_catalogue().expect("derive catalogue");

        // Then its (kind, path) pairs are non-decreasing.
        let keys: Vec<_> = catalogue
            .iter()
            .map(|b| (b.kind.rank(), &b.relative))
            .collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted, "catalogue must be sorted by kind then path");
    }

    #[rstest::rstest]
    #[test]
    fn catalogue_order_is_stable_across_runs() {
        // Given the catalogue derived once.
        let first = bundled_catalogue().expect("derive catalogue");

        // When deriving it again.
        let second = bundled_catalogue().expect("derive catalogue");

        // Then both derivations agree, entry for entry.
        let order = |catalogue: &[Bundled]| -> Vec<(Kind, MarkedPathBuf<Relative>)> {
            catalogue
                .iter()
                .map(|b| (b.kind, b.relative.clone()))
                .collect()
        };
        assert_eq!(order(&first), order(&second));
    }

    #[rstest::rstest]
    #[case("themes/default.toml", Kind::Theme, "default.toml")]
    #[case("personas/general.md", Kind::Persona, "general.md")]
    #[case("prompts/plan.md", Kind::Prompt, "plan.md")]
    fn top_level_directory_selects_the_kind_and_relative_path(
        #[case] embedded: &str,
        #[case] kind: Kind,
        #[case] relative: &str,
    ) {
        // Given a `res/`-relative path.
        // When splitting it.
        let (resolved, rest) = split_kind(Path::new(embedded)).expect("known kind");

        // Then the top-level directory named the kind and the remainder is the
        // destination-relative path.
        assert_eq!(resolved, kind);
        assert_eq!(rest, Path::new(relative));
    }

    #[rstest::rstest]
    #[case("prompts-extra/thing.md")]
    #[case("README.md")]
    fn unmappable_top_level_directory_is_an_error(#[case] path: &str) {
        // Given a path whose top-level directory names no kind.
        // When splitting it.
        let result = split_kind(Path::new(path));

        // Then it is an error, never a silent skip.
        assert!(result.is_err(), "{path} should not resolve to a kind");
    }

    /// Pins the singleton-flag documentation: the claim that `singleton`
    /// applies only to `local_http` servers was verified wrong (a stdio
    /// singleton shares one child across sessions), and the shipped
    /// `[mcp.overgraph]` entry relies on that. If this test fails, someone
    /// reintroduced the stale claim into a shipped doc.
    #[rstest::rstest]
    #[test]
    fn shipped_docs_never_claim_singleton_is_local_http_only() {
        // Given the bundled resources and the shipped jinn.toml template.
        let catalogue = bundled_catalogue().expect("derive catalogue");
        let skills = catalogue
            .iter()
            .filter(|b| b.kind == Kind::Skill && b.relative.as_path().ends_with("mcp-servers.md"))
            .collect::<Vec<_>>();

        // When reading the shipped reference text.
        let reference = skills
            .iter()
            .find_map(|b| {
                RESOURCES
                    .get_file(format!("skills/{}", b.relative))
                    .map(|f| String::from_utf8_lossy(f.contents()).into_owned())
            })
            .expect("mcp-servers.md ships as a bundled skill reference");

        // Then the reference ships, and it carries no transport-restricted
        // singleton claim anywhere in its text.
        assert!(
            !reference.contains("only applies for `local_http`"),
            "stale singleton claim reintroduced into mcp-servers.md"
        );
    }

    /// Pins the canvas offline guarantee (invariant 6): the shipped canvas
    /// skill and the slice's embedded template and stylesheet must name no
    /// fetchable remote resource. XML namespace URLs are identifiers, not
    /// fetches, so `http://www.w3.org/...` shapes are excluded — see the
    /// trap in the canvas contract.
    #[rstest::rstest]
    #[test]
    fn shipped_canvas_files_name_no_remote_resources() {
        // Given every shipped text file the canvas feature authors.
        let catalogue = bundled_catalogue().expect("derive catalogue");
        let skill_texts = catalogue
            .iter()
            .filter(|b| b.kind == Kind::Skill && b.relative.as_path().starts_with("canvas/"))
            .filter_map(|b| {
                RESOURCES
                    .get_file(format!("skills/{}", b.relative))
                    .map(|f| String::from_utf8_lossy(f.contents()).into_owned())
            })
            .collect::<Vec<_>>();
        assert!(!skill_texts.is_empty(), "canvas skill must ship");
        let mut texts = skill_texts;
        // The artifact skeleton is now itself a shipped skill reference:
        // scan it with the rest of the skill tree.
        texts.push(
            RESOURCES
                .get_file("skills/canvas/references/artifact-template.md")
                .map(|f| String::from_utf8_lossy(f.contents()).into_owned())
                .expect("the canvas artifact template ships as a skill reference"),
        );
        texts
            .push(String::from_utf8_lossy(jinn_canvas::vendor::authored_css_source()).into_owned());

        // When scanning each text for fetch-shaped remote URLs.
        // (xmlns/xlink namespace identifiers are excluded by their shapes.)
        let violations: Vec<String> = texts
            .iter()
            .flat_map(|text| remote_resource_urls(text))
            .collect();

        // Then no shipped canvas text names one.
        assert!(
            violations.is_empty(),
            "canvas shipped text names a remote resource: {violations:?}"
        );
    }

    /// Collects http(s) URLs in src/href/fetch-shaped contexts, skipping
    /// XML namespace identifiers (`xmlns="…"`, `xlink:href="http://www.w3.org/…"`).
    fn remote_resource_urls(text: &str) -> Vec<String> {
        let occurrences = text
            .match_indices("http://")
            .chain(text.match_indices("https://"));
        occurrences
            .filter_map(|(idx, needle)| {
                let before = text.get(..idx)?;
                // XML namespace identifiers are names, not fetches;
                // template comments mention URL shapes only as prose.
                let namespace_context = before.ends_with("xmlns=")
                    || before.ends_with("xmlns:")
                    || before.ends_with("xlink:href=\"")
                    || before.trim_end().ends_with("<!--");
                if namespace_context {
                    return None;
                }
                let after = text.get(idx..)?;
                let end = after.find(['"', ')', ' ', '\n']).unwrap_or(after.len());
                let url = after.get(..end)?;
                (needle.starts_with("http")).then(|| url.to_owned())
            })
            .collect()
    }
}
