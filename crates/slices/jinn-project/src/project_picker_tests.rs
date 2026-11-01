// Copyright (C) 2026 Jayson Lennon
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as
// published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! The project picker's observable behavior.
//!
//! The picker's rows come from curated project directories, so the tests seed
//! the cell directly and drive the real render pass: what the widget lays out
//! is what the navigation keys must page by, and the cell must carry exactly
//! that.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test module, panics are acceptable"
)]

use jinn_project_msg::ProjectPickerState;
use jinn_slices::Slices;
use jinn_slices::cell::TypedCell;

/// The slice's registry with its picker cell seeded, the way a render frame
/// sees it: the cell the render pass resolves through `RenderFacts` and the
/// cell the assertions read are the same one.
struct Wired {
    slices: Slices,
    cell: TypedCell<ProjectPickerState>,
}

impl Wired {
    /// Builds the registry with `projects` already loaded into the cell.
    fn new(projects: &[&str]) -> Self {
        let slices = Slices::new();
        jinn_cell_catalog::register_all_cells(&slices);
        let cell = slices
            .reader::<ProjectPickerState>(&jinn_project_msg::project_picker_slot())
            .expect("the catalog registers the project-picker cell before any slice activates");
        cell.update(|picker: &mut ProjectPickerState| {
            crate::project_picker_actions::open(
                picker,
                &projects_for(projects),
                &jinn_theme::default_theme(),
            );
        });
        Self { slices, cell }
    }

    /// Draws one frame of the picker at `area` through the real render pass.
    fn draw(&self, area: ratatui::layout::Rect) {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let facts = jinn_slices::RenderFacts::new(jinn_theme::default_theme(), &self.slices);
        let mut terminal =
            Terminal::new(TestBackend::new(area.width, area.height)).expect("terminal");
        terminal
            .draw(|frame| {
                let popup = crate::project_picker_render::project_picker_overlay_rect(&area)
                    .expect("geometry fn yields a popup rect");
                crate::project_picker_render::render_project_picker(frame, popup, &facts);
            })
            .expect("draw");
    }
}

/// `ProjectConfig` rows for the named paths.
fn projects_for(paths: &[&str]) -> Vec<jinn_preferences_config::schemas::ProjectConfig> {
    #[expect(
        clippy::disallowed_types,
        reason = "fills the schema-compat [[project.entry]] field, which is the raw TOML value by design"
    )]
    let to_row = |path: &str| jinn_preferences_config::schemas::ProjectConfig {
        path: std::path::PathBuf::from(path),
    };
    paths.iter().map(|path| to_row(path)).collect()
}

/// Draws one frame through the picker's real render pass, then re-draws the
/// identical widget construction into a mirror frame to learn what the widget
/// reported — the expectation is a widget-returned `PickerLayout`, never hand
/// arithmetic.
fn draw_and_report(
    wired: &Wired,
    area: ratatui::layout::Rect,
) -> jinn_selection_widget::PickerLayout {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::cell::Cell;

    wired.draw(area);

    // The identical second construction: the same widget builder the render
    // pass calls, against the same cell state and popup rect.
    let report = Cell::new(None);
    let mut mirror = Terminal::new(TestBackend::new(area.width, area.height)).expect("terminal");
    mirror
        .draw(|frame| {
            let guard = wired.cell.read();
            let popup = crate::project_picker_render::project_picker_overlay_rect(&area)
                .expect("geometry fn yields a popup rect");
            let widget = crate::project_picker_render::project_picker_widget(
                &guard,
                &jinn_theme::default_theme(),
            );
            report.set(Some(widget.render(frame, popup)));
        })
        .expect("mirror draw");
    report.into_inner().expect("render reports a PickerLayout")
}

#[rstest::rstest]
#[test]
fn the_unmeasured_viewport_is_the_msg_crate_fallback() {
    // Given a picker state as the cell catalog seeds it.
    let state = ProjectPickerState::default();

    // When reading the viewport no frame has measured yet.
    // Then it is the documented fallback, not zero — paging before the first
    // frame behaves the way it always has.
    assert_eq!(
        state.results_viewport,
        jinn_project_msg::RESULTS_VIEWPORT_FALLBACK
    );
}

#[rstest::rstest]
#[test]
fn drawing_a_frame_publishes_the_row_count_the_widget_laid_out() {
    // Given a picker holding more projects than a popup can show.
    let projects: Vec<String> = (0..40).map(|i| format!("/p{i:02}")).collect();
    let borrowed: Vec<&str> = projects.iter().map(String::as_str).collect();
    let wired = Wired::new(&borrowed);

    // When a frame is drawn through the real render pass at 120x50.
    let frame = ratatui::layout::Rect::new(0, 0, 120, 50);
    let reported = draw_and_report(&wired, frame);

    // Then the cell carries exactly what the widget reported for that frame.
    let published = wired.cell.read().results_viewport;
    assert_eq!(published, usize::from(reported.inner.height));
    assert_ne!(
        published,
        jinn_project_msg::RESULTS_VIEWPORT_FALLBACK,
        "the measurement must replace the fallback rather than sit beside it"
    );
}

#[rstest::rstest]
#[test]
fn the_published_row_count_still_matches_the_arithmetic_it_replaced() {
    // Given a picker holding more projects than a popup can show.
    let projects: Vec<String> = (0..40).map(|i| format!("/p{i:02}")).collect();
    let borrowed: Vec<&str> = projects.iter().map(String::as_str).collect();
    let wired = Wired::new(&borrowed);

    // When a frame is drawn through the real render pass at 120x50.
    let frame = ratatui::layout::Rect::new(0, 0, 120, 50);
    wired.draw(frame);

    // Then the published count is the 36 rows the deleted arithmetic produced
    // at this size (a 41-row popup, less borders, input, separator, and the
    // one keybind footer) — the layout report agreeing with its predecessor.
    let published = wired.cell.read().results_viewport;
    assert_eq!(published, 36);
}
