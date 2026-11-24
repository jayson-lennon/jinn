//! The webui regions — one module per independently editable region
//! of the page, plus the [`crate::region`] trait and registry that
//! bind them.
//!
//! Pure grouping shape (per the style guide): the modules hold the
//! logic; this file holds nothing but the `mod` declarations. A
//! region's module owns its guard render, its pull-route handler, its
//! contract container, and its settle declaration — everything the
//! server half of the region contract names. The TypeScript half of
//! each region's browser behavior rides declarative Datastar attributes

pub mod applets;
pub mod composer;
pub mod mcp_servers;
pub mod menu;
pub mod model_picker;
pub mod persona_picker;
pub mod session_new;
pub mod sessions;
pub mod skills;
pub mod tools;
pub mod transcript;
