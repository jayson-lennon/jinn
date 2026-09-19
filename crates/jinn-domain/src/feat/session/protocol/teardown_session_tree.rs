//! Tear down a session, then archive it and all of its descendants.
//!
//! Sent by the intent handler after the user confirms the teardown-tree
//! prompt (`X` twice in the sidebar sessions section). Only the subtree root
//! travels on the bus — the session-persistence actor resolves the
//! authoritative descendant closure from real [`Session::parent_session`]
//! links across memory and the store, re-checks busy, runs the root's pending
//! teardown first, then archives every member all-or-nothing once teardown
//! succeeds.
//!
//! [`Session::parent_session`]: crate::feat::session::session::Session

use serde::{Deserialize, Serialize};

use crate::BusMessage;
use crate::protocol::SessionId;

/// Tear down a session, then archive its subtree (resolved by the actor).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeardownSessionTree {
    /// The root of the subtree to tear down and archive (the sidebar
    /// selection at confirm time).
    pub root: SessionId,
}

impl BusMessage for TeardownSessionTree {}

jinn_slices::crossing_schema!(TeardownSessionTree, "TeardownSessionTree",
trouper::schema::SchemaKind::Command,
description: "Tear down a subtree root and archive its members.",
fields: ["root" => trouper::schema::FieldTy::Uuid,]);
