//! The region registry — the unit of parallel work in the web UI.
//!
//! A region is one independently editable slice of the page: sessions,
//! transcript, composer, model picker. Each owns up to five parts — its
//! server render, its GET pull route, its SSE event, its client swap
//! (the TypeScript module half), and its settle convergence (which
//! events re-render it when a turn lands). The registry is what makes
//! two agents' region assignments disjoint: the region module is the
//! only file a region change touches, and [`regions`] is the single
//! list a new region appends one line to.
//!
//! The trait is the server half of the region contract. Render
//! functions live in the region modules; the fan-out asks the registry
//! which fragments a settle needs (by [`WebRegion::settle_kinds`])
//! instead of naming regions itself, and the router folds the pull
//! routes out of [`WebRegion::pull`] — so adding a region never edits
//! `fanout.rs` or `server.rs`.

use axum::routing::MethodRouter;
use jinn_kernel::common::state::StateReadGuard;
use jinn_webui_msg::SseKind;

pub use crate::regions::composer;
pub use crate::regions::menu;
pub use crate::regions::model_picker;
pub use crate::regions::sessions;
pub use crate::regions::transcript;

/// One region's declared parts: which of the five it carries.
///
/// The composer legitimately has no GET pull route (its swaps ride
/// the POST responses) and the picker legitimately has no SSE event
/// (its swaps ride the POST response only) — so the conformance gate
/// asserts **declared** parts, never a blind all-five. A region
/// declaring a part it does not implement fails the gate, as does
/// implementing a part it does not declare.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegionParts {
    /// The region renders from a state guard — the fan-out's settle
    /// and refresh pushes name it.
    pub settle_render: bool,
    /// The region owns a GET fragment route a page can pull.
    pub pull_route: bool,
    /// The region owns at least one SSE event kind.
    pub sse_event: bool,
    /// The region fills one of the page shell's inline regions (a
    /// page render asks the region for its fragment).
    pub page_fill: bool,
}

/// One region's GET pull binding: the route pattern it serves and the
/// handler that answers it. Returned by [`WebRegion::pull`]; the
/// router folds these straight in.
pub struct RegionPull {
    /// The route pattern, spelled by the contract.
    pub pattern: &'static str,
    /// The region's `GET` handler over the shared server state.
    pub handler: MethodRouter<crate::server::ServerShared>,
}

/// One independently editable region of the web page.
///
/// Implementations live beside the trait, one module per region, each
/// a zero-sized type. The trait is the *declaration*: what the region
/// is named, which parts it carries, which SSE kinds it owns, and how
/// to render its fragment. The HTTP handlers and the fan-out consume
/// the registry — neither names a region directly.
pub trait WebRegion: Sync + 'static {
    /// The region's stable id ("sessions", "transcript", …). Also the
    /// client region module's filename stem.
    fn id(&self) -> &'static str;

    /// The parts this region declares. The conformance gate asserts
    /// every declared part exists.
    fn parts(&self) -> RegionParts;

    /// The SSE event kinds this region owns — empty when the region
    /// swaps only from POST responses (`sse_event: false`).
    ///
    /// One owner per kind: the fan-out resolves a kind's fragment by
    /// asking which region owns it, so two regions claiming one kind
    /// would make those pushes ambiguous. The conformance gate
    /// enforces single ownership.
    fn event_kinds(&self) -> &'static [SseKind];

    /// The event kinds a settle-to-idle pushes for this region, in
    /// push order. Guard-rendered kinds only — `Chat` never appears
    /// (the partial token buffer flushes separately, before the
    /// settle, and its cursor is not zero).
    fn settle_kinds(&self) -> &'static [SseKind] {
        &[]
    }

    /// The region's GET pull binding — `None` when the region has no
    /// pull route (`pull_route: false`).
    fn pull(&self) -> Option<RegionPull>;

    /// The page-shell container id this region swaps into, from the
    /// contract — `None` when the region fills nothing (`page_fill:
    /// false`).
    fn container_id(&self) -> Option<&'static str>;

    /// The region's stylesheet fragment, joined into the page shell
    /// after the base chrome in registry order. Empty by default.
    ///
    /// Order is the tie-breaker: equal-specificity rules resolve last
    /// wins, so a region may only override the base or an *earlier*
    /// region — never a later one.
    fn css(&self) -> &'static str {
        ""
    }

    /// Renders the region's fragment from a state guard — the fan-out
    /// settle and refresh shape. Session-scoped regions use the id;
    /// page-global regions (sessions) ignore it.
    ///
    /// A render failure degrades to a blank fragment (logged), never
    /// an error — a pushed fragment that fails must not sever the
    /// stream carrying the others.
    fn render(&self, session_id: &jinn_core_types::SessionId, guard: &StateReadGuard<'_>)
    -> String;
}

/// The regions, in registration order.
///
/// Registration order is behavior: the fan-out's settle iterates this
/// order, and the CSS join order breaks equal-specificity ties (last
/// rule wins). Prepend nothing; append new regions at the tail.
pub fn regions() -> &'static [&'static dyn WebRegion] {
    &[
        &sessions::SessionsRegion,
        &transcript::TranscriptRegion,
        &composer::ComposerRegion,
        &model_picker::ModelPickerRegion,
        // The menu appends at the tail: registration order is the CSS
        // join's tie-breaker, so a new region never overrides an
        // established one's rules (Invariant: append, never reorder).
        &menu::MenuRegion,
        // The new-session modal appends after the menu: same rule —
        // the tail region's CSS wins ties, and the modal must be able
        // to overlay whatever the established regions drew.
        &crate::regions::session_new::SessionNewRegion,
        // The persona modal appends at the tail like every region
        // before it: its overlay CSS may need to win a tie against
        // the modals that registered earlier.
        &crate::regions::persona_picker::PersonaRegion,
        // The MCP servers modal appends at the tail like every region
        // before it: same rule — its overlay CSS may need to win a
        // tie against the modals that registered earlier.
        &crate::regions::mcp_servers::McpServersRegion,
        // The tools modal appends at the tail like every region before
        // it: same rule — its overlay CSS may need to win a tie
        // against the modals that registered earlier.
        &crate::regions::tools::ToolsRegion,
    ]
}

/// The region that owns an SSE kind — the fan-out's push resolution.
/// The first declarant wins; [`check_region`] keeps that list at
/// exactly one.
pub(crate) fn owner_of(kind: SseKind) -> Option<&'static dyn WebRegion> {
    regions()
        .iter()
        .copied()
        .find(|region| region.event_kinds().contains(&kind))
}

/// Why a region fails the conformance gate — one gap, named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegionGap {
    /// `settle_render: true` but the region owns no SSE kind a settle
    /// could push (a settle renders through the fan-out, which only
    /// speaks SSE).
    SettleWithoutEvent {
        /// The region that declared the impossible combination.
        region: &'static str,
    },
    /// `pull_route: true` but [`WebRegion::pull`] returns `None`.
    RouteWithoutBinding {
        /// The region that declared the missing binding.
        region: &'static str,
    },
    /// `pull_route: false` but [`WebRegion::pull`] returns `Some`.
    BindingWithoutRoute {
        /// The region that declared the extra binding.
        region: &'static str,
    },
    /// `sse_event: true` but [`WebRegion::event_kinds`] is empty.
    EventDeclaredNoneOwned {
        /// The region that declared the empty set.
        region: &'static str,
    },
    /// `page_fill: true` but [`WebRegion::container_id`] is `None`.
    FillWithoutContainer {
        /// The region that declared the missing container.
        region: &'static str,
    },
    /// The region's id is not unique in the registry — the client
    /// module pairing and the owner lookup both key on it.
    DuplicateId {
        /// The duplicated id.
        id: &'static str,
    },
    /// Two regions claim the same SSE kind — the fan-out's
    /// [`owner_of`] resolution would be ambiguous.
    SharedEventKind {
        /// The contested kind's event name.
        event: String,
    },
    /// A settle kind names an SSE kind no region owns (the push
    /// would render through nothing).
    SettleKindUnowned {
        /// The orphaned kind's event name.
        event: String,
    },
}

impl std::fmt::Display for RegionGap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SettleWithoutEvent { region } => write!(
                f,
                "region `{region}` declares settle_render but owns no SSE kind"
            ),
            Self::RouteWithoutBinding { region } => write!(
                f,
                "region `{region}` declares pull_route but binds no pull handler"
            ),
            Self::BindingWithoutRoute { region } => write!(
                f,
                "region `{region}` binds a pull handler without declaring pull_route"
            ),
            Self::EventDeclaredNoneOwned { region } => write!(
                f,
                "region `{region}` declares sse_event but owns no SSE kind"
            ),
            Self::FillWithoutContainer { region } => write!(
                f,
                "region `{region}` declares page_fill but names no container id"
            ),
            Self::DuplicateId { id } => {
                write!(f, "region id `{id}` is registered more than once")
            }
            Self::SharedEventKind { event } => {
                write!(f, "SSE kind `{event}` is claimed by more than one region")
            }
            Self::SettleKindUnowned { event } => write!(
                f,
                "settle kind `{event}` has no owning region — the settle would push nothing"
            ),
        }
    }
}

/// The region conformance gate: every **declared** part must exist.
///
/// Deliberately declaration-shaped, not all-five-shaped: the composer
/// legitimately has no GET pull route and the picker legitimately has
/// no SSE event — a blind all-five assert would fail honest regions
/// and train the next contributor to stop declaring parts. A plain
/// function (not a test) so the negative tests can drive it directly.
///
/// # Errors
///
/// Returns every gap found, in a stable order; an empty vec is a
/// conforming registry.
pub fn check_region(region: &dyn WebRegion) -> Vec<RegionGap> {
    let mut gaps = Vec::new();
    let id = region.id();
    let parts = region.parts();

    if parts.settle_render && region.event_kinds().is_empty() {
        gaps.push(RegionGap::SettleWithoutEvent { region: id });
    }
    match (parts.pull_route, region.pull().is_some()) {
        (true, false) => gaps.push(RegionGap::RouteWithoutBinding { region: id }),
        (false, true) => gaps.push(RegionGap::BindingWithoutRoute { region: id }),
        _ => {}
    }
    if parts.sse_event && region.event_kinds().is_empty() {
        gaps.push(RegionGap::EventDeclaredNoneOwned { region: id });
    }
    if !parts.sse_event && !region.event_kinds().is_empty() {
        // Owning kinds while disclaiming the part is the same lie in
        // reverse: the fan-out would push for a region that claims it
        // has no SSE surface.
        gaps.push(RegionGap::EventDeclaredNoneOwned { region: id });
    }
    match (parts.page_fill, region.container_id()) {
        (true, None) => gaps.push(RegionGap::FillWithoutContainer { region: id }),
        _ => {}
    }
    gaps
}

/// The whole-registry gate: id uniqueness, SSE-kind single ownership,
/// and settle-kind ownership, on top of every region's own
/// [`check_region`].
///
/// # Errors
///
/// Returns every gap found; an empty vec is a conforming registry.
pub fn check_registry() -> Vec<RegionGap> {
    let mut gaps = Vec::new();
    let registry = regions();

    let mut seen_ids: Vec<&str> = Vec::new();
    for region in registry {
        if seen_ids.contains(&region.id()) {
            gaps.push(RegionGap::DuplicateId { id: region.id() });
        }
        seen_ids.push(region.id());
        gaps.extend(check_region(*region));
    }

    // SSE kinds: exactly one owner each, and every settle kind owned.
    let mut owners: Vec<(SseKind, &'static str)> = Vec::new();
    for region in registry {
        for kind in region.event_kinds() {
            if owners.iter().any(|(k, _)| k == kind) {
                gaps.push(RegionGap::SharedEventKind {
                    event: kind.event_name().to_owned(),
                });
            } else {
                owners.push((*kind, region.id()));
            }
        }
    }
    for region in registry {
        for kind in region.settle_kinds() {
            if !owners.iter().any(|(k, _)| k == kind) {
                gaps.push(RegionGap::SettleKindUnowned {
                    event: kind.event_name().to_owned(),
                });
            }
        }
    }
    gaps
}
