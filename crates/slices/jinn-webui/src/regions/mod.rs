//! The webui regions — one module per independently editable region
//! of the page, plus the [`crate::region`] trait and registry that
//! bind them.
//!
//! Pure grouping shape (per the style guide): the modules hold the
//! logic; this file holds nothing but the `mod` declarations. A
//! region's module owns its guard render, its pull-route handler, its
//! contract container, and its settle declaration — everything the
//! server half of the region contract names. The TypeScript half of
//! each region lives in `webui/client/src/regions/<id>.ts`.

pub mod composer;
pub mod model_picker;
pub mod sessions;
pub mod transcript;
