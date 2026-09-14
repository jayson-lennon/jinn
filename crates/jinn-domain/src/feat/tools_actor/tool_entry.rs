//! Tool picker entry type.
//!
//! The rendering lives in the tool picker's spec (`feat::picker::tool_spec`);
//! this struct is the plain domain data the spec wraps.

use crate::feat::theme::Theme;

/// A tool entry ready for display in the tool picker.
#[derive(Debug, Clone)]
pub struct ToolEntry {
    /// Tool name (unique identifier, e.g., "bash", "edit").
    pub name: String,
    /// Human-readable tool description.
    pub description: String,
    /// Whether the tool is currently enabled for this session.
    pub enabled: bool,
    /// Theme for styling.
    pub theme: Theme,
}
