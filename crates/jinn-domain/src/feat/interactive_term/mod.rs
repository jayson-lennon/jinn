//! Terminal tool-side protocol, prefs, and settle helpers.
//!
//! The interaction surface (keybinds, scopes, overlay rendering, actor
//! family) lives in the term slice crate (`jinn-term`) and its
//! vocabulary crate (`jinn-term-msg`); what remains kernel-side is the
//! spawn/send/kill tool plumbing that predates the slice split.

pub mod prefs;
pub mod protocol;
pub mod pty_session;
pub mod settle;
pub mod terminal_tab_state;
