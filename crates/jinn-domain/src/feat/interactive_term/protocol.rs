//! Interactive-term protocol — ask messages and events for the coordinator.
//!
//! The three tools `ask` this actor directly (request/reply, mirroring the
//! `restart_mcp_server` tool); every message is keyed by the owning chat
//! [`crate::protocol::SessionId`] — the terminal's identity. The takeover UI
//! sends [`SendTermKey`](command::SendTermKey) and receives
//! [`TermScreenUpdated`](event::TermScreenUpdated) events.

pub mod command;
pub mod event;
