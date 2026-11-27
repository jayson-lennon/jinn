//! Authoritative live session state and snapshot contracts.
//!
//! This neutral crate owns the coherent session aggregate that turn, store,
//! lifecycle, and frontend adapters coordinate through. It does not depend on
//! those implementation slices or on the domain kernel.

#![forbid(unsafe_code)]

pub mod assembly_projection;
pub mod chat_session;
pub mod core;
pub mod fields;
pub mod mutation_accumulator;
pub mod read_projection;
mod runtime;
pub mod session_map;
pub mod session_store;
pub mod session_store_service;
pub mod snapshot;
pub mod steering_buffer;
pub mod token_stats;
pub mod tree_aggregate;
mod tree_projection;
pub mod turn_count;

#[cfg(test)]
mod token_stats_tests;
#[cfg(test)]
mod tree_aggregate_tests;
#[cfg(test)]
#[path = "workflow_source_persistence_tests.rs"]
mod workflow_source_persistence_tests;

#[cfg(test)]
#[path = "lifecycle_source_persistence_tests.rs"]
mod lifecycle_source_persistence_tests;
#[cfg(test)]
mod working_time_persistence_tests;

pub use assembly_projection::AssemblySessionProjection;
pub use chat_session::{
    ChatSessionState, PinDisplayRow, PinRowLabel, StreamingError, truncate_to_width,
};
pub use core::SessionCore;
pub use fields::{
    SessionHistoryWorkFields, SessionIdentityMetadataFields, SessionIntegrationFields,
    SessionLifecycleLocationFields, SessionProfile, SessionStorageFields, default_cwd,
    default_persist, default_prep_mode,
};
pub use read_projection::SessionReadProjection;
pub use runtime::{SessionCoreEphemeral, SessionUi};
pub use session_map::{SessionLoadGuard, SessionMap};
pub use session_store::{SessionStore, SessionStoreError, SessionStoreService};
pub use snapshot::{SessionRevision, SessionSnapshot, SessionSnapshotMetadata};
pub use token_stats::aggregate_session_stats;
pub use tree_aggregate::{FrozenTreeNode, aggregate_tree_stats, find_tree_root};
pub use tree_projection::{snapshot_frozen_node, snapshot_frozen_node_from_snapshot};
pub use turn_count::compute_turn_count;
