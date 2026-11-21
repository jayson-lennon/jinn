//! MCP server rows as shared vocabulary — one builder feeding both
//! surfaces.
//!
//! The TUI inspector (`jinn-mcp-slice`'s picker) and the web modal
//! (`jinn-webui`'s mcp-servers region) show the same thing: every
//! configured server, its launch summary, whether the viewed session
//! enables it, and the live connection status. Building those rows in
//! two crates is the drift the provider-rows precedent exists to
//! prevent — the enabled marks or the order could disagree between the
//! two surfaces with no test holding them together. This module is the
//! one builder: the config catalog, the session's enabled set, and the
//! runtime-cell status overlay go in, and both surfaces render from
//! the same [`McpServerRow`]s out.
//!
//! Pure data and pure functions only (the `-msg` contract): no theme,
//! no rendering, no HTML. Presentation stays in each surface.

use std::collections::{BTreeMap, BTreeSet};

use crate::McpConnectionStatus;

/// The display state of one server's status badge — the shared
/// derivation both surfaces render from.
///
/// The rule this type encodes is the whole point of the type: a
/// **disabled** server is `Off` no matter what the runtime cell still
/// holds. The cell is a status *history* — it keeps the last report a
/// connection made, and a disable's kill lands asynchronously after
/// the enablement flip — so reading it unfiltered shows a killed
/// connection as `running`. Enablement is the user's stated truth and
/// it is synchronous; the cell is the connection's reported truth and
/// it lags. `Off` wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpRowBadge {
    /// Disabled for the viewed session — no connection wanted.
    Off,
    /// Enabled and coming up (or explicitly reporting `Starting`).
    Starting,
    /// Enabled, live connection, tools registered.
    Running,
    /// Enabled; the connection failed or was torn down.
    Dead,
}

impl McpRowBadge {
    /// Derives the badge from the row's enablement mark and the
    /// runtime cell's status for this server.
    ///
    /// The single derivation every surface shares: the TUI picker's
    /// preview badge and the web modal's status label both call this,
    /// so a stale cell report can never render as a running connection
    /// the session does not want.
    #[must_use]
    pub fn derive(enabled: bool, status: Option<McpConnectionStatus>) -> Self {
        if !enabled {
            return Self::Off;
        }
        match status {
            None | Some(McpConnectionStatus::Starting) => Self::Starting,
            Some(McpConnectionStatus::Running) => Self::Running,
            Some(McpConnectionStatus::Dead) => Self::Dead,
        }
    }

    /// The lowercase badge label both surfaces render verbatim.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Dead => "dead",
        }
    }
}

/// One configured MCP server as both surfaces render it.
///
/// Presentation-free: the name, the launch summary, the viewed
/// session's enablement mark, and the live status overlay. A status of
/// `None` means "enabled but not yet reported" — both surfaces render
/// that as `starting`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerRow {
    /// The server name — the `[mcp.<name>]` table key, the per-session
    /// enablement identifier, and the toggle POST's payload field.
    pub name: String,
    /// Human-readable launch summary (`command + args`, the config
    /// entry's `description_for_picker()` shape).
    pub description: String,
    /// Whether the viewed session has this server enabled.
    pub enabled: bool,
    /// The live connection status the runtime cell reports for this
    /// session's view. `None` when the server has not reported (or is
    /// disabled and has never run).
    ///
    /// Render through [`McpRowBadge::derive`], never raw: a disabled
    /// row must show `off` even when the cell still holds the killed
    /// connection's last report.
    pub status: Option<McpConnectionStatus>,
}

/// Builds every configured server's row, sorted case-insensitively by
/// name.
///
/// `servers` is the config catalog as `(name, description)` pairs (the
/// description already the `description_for_picker()` shape — this
/// function does not re-derive it). `enabled` is the viewed session's
/// enabled set; `statuses` is the session's **effective** status
/// projection (`McpRuntimeState::effective_statuses` — a singleton's
/// status lives outside the per-session bucket, and a per-session
/// reader would show a running singleton as perpetually starting).
///
/// The sort key is `name.to_lowercase()`, exactly the TUI inspector's
/// key — the tests pin the resulting order, so a different key here
/// would be a TUI behavior change, not a shared improvement.
#[must_use]
pub fn build_rows(
    servers: &[(String, String)],
    enabled: &BTreeSet<String>,
    statuses: &BTreeMap<String, McpConnectionStatus>,
) -> Vec<McpServerRow> {
    let mut rows: Vec<McpServerRow> = servers
        .iter()
        .map(|(name, description)| McpServerRow {
            name: name.clone(),
            description: description.clone(),
            enabled: enabled.contains(name),
            status: statuses.get(name).copied(),
        })
        .collect();
    rows.sort_by_key(|row| row.name.to_lowercase());
    rows
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::*;

    fn servers() -> Vec<(String, String)> {
        vec![
            ("Weather".to_owned(), "weather --city x".to_owned()),
            ("browser".to_owned(), "jinn mcp browser".to_owned()),
            ("alpha".to_owned(), "alpha-server".to_owned()),
        ]
    }

    #[rstest::rstest]
    #[test]
    fn rows_sort_case_insensitively_by_name() {
        // Given a catalog whose names differ in case ("Weather" vs
        // lowercase peers).
        let enabled = BTreeSet::new();
        let statuses = BTreeMap::new();

        // When building the rows.
        let rows = build_rows(&servers(), &enabled, &statuses);

        // Then the order is case-insensitive alphabetical — "Weather"
        // sorts with the w's, not ahead of every lowercase name.
        let names: Vec<&str> = rows.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names, ["alpha", "browser", "Weather"]);
    }

    #[rstest::rstest]
    #[test]
    fn rows_mark_enablement_from_the_session_set() {
        // Given a catalog and a session that enabled two of the three.
        let enabled = BTreeSet::from(["alpha".to_owned(), "Weather".to_owned()]);
        let statuses = BTreeMap::new();

        // When building the rows.
        let rows = build_rows(&servers(), &enabled, &statuses);

        // Then exactly the enabled names carry the mark — matching is
        // exact (the set holds the config's own spelling).
        let marks: BTreeMap<&str, bool> = rows
            .iter()
            .map(|row| (row.name.as_str(), row.enabled))
            .collect();
        assert_eq!(
            marks,
            BTreeMap::from([("alpha", true), ("browser", false), ("Weather", true)])
        );
    }

    #[rstest::rstest]
    #[test]
    fn rows_overlay_the_effective_status_projection() {
        // Given a status projection naming one server Running.
        let enabled = BTreeSet::new();
        let statuses = BTreeMap::from([("browser".to_owned(), McpConnectionStatus::Running)]);

        // When building the rows.
        let rows = build_rows(&servers(), &enabled, &statuses);

        // Then the named server carries the status and every other row
        // reads `None` (the "not yet reported" state).
        let statuses_by_name: BTreeMap<&str, Option<McpConnectionStatus>> = rows
            .iter()
            .map(|row| (row.name.as_str(), row.status))
            .collect();
        assert_eq!(
            statuses_by_name,
            BTreeMap::from([
                ("alpha", None),
                ("browser", Some(McpConnectionStatus::Running)),
                ("Weather", None),
            ])
        );
    }

    #[rstest::rstest]
    #[test]
    fn rows_carry_the_description_verbatim() {
        // Given a catalog with launch-summary descriptions.
        let enabled = BTreeSet::new();
        let statuses = BTreeMap::new();

        // When building the rows.
        let rows = build_rows(&servers(), &enabled, &statuses);

        // Then each row's description is the input pair's, unmodified
        // — the builder does not re-derive the summary.
        let alpha = rows
            .iter()
            .find(|row| row.name == "alpha")
            .expect("alpha row");
        assert_eq!(alpha.description, "alpha-server");
    }

    #[rstest::rstest]
    #[case(false, None, McpRowBadge::Off)]
    #[case(false, Some(McpConnectionStatus::Starting), McpRowBadge::Off)]
    #[case(false, Some(McpConnectionStatus::Running), McpRowBadge::Off)]
    #[case(false, Some(McpConnectionStatus::Dead), McpRowBadge::Off)]
    #[case(true, None, McpRowBadge::Starting)]
    #[case(true, Some(McpConnectionStatus::Starting), McpRowBadge::Starting)]
    #[case(true, Some(McpConnectionStatus::Running), McpRowBadge::Running)]
    #[case(true, Some(McpConnectionStatus::Dead), McpRowBadge::Dead)]
    fn badge_derive_disabled_overrides_any_cell_report(
        #[case] enabled: bool,
        #[case] status: Option<McpConnectionStatus>,
        #[case] expected: McpRowBadge,
    ) {
        // Given every combination of enablement and cell status.
        // When deriving the badge.
        // Then a disabled row is Off regardless of the stale report,
        // and an enabled row follows the connection's report with
        // no-report rendered as starting.
        assert_eq!(McpRowBadge::derive(enabled, status), expected);
    }

    #[rstest::rstest]
    #[case(McpRowBadge::Off, "off")]
    #[case(McpRowBadge::Starting, "starting")]
    #[case(McpRowBadge::Running, "running")]
    #[case(McpRowBadge::Dead, "dead")]
    fn badge_labels_are_lowercase_words(#[case] badge: McpRowBadge, #[case] label: &str) {
        // Given each badge state.
        // When reading its label.
        // Then the label is the lowercase word both surfaces render.
        assert_eq!(badge.label(), label);
    }
}
