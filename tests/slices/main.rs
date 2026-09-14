//! Slice-composition integration tests.
//!
//! These exercise the composed system — real slice activation over the
//! kernel's registries, real route rows, real cells and actors — the
//! layer no individual crate can test in isolation. They live here (the
//! root crate's `tests/`) so `just check` and IDE analysis never compile
//! slice crates into the kernel or the tui crate graph.
//!
//! One test binary, one file per slice (plus the composition seam), each
//! mirroring the slice it tests:
//!
//! - [`discord`] — discord's rows in the composed keymap
//! - [`quake_bar`] — quake-bar's rows in the composed keymap
//! - [`dashboard`] — dashboard's scope, cell, actor, rendering
//! - [`session_init`] — the discovery relay chain (kameo → trouper →
//!   keyed worker → reverse relays → kameo)
//! - [`composition`] — the shared seam (every slice's rows attached)
//!
//! Coverage split:
//! - tui unit tests: synthetic/slice-shaped inputs only
//! - slice crate tests: each slice's own row shape and behavior
//! - these tests: the composition of both (keys resolve across the
//!   built-in keymap plus every slice's rows; rendering against real
//!   slice cells; actors applying routed messages).

#![allow(clippy::expect_used, clippy::panic, reason = "test code")]

mod common;

mod composition;
mod dashboard;
mod discord;
mod quake_bar;
mod session_init;
