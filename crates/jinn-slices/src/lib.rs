//! Slice vocabulary — typed cells, views, and the slots that name them.
//!
//! Today jinn's render state is one `AppState` guarded by a single
//! `RwLock`, with ~25 actors writing through TCaps tokens that gate
//! *where* in the struct an actor may write. [`Slices`] replaces that
//! convention with structure: each slice of render state (dashboard
//! status, quake bar, terminal screen, …) lives in its own typed cell,
//! `register` mints **exactly one** write handle for it, and everyone
//! else holds read handles. "Who can write this slice" becomes
//! grep-provable — find the handle, find the writer.
//!
//! Keys are dynamic strings ([`SlotKey`]), not an enum of known features,
//! so plugin-contributed slices are first-class residents: a WASM guest's
//! host-side coordinator can register a cell under the guest's namespace
//! exactly like a built-in feature does.
//!
//! Read access is not scarce; write access is.
//!
//! This crate is the future extraction seam for slice *features*: it
//! depends only on `jinn-theme` and `ratatui` (for the view layer) —
//! never on `jinn-domain`.

#![cfg_attr(
    test,
    allow(
        clippy::expect_used,
        clippy::panic,
        reason = "test assertions on infallible registration"
    )
)]

pub mod cell;
pub mod cwd_root;
pub mod fabric;
pub mod focus;
pub mod host;
pub mod line_input;
pub mod mode;
pub mod overlay;
pub mod picker_kind;
pub mod render_facts;
pub mod route;
pub mod scope_focus_state;
pub mod service_status;
pub mod sidebar_section_id;
pub mod slice_scope;
pub mod slices;
pub mod status_bar_state;
pub mod tui_signals;
pub mod view;

pub use cell::TypedCell;
pub use cwd_root::CwdRoot;
pub use fabric::ActorShutdownCompleted;
pub use fabric::ActorStarted;
pub use fabric::ActorStarting;
pub use focus::{FocusScope, ScopeStack};
pub use host::ConfigSectionError;
pub use host::Direction;
pub use host::SliceHost;
pub use line_input::LineInput;
pub use mode::Mode;
pub use overlay::OverlayViewFn;
pub use overlay::OverlayViews;
pub use picker_kind::PickerKind;
pub use render_facts::AppFact;
pub use render_facts::RenderFacts;
pub use scope_focus_state::ScopeFocusState;
pub use scope_focus_state::scope_focus_slot;
pub use sidebar_section_id::SidebarSectionId;
pub use status_bar_state::StatusBarState;
pub use status_bar_state::status_bar_slot;
pub use tui_signals::TuiSignals;

/// The slice host specialized to jinn's render facts — the spelling
/// slices use in their `activate` signatures instead of naming the
/// generic parameter everywhere.
pub type AppSliceHost<'a> = SliceHost<'a, RenderFacts>;
pub use route::ActionCtx;
pub use route::ActionFn;
pub use route::BindSite;
pub use route::BusMessage;
pub use route::DynamicIntent;
pub use route::EditIntent;
pub use route::InputHook;
pub use route::KeyRoutes;
pub use route::PublishClosure;
pub use route::RouteId;
pub use route::RouteOutcome;
pub use route::RouteResult;
pub use route::RouteRow;
pub use route::ScopeSignal;
pub use route::SliceActionState;
pub use service_status::ServiceStatusUpdate;
pub use slice_scope::SliceScopeId;
pub use slices::Slices;
pub use slices::SlotKey;
pub use slices::SlotTaken;
pub use view::SliceView;
pub use view::ViewCx;

/// Implements [`trouper::schema::Schema`] for a crossing message type.
///
/// `name` mirrors the Rust type name so trouper exports read the same on
/// both sides of the bridge; all crossing schemas are version 1.
#[macro_export]
macro_rules! crossing_schema {
    ($ty:ty, $name:literal, $kind:expr, description: $desc:literal, fields: [$($field:literal => $fty:expr),* $(,)?]) => {
        impl ::trouper::schema::Schema for $ty {
            fn schema_def() -> ::trouper::schema::SchemaDef {
                ::trouper::schema::SchemaDef {
                    name: $name.to_owned(),
                    version: 1,
                    kind: $kind,
                    fields: vec![$(::trouper::schema::FieldDef::required($field, $fty)),*],
                    description: Some($desc.to_owned()),
                }
            }
        }
    };
}
