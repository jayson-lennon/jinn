//! Theme struct - the resolved set of semantic colors used by the renderer.

use std::collections::HashMap;

use ratatui::style::{Color, Style};

use crate::color::ThemeColor;
use crate::default_theme;

/// Resolved theme with all semantic color fields.
///
/// Every field is a [`Color`] - fully resolved from whatever format the
/// TOML file specified. Missing TOML fields fall back to the default theme.
///
/// The theme is stored in `AppState.frontend` and read by all render sites.
#[derive(Debug, Clone)]
pub struct Theme {
    // Borders & focus
    /// Focused/active accent color. Used for borders, selected gutter, active tabs.
    pub focus_accent: Color,
    /// Unfocused/inactive border color.
    pub border_unfocused: Color,
    /// Popup window title color.
    pub popup_title: Color,

    // Text
    /// Primary content text color (assistant messages, user messages, values).
    pub primary_text: Color,
    /// Muted/dim text (system entries, descriptions, table separators).
    pub muted_text: Color,
    /// Subagent session title color (sidebar). Marks machine-spawned
    /// sessions apart from user-initiated ones.
    pub subagent_fg: Color,
    pub subagent_bg: Color,
    /// Attendant session rows in the session tree — distinct from the error
    /// colour and from the subagent pair.
    pub attendant_fg: Color,
    /// Marker for an attendant that will not dispatch on this run.
    ///
    /// Carries the streaming color's meaning — a session with no messages
    /// moving through it — without sharing the key, because a session can
    /// be both a stream's target and paused on its own row.
    pub attendant_paused: Color,
    /// Marker for an attendant that runs on its parent's completion, with
    /// no one asking. Defaulted to the attendant's own pink: the glyph
    /// marks the same rows `attendant_fg` already marks, and saying so
    /// with a second pink would be noise.
    pub attendant_parent_trigger: Color,
    pub attendant_bg: Color,
    /// Error text color.
    pub error_text: Color,
    /// Dormant-but-healthy state color (a passivated actor in the
    /// dashboard's State column). Deliberately not `error_text`: the actor
    /// is evicted for idleness and returns on the next send, so painting it
    /// as a failure turns a normal idle cycle into an incident.
    pub dormant_fg: Color,

    // Status
    /// Success/healthy status color (running actors, fresh data, active markers).
    pub success: Color,
    /// Warning/starting status color (stale data, starting actors).
    pub warning: Color,
    /// Streaming indicator color.
    pub streaming: Color,

    // Chat log backgrounds
    /// Gutter column background (left margin).
    pub gutter_bg: Color,
    /// Gutter color for entries included in context (not ignored or pinned).
    pub gutter_context_included: Color,
    /// User message block background.
    pub user_block_bg: Color,
    /// Tool entry text foreground (tool calls, tool results, skills).
    pub tool_fg: Color,
    /// Tool result success block background.
    pub tool_success_bg: Color,
    /// Tool result failure block background.
    pub tool_failure_bg: Color,
    /// Tool result pending (still executing) block background.
    pub tool_pending_bg: Color,
    /// Challenge alert block background (bright, attention-grabbing).
    pub challenge_alert_bg: Color,
    /// Challenge alert block foreground (dark text on the bright background).
    pub challenge_alert_fg: Color,
    /// Compaction summary block background.
    pub compaction_block_bg: Color,
    /// Rule interrupt block background — guidance the harness injected mid-turn.
    ///
    /// Deliberately not the user-block color: an interrupt must not read as
    /// something the user typed.
    pub rule_interrupt_bg: Color,
    /// Sources (annotation) header background — bright, attention-grabbing bar.
    pub sources_header_bg: Color,
    /// Sources (annotation) header foreground (dark text on the bright background).
    pub sources_header_fg: Color,
    /// Tool result truncation indicator foreground.
    pub truncation_fg: Color,
    /// Subagent band background — the padding rows above and below `task`
    /// tool calls and their results, marking the subagent card. A dark,
    /// low-contrast purple so status tints stay readable inside the card.

    // Picker
    /// Picker active item marker color (the `>` prefix).
    pub picker_active_marker: Color,
    /// Picker selected row background.
    pub picker_selected_bg: Color,
    /// Picker fuzzy match highlight background.
    pub picker_highlight_bg: Color,

    // Tab bar
    /// Active tab text color.
    pub tab_active_fg: Color,
    /// Active tab background color.
    pub tab_active_bg: Color,
    /// Inactive tab text color.
    pub tab_inactive_fg: Color,

    // Selection highlight
    /// Selection highlight foreground (fallback for identical fg/bg).
    pub selection_fg: Color,
    /// Selection highlight background (fallback for identical fg/bg).
    pub selection_bg: Color,

    // In-flight session tint
    /// Background wash for a row whose session has a disposal operation in flight.
    pub in_flight_bg: Color,
    /// Text color for a row whose session has a disposal operation in flight.
    pub in_flight_fg: Color,

    // Provider picker
    /// Accent color used for hotkeys.
    pub accent_action: Color,
    /// Fresh data age color.
    pub age_fresh: Color,
    /// Stale data age color.
    pub age_stale: Color,

    // Attendant properties popup
    /// The selected choice in an attendant properties row. Mirrors
    /// `age_fresh` by default but is an independent key, so the two can
    /// diverge without a theme-file breaking change.
    pub attendant_option_active: Color,

    // Scroll indicator
    /// Scroll indicator background.
    pub scroll_indicator_bg: Color,
    /// Sidebar resize mode border color.
    pub sidebar_resize_accent: Color,

    // Chat input submit-mode badge
    /// Color for the `[QUEUE]` mode badge on the chat input border.
    pub input_mode_queue: Color,
    /// Color for the `[STEER]` mode badge on the chat input border.
    pub input_mode_steer: Color,

    // Info popup
    /// Background color for info/debug popup overlays (audit, debug, etc.).
    pub infopopup_bg: Color,
    /// Title/header text color for info popups.
    pub infopopup_title: Color,
    /// Border color for info popups.
    pub infopopup_border: Color,
    /// Body text color for info popups.
    pub infopopup_fg: Color,

    // Quake bar
    /// Background color for the quake bar overlay.
    pub quake_bar_bg: Color,
}

impl Theme {
    /// Returns a map from theme field name to its resolved foreground style.
    ///
    /// Every key matches a `Theme` field name; the value is `Style::default().fg(color)`.
    /// This is the single source of truth for the badge style vocabulary.
    #[must_use]
    pub fn style_map(&self) -> HashMap<&'static str, Style> {
        /// One `m.insert` per theme field, `fg` by default.
        macro_rules! insert_style {
            ($m:ident, fg: { $($name:literal => $field:ident),* $(,)? }
                    bg: { $($bname:literal => $bfield:ident),* $(,)? }) => {
                $( $m.insert($name, Style::default().fg(self.$field)); )*
                $( $m.insert($bname, Style::default().bg(self.$bfield)); )*
            };
        }
        let mut m = HashMap::new();
        insert_style! {
            m, fg: {
            "focus_accent" => focus_accent,
            "border_unfocused" => border_unfocused,
            "popup_title" => popup_title,
            "primary_text" => primary_text,
            "muted_text" => muted_text,
            "subagent_fg" => subagent_fg,
            "attendant_fg" => attendant_fg,
            "attendant_paused" => attendant_paused,
            "attendant_parent_trigger" => attendant_parent_trigger,
            "error_text" => error_text,
            "dormant_fg" => dormant_fg,
            "success" => success,
            "warning" => warning,
            "streaming" => streaming,
            "gutter_bg" => gutter_bg,
            "gutter_context_included" => gutter_context_included,
            "user_block_bg" => user_block_bg,
            "tool_fg" => tool_fg,
            "tool_success_bg" => tool_success_bg,
            "tool_failure_bg" => tool_failure_bg,
            "tool_pending_bg" => tool_pending_bg,
            "challenge_alert_bg" => challenge_alert_bg,
            "challenge_alert_fg" => challenge_alert_fg,
            "compaction_block_bg" => compaction_block_bg,
            "rule_interrupt_bg" => rule_interrupt_bg,
            "sources_header_bg" => sources_header_bg,
            "sources_header_fg" => sources_header_fg,
            "truncation_fg" => truncation_fg,
            "picker_active_marker" => picker_active_marker,
            "picker_selected_bg" => picker_selected_bg,
            "picker_highlight_bg" => picker_highlight_bg,
            "tab_active_fg" => tab_active_fg,
            "tab_active_bg" => tab_active_bg,
            "tab_inactive_fg" => tab_inactive_fg,
            "selection_fg" => selection_fg,
            "selection_bg" => selection_bg,
            "in_flight_bg" => in_flight_bg,
            "in_flight_fg" => in_flight_fg,
            "accent_action" => accent_action,
            "age_fresh" => age_fresh,
            "age_stale" => age_stale,
            "attendant_option_active" => attendant_option_active,
            "scroll_indicator_bg" => scroll_indicator_bg,
            "sidebar_resize_accent" => sidebar_resize_accent,
            "input_mode_queue" => input_mode_queue,
            "input_mode_steer" => input_mode_steer,
            "infopopup_bg" => infopopup_bg,
            "infopopup_title" => infopopup_title,
            "infopopup_border" => infopopup_border,
            "infopopup_fg" => infopopup_fg,
            } bg: {
            "subagent_bg" => subagent_bg,
            "attendant_bg" => attendant_bg,
            "quake_bar_bg" => quake_bar_bg,
            }
        }
        m
    }
}

/// TOML-serializable theme file with optional fields.
///
/// All fields are `Option<ThemeColor>`. Missing fields are resolved from
/// the default theme when loading.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ThemeFile {
    #[serde(default)]
    pub focus_accent: Option<ThemeColor>,
    #[serde(default)]
    pub border_unfocused: Option<ThemeColor>,
    #[serde(default)]
    pub popup_title: Option<ThemeColor>,

    #[serde(default)]
    pub primary_text: Option<ThemeColor>,
    #[serde(default)]
    pub muted_text: Option<ThemeColor>,
    #[serde(default)]
    pub subagent_fg: Option<ThemeColor>,
    /// Subagent block background shown on `task` tool calls and their results.
    #[serde(default)]
    pub subagent_bg: Option<ThemeColor>,
    /// Attendant session rows in the session tree.
    #[serde(default)]
    pub attendant_fg: Option<ThemeColor>,
    #[serde(default)]
    pub attendant_paused: Option<ThemeColor>,
    #[serde(default)]
    pub attendant_parent_trigger: Option<ThemeColor>,
    /// Attendant row background, paired with `attendant_fg`.
    #[serde(default)]
    pub attendant_bg: Option<ThemeColor>,
    #[serde(default)]
    pub error_text: Option<ThemeColor>,
    pub dormant_fg: Option<ThemeColor>,

    #[serde(default)]
    pub success: Option<ThemeColor>,
    #[serde(default)]
    pub warning: Option<ThemeColor>,
    #[serde(default)]
    pub streaming: Option<ThemeColor>,

    #[serde(default)]
    pub gutter_bg: Option<ThemeColor>,
    #[serde(default)]
    pub gutter_context_included: Option<ThemeColor>,
    #[serde(default)]
    pub user_block_bg: Option<ThemeColor>,
    #[serde(default)]
    pub tool_fg: Option<ThemeColor>,
    #[serde(default)]
    pub tool_success_bg: Option<ThemeColor>,
    #[serde(default)]
    pub tool_failure_bg: Option<ThemeColor>,
    #[serde(default)]
    pub tool_pending_bg: Option<ThemeColor>,
    #[serde(default)]
    pub challenge_alert_bg: Option<ThemeColor>,
    #[serde(default)]
    pub challenge_alert_fg: Option<ThemeColor>,
    #[serde(default)]
    pub compaction_block_bg: Option<ThemeColor>,
    #[serde(default)]
    pub rule_interrupt_bg: Option<ThemeColor>,
    #[serde(default)]
    pub sources_header_bg: Option<ThemeColor>,
    #[serde(default)]
    pub sources_header_fg: Option<ThemeColor>,
    #[serde(default)]
    pub truncation_fg: Option<ThemeColor>,
    #[serde(default)]
    pub picker_active_marker: Option<ThemeColor>,
    #[serde(default)]
    pub picker_selected_bg: Option<ThemeColor>,
    #[serde(default)]
    pub picker_highlight_bg: Option<ThemeColor>,

    #[serde(default)]
    pub tab_active_fg: Option<ThemeColor>,
    #[serde(default)]
    pub tab_active_bg: Option<ThemeColor>,
    #[serde(default)]
    pub tab_inactive_fg: Option<ThemeColor>,

    #[serde(default)]
    pub selection_fg: Option<ThemeColor>,
    #[serde(default)]
    pub selection_bg: Option<ThemeColor>,

    /// Background wash for a row whose session has a disposal operation in flight.
    #[serde(default)]
    pub in_flight_bg: Option<ThemeColor>,
    /// Text color for a row whose session has a disposal operation in flight.
    #[serde(default)]
    pub in_flight_fg: Option<ThemeColor>,

    /// Accent color used for hotkeys.
    #[serde(default)]
    pub accent_action: Option<ThemeColor>,
    #[serde(default)]
    pub age_fresh: Option<ThemeColor>,
    #[serde(default)]
    pub age_stale: Option<ThemeColor>,
    #[serde(default)]
    pub attendant_option_active: Option<ThemeColor>,

    #[serde(default)]
    pub scroll_indicator_bg: Option<ThemeColor>,
    #[serde(default)]
    pub sidebar_resize_accent: Option<ThemeColor>,

    #[serde(default)]
    pub input_mode_queue: Option<ThemeColor>,
    #[serde(default)]
    pub input_mode_steer: Option<ThemeColor>,

    // Info popup
    #[serde(default)]
    pub infopopup_bg: Option<ThemeColor>,
    #[serde(default)]
    pub infopopup_title: Option<ThemeColor>,
    #[serde(default)]
    pub infopopup_border: Option<ThemeColor>,
    #[serde(default)]
    pub infopopup_fg: Option<ThemeColor>,

    // Quake bar
    #[serde(default)]
    pub quake_bar_bg: Option<ThemeColor>,
}

impl ThemeFile {
    /// Resolves this file into a full [`Theme`], filling missing fields
    /// from the default theme.
    #[must_use]
    pub fn resolve(&self) -> Theme {
        self.resolve_with_fallback(&default_theme())
    }

    /// Resolves this file into a full [`Theme`] using the given fallback theme.
    ///
    /// Missing fields (`None`) are filled from `fallback`. Used internally to
    /// break the circular dependency between [`default_theme`] and
    /// [`ThemeFile::resolve`].
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn resolve_with_fallback(&self, fallback: &Theme) -> Theme {
        Theme {
            focus_accent: self
                .focus_accent
                .map_or(fallback.focus_accent, crate::color::ThemeColor::inner),
            border_unfocused: self
                .border_unfocused
                .map_or(fallback.border_unfocused, crate::color::ThemeColor::inner),
            popup_title: self
                .popup_title
                .map_or(fallback.popup_title, crate::color::ThemeColor::inner),
            primary_text: self
                .primary_text
                .map_or(fallback.primary_text, crate::color::ThemeColor::inner),
            muted_text: self
                .muted_text
                .map_or(fallback.muted_text, crate::color::ThemeColor::inner),
            subagent_fg: self
                .subagent_fg
                .map_or(fallback.subagent_fg, crate::color::ThemeColor::inner),
            subagent_bg: self
                .subagent_bg
                .map_or(fallback.subagent_bg, crate::color::ThemeColor::inner),
            attendant_fg: self
                .attendant_fg
                .map_or(fallback.attendant_fg, crate::color::ThemeColor::inner),
            attendant_paused: self
                .attendant_paused
                .map_or(fallback.attendant_paused, crate::color::ThemeColor::inner),
            attendant_parent_trigger: self.attendant_parent_trigger.map_or(
                fallback.attendant_parent_trigger,
                crate::color::ThemeColor::inner,
            ),
            attendant_bg: self
                .attendant_bg
                .map_or(fallback.attendant_bg, crate::color::ThemeColor::inner),
            error_text: self
                .error_text
                .map_or(fallback.error_text, crate::color::ThemeColor::inner),
            dormant_fg: self
                .dormant_fg
                .map_or(fallback.dormant_fg, crate::color::ThemeColor::inner),
            success: self
                .success
                .map_or(fallback.success, crate::color::ThemeColor::inner),
            warning: self
                .warning
                .map_or(fallback.warning, crate::color::ThemeColor::inner),
            streaming: self
                .streaming
                .map_or(fallback.streaming, crate::color::ThemeColor::inner),

            gutter_bg: self
                .gutter_bg
                .map_or(fallback.gutter_bg, crate::color::ThemeColor::inner),
            gutter_context_included: self.gutter_context_included.map_or(
                fallback.gutter_context_included,
                crate::color::ThemeColor::inner,
            ),
            user_block_bg: self
                .user_block_bg
                .map_or(fallback.user_block_bg, crate::color::ThemeColor::inner),
            tool_fg: self
                .tool_fg
                .map_or(fallback.tool_fg, crate::color::ThemeColor::inner),
            tool_success_bg: self
                .tool_success_bg
                .map_or(fallback.tool_success_bg, crate::color::ThemeColor::inner),
            tool_failure_bg: self
                .tool_failure_bg
                .map_or(fallback.tool_failure_bg, crate::color::ThemeColor::inner),
            tool_pending_bg: self
                .tool_pending_bg
                .map_or(fallback.tool_pending_bg, crate::color::ThemeColor::inner),
            challenge_alert_bg: self
                .challenge_alert_bg
                .map_or(fallback.challenge_alert_bg, crate::color::ThemeColor::inner),
            challenge_alert_fg: self
                .challenge_alert_fg
                .map_or(fallback.challenge_alert_fg, crate::color::ThemeColor::inner),
            compaction_block_bg: self.compaction_block_bg.map_or(
                fallback.compaction_block_bg,
                crate::color::ThemeColor::inner,
            ),
            rule_interrupt_bg: self
                .rule_interrupt_bg
                .map_or(fallback.rule_interrupt_bg, crate::color::ThemeColor::inner),
            sources_header_bg: self
                .sources_header_bg
                .map_or(fallback.sources_header_bg, crate::color::ThemeColor::inner),
            sources_header_fg: self
                .sources_header_fg
                .map_or(fallback.sources_header_fg, crate::color::ThemeColor::inner),
            truncation_fg: self
                .truncation_fg
                .map_or(fallback.truncation_fg, crate::color::ThemeColor::inner),
            picker_active_marker: self.picker_active_marker.map_or(
                fallback.picker_active_marker,
                crate::color::ThemeColor::inner,
            ),
            picker_selected_bg: self
                .picker_selected_bg
                .map_or(fallback.picker_selected_bg, crate::color::ThemeColor::inner),
            picker_highlight_bg: self.picker_highlight_bg.map_or(
                fallback.picker_highlight_bg,
                crate::color::ThemeColor::inner,
            ),
            tab_active_fg: self
                .tab_active_fg
                .map_or(fallback.tab_active_fg, crate::color::ThemeColor::inner),
            tab_active_bg: self
                .tab_active_bg
                .map_or(fallback.tab_active_bg, crate::color::ThemeColor::inner),
            tab_inactive_fg: self
                .tab_inactive_fg
                .map_or(fallback.tab_inactive_fg, crate::color::ThemeColor::inner),
            selection_fg: self
                .selection_fg
                .map_or(fallback.selection_fg, crate::color::ThemeColor::inner),
            selection_bg: self
                .selection_bg
                .map_or(fallback.selection_bg, crate::color::ThemeColor::inner),
            in_flight_bg: self
                .in_flight_bg
                .map_or(fallback.in_flight_bg, crate::color::ThemeColor::inner),
            in_flight_fg: self
                .in_flight_fg
                .map_or(fallback.in_flight_fg, crate::color::ThemeColor::inner),
            accent_action: self
                .accent_action
                .map_or(fallback.accent_action, crate::color::ThemeColor::inner),
            age_fresh: self
                .age_fresh
                .map_or(fallback.age_fresh, crate::color::ThemeColor::inner),
            age_stale: self
                .age_stale
                .map_or(fallback.age_stale, crate::color::ThemeColor::inner),
            attendant_option_active: self.attendant_option_active.map_or(
                fallback.attendant_option_active,
                crate::color::ThemeColor::inner,
            ),
            scroll_indicator_bg: self.scroll_indicator_bg.map_or(
                fallback.scroll_indicator_bg,
                crate::color::ThemeColor::inner,
            ),
            sidebar_resize_accent: self.sidebar_resize_accent.map_or(
                fallback.sidebar_resize_accent,
                crate::color::ThemeColor::inner,
            ),

            input_mode_queue: self
                .input_mode_queue
                .map_or(fallback.input_mode_queue, crate::color::ThemeColor::inner),
            input_mode_steer: self
                .input_mode_steer
                .map_or(fallback.input_mode_steer, crate::color::ThemeColor::inner),
            infopopup_bg: self
                .infopopup_bg
                .map_or(fallback.infopopup_bg, crate::color::ThemeColor::inner),
            infopopup_title: self
                .infopopup_title
                .map_or(fallback.infopopup_title, crate::color::ThemeColor::inner),
            infopopup_border: self
                .infopopup_border
                .map_or(fallback.infopopup_border, crate::color::ThemeColor::inner),
            infopopup_fg: self
                .infopopup_fg
                .map_or(fallback.infopopup_fg, crate::color::ThemeColor::inner),
            quake_bar_bg: self
                .quake_bar_bg
                .map_or(fallback.quake_bar_bg, crate::color::ThemeColor::inner),
        }
    }

    /// Resolve a single optional ThemeColor into a Color (Reset when absent).
    fn resolve_field(field: Option<crate::color::ThemeColor>) -> Color {
        field.map_or(Color::Reset, crate::color::ThemeColor::inner)
    }

    /// Resolves this file without any fallback theme.
    ///
    /// Missing fields default to [`Color::Reset`]. Used by [`default_theme`]
    /// to avoid the circular dependency with [`ThemeFile::resolve`]. The
    /// embedded `default.toml` has all fields set, so `Reset` is never used.
    #[must_use]
    pub fn resolve_standalone(&self) -> Theme {
        Theme {
            focus_accent: Self::resolve_field(self.focus_accent),
            border_unfocused: Self::resolve_field(self.border_unfocused),
            popup_title: Self::resolve_field(self.popup_title),
            primary_text: Self::resolve_field(self.primary_text),
            muted_text: Self::resolve_field(self.muted_text),
            subagent_fg: Self::resolve_field(self.subagent_fg),
            subagent_bg: Self::resolve_field(self.subagent_bg),
            attendant_fg: Self::resolve_field(self.attendant_fg),
            attendant_paused: Self::resolve_field(self.attendant_paused),
            attendant_parent_trigger: Self::resolve_field(self.attendant_parent_trigger),
            attendant_bg: Self::resolve_field(self.attendant_bg),
            error_text: Self::resolve_field(self.error_text),
            dormant_fg: Self::resolve_field(self.dormant_fg),
            success: Self::resolve_field(self.success),
            warning: Self::resolve_field(self.warning),
            streaming: Self::resolve_field(self.streaming),

            gutter_bg: Self::resolve_field(self.gutter_bg),
            gutter_context_included: Self::resolve_field(self.gutter_context_included),
            user_block_bg: Self::resolve_field(self.user_block_bg),
            tool_fg: Self::resolve_field(self.tool_fg),
            tool_success_bg: Self::resolve_field(self.tool_success_bg),
            tool_failure_bg: Self::resolve_field(self.tool_failure_bg),
            tool_pending_bg: Self::resolve_field(self.tool_pending_bg),
            challenge_alert_bg: Self::resolve_field(self.challenge_alert_bg),
            challenge_alert_fg: Self::resolve_field(self.challenge_alert_fg),
            compaction_block_bg: Self::resolve_field(self.compaction_block_bg),
            rule_interrupt_bg: Self::resolve_field(self.rule_interrupt_bg),
            sources_header_bg: Self::resolve_field(self.sources_header_bg),
            sources_header_fg: Self::resolve_field(self.sources_header_fg),
            truncation_fg: Self::resolve_field(self.truncation_fg),
            picker_active_marker: Self::resolve_field(self.picker_active_marker),
            picker_selected_bg: Self::resolve_field(self.picker_selected_bg),
            picker_highlight_bg: Self::resolve_field(self.picker_highlight_bg),
            tab_active_fg: Self::resolve_field(self.tab_active_fg),
            tab_active_bg: Self::resolve_field(self.tab_active_bg),
            tab_inactive_fg: Self::resolve_field(self.tab_inactive_fg),
            selection_fg: Self::resolve_field(self.selection_fg),
            selection_bg: Self::resolve_field(self.selection_bg),
            in_flight_bg: Self::resolve_field(self.in_flight_bg),
            in_flight_fg: Self::resolve_field(self.in_flight_fg),
            accent_action: Self::resolve_field(self.accent_action),
            age_fresh: Self::resolve_field(self.age_fresh),
            age_stale: Self::resolve_field(self.age_stale),
            attendant_option_active: Self::resolve_field(self.attendant_option_active),
            scroll_indicator_bg: Self::resolve_field(self.scroll_indicator_bg),
            sidebar_resize_accent: Self::resolve_field(self.sidebar_resize_accent),
            infopopup_bg: Self::resolve_field(self.infopopup_bg),
            infopopup_title: Self::resolve_field(self.infopopup_title),
            infopopup_border: Self::resolve_field(self.infopopup_border),
            infopopup_fg: Self::resolve_field(self.infopopup_fg),
            quake_bar_bg: Self::resolve_field(self.quake_bar_bg),
            input_mode_queue: self
                .input_mode_queue
                .map_or(Color::Reset, crate::color::ThemeColor::inner),
            input_mode_steer: self
                .input_mode_steer
                .map_or(Color::Reset, crate::color::ThemeColor::inner),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]
    use super::*;

    /// A theme file with every field left unset, so resolution must fall back
    /// to the default theme for each one.
    fn all_fields_unset() -> ThemeFile {
        ThemeFile {
            focus_accent: None,
            border_unfocused: None,
            popup_title: None,
            primary_text: None,
            muted_text: None,
            subagent_fg: None,
            subagent_bg: None,
            attendant_fg: None,
            attendant_paused: None,
            attendant_parent_trigger: None,
            attendant_bg: None,
            error_text: None,
            dormant_fg: None,
            success: None,
            warning: None,
            streaming: None,

            gutter_bg: None,
            gutter_context_included: None,
            user_block_bg: None,
            tool_fg: None,
            tool_success_bg: None,
            tool_failure_bg: None,
            tool_pending_bg: None,
            challenge_alert_bg: None,
            challenge_alert_fg: None,
            compaction_block_bg: None,
            rule_interrupt_bg: None,
            sources_header_bg: None,
            sources_header_fg: None,
            truncation_fg: None,
            picker_active_marker: None,
            picker_selected_bg: None,
            picker_highlight_bg: None,
            tab_active_fg: None,
            tab_active_bg: None,
            tab_inactive_fg: None,
            selection_fg: None,
            selection_bg: None,
            in_flight_bg: None,
            in_flight_fg: None,
            accent_action: None,
            age_fresh: None,
            age_stale: None,
            attendant_option_active: None,
            scroll_indicator_bg: None,
            sidebar_resize_accent: None,
            input_mode_queue: None,
            input_mode_steer: None,
            infopopup_bg: None,
            infopopup_title: None,
            infopopup_border: None,
            infopopup_fg: None,
            quake_bar_bg: None,
        }
    }

    /// A theme file with every field set to a distinct, concrete color.
    fn all_fields_set() -> ThemeFile {
        ThemeFile {
            focus_accent: Some(ThemeColor(Color::Rgb(255, 0, 0))),
            border_unfocused: Some(ThemeColor(Color::DarkGray)),
            primary_text: Some(ThemeColor(Color::White)),
            muted_text: Some(ThemeColor(Color::DarkGray)),
            subagent_fg: Some(ThemeColor(Color::Rgb(152, 128, 208))),
            subagent_bg: Some(ThemeColor(Color::Rgb(70, 58, 105))),
            attendant_fg: Some(ThemeColor(Color::Rgb(255, 105, 180))),
            attendant_bg: Some(ThemeColor(Color::Rgb(56, 48, 17))),
            attendant_paused: Some(ThemeColor(Color::Rgb(128, 132, 144))),
            attendant_parent_trigger: Some(ThemeColor(Color::Rgb(255, 105, 180))),
            error_text: Some(ThemeColor(Color::Red)),
            dormant_fg: Some(ThemeColor(Color::Blue)),
            success: Some(ThemeColor(Color::Green)),
            warning: Some(ThemeColor(Color::Yellow)),
            streaming: Some(ThemeColor(Color::Cyan)),

            gutter_bg: Some(ThemeColor(Color::Rgb(25, 27, 30))),
            gutter_context_included: Some(ThemeColor(Color::Rgb(30, 50, 110))),
            user_block_bg: Some(ThemeColor(Color::Rgb(52, 53, 65))),
            tool_fg: Some(ThemeColor(Color::Rgb(88, 95, 106))),
            tool_success_bg: Some(ThemeColor(Color::Rgb(40, 50, 40))),
            tool_failure_bg: Some(ThemeColor(Color::Rgb(60, 40, 40))),
            tool_pending_bg: Some(ThemeColor(Color::Rgb(45, 45, 50))),
            challenge_alert_bg: Some(ThemeColor(Color::Rgb(255, 200, 0))),
            challenge_alert_fg: Some(ThemeColor(Color::Rgb(16, 16, 16))),
            compaction_block_bg: Some(ThemeColor(Color::Rgb(60, 50, 80))),
            // Dark amber: near-black like the user block's base value but
            // yellow-biased, so it reads as harness steering at a glance.
            rule_interrupt_bg: Some(ThemeColor(Color::Rgb(72, 61, 20))),
            sources_header_bg: Some(ThemeColor(Color::Rgb(255, 200, 0))),
            sources_header_fg: Some(ThemeColor(Color::Rgb(16, 16, 16))),
            truncation_fg: Some(ThemeColor(Color::Rgb(83, 83, 83))),
            picker_active_marker: Some(ThemeColor(Color::Green)),
            picker_selected_bg: Some(ThemeColor(Color::DarkGray)),
            picker_highlight_bg: Some(ThemeColor(Color::DarkGray)),
            tab_active_fg: Some(ThemeColor(Color::Black)),
            tab_active_bg: Some(ThemeColor(Color::Yellow)),
            tab_inactive_fg: Some(ThemeColor(Color::Gray)),
            selection_fg: Some(ThemeColor(Color::Black)),
            selection_bg: Some(ThemeColor(Color::White)),
            in_flight_bg: None,
            in_flight_fg: None,
            accent_action: Some(ThemeColor(Color::Rgb(255, 165, 0))),
            age_fresh: Some(ThemeColor(Color::LightGreen)),
            age_stale: Some(ThemeColor(Color::Red)),
            attendant_option_active: Some(ThemeColor(Color::LightGreen)),
            scroll_indicator_bg: Some(ThemeColor(Color::Black)),
            sidebar_resize_accent: Some(ThemeColor(Color::Green)),
            infopopup_bg: Some(ThemeColor(Color::Rgb(40, 44, 52))),
            infopopup_title: Some(ThemeColor(Color::Yellow)),
            infopopup_border: Some(ThemeColor(Color::Cyan)),
            infopopup_fg: Some(ThemeColor(Color::Rgb(220, 220, 220))),
            quake_bar_bg: Some(ThemeColor(Color::Rgb(42, 28, 24))),
            popup_title: Some(ThemeColor(Color::Yellow)),
            input_mode_queue: Some(ThemeColor(Color::DarkGray)),
            input_mode_steer: Some(ThemeColor(Color::Magenta)),
        }
    }

    /// An all-unset theme file with only `focus_accent` overridden.
    fn only_focus_accent_set() -> ThemeFile {
        ThemeFile {
            focus_accent: Some(ThemeColor(Color::Red)),
            ..all_fields_unset()
        }
    }

    #[rstest::rstest]
    fn empty_theme_file_resolves_to_default() {
        // Given an empty theme file (all fields unset).
        let file = all_fields_unset();

        // When resolving.
        let theme = file.resolve();
        let default = default_theme();

        // Then all fields match the default theme.
        assert_eq!(theme.focus_accent, default.focus_accent);
        assert_eq!(theme.muted_text, default.muted_text);
        assert_eq!(theme.popup_title, default.popup_title);
        assert_eq!(theme.gutter_bg, default.gutter_bg);
        assert_eq!(theme.sidebar_resize_accent, default.sidebar_resize_accent);
        assert_eq!(theme.infopopup_bg, default.infopopup_bg);
        assert_eq!(theme.infopopup_title, default.infopopup_title);
        assert_eq!(theme.infopopup_border, default.infopopup_border);
        assert_eq!(theme.infopopup_fg, default.infopopup_fg);
    }

    #[rstest::rstest]
    fn partial_theme_file_overrides_only_specified() {
        // Given a theme file with only focus_accent set.
        let file = only_focus_accent_set();

        // When resolving.
        let theme = file.resolve();
        let default = default_theme();

        // Then focus_accent is overridden.
        assert_eq!(theme.focus_accent, Color::Red);
        // And other fields remain default.
        assert_eq!(theme.muted_text, default.muted_text);
        assert_eq!(theme.gutter_bg, default.gutter_bg);
    }

    #[rstest::rstest]
    fn theme_file_from_toml() {
        // Given a TOML string with one field.
        let toml_str = "focus_accent = \"red\"";
        let file: ThemeFile = toml::from_str(toml_str).expect("parse");

        // When resolving.
        let theme = file.resolve();

        // Then focus_accent is Red and everything else is default.
        assert_eq!(theme.focus_accent, Color::Red);
        assert_eq!(theme.muted_text, default_theme().muted_text);
    }

    #[rstest::rstest]
    fn theme_file_round_trip() {
        // Given a theme file with all fields set.
        let original = all_fields_set();

        // When serializing to TOML and back.
        let toml_str = toml::to_string(&original).expect("serialize");
        let restored: ThemeFile = toml::from_str(&toml_str).expect("parse");

        // Then the resolved themes are identical.
        assert_eq!(
            original.resolve().focus_accent,
            restored.resolve().focus_accent
        );
        assert_eq!(original.resolve().gutter_bg, restored.resolve().gutter_bg);
        assert_eq!(
            original.resolve().subagent_fg,
            restored.resolve().subagent_fg
        );
        assert_eq!(
            original.resolve().subagent_bg,
            restored.resolve().subagent_bg
        );
        assert_eq!(
            original.resolve().input_mode_queue,
            restored.resolve().input_mode_queue
        );
        assert_eq!(
            original.resolve().input_mode_steer,
            restored.resolve().input_mode_steer
        );
    }

    #[rstest::rstest]
    #[test]
    fn input_mode_fields_fall_back_when_absent() {
        // Given a ThemeFile with both input_mode fields explicitly None.
        let fallback = crate::default_theme::default_theme();
        let sparse = all_fields_unset();

        // When resolving with the fallback.
        let resolved = sparse.resolve_with_fallback(&fallback);

        // Then both fields take the fallback values.
        assert_eq!(resolved.input_mode_queue, fallback.input_mode_queue);
        assert_eq!(resolved.input_mode_steer, fallback.input_mode_steer);
    }
}
