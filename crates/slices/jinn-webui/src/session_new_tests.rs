//! The new-session region and delivery tests: the form fragment's
//! render (projects, blank-first lifecycles, the conditional args
//! input, the no-projects refusal), the create POST's four refusals,
//! and a creation success through the shared path with its publish
//! closures drained.

//! Test code: assertions use expect/panic to state what went wrong.
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions need expect/panic to state what went wrong"
)]

use jinn_app_state::AppState;
use jinn_kernel::State;
use jinn_slices::Slices;
use jinn_testutil::bus_harness::TestHarness;
use jinn_testutil::bus_harness::await_recorded;
use jinn_webui_msg::CreateSessionForm;

use crate::server::ServerShared;

/// A two-lifecycle config document: one shell setup with two
/// parameters, one without a setup at all.
///
/// The second project is the non-`~` absolute-path specimen. Its
/// spelling is platform-conditional: `/abs/other` is not absolute on
/// Windows (no drive prefix), so the production validator would refuse
/// it there — the specimen must be a valid absolute path per host.
const CONFIG_DOC_TEMPLATE: &str = r#"
[[project.entry]]
path = "~/code/demo"

[[project.entry]]
path = "{abs_specimen}"

[[session_lifecycle.script]]
name = "worktree"
setup_command = "git worktree add ~/wt/$1 $2"

[[session_lifecycle.script]]
name = "bare"
"#;

/// The absolute-path specimen the config document names, per platform.
#[cfg(windows)]
const ABS_SPECIMEN: &str = "C:/abs/other";
#[cfg(not(windows))]
const ABS_SPECIMEN: &str = "/abs/other";

/// The project+lifecycle config layer the creation tests run under.
fn config_doc() -> jinn_config::ConfigLayer {
    let document = CONFIG_DOC_TEMPLATE.replace("{abs_specimen}", ABS_SPECIMEN);
    jinn_config::testutil::config_layer(&document)
}

/// The harness: fresh state, seeded cell registry, and the shared
/// handler state over the given config layer.
async fn harness_with(config: jinn_config::ConfigLayer) -> (TestHarness, State, ServerShared) {
    // The harness owns the fabric: the creation closures drain through
    // the bridge onto THIS bus, and the recorder taps ride the same
    // trouper system — the real delivery path, no recording panic (a
    // recording `BusService` panics on erased publishes, and the
    // creation closures are exactly that).
    let harness = TestHarness::new().await;
    let bus = harness.bus();
    let slices = Slices::new();
    jinn_cell_catalog::register_all_cells(&slices);
    // The catalog already registered the provider slot; take the
    // handle it minted (composition's own access form).
    let provider_cell = slices
        .reader::<jinn_provider_selection_msg::ProviderCell>(
            &jinn_provider_selection_msg::provider_state_slot(),
        )
        .expect("the catalog registered the provider slot");
    // The catalog already registered the personas slot; take the
    // handle it minted (composition's own access form).
    let personas_cell = slices
        .reader::<jinn_persona_msg::Personas>(&jinn_persona_msg::personas_slot())
        .expect("the catalog registered the personas slot");
    let mcp_runtime_cell = slices
        .reader::<jinn_mcp_msg::McpRuntimeState>(&jinn_mcp_msg::mcp_runtime_slot())
        .expect("the catalog registered the MCP runtime slot");
    let applets_catalog_cell = slices
        .reader::<jinn_applets_msg::AppletsCatalog>(&jinn_applets_msg::applets_catalog_slot())
        .expect("the catalog registered the applets catalog slot");
    let applets_picker_cell = slices
        .reader::<jinn_applets_msg::AppletsPickerState>(&jinn_applets_msg::applets_picker_slot())
        .expect("the catalog registered the applets picker slot");
    let state = {
        let app = AppState::default();
        app.session.attach_slices(slices.clone());
        State::new(app)
    };
    // The catalog already registered the unread slot; take the handle
    // it minted (composition's own access form).
    let unread_marks = slices
        .reader::<jinn_webui_msg::UnreadSessionMarks>(&jinn_webui_msg::webui_unread_slot())
        .expect("the catalog registered the unread slot");
    let fanout = crate::fanout::WebUiFanoutActor::spawn(
        harness.system(),
        state.clone(),
        bus.clone(),
        unread_marks,
    );
    let shared = ServerShared::new(
        state.clone(),
        bus.clone(),
        jinn_session_state::SessionStoreService::new(std::sync::Arc::new(
            jinn_kernel::common::services::test_services::FakeSessionStore,
        )),
        jinn_kernel::common::turn_ledger::TurnLedger::default(),
        provider_cell,
        personas_cell,
        mcp_runtime_cell,
        applets_catalog_cell,
        applets_picker_cell,
        jinn_provider_config::ProviderRegistryService::new(
            jinn_provider_config::ProviderRegistry::from_config(
                jinn_provider_config::ProvidersConfig {
                    providers: std::collections::BTreeMap::default(),
                    aliases: vec![],
                    default_provider: None,
                    endpoint_defaults: vec![],
                },
            )
            .expect("an empty providers config expands"),
        ),
        jinn_provider_config::ApiKeysService::new(jinn_provider_config::ApiKeys::new()),
        config,
        fanout,
        0,
        "test".to_owned(),
    );
    (harness, state, shared)
}

// ── form fragment renders ────────────────────────────────────────────

#[rstest::rstest]
#[test]
fn the_form_renders_project_and_lifecycle_options_with_blank_first() {
    // Given the project+lifecycle config.
    let config = config_doc();

    // When rendering the form fragment for the blank selection.
    let html = crate::regions::session_new::form_fragment(&config, "").expect("the form renders");

    // Then both project paths render as option values, the lifecycle
    // select names every configured lifecycle, and the blank row is
    // the select's first entry.
    assert!(html.contains(r#"value="~/code/demo""#), "{html}");
    let specimen_option = format!(r#"value="{ABS_SPECIMEN}""#);
    assert!(html.contains(&specimen_option), "{html}");
    assert!(html.contains(r#"value="worktree""#), "{html}");
    assert!(html.contains(r#"value="bare""#), "{html}");
    let lifecycle_select = html
        .split(r#"id="session-new-lifecycle""#)
        .nth(1)
        .expect("the lifecycle select renders");
    let first_option = lifecycle_select
        .split("<option ")
        .nth(1)
        .expect("the lifecycle select carries options");
    assert!(
        first_option.starts_with(r#"value="""#),
        "the blank row renders first, got {first_option}"
    );
    // And the args input is absent: the blank lifecycle declares no
    // parameters.
    assert!(!html.contains(r#"id="session-new-args""#), "{html}");
}

#[rstest::rstest]
#[test]
fn selecting_a_parameterized_lifecycle_renders_the_args_input() {
    // Given the project+lifecycle config.
    let config = config_doc();

    // When rendering the form fragment for the two-parameter
    // lifecycle.
    let html =
        crate::regions::session_new::form_fragment(&config, "worktree").expect("the form renders");

    // Then the args input renders, the lifecycle row is selected, and
    // the param count rides the option attribute.
    assert!(html.contains(r#"id="session-new-args""#), "{html}");
    assert!(
        html.contains(r#"value="worktree" selected"#),
        "the re-selected lifecycle renders selected, got {html}"
    );
    assert!(
        html.contains(r#"data-param-count="2""#),
        "the param count rides the option, got {html}"
    );
}

#[rstest::rstest]
#[test]
fn the_args_input_shows_the_setup_replacement_tokens() {
    // Given the project+lifecycle config whose `worktree` setup is
    // `git worktree add ~/wt/$1 $2`.
    let config = config_doc();

    // When rendering the form fragment for that lifecycle.
    let html =
        crate::regions::session_new::form_fragment(&config, "worktree").expect("the form renders");

    // Then the input's placeholder carries the Discord-style token
    // prompt — `Please enter: <1> <2>` — so the user knows what to
    // type.
    assert!(
        html.contains(r#"placeholder="Please enter: &#60;1&#62; &#60;2&#62;""#),
        "the token prompt rides the placeholder, got {html}"
    );
}

#[rstest::rstest]
#[test]
fn a_named_parameter_renders_its_angle_name_and_a_splat_renders_args() {
    // Given a config whose lifecycle names a `<branch>` parameter and
    // splats the rest.
    let config = jinn_config::testutil::config_layer(
        r#"
[[project.entry]]
path = "~/code/demo"

[[session_lifecycle.script]]
name = "branchy"
setup_command = "setup.sh <branch> $@"
"#,
    );

    // When rendering the form fragment for that lifecycle.
    let html =
        crate::regions::session_new::form_fragment(&config, "branchy").expect("the form renders");

    // Then the token prompt lists the named parameter and the splat
    // as `<branch> <args>`.
    assert!(
        html.contains(r#"placeholder="Please enter: &#60;branch&#62; &#60;args&#62;""#),
        "named + splat tokens render, got {html}"
    );
}

#[rstest::rstest]
#[test]
fn a_lifecycle_without_setup_declares_no_parameters() {
    // Given the project+lifecycle config.
    let config = config_doc();

    // When rendering the form fragment for the setup-less lifecycle.
    let html =
        crate::regions::session_new::form_fragment(&config, "bare").expect("the form renders");

    // Then the args input is absent — no template, no minimum.
    assert!(!html.contains(r#"id="session-new-args""#), "{html}");
}

#[rstest::rstest]
#[test]
fn an_unknown_lifecycle_repull_is_refused() {
    // Given the project+lifecycle config.
    let config = config_doc();

    // When rendering the form fragment for a lifecycle that is not
    // configured.
    let result = crate::regions::session_new::form_fragment(&config, "bogus");

    // Then the re-pull is refused with a 400 naming the value.
    let error = result.expect_err("an unknown lifecycle re-pull refuses");
    let (status, sentence) = error;
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
    assert!(sentence.contains("bogus"), "{sentence}");
}

#[rstest::rstest]
#[test]
fn no_configured_projects_renders_the_refusal_not_a_form() {
    // Given an empty config layer.
    let config = jinn_config::empty_config_layer().clone();

    // When rendering the form fragment.
    let html =
        crate::regions::session_new::form_fragment(&config, "").expect("the refusal renders");

    // Then the refusal wrapper renders instead of the form: no
    // selects, no submit, the explanatory copy present.
    assert!(html.contains(r#"id="session-new-empty""#), "{html}");
    assert!(!html.contains(r#"id="session-new-project""#), "{html}");
    assert!(!html.contains(r#"id="session-new-submit""#), "{html}");
    assert!(
        html.contains("[[project.entry]]"),
        "the copy names the config to edit, got {html}"
    );
}

// ── create POST refusals ─────────────────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn create_refuses_an_unconfigured_project() {
    // Given the project+lifecycle config and a form naming a project
    // that is not configured.
    let (harness, state, _shared) = harness_with(config_doc()).await;
    let form = CreateSessionForm {
        project: "/not/configured".to_owned(),
        lifecycle: String::new(),
        args: None,
    };

    // When handling the create.
    let result = crate::delivery::handle_create(&state, &harness.bus(), &config_doc(), &form);

    // Then the refusal is a 400 naming the project.
    let error = result.expect_err("an unknown project refuses");
    assert_eq!(error.status(), 400);
    assert!(error.sentence().contains("/not/configured"));
}

#[rstest::rstest]
#[tokio::test]
async fn create_refuses_a_project_when_none_are_configured() {
    // Given an empty config layer and a form naming anything.
    let (harness, state, _shared) = harness_with(jinn_config::empty_config_layer().clone()).await;
    let form = CreateSessionForm::default();

    // When handling the create.
    let result = crate::delivery::handle_create(
        &state,
        &harness.bus(),
        jinn_config::empty_config_layer(),
        &form,
    );

    // Then the refusal is the unknown-project 400 (an empty list
    // matches nothing).
    let error = result.expect_err("no configured project refuses");
    assert_eq!(error.status(), 400);
}

#[rstest::rstest]
#[tokio::test]
async fn create_refuses_an_unknown_lifecycle() {
    // Given the project+lifecycle config and a form naming a
    // lifecycle that is not configured.
    let (harness, state, _shared) = harness_with(config_doc()).await;
    let form = CreateSessionForm {
        project: "~/code/demo".to_owned(),
        lifecycle: "bogus".to_owned(),
        args: None,
    };

    // When handling the create.
    let result = crate::delivery::handle_create(&state, &harness.bus(), &config_doc(), &form);

    // Then the refusal is a 400 naming the lifecycle — the shared
    // function would silently fall back to blank, so the web
    // validates before calling it.
    let error = result.expect_err("an unknown lifecycle refuses");
    assert_eq!(error.status(), 400);
    assert!(error.sentence().contains("bogus"), "{}", error.sentence());
}

#[rstest::rstest]
#[tokio::test]
async fn create_refuses_args_short_of_the_lifecycle_minimum() {
    // Given the project+lifecycle config and a form carrying one arg
    // for a two-parameter lifecycle.
    let (harness, state, _shared) = harness_with(config_doc()).await;
    let form = CreateSessionForm {
        project: "~/code/demo".to_owned(),
        lifecycle: "worktree".to_owned(),
        args: Some("only-one".to_owned()),
    };

    // When handling the create.
    let result = crate::delivery::handle_create(&state, &harness.bus(), &config_doc(), &form);

    // Then the refusal is a 400 naming the shortfall (have/want).
    let error = result.expect_err("short args refuse");
    assert_eq!(error.status(), 400);
    let sentence = error.sentence();
    assert!(sentence.contains("takes 2"), "{sentence}");
    assert!(sentence.contains("got 1"), "{sentence}");
}

// ── creation success ─────────────────────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn create_runs_the_shared_path_and_names_the_created_session() {
    // Given the project+lifecycle config, a recorder for the
    // lifecycle contract's real delivery path, and a form for the
    // blank lifecycle.
    let (harness, state, _shared) = harness_with(config_doc()).await;
    let recorder = harness
        .spawn_recorder::<jinn_session_lifecycle_msg::SessionCreated>()
        .await;
    let form = CreateSessionForm {
        project: "~/code/demo".to_owned(),
        lifecycle: String::new(),
        args: None,
    };

    // When handling the create.
    let created = crate::delivery::handle_create(&state, &harness.bus(), &config_doc(), &form)
        .expect("the blank-lifecycle create succeeds");

    // Then the returned id IS the created session, it is the
    // instance's active session, the session's cwd is the expanded
    // project path, and the lifecycle stamp is absent (blank).
    let active = state.read().session.active_session_id().clone();
    assert_eq!(created, active, "creation names the created session");
    let guard = state.read();
    let session = guard.session.get(&created).expect("the created session");
    let home = jinn_chat_input_msg::home_dir();
    assert_eq!(
        session.cwd().to_string(),
        home.join("code/demo")
            .expect("home join succeeds")
            .to_string(),
        "the ~-prefixed project expanded to the starting cwd"
    );
    assert_eq!(session.lifecycle_name(), None);
    drop(guard);

    // And the SessionCreated event reaches the bus through the
    // bridge drain (the real delivery path, via the recorder tap).
    let events = await_recorded(&recorder, 1, std::time::Duration::from_secs(5)).await;
    assert_eq!(events.len(), 1, "one SessionCreated event");
    assert_eq!(events.first().expect("one event").session_id, created);
}

#[rstest::rstest]
#[tokio::test]
async fn create_with_a_scripted_lifecycle_drains_the_setup_command() {
    // Given the config and a form for the two-parameter lifecycle,
    // with args quoting one value.
    let (harness, state, _shared) = harness_with(config_doc()).await;
    let recorder = harness
        .spawn_recorder::<jinn_session_lifecycle_msg::RunSessionSetup>()
        .await;
    let form = CreateSessionForm {
        project: "~/code/demo".to_owned(),
        lifecycle: "worktree".to_owned(),
        args: Some(r#"feature/big "two words""#.to_owned()),
    };

    // When handling the create.
    let created = crate::delivery::handle_create(&state, &harness.bus(), &config_doc(), &form)
        .expect("the scripted create succeeds");

    // Then the setup command runs for the created session with the
    // parsed args — the quote-preserving split the TUI arg popup
    // uses.
    let events = await_recorded(&recorder, 1, std::time::Duration::from_secs(5)).await;
    assert_eq!(events.len(), 1, "one RunSessionSetup command");
    let setup = events.first().expect("one command");
    assert_eq!(setup.session_id, created);
    assert_eq!(
        setup.args,
        vec!["feature/big".to_owned(), "two words".to_owned()],
    );
}

#[rstest::rstest]
#[tokio::test]
async fn create_persists_the_scripted_session() {
    // Given the config and a scripted-lifecycle form — the shared
    // path emits `PersistSession` on the setup branch (the blank
    // branch emits only `SessionCreated`, asserted above).
    //
    // A Command routes to ONE declarant, and the recorder's tap is a
    // declarant; with no store actor spawned in this harness the tap
    // is the only one, so the drained command lands in it.
    let (harness, state, _shared) = harness_with(config_doc()).await;
    let recorder = harness
        .spawn_recorder::<jinn_session_store_msg::PersistSession>()
        .await;
    let form = CreateSessionForm {
        project: ABS_SPECIMEN.to_owned(),
        lifecycle: "worktree".to_owned(),
        args: Some("branch extra".to_owned()),
    };

    // When handling the create.
    let created = crate::delivery::handle_create(&state, &harness.bus(), &config_doc(), &form)
        .expect("the create succeeds");

    // Then the persist command names the created session (the shared
    // path emits it; the bridge drains it to the store actor).
    let events = await_recorded(&recorder, 1, std::time::Duration::from_secs(5)).await;
    assert_eq!(events.len(), 1, "one PersistSession command");
    assert_eq!(events.first().expect("one command").session_id, created);
}
