#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test file, panics are acceptable"
)]

use super::render::*;
use jinn_domain::FocusScope;
use jinn_domain::feat::session::chat_entry::ChatEntry;
use jinn_domain::feat::ui::chat_log::GUTTER_WIDTH;
use jinn_selection_widget::compute_popup_rect;
use jinn_testutil::setup_term;
use ratatui::layout::Rect;
use ratatui::style::Color;

/// Creates a minimal `TuiApp` for render testing.
///
/// Mirrors production composition (`actor_wiring`) by populating the
/// picker spec registry — spec-driven pickers render (and refresh) only
/// through specs the registry holds.
async fn render_test_app() -> crate::TuiApp {
    let services = jinn_domain::Services {
        picker_registry: jinn_domain::feat::picker::registry::build_picker_registry(),
        ..jinn_domain::Services::new_fake().await
    };
    crate::TuiApp::test_builder()
        .services(services)
        .build()
        .await
}

#[rstest::rstest]
#[tokio::test]
async fn render_registers_content_rect_for_selectable_chat_log() {
    // Given a TuiApp rendered in Chat tab with a 80x24 terminal.

    let mut app = render_test_app().await;
    // Default tab is Chat.

    let (mut terminal, _area) = setup_term(80, 24);

    // When rendering.
    terminal
        .draw(|frame| {
            app.render(frame);
        })
        .unwrap();

    // Then the chat area rect is registered as selectable, excluding the gutter.
    // Chat log is selectable - the selectable area starts after the gutter column.
    let layout = AppLayout::new(frame_area(80, 24), 1, 12, 30);
    let content = layout.content;
    let expected = Rect {
        x: content.x + GUTTER_WIDTH,
        y: content.y,
        width: content.width.saturating_sub(GUTTER_WIDTH),
        height: content.height,
    };
    let found = app
        .selectable_rects
        .find_for_position(expected.x + 1, expected.y + 1);
    assert!(
        found.is_some(),
        "chat log content rect should be selectable"
    );
    assert_eq!(found.unwrap(), expected);
}

#[rstest::rstest]
#[tokio::test]
async fn picker_popup_rect_is_selectable() {
    // Given a TuiApp rendered with Mode::Picker.

    let mut app = render_test_app().await;
    // Switch to Picker mode with an active provider picker.
    app.core
        .state
        .write_test_no_cap()
        .frontend
        .scope_push(jinn_domain::FocusScope::Picker {
            kind: jinn_domain::PickerKind::Provider,
        });

    let (mut terminal, _area) = setup_term(80, 24);

    // When rendering.
    terminal
        .draw(|frame| {
            app.render(frame);
        })
        .unwrap();

    // Then the picker popup rect is registered as selectable.
    let popup_rect = compute_popup_rect(Rect::new(0, 0, 80, 24));
    // Query position inside popup but outside the content area (popup extends
    // further right than the content column which ends at the border).
    let outside_content_x = popup_rect.x + popup_rect.width.saturating_sub(5);
    let found = app.selectable_rects.find_for_position(outside_content_x, 0);
    assert!(found.is_some(), "picker popup rect should be selectable");
    assert_eq!(found.unwrap(), popup_rect);
}

#[rstest::rstest]
#[tokio::test]
async fn content_area_rect_is_selectable() {
    // Given a TuiApp rendered with Mode::Picker.

    let mut app = render_test_app().await;
    // Switch to Picker mode with an active provider picker.
    app.core
        .state
        .write_test_no_cap()
        .frontend
        .scope_push(jinn_domain::FocusScope::Picker {
            kind: jinn_domain::PickerKind::Provider,
        });

    let (mut terminal, _area) = setup_term(80, 24);

    // When rendering.
    terminal
        .draw(|frame| {
            app.render(frame);
        })
        .unwrap();

    // Then the content area rect is also still selectable (chat-log is selectable).
    // Query a position inside the gutter-excluded selectable rect.
    let layout = AppLayout::new(frame_area(80, 24), 1, 12, 30);
    let content = layout.content;
    let select_x = content.x + GUTTER_WIDTH + 1;
    let content_found = app
        .selectable_rects
        .find_for_position(select_x, content.y + 1);
    assert!(
        content_found.is_some(),
        "content rect should also be selectable alongside picker"
    );
}

/// Helper to create a Rect matching the terminal dimensions.
fn frame_area(w: u16, h: u16) -> Rect {
    Rect::new(0, 0, w, h)
}

/// Helper to find the minimap arrow cell position.
///
/// The arrow renders at the rightmost column of the chat_log_area at the
/// midpoint row (chat_log_height / 2). The chat_log_area is the content area
/// minus 2 bottom lines.
fn arrow_cell_position(layout: &AppLayout) -> (u16, u16) {
    let bottom_lines: u16 = 2;
    let chat_log_height = layout.content.height.saturating_sub(bottom_lines);
    let midpoint = chat_log_height / 2;
    let x = layout.content.x + layout.content.width.saturating_sub(1);
    let y = layout.content.y + midpoint;
    (x, y)
}

#[rstest::rstest]
#[tokio::test]
async fn minimap_arrow_is_yellow_when_normal_scope() {
    // Given a TuiApp rendered with Normal scope and one chat entry.
    let mut app = render_test_app().await;
    app.core
        .state
        .write_test_no_cap()
        .frontend
        .scope_clear_overlays();
    app.core
        .state
        .write_test_no_cap()
        .active_session_mut()
        .push_entry(ChatEntry::user("hello"));
    let (mut terminal, _area) = setup_term(80, 24);

    // When rendering.
    terminal
        .draw(|frame| {
            app.render(frame);
        })
        .unwrap();

    // Then the minimap arrow is Yellow (focus_accent).
    let layout = AppLayout::new(frame_area(80, 24), 1, 12, 30);
    let (x, y) = arrow_cell_position(&layout);
    let buffer = terminal.backend().buffer();
    let cell = buffer.cell((x, y)).expect("minimap arrow cell");
    assert_eq!(cell.symbol(), ">");
    assert_eq!(cell.fg, Color::Yellow);
}

#[rstest::rstest]
#[tokio::test]
async fn minimap_arrow_is_darkgray_when_input_scope() {
    // Given a TuiApp rendered with Input scope and one chat entry.
    let mut app = render_test_app().await;
    app.core
        .state
        .write_test_no_cap()
        .frontend
        .scope_push(FocusScope::Input);
    app.core
        .state
        .write_test_no_cap()
        .active_session_mut()
        .push_entry(ChatEntry::user("hello"));
    let (mut terminal, _area) = setup_term(80, 24);

    // When rendering.
    terminal
        .draw(|frame| {
            app.render(frame);
        })
        .unwrap();

    // Then the minimap arrow is DarkGray (border_unfocused).
    let layout = AppLayout::new(frame_area(80, 24), 1, 12, 30);
    let (x, y) = arrow_cell_position(&layout);
    let buffer = terminal.backend().buffer();
    let cell = buffer.cell((x, y)).expect("minimap arrow cell");
    assert_eq!(cell.fg, Color::DarkGray);
}

#[rstest::rstest]
#[tokio::test]
async fn gutter_area_is_not_selectable() {
    // Given a TuiApp rendered in Chat tab with a 80x24 terminal.
    let mut app = render_test_app().await;
    let (mut terminal, _area) = setup_term(80, 24);

    // When rendering.
    terminal
        .draw(|frame| {
            app.render(frame);
        })
        .unwrap();

    // Then clicking in the gutter (first column of content area) is not selectable.
    let layout = AppLayout::new(frame_area(80, 24), 1, 12, 30);
    let content = layout.content;
    let found = app
        .selectable_rects
        .find_for_position(content.x, content.y + 1);
    assert!(found.is_none(), "gutter area should not be selectable");
}

#[rstest::rstest]
#[tokio::test]
async fn cwd_input_popup_renders_and_is_selectable() {
    // Given a TuiApp rendered with the cwd popup's dynamic scope, the cwd
    // slice activated so its overlay + cell are registered.
    let mut app = render_test_app().await;
    {
        let services = &mut app.services;
        let mut host = jinn_slices::SliceHost::new(
            &services.slices,
            &mut services.viewport,
            &services.overlay_views,
            &services.key_routes,
            &services.trouper_system,
        );
        jinn_cwd::activate(&mut host);
        if let Err(error) = host.finalize(&|_key| None) {
            panic!("cwd slice finalize failed: {error}");
        }
    }
    app.core
        .state
        .write_test_no_cap()
        .frontend
        .scope_push(FocusScope::Dynamic(jinn_cwd::cwd_scope()));
    let (mut terminal, _area) = setup_term(80, 24);

    // When rendering.
    terminal
        .draw(|frame| {
            app.render(frame);
        })
        .unwrap();

    // Then the cwd popup rect is registered as selectable.
    let popup_rect = jinn_cwd::cwd_input_popup_rect(frame_area(80, 24));
    let probe = app
        .selectable_rects
        .find_for_position(popup_rect.x + 1, popup_rect.y + 1);
    assert!(probe.is_some(), "cwd input popup rect should be selectable");
    assert_eq!(probe.unwrap(), popup_rect);
}

#[rstest::rstest]
#[tokio::test]
async fn chat_layout_still_draws_vertical_border_for_sidebar() {
    // Given a TuiApp rendered in the default Chat tab (sidebar width 30).
    let mut app = render_test_app().await;
    let (mut terminal, _area) = setup_term(80, 24);
    terminal
        .draw(|frame| {
            app.render(frame);
        })
        .unwrap();

    // When reading the cell at the chat border column on a content row.
    let layout = AppLayout::new(frame_area(80, 24), 1, 12, 30);
    let buffer = terminal.backend().buffer();
    let cell = buffer
        .cell((layout.border.x, layout.content.y + 1))
        .expect("chat border cell");

    // Then the vertical border glyph (│) is drawn — chat rendering is unchanged.
    assert_eq!(
        cell.symbol(),
        "\u{2502}",
        "chat tab must still render the sidebar border (regression guard)",
    );
}

#[rstest::rstest]
#[tokio::test]
async fn mcp_inspector_renders_server_list_and_logs_pane() {
    // Given the MCP server inspector open with one server selected + running.
    let mut app = render_test_app().await;
    {
        use jinn_domain::feat::picker::mcp_picker_entry::McpServerEntry;
        use jinn_domain::feat::theme::default_theme;
        use jinn_domain::feat::ui::picker_states::PickerExt;
        use jinn_mcp_msg::McpConnectionStatus;
        let mut w = app.core.state.write_test_no_cap();
        // Seed the active session's live data sources so the per-frame refresh
        // produces the right preview.
        w.active_session_mut()
            .set_mcp_server_status("excalimate", McpConnectionStatus::Running);
        w.active_session_mut()
            .set_mcp_server_stderr("excalimate", "hello from stderr".to_owned());
        let entry = McpServerEntry::new(
            "excalimate".to_owned(),
            "npx @excalimate/mcp-server".to_owned(),
            true,
            default_theme(),
        );
        // Wrap the entry through the spec (storage holds PickerEntry<T>).
        let wrapped = jinn_domain::feat::picker::registry::build_picker_registry()
            .make_items(
                jinn_domain::feat::picker::registry::MCP_SERVER_ID,
                vec![entry],
            )
            .expect("mcp-server spec registered");
        w.frontend.mcp_server_picker_mut().set_items(wrapped);
        w.frontend.scope_push(jinn_domain::FocusScope::Picker {
            kind: jinn_domain::PickerKind::McpServer,
        });
    }

    let (mut terminal, _area) = setup_term(100, 30);

    // When rendering.
    terminal
        .draw(|frame| {
            app.render(frame);
        })
        .unwrap();

    // Then the buffer mentions the server name, the logs badge, and the stderr tail.
    let buf = terminal.backend().buffer();
    let rendered: String = buf
        .content
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect();
    assert!(
        rendered.contains("excalimate"),
        "server list shows the server name"
    );
    assert!(
        rendered.contains("running"),
        "logs pane shows the status badge"
    );
    assert!(
        rendered.contains("hello from stderr"),
        "logs pane shows the stderr tail"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn mcp_inspector_tools_pane_renders_tool_names() {
    // Given the MCP inspector open in Tools mode with one advertised tool.
    let mut app = render_test_app().await;
    {
        use jinn_domain::feat::picker::mcp_picker_entry::{McpPreviewMode, McpServerEntry};
        use jinn_domain::feat::theme::default_theme;
        use jinn_domain::feat::ui::picker_states::PickerExt;
        let registry_cell = app
            .services
            .slices
            .reader::<jinn_tools_msg::ToolRegistry>(&jinn_tools_msg::tools_registry_slot())
            .expect("tools registry seeded by render test app");
        let session_id = {
            let w = app.core.state.read();
            w.active_session().session_id().clone()
        };
        // Seed a tool definition so the per-frame refresh surfaces it in tools mode.
        let mut defs = std::collections::BTreeMap::new();
        defs.insert(
            "mcp__excalimate__create_scene".to_owned(),
            jinn_core_types::ToolDefinition {
                name: "mcp__excalimate__create_scene".to_owned(),
                description: "Create a scene".to_owned(),
                parameters: serde_json::Value::Object(serde_json::Map::new()),
                prompt_snippet: None,
                prompt_guidelines: Vec::new(),
                server_tool_type: None,
            },
        );
        registry_cell.update(|registry| {
            registry.session.insert(session_id, defs);
        });
        // Entry starts in Logs mode; flip to Tools so the rendered pane shows tools.
        let mut entry = McpServerEntry::new(
            "excalimate".to_owned(),
            "npx @excalimate/mcp-server".to_owned(),
            true,
            default_theme(),
        );
        entry.preview_mode = McpPreviewMode::Tools;
        // Wrap the entry through the spec (storage holds PickerEntry<T>).
        let wrapped = jinn_domain::feat::picker::registry::build_picker_registry()
            .make_items(
                jinn_domain::feat::picker::registry::MCP_SERVER_ID,
                vec![entry],
            )
            .expect("mcp-server spec registered");
        let mut w = app.core.state.write_test_no_cap();
        w.frontend.mcp_server_picker_mut().set_items(wrapped);
        w.frontend.scope_push(jinn_domain::FocusScope::Picker {
            kind: jinn_domain::PickerKind::McpServer,
        });
    }

    let (mut terminal, _area) = setup_term(100, 30);

    // When rendering.
    terminal
        .draw(|frame| {
            app.render(frame);
        })
        .unwrap();

    // Then the tools pane shows the advertised tool name.
    let buf = terminal.backend().buffer();
    let rendered: String = buf
        .content
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect();
    assert!(
        rendered.contains("create_scene"),
        "tools pane shows the tool name"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn which_key_help_renders_above_the_terminal_overlay() {
    // Given an app with the terminal overlay open in view mode and the
    // which-key help activated (as if `?` had been pressed).
    let mut app = render_test_app().await;
    app.core
        .state
        .write_test_no_cap()
        .frontend
        .scope_swap_base(jinn_domain::FocusScope::Dynamic(jinn_term_msg::view_scope()));
    app.which_key.active = true;

    let (mut terminal, _area) = setup_term(80, 24);

    // When rendering.
    terminal
        .draw(|frame| {
            app.render(frame);
        })
        .unwrap();

    // Then the help popup's title survives — the overlay did not paint
    // over it.
    let buf = terminal.backend().buffer();
    let rendered: String = buf
        .content
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect();
    assert!(
        rendered.contains("Shortcuts"),
        "which-key help must render above the terminal overlay, got: {rendered}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn which_key_help_renders_in_base_scopes() {
    // Given a plain chat-scope app with the which-key help activated.
    let mut app = render_test_app().await;
    app.which_key.active = true;

    let (mut terminal, _area) = setup_term(80, 24);

    // When rendering.
    terminal
        .draw(|frame| {
            app.render(frame);
        })
        .unwrap();

    // Then the help popup still renders (the reordering regressed nothing).
    let buf = terminal.backend().buffer();
    let rendered: String = buf
        .content
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect();
    assert!(
        rendered.contains("Shortcuts"),
        "which-key help must render in base scopes"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn model_picker_renders_telescope_layout_with_filter() {
    // Given a provider picker open with entries loaded and filter "ol".
    let mut app = render_test_app_with_provider().await;
    {
        let mut w = app.core.state.write_test_no_cap();
        w.frontend.scope_push(jinn_domain::FocusScope::Picker {
            kind: jinn_domain::PickerKind::Provider,
        });
        // Load entries through the spec (raw entry matching the configured
        // ollama model).
        let wrapped = jinn_domain::feat::picker::registry::build_picker_registry()
            .make_items(
                jinn_domain::feat::picker::registry::PROVIDER_ID,
                vec![
                    jinn_domain::feat::provider::picker_entry::ProviderPickerEntry {
                        provider_id: "ollama/llama3".to_owned(),
                        name: "ollama".to_owned(),
                        provider_name: "ollama".to_owned(),
                        backend: "ollama".to_owned(),
                        model: "llama3".to_owned(),
                        search_text: "llama3 ollama".to_owned(),
                        is_alias: false,
                        alias_target: None,
                        is_available: true,
                        is_remote: false,
                        is_active: false,
                        selected: false,
                        theme: jinn_domain::feat::theme::default_theme(),
                    },
                ],
            )
            .expect("provider spec registered");
        w.provider.provider_picker.set_items(wrapped);
        w.provider.provider_picker.insert_char('o');
        w.provider.provider_picker.insert_char('l');
    }

    let (mut terminal, _area) = setup_term(80, 24);

    // When rendering.
    terminal
        .draw(|frame| {
            app.render(frame);
        })
        .unwrap();

    // Then the popup shows the filter prompt "> ol" on the first inner row.
    let buffer = terminal.backend().buffer().clone();
    let popup = jinn_selection_widget::compute_popup_rect(ratatui::layout::Rect::new(0, 0, 80, 24));
    let filter_cell = buffer
        .cell((popup.x + 1, popup.y + 1))
        .expect("filter cell");
    assert_eq!(filter_cell.symbol(), ">");
    // And the results area shows the llama3 entry (filtered by "ol").
    let rendered = rendered_string(&buffer);
    assert!(
        rendered.contains("llama3"),
        "filtered entries render: {rendered}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn model_picker_uses_dark_gray_border() {
    // Given a provider picker open with entries loaded.
    let mut app = render_test_app_with_provider().await;
    {
        let w = app.core.state.write_test_no_cap();
        w.frontend.scope_push(jinn_domain::FocusScope::Picker {
            kind: jinn_domain::PickerKind::Provider,
        });
    }

    let (mut terminal, _area) = setup_term(80, 24);

    // When rendering.
    terminal
        .draw(|frame| {
            app.render(frame);
        })
        .unwrap();

    // Then the popup border color is DarkGray.
    let buffer = terminal.backend().buffer().clone();
    let popup = jinn_selection_widget::compute_popup_rect(ratatui::layout::Rect::new(0, 0, 80, 24));
    let border_cell = buffer.cell((popup.x, popup.y)).expect("border cell");
    assert_eq!(border_cell.fg, ratatui::style::Color::DarkGray);
}

#[rstest::rstest]
#[tokio::test]
async fn model_picker_no_active_marker_for_active_model() {
    // Given the active session is on ollama/llama3 with entries loaded.
    let mut app = render_test_app_with_provider().await;
    {
        let mut w = app.core.state.write_test_no_cap();
        w.frontend.scope_push(jinn_domain::FocusScope::Picker {
            kind: jinn_domain::PickerKind::Provider,
        });
        // Wrap entries through the spec; the check column stays empty in
        // single mode (nothing selected).
        let wrapped = jinn_domain::feat::picker::registry::build_picker_registry()
            .make_items(
                jinn_domain::feat::picker::registry::PROVIDER_ID,
                vec![
                    jinn_domain::feat::provider::picker_entry::ProviderPickerEntry {
                        provider_id: "ollama/llama3".to_owned(),
                        name: "ollama".to_owned(),
                        provider_name: "ollama".to_owned(),
                        backend: "ollama".to_owned(),
                        model: "llama3".to_owned(),
                        search_text: "llama3 ollama".to_owned(),
                        is_alias: false,
                        alias_target: None,
                        is_available: true,
                        is_remote: false,
                        is_active: true,
                        selected: false,
                        theme: jinn_domain::feat::theme::default_theme(),
                    },
                ],
            )
            .expect("provider spec registered");
        w.provider.provider_picker.set_items(wrapped);
    }

    let (mut terminal, _area) = setup_term(80, 24);

    // When rendering.
    terminal
        .draw(|frame| {
            app.render(frame);
        })
        .unwrap();

    // Then the first result row carries no ">" marker (no active marker; the
    // check column is only populated in alloy mode).
    let buffer = terminal.backend().buffer().clone();
    let popup = jinn_selection_widget::compute_popup_rect(ratatui::layout::Rect::new(0, 0, 80, 24));
    let marker_cell = buffer
        .cell((popup.x + 3, popup.y + 3))
        .expect("marker cell");
    assert_ne!(marker_cell.symbol(), ">");
}

/// A TuiApp whose services carry a provider config (one ollama model) so the
/// provider loader can build entries, plus the real picker registry.
async fn render_test_app_with_provider() -> crate::TuiApp {
    use jinn_domain::common::services::test_services::TestServices;
    use jinn_domain::feat::provider_infra::{ProviderEntry, ProvidersConfig};
    use std::collections::BTreeMap;
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
    let services = jinn_domain::Services {
        picker_registry: jinn_domain::feat::picker::registry::build_picker_registry(),
        ..TestServices::builder().with_providers(config).build()
    };
    crate::TuiApp::test_builder()
        .services(services)
        .build()
        .await
}

/// Concatenates a buffer's cells into one string for substring assertions.
fn rendered_string(buffer: &ratatui::buffer::Buffer) -> String {
    buffer
        .content
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect()
}
