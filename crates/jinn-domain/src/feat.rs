//! Feature modules - domain-specific logic, actors, and UI elements.

pub mod auto_prune_worker;
pub mod chat_entry_selection;
pub mod chat_input;
pub mod compaction_worker;
pub mod context;
pub mod endpoint;
pub mod file_lister;
pub mod global;
pub mod history_worker;
pub mod image_convert;
pub mod install;
pub mod intent;
pub mod llm_actor;
pub mod navigation;
pub mod persona;
pub mod picker;
pub mod plugin;
pub mod plugin_actor;
pub mod plugin_coordinator_actor;
pub mod project;
pub mod provider;
pub use jinn_provider_config as provider_infra;
pub mod pruner_accumulation_input;
pub mod queue_actor;
pub mod reasoning;
pub mod session;
pub mod session_lifecycle;
pub mod session_search;
pub mod skills;
pub mod theme;

pub mod ui;
