//! Protocol-level LLM message types.
//!
//! [`LlmMessage`] is defined in `jinn-core-types` and re-exported here so
//! `crate::LlmMessage` paths in the provider internals keep resolving.

pub use jinn_core_types::llm_message::LlmMessage;
