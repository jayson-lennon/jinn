//! Persona data model.
//!
//! The type lives in `jinn-slices` (shared vocabulary — the persona
//! slice's cell and the kernel's consumers both use it); this shim keeps
//! the kernel's `crate::feat::persona::Persona` paths resolving.

pub use jinn_persona_msg::Persona;
