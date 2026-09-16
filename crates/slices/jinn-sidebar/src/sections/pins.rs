//! Pinned entries sidebar section.

pub mod pins_section;
pub mod state;
pub mod validator;

pub use pins_section::PinsSection;
pub use pins_section::{navigate, pins_section_content_height, receive_cursor};
pub use state::PinsState;

#[cfg(test)]
mod pins_section_tests;
