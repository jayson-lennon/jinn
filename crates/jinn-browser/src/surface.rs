//! The tool-surface data types: everything that crosses the daemon's
//! HTTP boundary and therefore compiles on both sides of the feature
//! split — the daemon serializes them, every client deserializes them.
//!
//! Chaser/CDP types never appear here; this is the wire vocabulary.

use serde::{Deserialize, Serialize};

/// One open tab, as the tools see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabInfo {
    /// The daemon-minted id (`t1`, `t2`, …).
    pub tab: String,
    /// The current URL, when known.
    pub url: Option<String>,
    /// The current title, when known.
    pub title: Option<String>,
}

/// One interactive element from a snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotRef {
    /// The ref id — `e1` for the top frame, `f1:e5` for the fifth node
    /// of the first same-origin iframe — stable within the tab until
    /// the next snapshot.
    pub id: String,
    /// The tag name, lowercased.
    pub tag: String,
    /// Visible text, when the element carries any.
    pub text: Option<String>,
    /// The element's `id` attribute, when present.
    pub html_id: Option<String>,
    /// Input value, when the element is an input carrying one.
    pub value: Option<String>,
    /// The owning frame: `null` for the top frame, `f<k>` for a
    /// same-origin iframe's document.
    #[serde(default)]
    pub frame: Option<String>,
    /// Whether a ref click can act on this row. Iframe placeholder rows
    /// (frame discovery, any origin) are `false`: they describe the
    /// frame — for cross-origin frames the coordinates are the element
    /// center, for use with `click_at` — but cannot be clicked by ref.
    #[serde(default = "default_true")]
    pub clickable: bool,
    /// The element's viewport-absolute center x, CSS px (iframe content
    /// already offset by its ancestor frame rects).
    #[serde(default)]
    pub x: Option<f64>,
    /// The element's viewport-absolute center y, CSS px.
    #[serde(default)]
    pub y: Option<f64>,
}

/// Serde default for the clickable flag (absent in older payloads).
fn default_true() -> bool {
    true
}

/// The result of one snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// The tab snapshotted.
    pub tab: String,
    /// The current URL.
    pub url: Option<String>,
    /// The current title.
    pub title: Option<String>,
    /// Interactive elements, document order.
    pub refs: Vec<SnapshotRef>,
    /// Human-readable tree rendering with the ref ids inline.
    pub tree: String,
}

/// The conditions `wait` supports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WaitCondition {
    /// The next page load lifecycle event.
    Navigation,
    /// The page reaches the network-idle lifecycle state.
    NetworkIdle,
    /// The page's full text contains this substring.
    Text(String),
    /// This many milliseconds.
    Ms(u64),
}
