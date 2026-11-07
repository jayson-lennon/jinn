//! The canvas contract — what crosses the canvas slice boundary.
//!
//! The canvas actor serves one HTML artifact per process; the `canvas`
//! tool asks it to serve (or re-target) through [`CanvasCommand`] and
//! gets the resolved bind back in the reply. The wire payloads live
//! here so the tool layer and the actor never share an implementation
//! crate: the tool names this crate, the actor implements the trait,
//! and neither sees the other's internals.
//!
//! Pure data + the handle trait — no actor, no server, no I/O.

use jinn_slices::BusMessage;

/// The canvas dashboard row's name — the key both the canvas actor's
/// [`jinn_slices::ServiceStatusUpdate`] publications and the row itself
/// are filed under.
pub const CANVAS_ROW_NAME: &str = "canvas";

/// Serve (or re-target) one HTML artifact on the canvas actor's server.
///
/// The path arrives pre-resolved: the caller joins it against the
/// session's working directory and hands over an absolute path string.
/// First serve establishes the listener and the directory watcher;
/// every later serve swaps the served file on the live listener.
///
/// The reply (a [`CanvasServeOutcome`]) rides the ask's reply channel;
/// this schema declares the request half.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, trouper::schema::Command)]
#[schema(description = "Serve one HTML artifact on the canvas server.")]
pub struct CanvasCommand {
    /// Absolute path of the HTML artifact to serve.
    pub path: String,
}

impl BusMessage for CanvasCommand {}

/// The ask's reply: what the canvas actor did with a [`CanvasCommand`].
///
/// `ok: false` carries the failure sentence the tool relays verbatim —
/// the actor's validation errors are phrased as user-facing prose, not
/// codes, so the model can act on them without a decoding table.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, trouper::schema::Event)]
#[schema(description = "Reply payload for a canvas serve ask.")]
pub struct CanvasServeOutcome {
    /// Whether the artifact is now being served.
    pub ok: bool,
    /// The full URL on success (`http://<bind>`), empty on failure.
    pub url: String,
    /// The canonical path now being served, empty on failure.
    pub artifact: String,
    /// The failure sentence on failure, empty on success.
    pub error: String,
    /// The resolved bind the server answered on. Not part of the tool
    /// surface — carried so the dashboard row and a future status probe
    /// read the same value the URL was built from.
    pub bind: String,
}

impl CanvasServeOutcome {
    /// The success outcome: URL and artifact as served, built from the
    /// resolved bind.
    #[must_use]
    pub fn served(url: String, bind: std::net::SocketAddr, artifact: &std::path::Path) -> Self {
        Self {
            ok: true,
            url,
            artifact: artifact.to_string_lossy().into_owned(),
            error: String::new(),
            bind: bind.to_string(),
        }
    }

    /// The failure outcome: the sentence naming the cause and its fix.
    #[must_use]
    pub fn refused(error: String) -> Self {
        Self {
            ok: false,
            url: String::new(),
            artifact: String::new(),
            error,
            bind: String::new(),
        }
    }
}

impl BusMessage for CanvasServeOutcome {}

/// Why a serve ask never produced an outcome.
///
/// Transport-level only: every domain refusal (missing file,
/// bind failure) arrives as `ok: false` on a real outcome, because
/// the actor answers every command it receives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CanvasServeTransportError {
    /// The actor never replied within the ask's bound.
    Timeout,
    /// The actor could not be reached (stopped, mailbox failure).
    Mailbox,
}

/// Drain every accumulated canvas metric event.
///
/// The session-turn enqueue path asks this before dispatching a user
/// message; the reply carries whatever interaction events the served
/// page posted since the last drain. Draining consumes the buffer —
/// each event reaches the agent exactly once, riding the next user
/// message as a `<canvas-metrics>` block.
///
/// Dormant server (nothing ever served): the actor replies with an
/// empty outcome rather than an error — "no activity" is a normal
/// answer, and the message then carries no block.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, trouper::schema::Command)]
#[schema(description = "Drain the accumulated canvas metric events.")]
pub struct DrainCanvasMetrics;

impl BusMessage for DrainCanvasMetrics {}

/// One drained metric event, render-ready for the XML block.
///
/// `event` is the page's own JSON body, verbatim — the server imposed
/// no schema, so the agent's frontend owns the field names. The stamps
/// (`received_at`, `artifact`) are jinn's, for ordering and context.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, trouper::schema::Event)]
#[schema(description = "Reply payload for a canvas metrics drain ask.")]
pub struct CanvasMetricEvent {
    /// The event as the page posted it: at minimum `name`, `at`, `props`.
    pub event: serde_json::Value,
    /// When jinn received the post (RFC 3339) — the ordering the block
    /// renders in, since page clocks can drift.
    pub received_at: String,
    /// The artifact path being served when the event landed.
    pub artifact: String,
}

impl BusMessage for CanvasMetricEvent {}

/// The drain ask's reply: the accumulated events, oldest first. An
/// empty vector is the dormant-server and quiet-page answer.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, trouper::schema::Event)]
#[schema(description = "The drained canvas metric events, oldest first.")]
pub struct CanvasMetricsDrainOutcome {
    /// The drained events, oldest first. Empty on a dormant server or
    /// after quiet pages.
    pub events: Vec<CanvasMetricEvent>,
}

impl CanvasMetricsDrainOutcome {
    /// The empty outcome — the dormant-server and quiet-page answer.
    #[must_use]
    pub fn empty() -> Self {
        Self { events: Vec::new() }
    }
}

impl BusMessage for CanvasMetricsDrainOutcome {}

/// The canvas actor's handle — the tool-facing seam for serving, and
/// the session-turn seam for draining metrics.
///
/// The actor-side implementation bounds each ask inside its methods
/// (the `restart_mcp` seam precedent), so a hung actor yields
/// [`CanvasServeTransportError`] rather than hanging the caller.
#[async_trait::async_trait]
pub trait CanvasServerHandle: Send + Sync {
    /// Serves (or re-targets) the artifact at `path`.
    ///
    /// # Errors
    ///
    /// Returns the transport error when the round trip never completed;
    /// domain refusals come back as a failed [`CanvasServeOutcome`].
    async fn serve(&self, path: String) -> Result<CanvasServeOutcome, CanvasServeTransportError>;

    /// Drains the accumulated metric events, consuming the buffer.
    ///
    /// # Errors
    ///
    /// Returns the transport error when the round trip never completed;
    /// a live-but-quiet server is an empty outcome, not an error.
    async fn drain_metrics(&self) -> Result<CanvasMetricsDrainOutcome, CanvasServeTransportError>;

    /// Debug name (service-trait convention).
    fn name(&self) -> &'static str {
        "canvas-server"
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test assertions need expect")]

    use super::{CanvasCommand, CanvasServeOutcome};

    #[rstest::rstest]
    #[test]
    fn serve_outcome_success_carries_url_and_artifact_without_error() {
        // Given a served artifact.
        // When building the success outcome.
        let outcome = CanvasServeOutcome::served(
            "http://127.0.0.1:43121".to_owned(),
            "127.0.0.1:43121".parse().expect("bind parses"),
            std::path::Path::new("/tmp/chart.html"),
        );

        // Then it is ok, carries both strings, and no error.
        assert!(outcome.ok);
        assert_eq!(outcome.url, "http://127.0.0.1:43121");
        assert_eq!(outcome.artifact, "/tmp/chart.html");
        assert_eq!(outcome.error, "");
    }

    #[rstest::rstest]
    #[test]
    fn serve_outcome_failure_carries_the_sentence_only() {
        // Given a refused serve.
        // When building the failure outcome.
        let outcome =
            CanvasServeOutcome::refused("canvas artifact must be an existing HTML file".to_owned());

        // Then the sentence is the payload and the URL/artifact stay empty.
        assert!(!outcome.ok);
        assert_eq!(
            outcome.error,
            "canvas artifact must be an existing HTML file"
        );
        assert_eq!(outcome.url, "");
        assert_eq!(outcome.artifact, "");
    }

    #[rstest::rstest]
    #[test]
    fn canvas_command_round_trips_through_json() {
        // Given a serve command.
        let command = CanvasCommand {
            path: "/home/j/proj/chart.html".to_owned(),
        };

        // When serializing then re-parsing.
        let json = serde_json::to_string(&command).expect("serialize");
        let back: CanvasCommand = serde_json::from_str(&json).expect("reparse");

        // Then the path survives.
        assert_eq!(back.path, command.path);
    }
}
