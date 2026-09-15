#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "test code"
)]

//! Provider picker render tests.

use crate::common::app_state::{AppState, FocusScope};
use crate::common::render_ctx::RenderCtx;
use crate::common::services::Services;
use crate::feat::provider_infra::{ProviderEntry, ProvidersConfig};
use crate::protocol::PickerKind;
use jinn_selection_widget::compute_popup_rect;
use jinn_testutil::setup_term;
use ratatui::layout::Rect;
use ratatui::style::Color;
use std::collections::BTreeMap;

use super::loader::load_provider_picker_items;
use super::render::render_provider_picker;
use crate::feat::session::model_selection::ModelSelection;

fn picker_state_with_ollama() -> (AppState, Services) {
    let config = ProvidersConfig {
        providers: BTreeMap::from([(
            "ollama".to_owned(),
            ProviderEntry {
                model_info: Vec::new(),
                backend: "ollama".to_owned(),
                models: vec!["llama3".to_owned()],
                base_url: Some("http://localhost:11434".to_owned()),
                api_key_env: None,
                requires_key: false,
                extra_body: None,
                context_length: None,
            },
        )]),
        aliases: vec![],
        default_provider: None,
    };
    let services = crate::common::services::test_services::TestServices::builder()
        .with_providers(config)
        .build();
    (AppState::default_with_scope_focus(), services)
}

/// Helper to load provider entries into the picker state.
fn load_picker_items(state: &mut AppState, services: &Services) {
    let mut view = crate::common::tcaps::provider::ProviderView::from_app_state_for_test(state);
    load_provider_picker_items(services, &mut view);
}

#[rstest::rstest]
fn render_provider_picker_shows_telescope_layout() {
    // Given a terminal area and picker state with filter "ol".

    let (mut state, services) = picker_state_with_ollama();
    state.frontend.scope_push(FocusScope::Picker {
        kind: PickerKind::Provider,
    });
    load_picker_items(&mut state, &services);
    state.provider.provider_picker.insert_char('o');
    state.provider.provider_picker.insert_char('l');

    let (mut terminal, _area) = setup_term(80, 24);

    // When rendering the picker.
    terminal
        .draw(|frame| {
            let area = frame.area();
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            render_provider_picker(frame, area, &ctx);
        })
        .unwrap();

    // Then the popup contains the filter text with "> ol".
    let buffer = terminal.backend().buffer().clone();
    let popup = compute_popup_rect(Rect::new(0, 0, 80, 24));
    // Filter is on the first inner row: popup.y + 1
    let filter_y = popup.y + 1;
    let filter_cell = buffer.cell((popup.x + 1, filter_y)).expect("filter cell");
    assert_eq!(filter_cell.symbol(), ">");

    // Separator is on the second inner row.
    let sep_y = popup.y + 2;
    let sep_cell = buffer.cell((popup.x + 1, sep_y)).expect("sep cell");
    assert_eq!(sep_cell.symbol(), "\u{2500}");
}

#[rstest::rstest]
fn render_provider_picker_uses_dark_gray_border() {
    // Given a picker render.

    let (mut state, services) = picker_state_with_ollama();
    load_picker_items(&mut state, &services);

    let (mut terminal, _area) = setup_term(80, 24);

    terminal
        .draw(|frame| {
            let area = frame.area();
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            render_provider_picker(frame, area, &ctx);
        })
        .unwrap();

    // Then the border color is DarkGray, not Yellow.
    let buffer = terminal.backend().buffer().clone();
    let popup = compute_popup_rect(Rect::new(0, 0, 80, 24));
    let border_cell = buffer.cell((popup.x, popup.y)).expect("border cell");
    assert_eq!(border_cell.fg, Color::DarkGray);
}

#[rstest::rstest]
fn render_provider_picker_no_active_marker_for_active_model() {
    // Given a state with active_provider set to "ollama/llama3" and items loaded.

    let (mut state, services) = picker_state_with_ollama();
    state.frontend.scope_push(FocusScope::Picker {
        kind: PickerKind::Provider,
    });
    state
        .active_session_mut()
        .set_model(ModelSelection::Single("ollama/llama3".to_owned()));
    load_picker_items(&mut state, &services);

    let (mut terminal, _area) = setup_term(80, 24);

    // When rendering the picker.
    terminal
        .draw(|frame| {
            let area = frame.area();
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            render_provider_picker(frame, area, &ctx);
        })
        .unwrap();

    // Then the first result row does not contain ">".
    let buffer = terminal.backend().buffer().clone();
    let popup = compute_popup_rect(Rect::new(0, 0, 80, 24));
    // Results start at popup.y + 3 (border + input + separator)
    let result_y = popup.y + 3;
    // The first 2 chars are selection_marker (spaces, no checkmark since not selected).
    let marker_cell = buffer.cell((popup.x + 3, result_y)).expect("marker cell");
    assert_ne!(marker_cell.symbol(), ">");
}
