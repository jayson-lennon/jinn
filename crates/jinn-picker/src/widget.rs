//! Widget flavor and preview configuration — the geometry inputs of a spec.

/// Which selection-widget chrome a picker renders with.
///
/// Drives the erased render dispatch and the geometry math only — never
/// keybinds (those are authored as bind rows).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum PickerWidget {
    /// Flat filtered list (`SelectionWidget`).
    #[default]
    List,
    /// Hierarchical tree (`TreePickerWidget`).
    Tree,
    /// List plus a preview pane (`PreviewSelectionWidget`).
    Preview(PreviewSpec),
}

/// Preview-pane behavior for [`PickerWidget::Preview`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewSpec {
    /// Reset the preview scroll to 0 whenever the selection changes.
    /// Reproduces the skill picker's follow-the-cursor behavior.
    pub reset_scroll_on_selection_change: bool,
}

/// Widget-flavor query used by geometry math.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WidgetKind {
    /// Flat list.
    List,
    /// Tree.
    Tree,
    /// Preview split (vertical on wide terminals, stacked on narrow).
    Preview,
}

impl PickerWidget {
    /// The geometry-relevant flavor of this widget configuration.
    #[must_use]
    pub fn kind(&self) -> WidgetKind {
        match self {
            Self::List => WidgetKind::List,
            Self::Tree => WidgetKind::Tree,
            Self::Preview(_) => WidgetKind::Preview,
        }
    }
}
