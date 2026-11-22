//! Shared test utilities for jinn TUI rendering tests, actor-fabric
//! harnesses, and temp-directory creation.
//!
//! The modules here are the *sanctioned* surface for the questions a test
//! asks about the platform:
//!
//! - [`temp`] — every scratch directory is created through these wrappers, so
//!   it carries the `jinn-test-` prefix and a crashed run leaves greppable
//!   residue.
//! - [`paths`] — typed path construction, so a test cannot build a
//!   drive-relative or mixed-separator path.
//! - [`platform`] — shell resolution, process liveness, and orphan scanning,
//!   with the platform differences handled in one place.
//! - [`wait`] — deterministic waits, so a test does not sleep for a fixed
//!   interval and hope.

pub mod barrier;
pub mod bus_harness;
pub mod paths;
pub mod platform;
pub mod temp;
pub mod wait;

pub use barrier::{Barrier, BarrierAck, DEFAULT_BARRIER_TIMEOUT};
pub use paths::{TestRoot, canonicalize_display, strip_verbatim_prefix, volume_root};
pub use platform::{find_orphans, find_orphans_in_group, is_alive, rg_required, shell_required};
pub use temp::{
    JinnTempDir, TEST_PREFIX, TestTempBuilder, builder, retained_tempdir, tempdir, tempdir_in,
    tempdir_with_prefix,
};
pub use wait::{await_completion_then_assert_absent, poll_until, read_normalized};

// The typed path surface is re-exported so a test crate depends on
// `jinn-testutil` — which it already does — rather than on `marked-path`
// directly. One dependency means one version, so the types compose across
// crate boundaries instead of failing to unify.
pub use marked_path::{
    Absolute, CanonicalPath, MarkedPath, MarkedPathAccess, MarkedPathBuf, PathError, Relative,
};

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;

/// An empty slice registry, for a test that needs the type but no cells.
///
/// The registry is the seam a slice's render path reads its cells through.
/// A test that exercises a handler rather than a render path needs one to
/// hand to the handler, and needs it to hold nothing.
#[must_use]
pub fn empty_slices() -> jinn_slices::Slices {
    jinn_slices::Slices::new()
}

/// An empty route table, for a test that needs the type but no rows.
///
/// The table is what a slice's keybinds are looked up in. A test that
/// exercises a handler directly rather than through a key press needs one
/// to hand to the handler, and needs it to bind nothing.
#[must_use]
pub fn empty_routes() -> jinn_slices::route::KeyRoutes {
    jinn_slices::route::KeyRoutes::new()
}

/// Creates a test terminal and full-area rect for the given dimensions.
///
/// # Panics
///
/// Panics if `Terminal::new` fails, which should only happen with zero-sized dimensions.
pub fn setup_term(width: u16, height: u16) -> (Terminal<TestBackend>, Rect) {
    let backend = TestBackend::new(width, height);
    let terminal = Terminal::new(backend).unwrap();
    let area = Rect::new(0, 0, width, height);
    (terminal, area)
}

/// Extracts a single row from a ratatui buffer as a `String`.
pub fn buffer_row(buffer: &ratatui::buffer::Buffer, y: u16, width: u16) -> String {
    (0..width)
        .filter_map(|x| buffer.cell((x, y)).map(ratatui::buffer::Cell::symbol))
        .collect()
}

/// Extracts all rows from a ratatui buffer as `String`s.
pub fn buffer_rows(buffer: &ratatui::buffer::Buffer, width: u16, height: u16) -> Vec<String> {
    (0..height).map(|y| buffer_row(buffer, y, width)).collect()
}

/// A JSON null, for a test-built value whose shape no behavior reads.
///
/// A `ToolDefinition`'s schema is one: the tests that build one care that the
/// tool is *named* and registered, never that its parameters validate, and
/// `serde_json::Value::Null` says that without each test hand-rolling a
/// `json!({})` it does not mean.
#[must_use]
pub fn json_value() -> serde_json::Value {
    serde_json::Value::Null
}

// ---------------------------------------------------------------------------
// Test fabric: a real trouper system, no kernel Services.
// ---------------------------------------------------------------------------

/// A minimal test harness over a real trouper `ActorSystem`, with the
/// spawn/send surface slice crates actually use. Slice-crate tests spin
/// this up instead of the kernel's `Services`, which they must not
/// depend on.
///
/// Not `Default` — construction creates an actor system on the ambient
/// runtime, which is a deliberate, visible action (`new`), not a value
/// default.
pub struct TestFabric {
    /// The trouper system.
    system: trouper::system::ActorSystem,
}

impl TestFabric {
    /// The trouper system, for spawning actors under test.
    #[must_use]
    pub fn system(&self) -> &trouper::system::ActorSystem {
        &self.system
    }

    /// Creates a fresh trouper system.
    #[expect(
        clippy::new_without_default,
        reason = "construction creates an actor system on the ambient runtime — an action, not a value default"
    )]
    pub fn new() -> Self {
        let system = trouper::system::ActorSystem::new(trouper::system::SystemConfig::production());
        Self { system }
    }

    /// Publishes a typed message onto the fabric (schema broadcast).
    pub async fn send_to_topic<
        M: trouper::schema::Schema
            + serde::Serialize
            + Clone
            + Send
            + Sync
            + trouper::envelope::PayloadValue,
    >(
        &self,
        msg: M,
    ) {
        self.system.publish(msg).await;
    }
}
