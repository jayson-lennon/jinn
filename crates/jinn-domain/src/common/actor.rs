//! Actor SDK for building jinn actors.
//!
//! Provides shared actor utilities: bus message types.

pub mod actor_counter;
pub mod actor_name;
pub mod protocol;

pub use actor_counter::ActorCounter;
pub use actor_name::ActorName;
