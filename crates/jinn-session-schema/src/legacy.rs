//! Version-pinned snapshot of the `metadata` blob shape at migration v20.
//!
//! v20 backfills `metadata` for pre-v8 rows whose blob is `NULL` by
//! reconstructing one from the legacy columns. The reconstructed blob must
//! deserialize via jinn-kernel's **runtime** `PersistableCore`. Rather than
//! import that live type (which would couple this leaf crate to jinn-kernel
//! and break the build cycle), we snapshot the shape as it exists at v20.
//!
//! **This is not duplication — it is a snapshot.** If the live `PersistableCore`
//! gains a field in v21, this struct correctly stays frozen; v21 owns its own
//! logic for the new field. The coupling between this snapshot and the runtime
//! deserializer is explicit and pinned by a round-trip test in jinn-kernel.
//!
//! Field names and JSON representations must match the runtime
//! `PersistableCore`'s serde output exactly. The runtime derives
//! `Serialize`/`Deserialize` over:
//!
//! ```ignore
//! struct PersistableCore {
//!     session_id: SessionId(String),
//!     title: Option<String>,
//!     updated_at: jiff::Timestamp,        // serde → RFC 3339 string
//!     created_at: jiff::Timestamp,
//!     profile: SessionProfile,            // pass-through JSON object
//!     cwd: PathBuf,                       // serde → string
//!     parent_session: Option<SessionId>,
//!     #[serde(default)] fork_ordinal: Option<usize>,
//!     blobs: HashMap<String, Value>,
//!     lifecycle_name: Option<String>,
//!     lifecycle_args: Vec<String>,
//!     lifecycle_script_state: LifecycleScriptState,  // untagged-ish: "NothingRan" etc.
//!     #[serde(default)] task_list: TaskList,
//!     #[serde(default = "...")] persist: bool,
//! }
//! ```
//!
//! Most of these serialize transparently as primitives/JSON. `profile` is
//! carried as a raw `serde_json::Value` because a prior migration (v17/v19)
//! already normalized its `model` field — we pass it through untouched.
//
// The legacy column and metadata shapes are the historical on-disk contract:
// this module reproduces them byte-for-byte, so raw owning paths are the
// schema being preserved, not a hazard to re-type.
#![allow(
    clippy::disallowed_types,
    reason = "schema-compat: legacy column and v20 metadata shapes are reproduced byte-for-byte; re-typing user-authored values could change persisted bytes"
)]

use std::collections::HashMap;
use std::path::PathBuf;

use error_stack::{Report, ResultExt as _};
use jiff::Timestamp;
use serde::Serialize;

use crate::SchemaMigrationError;

/// The legacy (pre-v8) `sessions` column values, read verbatim from the row
/// whose `metadata` is `NULL`. All are the raw column text.
pub struct LegacySessionColumns {
    pub session_id: String,
    pub title: Option<String>,
    pub updated_at: String,
    pub created_at: String,
    pub parent_session: Option<String>,
    pub profile: String,
    pub blobs: String,
    pub cwd: String,
    pub lifecycle_name: Option<String>,
    pub lifecycle_args: String,
    pub lifecycle_script_state: String,
}

/// A frozen snapshot of the `metadata` blob as it exists at migration v20.
///
/// See the module docs for why this is a separate struct rather than an import
/// of jinn-kernel's live `PersistableCore`.
#[derive(Serialize)]
pub struct PersistableCoreV20 {
    pub session_id: String,
    pub title: Option<String>,
    pub updated_at: String,
    pub created_at: String,
    pub profile: serde_json::Value,
    pub cwd: PathBuf,
    pub parent_session: Option<String>,
    /// `None` for legacy rows — they predate fork ordinals.
    #[serde(default)]
    pub fork_ordinal: Option<usize>,
    pub blobs: HashMap<String, serde_json::Value>,
    pub lifecycle_name: Option<String>,
    pub lifecycle_args: Vec<(String, String)>,
    /// Serialized verbatim — the legacy column already holds the snake_case
    /// form (`nothing_ran`, `setup_ran`, `teardown_ran`) that matches the
    /// runtime `LifecycleScriptState`'s `#[serde(rename_all = "snake_case")]`.
    pub lifecycle_script_state: String,
    /// Legacy sessions never carried a task list. Default to empty.
    #[serde(default)]
    pub task_list: serde_json::Value,
    pub persist: bool,
}
impl PersistableCoreV20 {
    /// Reconstructs the `metadata` blob from the legacy columns, mirroring the
    /// pre-v20 legacy load branch.
    ///
    /// `profile`, `lifecycle_args`, and `lifecycle_script_state` are
    /// deserialized with fault tolerance (`.unwrap_or_default()`): the legacy
    /// columns may hold values from very old jinn versions that fail to parse,
    /// and a migration must never block on data the old system could have
    /// produced. `blobs` and the timestamps are stricter — a corrupt blob or
    /// unparseable timestamp is a real error.
    ///
    /// # Errors
    ///
    /// Returns an error if `blobs` JSON fails to deserialize, a timestamp fails
    /// to parse, or the reconstructed blob cannot be serialized.
    pub fn blob_from_legacy_columns(
        legacy: &LegacySessionColumns,
    ) -> Result<String, Report<SchemaMigrationError>> {
        // profile: the column was already normalized to {"model": {"single": ...}}
        // by migration v17. Default on parse failure.
        let profile: serde_json::Value = serde_json::from_str(&legacy.profile).unwrap_or_default();

        let blobs: HashMap<String, serde_json::Value> = serde_json::from_str(&legacy.blobs)
            .change_context(SchemaMigrationError)
            .attach("v20: failed to deserialize legacy blobs column")?;

        // Legacy columns hold bare JSON-ish array literals that may not be valid
        // JSON; the original legacy load branch used unwrap_or_default. Preserve
        // the tolerance here, and key bare values $1, $2, ... by index so the
        // migrated session keeps the positional render it was recorded with.
        let bare_args: Vec<String> =
            serde_json::from_str(&legacy.lifecycle_args).unwrap_or_default();
        let lifecycle_args: Vec<(String, String)> = bare_args
            .into_iter()
            .enumerate()
            .map(|(idx, value)| (format!("${}", idx + 1), value))
            .collect();

        // Validate timestamps parse (they're written RFC 3339). Don't coerce.
        let _updated: Timestamp = legacy
            .updated_at
            .parse()
            .change_context(SchemaMigrationError)
            .attach("v20: failed to parse legacy updated_at")?;
        let _created: Timestamp = legacy
            .created_at
            .parse()
            .change_context(SchemaMigrationError)
            .attach("v20: failed to parse legacy created_at")?;

        let persistable = Self {
            session_id: legacy.session_id.clone(),
            title: legacy.title.clone(),
            updated_at: legacy.updated_at.clone(),
            created_at: legacy.created_at.clone(),
            profile,
            cwd: PathBuf::from(&legacy.cwd),
            parent_session: legacy.parent_session.clone(),
            fork_ordinal: None,
            blobs,
            lifecycle_name: legacy.lifecycle_name.clone(),
            lifecycle_args,
            lifecycle_script_state: legacy.lifecycle_script_state.clone(),
            task_list: serde_json::json!({"phases": []}),
            persist: true,
        };

        serde_json::to_string(&persistable)
            .change_context(SchemaMigrationError)
            .attach("v20: failed to serialize backfilled metadata blob")
    }
}
