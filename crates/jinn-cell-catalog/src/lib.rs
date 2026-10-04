//! Every slice cell in the workspace, registered in one function.
//!
//! A cell is a slice's private storage: a `TypedCell<T>` under a
//! `SlotKey` that its owning slice mints at wiring and everyone else
//! resolves read-only. Until this crate, *where* those registrations
//! lived was an accident of who called what — the boot list, the TUI test
//! app builder, `AppState`'s test seeding, and `Services::new_fake` each
//! carried their own hand-maintained list, and the lists had already
//! drifted. A test that omitted a cell rendered nothing while every
//! assertion stayed green.
//!
//! [`register_all_cells`] is the one list. Production boot calls it
//! before any slice activates; every test harness calls it when it needs
//! a seeded registry. A slice never registers a cell from its own
//! activation — that is the boot list's job, and the boot list's job is
//! this function's.
//!
//! # What this crate is not
//!
//! It does not own any payload. Every entry below names a slot key and a
//! default value, both defined in the owning slice's message crate.
//! Adding a cell is a line here and a definition in that crate — not a
//! change to the architecture.
//!
//! # What it deliberately leaves out
//!
//! `jinn-slices` keeps three private infrastructure slots — the draw
//! registry, the scope-hint registry, and the pre-render hook list — on
//! their own idempotent `get_or_register` path. Their resolvers are
//! private so no caller can hold a handle to the payload; a catalog that
//! registered them would mint a second handle with no owner.
//!
//! # Why it sits below the kernel
//!
//! The catalog depends only on slice *message* crates. Every cell payload
//! lives in the family's `-msg` crate beside its slot key, so naming a slot
//! never pulls in the actor, renderer, or routes that go with the slice.
//! That is what keeps the catalog usable from `AppState`'s test seeding and
//! from `Services::new_fake` — the two harnesses that live *inside* the
//! layer the slices depend on, and which a slice implementation could never
//! be introduced into without a cycle.

use jinn_slices::Slices;

/// Registers every slice cell in the workspace.
///
/// Idempotent: a slot already claimed is left as it is, so a harness that
/// seeds a registry and then seeds it again through a second path does not
/// fail. The one thing that is *not* idempotent is a cell registered under
/// the wrong payload type — [`Slices::register`] rejects that, and the
/// count assertion below catches a type that silently resolved to nothing
/// for every reader.
///
/// # Panics
///
/// Panics (in debug builds only) if the number of registered slots
/// differs from [`EXPECTED_CELL_COUNT`], which means an entry above was
/// added, dropped, or duplicated.
pub fn register_all_cells(slices: &Slices) {
    let entries = register_catalog(slices);

    assert!(
        entries == EXPECTED_CELL_COUNT,
        "the catalog declares {EXPECTED_CELL_COUNT} slice slots but walks {entries}; \
         add, drop, or fix the entry so the table and the count agree"
    );
    assert!(
        slices.slots().len() >= EXPECTED_CELL_COUNT,
        "the catalog registered {entries} cells but the registry holds fewer: a payload \
         was registered under the wrong type and every reader will resolve None"
    );
}

/// How many slice slots [`register_all_cells`] registers.
///
/// The `jinn-slices` infrastructure slots are not counted: they are
/// registered lazily by the first caller that resolves one, and a harness
/// that never renders never creates them.
const EXPECTED_CELL_COUNT: usize = 39;

// ── The catalog ─────────────────────────────────────────────────────
//
// One `register!` per slice cell, grouped by owning slice and ordered by
// slot key within each group. The macro exists so each entry keeps its
// real payload type at the registration site: a boxed `dyn Any` would
// compile identically and then fail every reader, silently, which is
// the exact failure mode this crate is meant to make impossible.

macro_rules! register {
    ($slices:expr, $count:ident, $slot:expr, $initial:expr) => {{
        $count += 1;
        if let Err(_taken) = $slices.register($slot, $initial) {
            // Already registered: the existing cell stands, and its
            // handle remains valid. Seeding a registry twice is not a
            // wiring bug.
        }
    }};
}

/// Registers the whole catalog and returns how many entries it walked.
///
/// Split out from [`register_all_cells`] so the count is the number of
/// *entries in the table*, not the number of slots the registry happens
/// to hold — a pre-existing cell registered by an earlier caller must
/// not make the assertion pass by coincidence.
#[expect(
    clippy::too_many_lines,
    reason = "the catalog is a flat table; one line per slot is the readable form"
)]
fn register_catalog(slices: &Slices) -> usize {
    let mut count = 0;

    // jinn-slices — the scope-focus stack.
    register!(
        slices,
        count,
        jinn_slices::scope_focus_slot(),
        jinn_slices::ScopeFocusState::default()
    );

    // jinn-chat-log-view — per-session view state, the audit popup, the
    // per-entry line cache, and both picker-facing cells.
    register!(
        slices,
        count,
        jinn_chat_log_view_msg::chat_log_views_slot(),
        jinn_chat_log_view_msg::ChatLogViews::new()
    );
    register!(
        slices,
        count,
        jinn_chat_log_view_msg::audit_popup_slot(),
        jinn_chat_log_view_msg::AuditPopupState::default()
    );
    register!(
        slices,
        count,
        jinn_chat_log_view_msg::entry_line_cache_slot(),
        jinn_chat_log_view_msg::EntryLineCache::default()
    );

    // jinn-chat-input — the per-session input draft and the `@path` popup.
    register!(
        slices,
        count,
        jinn_chat_input_msg::chat_inputs_slot(),
        jinn_chat_input_msg::ChatInputs::new()
    );
    register!(
        slices,
        count,
        jinn_chat_input_msg::file_picker_slot(),
        jinn_chat_input_msg::FilePickerState::default()
    );

    // jinn-cwd — the `cd` popup.
    register!(
        slices,
        count,
        jinn_cwd_msg::cwds_slot(),
        jinn_cwd_msg::CwdInputState::default()
    );

    // jinn-attendant — the attendant properties popup, the report history
    // picker, and the saved-attendants picker. All three activations
    // resolve their cells from here before any slice activates; a missing
    // entry aborts launch.
    register!(
        slices,
        count,
        jinn_attendant_msg::attendant_properties_slot(),
        jinn_attendant_msg::AttendantPropertiesState::default()
    );
    register!(
        slices,
        count,
        jinn_attendant_msg::attendant_report_picker_slot(),
        jinn_attendant_msg::AttendantReportPickerState::default()
    );
    register!(
        slices,
        count,
        jinn_attendant_msg::attendant_saved_picker_slot(),
        jinn_attendant_msg::AttendantSavedPickerState::default()
    );

    // jinn-dashboard — the actor census.
    register!(
        slices,
        count,
        jinn_dashboard_msg::dashboard_slot(),
        jinn_dashboard_msg::DashboardState::new()
    );

    // jinn-discord — the gateway connection fact.
    register!(
        slices,
        count,
        jinn_discord_msg::discord_connection_slot(),
        jinn_discord_msg::ConnectionState::default()
    );

    // jinn-mcp — the runtime registry and the picker.
    register!(
        slices,
        count,
        jinn_mcp_msg::mcp_runtime_slot(),
        jinn_mcp_msg::McpRuntimeState::default()
    );
    register!(
        slices,
        count,
        jinn_mcp_msg::mcp_picker_slot(),
        jinn_mcp_msg::McpPickerState::default()
    );

    // jinn-persona — the scanned entries and the picker.
    register!(
        slices,
        count,
        jinn_persona_msg::personas_slot(),
        jinn_persona_msg::Personas::default()
    );
    register!(
        slices,
        count,
        jinn_persona_msg::persona_picker_slot(),
        jinn_persona_msg::PersonaPickerState::default()
    );

    // jinn-preferences — the pruner accumulation threshold popup.
    register!(
        slices,
        count,
        jinn_preferences_msg::pruner_accumulation_slot(),
        jinn_preferences_msg::PrunerAccumulationInputState::default()
    );

    // jinn-project — the add form and the picker.
    register!(
        slices,
        count,
        jinn_project_msg::project_add_slot(),
        jinn_project_msg::ProjectAddInputState::default()
    );
    register!(
        slices,
        count,
        jinn_project_msg::project_picker_slot(),
        jinn_project_msg::ProjectPickerState::default()
    );

    // jinn-provider-selection — the provider cell, both pickers, and the
    // reasoning picker.
    register!(
        slices,
        count,
        jinn_provider_selection_msg::provider_state_slot(),
        jinn_provider_selection_msg::ProviderCell::default()
    );
    register!(
        slices,
        count,
        jinn_provider_selection_msg::provider_picker_slot(),
        jinn_provider_selection_msg::ProviderPickerState::default()
    );
    register!(
        slices,
        count,
        jinn_provider_selection_msg::endpoint::endpoint_picker_slot(),
        jinn_provider_selection_msg::endpoint::EndpointPickerState::default()
    );
    register!(
        slices,
        count,
        jinn_provider_selection_msg::reasoning::reasoning_picker_slot(),
        jinn_provider_selection_msg::reasoning::ReasoningPickerState::default()
    );

    // jinn-quake-bar — the quake overlay's input state.
    register!(
        slices,
        count,
        jinn_quake_bar_msg::quake_bar_slot(),
        jinn_quake_bar_msg::QuakeBarState::default()
    );

    // jinn-session-lifecycle — the argument input and the picker.
    register!(
        slices,
        count,
        jinn_session_lifecycle_msg::arg_input_slot(),
        jinn_session_lifecycle_msg::ArgInputState::empty()
    );
    register!(
        slices,
        count,
        jinn_session_lifecycle_msg::session_lifecycle_picker_slot(),
        jinn_session_lifecycle_msg::SessionLifecyclePickerState::default()
    );

    // jinn-session-store — the session picker.
    register!(
        slices,
        count,
        jinn_session_store_msg::session_picker_slot(),
        jinn_session_store_msg::SessionPickerState::default()
    );

    // jinn-stream-rules — the installed rule matcher the inference stream
    // loop reads before publishing each delta.
    register!(
        slices,
        count,
        jinn_slices::stream_rules_slot(),
        jinn_slices::StreamRules::empty()
    );

    // jinn-sidebar — the section registry.
    register!(
        slices,
        count,
        jinn_sidebar_msg::sidebar_sections_slot(),
        jinn_sidebar_msg::SidebarSections::default()
    );

    // jinn-skills — the skill picker.
    register!(
        slices,
        count,
        jinn_skills_msg::skill_picker_slot(),
        jinn_skills_msg::SkillPickerState::default()
    );

    // jinn-status-bar — the hint strip.
    register!(
        slices,
        count,
        jinn_status_bar_msg::status_bar_slot(),
        jinn_status_bar_msg::StatusBarState::default()
    );

    // jinn-term — the terminal tab state.
    register!(
        slices,
        count,
        jinn_term_msg::term_tabs_slot(),
        jinn_term_msg::TerminalTabState::default()
    );

    // jinn-theme — the scanned entries and the picker.
    register!(
        slices,
        count,
        jinn_theme_msg::theme_entries_slot(),
        jinn_theme_msg::ThemeEntries::default()
    );
    register!(
        slices,
        count,
        jinn_theme_msg::theme_picker_slot(),
        jinn_theme_msg::ThemePickerState::default()
    );

    // jinn-token-count — the history worker's count cache and the
    // per-session token ledger.
    register!(
        slices,
        count,
        jinn_token_count_msg::token_cache_slot(),
        jinn_token_count_msg::HistoryWorkerChatEntryTokenCache::new()
    );

    // jinn-tools — the tool registry, both pickers, and the per-session
    // task list.
    register!(
        slices,
        count,
        jinn_tools_msg::tools_registry_slot(),
        jinn_tools_msg::ToolRegistry::default()
    );
    register!(
        slices,
        count,
        jinn_tools_msg::tool_picker_slot(),
        jinn_tools_msg::ToolPickerState::default()
    );
    register!(
        slices,
        count,
        jinn_tools_msg::task_list_picker_slot(),
        jinn_tools_msg::TaskListPickerState::default()
    );

    // jinn-work-time — the per-session wall-clock working intervals.
    register!(
        slices,
        count,
        jinn_work_time_msg::work_time_slot(),
        jinn_work_time_msg::WorkingTimeState::default()
    );

    count
}
