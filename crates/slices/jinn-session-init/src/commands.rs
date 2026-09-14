//! The slice's trouper commands — addressed to the discovery
//! partition's public path by the supervisor.
//!
//! All four carry the shard key (`session_id`) the kernel extracts to
//! route `jinn.discovery` → `jinn.discovery/<session_id>` and to
//! activate the per-session worker entity on demand. They also carry
//! the session's cwd: the worker scans that directory, not whatever
//! shared state happens to hold.

use std::path::PathBuf;

use jinn_core_types::SessionId;
use serde::{Deserialize, Serialize};

/// Scan all three resources for a session (skills, prompt templates,
/// context files) and settle the run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunDiscovery {
    /// The session whose environment drives the scans.
    pub session_id: SessionId,
    /// The working directory driving the scans.
    pub cwd: PathBuf,
}

/// Re-run only the skills scan for a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RescanSkills {
    /// The session whose environment drives the scan.
    pub session_id: SessionId,
    /// The working directory driving the scan.
    pub cwd: PathBuf,
}

/// Re-run only the prompt-templates scan for a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RescanPrompts {
    /// The session whose environment drives the scan.
    pub session_id: SessionId,
    /// The working directory driving the scan.
    pub cwd: PathBuf,
}

/// Re-run only the context-files scan for a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RescanContext {
    /// The session whose environment drives the scan.
    pub session_id: SessionId,
    /// The working directory driving the scan.
    pub cwd: PathBuf,
}

impl jinn_slices::BusMessage for RunDiscovery {}
impl jinn_slices::BusMessage for RescanSkills {}
impl jinn_slices::BusMessage for RescanPrompts {}
impl jinn_slices::BusMessage for RescanContext {}

impl trouper::schema::Schema for RunDiscovery {
    fn schema_def() -> trouper::schema::SchemaDef {
        trouper::schema::SchemaDef {
            name: "RunDiscovery".to_owned(),
            version: 1,
            kind: trouper::schema::SchemaKind::Command,
            fields: vec![
                trouper::schema::FieldDef::required("session_id", trouper::schema::FieldTy::Uuid)
                    .as_shard_key(),
                trouper::schema::FieldDef::required("cwd", trouper::schema::FieldTy::Str),
            ],
            description: Some(
                "Scan all three discovery resources for a session and settle the run.".to_owned(),
            ),
        }
    }
}

impl trouper::schema::Schema for RescanSkills {
    fn schema_def() -> trouper::schema::SchemaDef {
        trouper::schema::SchemaDef {
            name: "RescanSkills".to_owned(),
            version: 1,
            kind: trouper::schema::SchemaKind::Command,
            fields: vec![
                trouper::schema::FieldDef::required("session_id", trouper::schema::FieldTy::Uuid)
                    .as_shard_key(),
                trouper::schema::FieldDef::required("cwd", trouper::schema::FieldTy::Str),
            ],
            description: Some("Re-run the skills scan for a session.".to_owned()),
        }
    }
}

impl trouper::schema::Schema for RescanPrompts {
    fn schema_def() -> trouper::schema::SchemaDef {
        trouper::schema::SchemaDef {
            name: "RescanPrompts".to_owned(),
            version: 1,
            kind: trouper::schema::SchemaKind::Command,
            fields: vec![
                trouper::schema::FieldDef::required("session_id", trouper::schema::FieldTy::Uuid)
                    .as_shard_key(),
                trouper::schema::FieldDef::required("cwd", trouper::schema::FieldTy::Str),
            ],
            description: Some("Re-run the prompt-templates scan for a session.".to_owned()),
        }
    }
}

impl trouper::schema::Schema for RescanContext {
    fn schema_def() -> trouper::schema::SchemaDef {
        trouper::schema::SchemaDef {
            name: "RescanContext".to_owned(),
            version: 1,
            kind: trouper::schema::SchemaKind::Command,
            fields: vec![
                trouper::schema::FieldDef::required("session_id", trouper::schema::FieldTy::Uuid)
                    .as_shard_key(),
                trouper::schema::FieldDef::required("cwd", trouper::schema::FieldTy::Str),
            ],
            description: Some("Re-run the context-files scan for a session.".to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test code")]

    use super::*;
    use trouper::schema::FieldRole;
    use trouper::schema::Schema;

    #[rstest::rstest]
    #[case::run_discovery(RunDiscovery::schema_def())]
    #[case::rescan_skills(RescanSkills::schema_def())]
    #[case::rescan_prompts(RescanPrompts::schema_def())]
    #[case::rescan_context(RescanContext::schema_def())]
    fn slice_commands_declare_the_session_shard_key(#[case] schema: trouper::schema::SchemaDef) {
        // Given one of the slice's keyed commands' schemas.
        // Then exactly the session_id field is marked as the shard key.
        let keyed: Vec<_> = schema
            .fields
            .iter()
            .filter(|f| f.role == Some(FieldRole::ShardKey))
            .collect();
        assert_eq!(keyed.len(), 1);
        // And the field is the uuid-typed session id.
        assert_eq!(keyed[0].name, "session_id");
    }

    #[rstest::rstest]
    fn run_discovery_shard_key_roundtrips_as_uuid_string() {
        // Given a RunDiscovery command with a real session id.
        let id = SessionId::new();
        let command = RunDiscovery {
            session_id: id.clone(),
            cwd: std::env::temp_dir(),
        };
        let payload = serde_json::to_value(&command).expect("serialize");

        // When the kernel extracts the shard key from the payload.
        let key =
            trouper::pool::extract_shard_key(&RunDiscovery::schema_def(), "session_id", &payload)
                .expect("key extracts");

        // Then the key is the session's uuid string form.
        assert_eq!(key, id.to_string());
    }
}
