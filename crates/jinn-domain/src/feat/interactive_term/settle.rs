//! Settle detection + key encoding lives in `jinn-term-msg` (shared
//! with the term slice and the kernel's `interactive_term*` tools);
//! this module is a re-export shim.

pub use jinn_term_msg::settle::*;
