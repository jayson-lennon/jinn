//! Session capsule: cap + view + extension traits, colocated.
//!
//! Write access to [`SessionMap`] (the active session and session collection) is
//! gated by an unforgeable ZST token ([`SessionCap`]). The projection method
//! [`State::with_session`] hands the cap-holder a narrow borrowed view
//! ([`SessionView`]).
//!
//! The session capsule is the largest and most multiply-owned in jinn.
//! `ChatSessionState` already has a well-encapsulated public method API
//! (`push_entry`, `set_cwd`, `set_model`, `begin_streaming`, ...); that API is
//! the legitimate multi-actor interface. The cap's job is to make those the
//! _only_ write path.

use crate::common::session_map::SessionMap;
use crate::common::state::State;
use crate::feat::ui::frontend_state::FrontendState;

// ── The cap ──────────────────────────────────────────────────────────────────

/// Proof of authority to write the session collection ([`SessionMap`]). Minted
/// only via [`crate::common::tcaps::mint`].
#[derive(Clone, Copy, Debug)]
pub struct SessionCap(());

impl SessionCap {
    /// Private constructor scoped to the `tcaps/` subtree.
    ///
    /// MUST be `pub(in crate::common::tcaps)`, NOT `pub(crate)`.
    pub(in crate::common::tcaps) fn new() -> Self {
        Self(())
    }
}

// ── Per-struct narrow newtypes ─��─────────────────────────────────────────────

/// Narrow write-handle to the session collection. The tuple field is PRIVATE.
pub struct SessionOps<'a>(&'a mut SessionMap);

impl SessionOps<'_> {
    /// Direct mutable access to the session collection.
    ///
    /// Exposed as `&mut` because session-writing actors own the whole map and
    /// `ChatSessionState`'s public method surface is already the capsule wall.
    pub fn map(&mut self) -> &mut SessionMap {
        self.0
    }
}

// ── Composite facades ────────────────────────��───────────────────────────────

/// What a session-writer sees: mutable access to the session collection.
///
/// Reads of other sub-structs are available via the full snapshot
/// (`State::read`) and do not appear here.
pub struct SessionView<'a> {
    /// Mutable session collection.
    pub session: SessionOps<'a>,
}

/// Combined write view for sidebar reconciliation, which interleaves
/// session and sidebar sections (cell) writes.
///
/// Used by `SidebarStateActor` and the session actor's `remove_and_replace`.
pub struct SessionSidebarView<'a> {
    /// Mutable session collection.
    pub session: SessionOps<'a>,
    /// Mutable frontend state (for sessions_section + pins).
    pub frontend: &'a mut FrontendState,
}

/// What the session actor sees for pin-management writes: mutable access to
/// the session collection and the frontend pins sub-struct (which tracks
/// pinned-entry display state and must stay in sync with session pin state).
pub struct SessionPinsView<'a> {
    /// Mutable session collection.
    pub session: SessionOps<'a>,
    /// Mutable frontend state (for `.pins` access).
    pub frontend: &'a mut FrontendState,
}

// ── Projection entry points ──────────────────────────────────────────────────

impl State {
    /// Write access to the session collection, scoped via [`SessionView`].
    pub fn with_session<R, F>(&self, _cap: &SessionCap, f: F) -> R
    where
        F: FnOnce(&mut SessionView<'_>) -> R,
    {
        let mut guard = self.write_lock();
        let app = &mut *guard;
        f(&mut SessionView {
            session: SessionOps(&mut app.session),
        })
    }

    /// Write access for sidebar reconciliation: session map + the sidebar sections cell.
    pub fn with_session_sidebar<R, F>(
        &self,
        _session_cap: &SessionCap,
        _frontend_cap: &crate::common::tcaps::FrontendCap,
        f: F,
    ) -> R
    where
        F: FnOnce(&mut SessionSidebarView<'_>) -> R,
    {
        let mut guard = self.write_lock();
        let app = &mut *guard;
        f(&mut SessionSidebarView {
            session: SessionOps(&mut app.session),
            frontend: &mut app.frontend,
        })
    }

    /// Write access for pin management: session map + the sidebar sections cell.
    pub fn with_session_pins<R, F>(
        &self,
        _session_cap: &SessionCap,
        _frontend_cap: &crate::common::tcaps::FrontendCap,
        f: F,
    ) -> R
    where
        F: FnOnce(&mut SessionPinsView<'_>) -> R,
    {
        let mut guard = self.write_lock();
        let app = &mut *guard;
        f(&mut SessionPinsView {
            session: SessionOps(&mut app.session),
            frontend: &mut app.frontend,
        })
    }
}
