//! Multimodal content attachments for LLM messages.
//!
//! [`Attachment`] is defined in `jinn-core-types` (the foundational value-type
//! crate, the same home as `ToolCall`/`ToolDefinition`) and re-exported here so
//! `crate::Attachment` paths in the provider request builders keep resolving.

pub use jinn_core_types::attachment::Attachment;
