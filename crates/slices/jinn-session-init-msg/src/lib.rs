//! Session-init crossing contracts — the discovery command and event pairs.
//!
//! The producer is this slice's discovery worker, so the scan commands and
//! loaded events live in this crate. The portable [`PromptTemplate`] value is
//! owned by `jinn-context` and re-exported here to preserve existing producer
//! and consumer import paths. The crossing-schema ids remain unchanged.

mod command;
mod event;

use marked_path::{Absolute, MarkedPathBuf};

/// The scan cwd a legacy blob without one resolves to: the launch directory.
#[expect(
    clippy::expect_used,
    reason = "cwd and temp-dir fallback are absolute on supported platforms"
)]
pub(crate) fn default_cwd() -> MarkedPathBuf<Absolute> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| tempfile::env::temp_dir());
    MarkedPathBuf::<Absolute>::new(cwd).expect("cwd and temp dir are absolute")
}

use serde::{Deserialize, Serialize};

use jinn_core_types::SessionId;
use jinn_slices::BusMessage;

pub use command::ScanContextFiles;
pub use event::ContextFilesLoaded;
pub use jinn_context::PromptTemplate;

/// Rescan prompt templates for a specific session.
///
/// Carries the session's cwd: the worker scans user/system plus project-local
/// `.agents/prompts` dirs (most-local wins), and emits [`PromptTemplatesLoaded`].
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Command)]
#[schema(description = "Rescan prompt templates for a session.")]
pub struct RescanPromptTemplates {
    /// The session whose scan this is.
    pub session_id: SessionId,
    /// The working directory driving the scan.
    #[serde(default = "default_cwd")]
    pub cwd: MarkedPathBuf<Absolute>,
}
impl BusMessage for RescanPromptTemplates {}

/// Re-run full project-resource discovery for a specific session.
///
/// The manual refresh publishes this; the session-init supervisor forwards it
/// to the session's keyed discovery worker as the same full run a
/// watcher-detected edit triggers.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Command)]
#[schema(description = "Re-run full project-resource discovery for a session.")]
pub struct RefreshProjectResources {
    /// The session whose discovery re-runs.
    pub session_id: SessionId,
    /// The working directory driving the scans.
    #[serde(default = "default_cwd")]
    pub cwd: MarkedPathBuf<Absolute>,
}
impl BusMessage for RefreshProjectResources {}

/// Prompt templates loaded after a rescan.
///
/// Emitted by the prompt scan actor after scanning the prompts directory.
/// On success, `templates` contains the loaded templates and `error` is `None`.
/// On failure, `templates` is empty and `error` contains a description.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "Prompt templates loaded after a rescan.")]
pub struct PromptTemplatesLoaded {
    /// The session whose cwd drove the scan.
    pub session_id: SessionId,
    /// The loaded prompt templates.
    pub templates: Vec<PromptTemplate>,
    /// Error message if scanning failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl BusMessage for PromptTemplatesLoaded {}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::unwrap_used,
        clippy::unreachable,
        reason = "test code"
    )]

    use super::*;

    #[rstest::rstest]
    #[case("RescanPromptTemplates")]
    #[case("RefreshProjectResources")]
    #[case("PromptTemplatesLoaded")]
    #[case("ScanContextFiles")]
    #[case("ContextFilesLoaded")]
    fn crossing_schema_ids_are_stable(#[case] name: &str) {
        // Given the five crossing contracts.
        // When deriving their trouper schema ids.
        // Then the ids equal the type names (the compatibility surface).
        let id = match name {
            "RescanPromptTemplates" => {
                <RescanPromptTemplates as trouper::schema::Schema>::schema_id()
            }
            "RefreshProjectResources" => {
                <RefreshProjectResources as trouper::schema::Schema>::schema_id()
            }
            "PromptTemplatesLoaded" => {
                <PromptTemplatesLoaded as trouper::schema::Schema>::schema_id()
            }
            "ScanContextFiles" => <ScanContextFiles as trouper::schema::Schema>::schema_id(),
            "ContextFilesLoaded" => <ContextFilesLoaded as trouper::schema::Schema>::schema_id(),
            other => unreachable!("unhandled name: {other}"),
        };
        assert_eq!(id.name(), name);
    }

    #[rstest::rstest]
    #[test]
    fn prompt_template_roundtrips_through_serde() {
        // Given a prompt template.
        let template = PromptTemplate {
            name: "code-review".to_owned(),
            description: "Review the diff".to_owned(),
            body: "You are an expert...".to_owned(),
        };

        // When serializing and deserializing.
        let json = serde_json::to_string(&template).expect("serialize");
        let round: PromptTemplate = serde_json::from_str(&json).expect("deserialize");

        // Then the fields survive the roundtrip.
        assert_eq!(round.name, "code-review");
        assert_eq!(round.description, "Review the diff");
        assert_eq!(round.body, "You are an expert...");
    }
}
