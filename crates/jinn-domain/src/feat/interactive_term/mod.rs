//! Terminal intent + config helpers pending the route-row conversion.
//!
//! Everything else about this feature lives in the term slice crate
//! (`jinn-term`) and its vocabulary crate (`jinn-term-msg`).

pub mod overlay_intent;
pub mod prefs;
pub mod protocol;
pub mod pty_session;
pub mod settle;
pub mod takeover_intent;
pub mod terminal_tab_state;
