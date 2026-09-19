//! The session-history slice — the sole write path over session chat history.
//!
//! A lib-only slice: it owns [`HistoryEditor`] (every sanctioned mutation of
//! a session's history vector) and nothing else. There is **no actor, no
//! cell, and no `activate()`** here — the kernel session actor's fold
//! handlers (streaming, tool calls, stall retry) are the editor's sanctioned
//! callers, mutating history alongside the phase machine, in-flight guard,
//! and input drain in a single lock scope. The history *contracts*
//! (`PushChatEntry`, `HistoryAppended`, pin commands, …) live in the
//! `jinn-session-history-msg` crate.
//!
//! Kernel dependency (see Cargo.toml): the editor mutates session state
//! through feature-visible accessors; `SessionCore` stays kernel until the
//! lifecycle window splits it.

pub mod history_editor;

pub use history_editor::HistoryEditor;
