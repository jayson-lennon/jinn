//! Token-count cache re-export shim.
//!
//! The cache lives in `jinn-slices` (shared vocabulary — the token-count
//! slice's cell and the kernel's session actor / prune workers all use
//! it); this shim keeps the kernel's import paths resolving. The
//! eviction actor moved to the `jinn-token-count` slice crate.

pub use jinn_token_count_msg::HistoryWorkerChatEntryTokenCache;
