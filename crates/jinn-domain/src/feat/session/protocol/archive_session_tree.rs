//! Archive a session and all of its descendants.
//!
//! Sent by the intent handler after the user confirms the archive-tree prompt
//! (`A` twice in the sidebar sessions section). Only the subtree root travels
//! on the bus — the session-persistence actor resolves the authoritative
//! descendant closure from real [`Session::parent_session`] links across
//! memory and the store, re-checks busy, then archives every member
//! all-or-nothing.
//!
//! [`Session::parent_session`]: crate::feat::session::session::Session

use serde::{Deserialize, Serialize};

use crate::BusMessage;
use crate::protocol::SessionId;

/// Archive a session and all of its descendants (resolved by the actor).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveSessionTree {
    /// The root of the subtree to archive (the sidebar selection at confirm time).
    pub root: SessionId,
}

impl BusMessage for ArchiveSessionTree {}

jinn_slices::crossing_schema!(ArchiveSessionTree, "ArchiveSessionTree",
trouper::schema::SchemaKind::Command,
description: "Archive a session and its whole subtree.",
fields: ["root" => trouper::schema::FieldTy::Uuid,]);
