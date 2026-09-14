//! Skills crossing contracts — the events and commands other modules
//! and slices consume. The scanning actor itself lives in the
//! session-init slice; these types stay kernel-side because kernel
//! consumers (the session actor, the task settle listener) reference
//! them and the reverse bridge carries these exact Rust types.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::feat::skills::skill::Skill;

/// Emitted when skills have been scanned and loaded.
///
/// On success, `skills` contains the discovered skills and `error` is `None`.
/// On failure, `skills` is empty and `error` contains a description.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillsLoaded {
    /// The session whose cwd drove the scan.
    pub session_id: crate::SessionId,
    /// The discovered agent skills.
    pub skills: Vec<Skill>,
    /// Error message if scanning failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Command to trigger a skills scan for a specific session.
///
/// Carries the session's cwd: the discovery worker scans global +
/// project dirs discovered via the bounded walk, and writes the merged
/// result into that session's ephemeral discovered-skills set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanSkills {
    /// The session whose scan this is.
    pub session_id: crate::SessionId,
    /// The working directory driving the scan.
    #[serde(default)]
    pub cwd: PathBuf,
}

impl crate::common::bus::BusMessage for SkillsLoaded {}

jinn_slices::crossing_schema!(SkillsLoaded, "SkillsLoaded",
trouper::schema::SchemaKind::Event,
description: "Skills have been scanned and loaded for a session.",
fields: [
    "session_id" => trouper::schema::FieldTy::Uuid,
    "skills" => trouper::schema::FieldTy::List(Box::new(trouper::schema::FieldTy::Json)),
]);

impl crate::common::bus::BusMessage for ScanSkills {}

jinn_slices::crossing_schema!(ScanSkills, "ScanSkills",
trouper::schema::SchemaKind::Command,
description: "Trigger a skills scan for a session.",
fields: [
    "session_id" => trouper::schema::FieldTy::Uuid,
    "cwd" => trouper::schema::FieldTy::Str,
]);
