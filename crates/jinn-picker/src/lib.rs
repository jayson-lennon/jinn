//! Generic picker framework — behavior-described pickers, built once.
//!
//! A picker is a [`PickerSpec`]: a value built with the builder pattern that
//! owns every dimension of the picker's behavior — its entries, how each row
//! and preview render, which keys do what, what the custom status line says,
//! and what happens on open/confirm/close. Composition registers built specs
//! in a [`PickerRegistry`]; the keymap generator, the keybind footer line, and
//! the viewport geometry all derive from the spec's data, so the three cannot
//! drift.
//!
//! The crate depends on `jinn-selection-widget` (the state machines and
//! widgets), `jinn-slices` (the publish-closure message shape), and
//! `jinn-core-types` — never on any kernel crate. Domain-authored specs
//! capture what they need at builder time and hand back [`PickerOutcome`]s
//! carrying publish closures, exactly like slice route actions.

pub mod builder;
pub mod ctx;
pub mod entry;
pub mod hooks;
pub mod host;
pub mod id;
pub mod outcome;
pub mod preview_key;
pub mod registry;
pub mod render;
pub mod scroll;
pub mod widget;

#[cfg(test)]
mod test_host;

pub use builder::PickerSpec;
pub use ctx::{ActionCtx, LoadCtx, PreviewCtx, RowCtx, StatusCtx};
pub use entry::PickerEntry;
pub use hooks::{
    PickerBindAction, PickerLifecycleFn, PickerLoadFn, PickerPreviewFn, PickerPreviewKeyFn,
    PickerRowFn, PickerSearchFn, PickerSelectionChangeFn, PickerStatusFn,
};
pub use host::{Palette, PickerHost, SharedPreviewCache};
pub use id::PickerId;
pub use outcome::PickerOutcome;
pub use preview_key::PreviewKey;
pub use registry::BindRow;
pub use registry::ErasedPickerSpec;
pub use registry::PickerRegistry;
pub use registry::SpecHandle;
pub use registry::Tail;
pub use render::KeybindLine;
pub use render::keybind_line;
pub use scroll::PickerScrolls;
pub use widget::{PickerWidget, PreviewSpec, WidgetKind};
