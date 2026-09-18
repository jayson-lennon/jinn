//! Preferences / app-state bus protocol — commands and events.
//!
//! These types cross slice boundaries (the sidebar emits
//! [`UpdateAppState`], the project popup emits [`UpdatePreferences`]),
//! so they live beside the schemas in the kernel-free config crate with
//! their [`BusMessage`] impls. The `kameo::Message` impls that deliver
//! them to the actors live with the actors in the `jinn-preferences`
//! slice (orphan rule: the actor is local there).

pub mod app_state_command;
pub mod app_state_event;
pub mod command;
pub mod event;
