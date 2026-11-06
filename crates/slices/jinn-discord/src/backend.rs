//! The poise gateway frontend, merged into the slice crate as `backend`.
//!
//! Owns the poise gateway task and slash-command handlers that drive a running
//! jinn instance from Discord. The domain-layer pieces (config, thread-map DAO,
//! message splitter, bridge actor) live in the slice's top-level modules
//! ([`crate::authorize`], [`crate::thread_map`], [`crate::message_split`],
//! [`crate::bridge_subscriber`]).

pub mod commands;
pub mod feat;
pub mod gateway;
pub mod session_route;
pub mod spawn;

pub use gateway::{BotData, SpawnError};
pub use spawn::gateway_spawner;
