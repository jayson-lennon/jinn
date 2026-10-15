//! Tests for the properties popup's save path: the arming flow, the
//! untitled refusal, and the write itself, driven through route dispatch
//! against a real `AppState` and a real `jinn.toml` document.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

use std::collections::BTreeSet;
use std::sync::Arc;

use jinn_app_state::AppState;
use jinn_attendant_msg::{
    AttendantPropertiesState, OriginalValues, PopupStatus, SetMode, attendant_properties_scope,
    attendant_properties_slot,
};
use jinn_config::{ConfigLayer, InMemoryConfigStorage};
use jinn_core_types::{ChatEntry, FilterMode, NameFilter, PinPosition, SessionId, ToolDefinition};
use jinn_preferences_config::schemas::{
    AttendantEntryConfig, AttendantPinConfig, AttendantPinRole,
};
use jinn_session_state::ChatSessionState;
use jinn_slices::KeyRoutes;
use jinn_slices::cell::TypedCell;
use jinn_slices::route::{ActionCtx, DynamicIntent, ScopeSignal};

use crate::properties_overlay::attach_properties_rows;
use crate::seed_template_overlay::attach_seed_template_rows;

/// An AppState with one titled attendant, a popup cell seeded from it, and
/// a properties document the save writes to.
struct SaveFixture {
    state: AppState,
    slices: jinn_slices::Slices,
    routes: KeyRoutes,
    cell: TypedCell<AttendantPropertiesState>,
    config: ConfigLayer,
    attendant_id: SessionId,
}

impl SaveFixture {
    /// Builds the fixture, optionally titling the attendant. An untitled
    /// attendant is the default because that is the refusal case; a title
    /// is what makes an attendant savable at all.
    fn new(title: Option<&str>) -> Self {
        let doc = "# user's own comment\n".parse().expect("parses");
        let config = ConfigLayer::load(Arc::new(InMemoryConfigStorage::new(doc)))
            .expect("an empty document always loads");
        let mut state = AppState::default_with_scope_focus();
        let attendant_id = {
            let parent = ChatSessionState::new();
            let mut attendant = ChatSessionState::new_attendant(&parent, true);
            // Only a real title is stamped: an empty one is still an
            // untitled session, which is the case the save refuses.
            if let Some(title) = title {
                attendant.set_title(title.to_owned());
            }
            let id = attendant.session_id().clone();
            state.session.insert(attendant);
            state.session.set_active(id.clone());
            id
        };
        let slices = jinn_slices::Slices::new();
        let cell = slices
            .register(
                attendant_properties_slot(),
                AttendantPropertiesState::default(),
            )
            .expect("unclaimed slot");
        let routes = KeyRoutes::new();
        attach_properties_rows(&routes, &cell);
        crate::seed_template_overlay::attach_seed_template_rows(&routes, &cell);
        crate::seed_template_overlay::register_seed_template_input_hook(&routes, &cell);
        attach_seed_template_rows(&routes, &cell);
        Self {
            state,
            slices,
            routes,
            cell,
            config,
            attendant_id,
        }
    }

    /// Opens the popup over the attendant, as the sidebar opener does.
    fn open(&mut self) {
        let session = self
            .state
            .session
            .get(&self.attendant_id)
            .expect("attendant");
        let template = session.seed_template().to_owned();
        let cursor_pos = template.len();
        let popup = AttendantPropertiesState {
            session_id: Some(self.attendant_id.clone()),
            seed_template: jinn_slices::LineInput {
                input: template.clone(),
                cursor_pos,
            },
            pending_behavior: session.attendant_behavior(),
            pending_trigger: session.attendant_trigger(),
            pending_tool_set: OriginalValues::mode_of(session.tool_filter()),
            frozen_tools: OriginalValues::names_of(session.tool_filter()),
            pending_skill_set: OriginalValues::mode_of(session.skill_filter()),
            frozen_skills: OriginalValues::names_of(session.skill_filter()),
            pending_model_setting: session.attendant_model_setting(),
            original: Some(OriginalValues {
                trigger: session.attendant_trigger(),
                behavior: session.attendant_behavior(),
                prep_mode: session.attendant_is_prepping(),
                tool_set: session.tool_filter().cloned(),
                skill_set: session.skill_filter().cloned(),
                model_setting: session.attendant_model_setting(),
                template,
            }),
            ..AttendantPropertiesState::default()
        };
        self.cell.update(|s| *s = popup);
        self.state
            .frontend
            .scope_push(jinn_slices::FocusScope::Dynamic(
                attendant_properties_scope(),
            ));
    }

    /// Runs a properties row and applies any scope signal.
    fn press(&mut self, action: &'static str) -> jinn_slices::RouteResult {
        let ctx = ActionCtx {
            state: &mut self.state,
            slices: &self.slices,
            config: &self.config,
            key_bytes: Vec::new(),
        };
        let intent = DynamicIntent::new(attendant_properties_scope(), action, "test");
        let result = self
            .routes
            .action_for(&intent, ctx)
            .unwrap_or_else(|| panic!("no row bound for action {action:?}"));
        if let Some(ScopeSignal::PopIf(scope)) = result.scope_signal.clone()
            && self.state.frontend.scope() == jinn_slices::FocusScope::Dynamic(scope)
        {
            self.state.frontend.scope_pop();
        }
        result
    }

    /// Seeds the document with an entry already saved under `name`, as a
    /// previous run of this attendant would have left it.
    fn presaved(&mut self, name: &str) {
        let mut session = self
            .state
            .session
            .get(&self.attendant_id)
            .expect("attendant")
            .clone();
        session.set_title(name.to_owned());
        let entry = crate::saved_entry::entry_for_session(name.to_owned(), &session);
        self.config
            .put_list::<AttendantEntryConfig>(&[entry])
            .expect("the fixture document is always writable");
    }

    /// The saved entries, read back from the document.
    fn saved(&self) -> Vec<AttendantEntryConfig> {
        self.config
            .get_list::<AttendantEntryConfig>()
            .expect("list reads")
    }

    /// Pushes a pinned entry into the attendant's history.
    fn add_pin(&mut self, text: &str, position: PinPosition) {
        let entry = ChatEntry {
            pin_position: Some(position),
            ..ChatEntry::user(text)
        };
        self.state
            .session
            .get_mut(&self.attendant_id)
            .expect("attendant")
            .push_entry(entry);
    }

    /// Every key the popup binds, as a composition probe would see them.
    fn bound_keys(&self) -> Vec<&'static str> {
        self.routes.rows().into_iter().map(|row| row.key).collect()
    }
}

#[rstest::rstest]
#[test]
fn an_untitled_attendant_cannot_be_saved() {
    // Given an open popup over an attendant that has never been titled.
    let mut fx = SaveFixture::new(None);
    fx.open();

    // When pressing the save key.
    fx.press("attendant-properties-save");

    // Then nothing is written — a session with no title has no identity to
    // save under.
    assert_eq!(
        fx.saved(),
        [] as [jinn_preferences_config::schemas::AttendantEntryConfig; 0]
    );
}

#[rstest::rstest]
#[test]
fn an_untitled_refusal_says_so_in_the_attendants_own_log() {
    // Given an open popup over an untitled attendant.
    let mut fx = SaveFixture::new(None);
    fx.open();

    // When pressing the save key.
    let result = fx.press("attendant-properties-save");

    // Then a line is published to the attendant itself, so the user learns
    // why nothing happened rather than concluding the key is broken.
    assert_eq!(result.message_names, vec!["PushChatEntry"]);
}

#[rstest::rstest]
#[test]
fn a_save_writes_the_pops_pending_edits() {
    // Given an open popup whose pending values the session does not
    // yet hold — the user typed them but committed nothing.
    let mut fx = SaveFixture::new(Some("nightly"));
    fx.open();
    let session = fx
        .state
        .session
        .get(&fx.attendant_id)
        .expect("attendant")
        .clone();
    let original_template = session.seed_template().to_owned();
    fx.cell
        .update(|p| p.seed_template.input = "the edited template".to_owned());

    // When pressing the save key.
    fx.press("attendant-properties-save");

    // Then the entry holds the edited template, not the pre-edit one.
    // Pressing save means wanting these settings, so the popup's pending
    // values are what get written.
    assert_eq!(fx.saved()[0].seed_template, "the edited template");
    // And the session holds them too, so the commit is not popup-only.
    let after = fx.state.session.get(&fx.attendant_id).expect("attendant");
    assert_eq!(after.seed_template(), "the edited template");
    assert_ne!(after.seed_template(), original_template);
}

#[rstest::rstest]
#[test]
fn an_overwrite_replaces_the_whole_entry() {
    // Given a popup armed against an entry holding fields the session no
    // longer has — the user cleared them, so the new entry must not.
    let mut fx = SaveFixture::new(Some("nightly"));
    fx.open();
    let mut seeded = fx.saved();
    seeded.push(AttendantEntryConfig {
        name: "nightly".to_owned(),
        seed_template: "the original".to_owned(),
        tool_filter: Some(jinn_core_types::NameFilter::deny(["write".to_owned()])),
        pins: vec![AttendantPinConfig {
            role: AttendantPinRole::User,
            text: "a stale instruction".to_owned(),
        }],
        ..AttendantEntryConfig::default()
    });
    fx.config
        .put_list::<AttendantEntryConfig>(&seeded)
        .expect("seed writes");

    // When overwriting it with an entry that has neither field.
    fx.press("attendant-properties-save");
    fx.press("attendant-properties-save");

    // Then both are gone rather than merged back in: an overwrite that
    // only adds fields is not an overwrite.
    let saved = fx.saved();
    assert_eq!(saved.len(), 1);
    assert!(
        saved[0].pins.is_empty(),
        "stale pins survived the overwrite: {:?}",
        saved[0].pins
    );
    assert!(
        saved[0].tool_filter.is_none(),
        "stale tool_filter survived: {:?}",
        saved[0].tool_filter
    );
    assert_ne!(saved[0].seed_template, "the original");
}

#[rstest::rstest]
#[test]
fn closing_after_a_save_reverts_to_the_saved_values() {
    // Given a popup whose pending values differ from the session, saved.
    let mut fx = SaveFixture::new(Some("nightly"));
    fx.open();
    fx.cell
        .update(|p| p.seed_template.input = "the saved template".to_owned());
    fx.press("attendant-properties-save");
    // The user keeps editing after the save.
    fx.cell
        .update(|p| p.seed_template.input = "an unsaved thought".to_owned());

    // When closing with the cancel key.
    fx.press("attendant-properties-leave");

    // Then the popup reverts to what was saved, not to what was on screen
    // when it opened — otherwise closing would undo a save the user was
    // told had succeeded.
    assert_eq!(
        fx.cell.read().seed_template.input,
        "the saved template",
        "revert target is stale"
    );
}

#[rstest::rstest]
#[test]
fn closing_without_saving_still_reverts_to_the_session() {
    // Given a popup with unsaved pending values.
    let mut fx = SaveFixture::new(Some("nightly"));
    fx.open();
    let session_template = fx
        .state
        .session
        .get(&fx.attendant_id)
        .expect("attendant")
        .seed_template()
        .to_owned();
    fx.cell
        .update(|p| p.seed_template.input = "never saved".to_owned());

    // When closing with the cancel key.
    fx.press("attendant-properties-leave");

    // Then the popup reverts to the session's values, which no save moved.
    assert_eq!(fx.cell.read().seed_template.input, session_template);
}

#[rstest::rstest]
#[test]
fn a_save_does_not_close_the_popup() {
    // Given an open popup over a new attendant.
    let mut fx = SaveFixture::new(Some("nightly"));
    fx.open();

    // When pressing the save key.
    let result = fx.press("attendant-properties-save");

    // Then the popup stays open, so the user can see what was written and
    // keep adjusting without reopening.
    assert!(
        !matches!(result.scope_signal, Some(ScopeSignal::PopIf(_))),
        "save closed the popup"
    );
    assert_eq!(
        fx.state.frontend.scope(),
        jinn_slices::FocusScope::Dynamic(attendant_properties_scope())
    );
}

#[rstest::rstest]
#[test]
fn a_new_attendant_saves_on_one_press() {
    // Given an open popup over a titled attendant no entry shares.
    let mut fx = SaveFixture::new(Some("nightly"));
    fx.open();

    // When pressing the save key once.
    fx.press("attendant-properties-save");

    // Then the entry is written immediately — a first save destroys
    // nothing, so it needs no second press.
    let saved = fx.saved();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].name, "nightly");
}

#[rstest::rstest]
#[test]
fn an_existing_name_arms_on_the_first_press_and_writes_nothing() {
    // Given an open popup over an attendant whose name an entry already
    // holds.
    let mut fx = SaveFixture::new(Some("nightly"));
    fx.open();
    let mut seeded = fx.saved();
    let existing = AttendantEntryConfig {
        name: "nightly".to_owned(),
        seed_template: "the original".to_owned(),
        ..AttendantEntryConfig::default()
    };
    seeded.push(existing);
    fx.config
        .put_list::<AttendantEntryConfig>(&seeded)
        .expect("seed writes");

    // When pressing the save key once.
    fx.press("attendant-properties-save");

    // Then nothing is replaced yet.
    assert_eq!(fx.saved()[0].seed_template, "the original");
    // And the popup reports itself armed.
    assert!(fx.cell.read().save_armed);
}

#[rstest::rstest]
#[test]
fn a_second_press_replaces_the_armed_entry() {
    // Given a popup armed against an existing same-named entry.
    let mut fx = SaveFixture::new(Some("nightly"));
    fx.open();
    let mut seeded = fx.saved();
    seeded.push(AttendantEntryConfig {
        name: "nightly".to_owned(),
        seed_template: "the original".to_owned(),
        ..AttendantEntryConfig::default()
    });
    fx.config
        .put_list::<AttendantEntryConfig>(&seeded)
        .expect("seed writes");
    fx.press("attendant-properties-save");
    assert!(fx.cell.read().save_armed);

    // When pressing the save key again.
    fx.press("attendant-properties-save");

    // Then the entry is replaced rather than duplicated.
    let saved = fx.saved();
    assert_eq!(saved.len(), 1);
    assert_ne!(saved[0].seed_template, "the original");
    // And the popup is unarmed again.
    assert!(!fx.cell.read().save_armed);
}

#[rstest::rstest]
#[test]
fn an_armed_popup_does_not_survive_a_cancel() {
    // Given a popup armed against an existing entry.
    let mut fx = SaveFixture::new(Some("nightly"));
    fx.open();
    let mut seeded = fx.saved();
    seeded.push(AttendantEntryConfig {
        name: "nightly".to_owned(),
        ..AttendantEntryConfig::default()
    });
    fx.config
        .put_list::<AttendantEntryConfig>(&seeded)
        .expect("seed writes");
    fx.press("attendant-properties-save");
    assert!(fx.cell.read().save_armed);

    // When leaving with esc.
    fx.press("attendant-properties-leave");

    // Then the arm is gone: reopening the popup must not offer to destroy
    // an entry the user never confirmed.
    assert!(!fx.cell.read().save_armed);
}

#[rstest::rstest]
#[test]
fn an_armed_popup_does_not_survive_an_apply() {
    // Given a popup armed against an existing entry.
    let mut fx = SaveFixture::new(Some("nightly"));
    fx.open();
    let mut seeded = fx.saved();
    seeded.push(AttendantEntryConfig {
        name: "nightly".to_owned(),
        ..AttendantEntryConfig::default()
    });
    fx.config
        .put_list::<AttendantEntryConfig>(&seeded)
        .expect("seed writes");
    fx.press("attendant-properties-save");

    // When applying the pending fields and closing.
    fx.press("attendant-properties-apply");

    // Then the arm is gone with the popup.
    assert!(!fx.cell.read().save_armed);
}

#[rstest::rstest]
#[test]
fn a_save_captures_the_attendants_pins_in_order() {
    // Given a titled attendant with two pinned entries and one unpinned
    // one between them.
    let mut fx = SaveFixture::new(Some("nightly"));
    fx.add_pin("first instruction", PinPosition::Top);
    let session_id = fx.attendant_id.clone();
    fx.state
        .session
        .get_mut(&session_id)
        .expect("attendant")
        .push_entry(ChatEntry::user("chatter"));
    fx.add_pin("second instruction", PinPosition::Bottom);
    fx.open();

    // When saving.
    fx.press("attendant-properties-save");

    // Then the entry carries both instructions, in history order, and not
    // the unpinned entry.
    let saved = fx.saved();
    let texts: Vec<&str> = saved[0].pins.iter().map(|pin| pin.text.as_str()).collect();
    assert_eq!(texts, vec!["first instruction", "second instruction"]);
}

#[rstest::rstest]
#[test]
fn a_save_preserves_the_documents_comments() {
    // Given a document carrying a user comment.
    let mut fx = SaveFixture::new(Some("nightly"));
    fx.open();

    // When saving.
    fx.press("attendant-properties-save");

    // Then the comment is still there — the save went through the config
    // layer's list API, which patches rather than re-serializes.
    assert!(
        fx.config.document_text().contains("# user's own comment"),
        "comment lost:\n{}",
        fx.config.document_text()
    );
}

#[rstest::rstest]
#[test]
fn a_save_records_the_sessions_run_configuration() {
    // Given a titled attendant whose trigger is live and whose behavior
    // is reset.
    let mut fx = SaveFixture::new(Some("nightly"));
    {
        let session = fx
            .state
            .session
            .get_mut(&fx.attendant_id)
            .expect("attendant");
        session.set_attendant_behavior(jinn_attendant_msg::AttendantBehavior::Reset);
        session.set_attendant_trigger(jinn_attendant_msg::AttendantTrigger::ParentCompleted);
        session.set_seed_template("review: <prior report>".to_owned());
    }
    fx.open();

    // When saving.
    fx.press("attendant-properties-save");

    // Then the entry carries the same run configuration, so a created
    // attendant is live from the first run.
    let saved = fx.saved();
    assert_eq!(
        saved[0].behavior,
        jinn_attendant_msg::AttendantBehavior::Reset
    );
    assert_eq!(
        saved[0].trigger,
        jinn_attendant_msg::AttendantTrigger::ParentCompleted
    );
    assert_eq!(saved[0].seed_template, "review: <prior report>");
}

#[rstest::rstest]
#[test]
fn a_save_records_the_sessions_tool_filter() {
    // Given a titled attendant whose tool filter withholds two tools.
    let mut fx = SaveFixture::new(Some("nightly"));
    {
        let session = fx
            .state
            .session
            .get_mut(&fx.attendant_id)
            .expect("attendant");
        session.set_tool_filter(Some(NameFilter::deny([
            "write".to_owned(),
            "bash".to_owned(),
        ])));
    }
    fx.open();

    // When saving.
    fx.press("attendant-properties-save");

    // Then the filter is on the entry, sorted, so the file does not churn.
    let filter = fx.saved()[0]
        .tool_filter
        .clone()
        .expect("filter recorded on save");
    assert_eq!(
        filter.names.iter().map(String::as_str).collect::<Vec<_>>(),
        vec!["bash", "write"]
    );
}

#[rstest::rstest]
#[test]
fn a_save_records_the_sessions_skill_filter() {
    // Given a titled attendant whose skill filter withholds one skill.
    let mut fx = SaveFixture::new(Some("nightly"));
    {
        let session = fx
            .state
            .session
            .get_mut(&fx.attendant_id)
            .expect("attendant");
        session.set_skill_filter(Some(NameFilter::deny(["dataviz".to_owned()])));
    }
    fx.open();

    // When saving.
    fx.press("attendant-properties-save");

    // Then the skill filter is on the entry.
    let filter = fx.saved()[0]
        .skill_filter
        .clone()
        .expect("skill filter recorded on save");
    assert!(!filter.permits("dataviz"));
    assert!(filter.permits("scream"));
}

#[rstest::rstest]
#[test]
fn an_allow_filtered_attendant_saves_with_its_mode_intact() {
    // Given a titled attendant restricted to two tools by an allow filter.
    let mut fx = SaveFixture::new(Some("narrow"));
    {
        let session = fx
            .state
            .session
            .get_mut(&fx.attendant_id)
            .expect("attendant");
        session.set_tool_filter(Some(NameFilter {
            mode: FilterMode::Allow,
            names: ["read".to_owned(), "mcp__github__*".to_owned()]
                .into_iter()
                .collect(),
        }));
    }
    fx.open();

    // When saving.
    fx.press("attendant-properties-save");

    // Then the entry records allow mode, not a blocklist of the same names.
    // Storing the names alone would restore as a deny filter and invert the
    // attendant's whole tool access on its next run.
    let filter = fx.saved()[0]
        .tool_filter
        .clone()
        .expect("filter recorded on save");
    assert_eq!(filter.mode, FilterMode::Allow);
    assert!(filter.permits("mcp__github__create_pr"));
    assert!(!filter.permits("bash"));
}

#[rstest::rstest]
#[test]
fn saving_after_a_freeze_records_the_captured_set_as_an_allow_list() {
    // Given a titled attendant whose tools are a blocklist, with a tool
    // registry offering three tools.
    let mut fx = SaveFixture::new(Some("narrow"));
    fx.state
        .tool_registry()
        .expect("the cell catalog registers the tool registry")
        .update(|registry| {
            for name in ["read", "write", "bash"] {
                registry.global.insert(
                    name.to_owned(),
                    ToolDefinition {
                        name: name.to_owned(),
                        description: format!("{name} description"),
                        parameters: jinn_testutil::json_value(),
                        prompt_snippet: None,
                        prompt_guidelines: Vec::new(),
                        server_tool_type: None,
                    },
                );
            }
        });
    {
        let session = fx
            .state
            .session
            .get_mut(&fx.attendant_id)
            .expect("attendant");
        session.set_tool_filter(Some(NameFilter::deny(["bash".to_owned()])));
    }
    // A composing attendant's cursor is caged at the prep row, so the walk
    // down to the tool set needs composition ended first.
    fx.state
        .session
        .get_mut(&fx.attendant_id)
        .expect("attendant")
        .set_attendant_is_prepping(false);
    fx.open();
    for _ in 0..3 {
        fx.press("attendant-properties-field-next");
    }
    fx.press("attendant-properties-pick-right");

    // When saving.
    fx.press("attendant-properties-save");

    // Then the entry carries allow mode naming exactly the two tools the
    // attendant could use when it was frozen. This is the whole point: the
    // file must carry the frozen set, not the blocklist the picker wrote,
    // and not a glob that would invert on the next run.
    let filter = fx.saved()[0]
        .tool_filter
        .clone()
        .expect("filter recorded on save");
    assert_eq!(filter.mode, FilterMode::Allow);
    assert_eq!(
        filter.names,
        ["read".to_owned(), "write".to_owned()]
            .into_iter()
            .collect()
    );
    // And the skill filter, whose row was left Live, is not recorded at all:
    // an absent filter is what makes the attendant inherit its parent's.
    assert!(fx.saved()[0].skill_filter.is_none());
}

#[rstest::rstest]
#[test]
fn the_save_key_binds_on_the_properties_scope_only() {
    // Given the popup's attached rows.
    let fx = SaveFixture::new(Some("nightly"));

    // When collecting every key the popup binds.
    let keys = fx.bound_keys();

    // Then the save key is among them, exactly once — a second binding
    // would make which stroke wins depend on attach order.
    assert_eq!(
        keys.iter().filter(|key| **key == "<c-s>").count(),
        1,
        "the save key must bind once in the popup: {keys:?}"
    );
}

// ── The status line ────────────────────────────────────────────────

#[rstest::rstest]
#[test]
fn an_armed_overwrite_is_announced_on_the_status_line() {
    // Given an open popup over a saved attendant, with the same name
    // already in the document.
    let mut fx = SaveFixture::new(Some("nightly"));
    fx.presaved("nightly");
    fx.open();

    // When saving onto the existing entry.
    fx.press("attendant-properties-save");

    // Then the status line asks before replacing it.
    assert_eq!(
        fx.cell.read().status,
        Some(PopupStatus::OverwriteArmed {
            name: "nightly".to_owned()
        })
    );
}

#[rstest::rstest]
#[test]
fn a_committed_save_is_announced_on_the_status_line() {
    // Given an open popup over a new attendant.
    let mut fx = SaveFixture::new(Some("nightly"));
    fx.open();

    // When saving.
    fx.press("attendant-properties-save");

    // Then the status line says it saved.
    assert_eq!(
        fx.cell.read().status,
        Some(PopupStatus::Saved {
            name: "nightly".to_owned()
        })
    );
}

#[rstest::rstest]
#[test]
fn an_untitled_refusal_is_announced_on_the_status_line() {
    // Given an open popup over an attendant with no title.
    let mut fx = SaveFixture::new(None);
    fx.open();

    // When saving.
    fx.press("attendant-properties-save");

    // Then the status line says why nothing was saved.
    match fx.cell.read().status.clone() {
        Some(PopupStatus::SaveFailed { reason }) => {
            assert!(
                reason.contains("no name"),
                "the reason is legible: {reason}"
            );
        }
        other => panic!("the line reports a refusal: {other:?}"),
    }
}

#[rstest::rstest]
#[test]
fn a_moving_the_cursor_clears_the_status_line() {
    // Given a popup whose last key armed an overwrite.
    let mut fx = SaveFixture::new(Some("nightly"));
    fx.open();
    fx.press("attendant-properties-save");
    assert!(fx.cell.read().status.is_some());

    // When moving to the next field.
    fx.press("attendant-properties-field-next");

    // Then the line is empty — it described the last key, not the popup.
    assert_eq!(fx.cell.read().status, None);
}

#[rstest::rstest]
#[test]
fn typing_in_the_template_editor_clears_the_status_line() {
    // Given a popup whose last key armed an overwrite, with the editor
    // open over the seed template.
    let mut fx = SaveFixture::new(Some("nightly"));
    fx.open();
    fx.press("attendant-properties-save");
    fx.press("attendant-properties-field-next");
    fx.press("attendant-properties-field-next");
    fx.press("attendant-properties-edit-template");
    fx.press("attendant-properties-save");
    assert!(fx.cell.read().status.is_some());

    // When typing in the editor.
    let hook = crate::properties_overlay::attendant_properties_input_hook(&fx.cell);
    hook(&jinn_slices::EditIntent::InsertChar('x'));

    // Then the line is empty.
    assert_eq!(fx.cell.read().status, None);
}

#[rstest::rstest]
#[test]
fn the_status_line_uses_the_tone_that_matches_the_outcome() {
    // Given a popup, a theme, and one state per outcome.
    let theme = jinn_theme::default_theme();
    let armed = AttendantPropertiesState {
        status: Some(PopupStatus::OverwriteArmed {
            name: "nightly".to_owned(),
        }),
        ..AttendantPropertiesState::default()
    };
    let saved = AttendantPropertiesState {
        status: Some(PopupStatus::Saved {
            name: "nightly".to_owned(),
        }),
        ..AttendantPropertiesState::default()
    };
    let refused = AttendantPropertiesState {
        status: Some(PopupStatus::SaveFailed {
            reason: "no".to_owned(),
        }),
        ..AttendantPropertiesState::default()
    };

    // When rendering each line.
    let line =
        |popup: &AttendantPropertiesState| crate::properties_overlay::status_line(popup, &theme);

    // Then each is in its outcome's theme color.
    let color = |popup: &AttendantPropertiesState| {
        line(popup)
            .spans
            .first()
            .and_then(|span| span.style.fg)
            .expect("the line is styled as one span")
    };
    assert_eq!(color(&armed), theme.warning);
    assert_eq!(color(&saved), theme.success);
    assert_eq!(color(&refused), theme.error_text);
}

#[rstest::rstest]
#[test]
fn a_malformed_document_refuses_the_save_rather_than_replacing_the_list() {
    // Given a document whose entry names a trigger variant that does not
    // exist, so the list cannot be read back.
    let malformed = r#"
[[attendant.entry]]
name = "nightly"
trigger = "parent-completed"
"#;
    let doc = format!("# user's own comment\n{malformed}")
        .parse()
        .expect("parses");
    let mut fx = SaveFixture::new(Some("nightly"));
    fx.config = ConfigLayer::load(Arc::new(InMemoryConfigStorage::new(doc))).expect("loads");
    fx.open();

    // When pressing the save key.
    fx.press("attendant-properties-save");

    // Then the popup says the save failed, rather than reporting
    // success for a write that would have replaced the unreadable
    // entries with just this one.
    assert!(
        matches!(fx.cell.read().status, Some(PopupStatus::SaveFailed { .. })),
        "the popup reports the failed save, got {:?}",
        fx.cell.read().status
    );
    // And the document still holds the entry the save must not have
    // clobbered.
    let after = fx
        .config
        .get_list::<jinn_preferences_config::schemas::AttendantEntryConfig>();
    assert!(after.is_err(), "the document is left as the user wrote it");
}

#[rstest::rstest]
#[test]
fn a_keystroke_between_the_two_presses_withdraws_the_confirmation() {
    // Given a popup armed against an existing entry, with the overwrite
    // prompt on its status line.
    let mut fx = SaveFixture::new(Some("nightly"));
    fx.open();
    fx.config
        .put_list::<AttendantEntryConfig>(&[AttendantEntryConfig {
            name: "nightly".to_owned(),
            seed_template: "the original".to_owned(),
            ..AttendantEntryConfig::default()
        }])
        .expect("seed writes");
    fx.press("attendant-properties-save");
    assert!(fx.cell.read().save_armed);

    // When the user presses some other key — moving the field focus, say.
    fx.press("attendant-properties-field-next");

    // Then the arm is withdrawn along with the prompt that offered it, so
    // the save key that follows is an ordinary first press, not a
    // confirmation of something the user was never shown.
    assert!(
        !fx.cell.read().save_armed,
        "the offer to overwrite is withdrawn with the prompt that displayed it"
    );
    assert!(fx.cell.read().status.is_none());

    // And pressing the save key again re-arms rather than overwriting.
    fx.press("attendant-properties-save");
    let saved = fx.saved();
    assert_eq!(
        saved[0].seed_template, "the original",
        "nothing was replaced"
    );
    assert!(fx.cell.read().save_armed, "the press re-arms the prompt");
}

#[rstest::rstest]
#[test]
fn saving_after_a_skill_freeze_records_the_skill_filter() {
    // Given a titled attendant with two skills discovered.
    let mut fx = SaveFixture::new(Some("skilled"));
    fx.state
        .session
        .get_mut(&fx.attendant_id)
        .expect("attendant")
        .set_discovered_skills(vec![skill("web-coder"), skill("reviewer")]);
    fx.state
        .session
        .get_mut(&fx.attendant_id)
        .expect("attendant")
        .set_attendant_is_prepping(false);
    fx.open();
    for _ in 0..4 {
        fx.press("attendant-properties-field-next");
    }

    // When freezing the skill row and saving.
    fx.press("attendant-properties-pick-right");
    fx.press("attendant-properties-save");

    // Then the entry carries an allow list naming the discovered skills.
    let filter = fx.saved()[0]
        .skill_filter
        .clone()
        .expect("skill filter recorded on save");
    assert_eq!(filter.mode, FilterMode::Allow);
    assert_eq!(
        filter.names,
        ["reviewer".to_owned(), "web-coder".to_owned()]
            .into_iter()
            .collect()
    );
}

/// A discovered skill named `name`, as the discovery scan would leave it.
fn skill(name: &str) -> jinn_skills_msg::Skill {
    jinn_skills_msg::Skill {
        name: name.to_owned(),
        description: format!("{name} description"),
        body: String::new(),
        file_path: std::path::PathBuf::from("/tmp/skill/SKILL.md"),
        base_dir: std::path::PathBuf::from("/tmp/skill"),
        source: jinn_skills_msg::SkillSource::Global,
    }
}

#[rstest::rstest]
#[test]
fn a_saved_frozen_skill_set_reopens_the_panel_as_frozen() {
    // Given an attendant whose saved entry carries a frozen skill set.
    let mut fx = SaveFixture::new(Some("skilled"));
    let filter = NameFilter {
        mode: FilterMode::Allow,
        names: ["web-coder".to_owned()].into_iter().collect(),
    };
    fx.state
        .session
        .get_mut(&fx.attendant_id)
        .expect("attendant")
        .set_skill_filter(Some(filter));
    fx.state
        .session
        .get_mut(&fx.attendant_id)
        .expect("attendant")
        .set_attendant_is_prepping(false);

    // When opening the panel.
    fx.open();

    // Then the skill row reads Frozen. A row that reads Live over a saved
    // allow list means the freeze did not survive the round trip, and the
    // next save would silently drop it.
    assert_eq!(fx.cell.read().pending_skill_set, SetMode::Frozen);
}

/// The end state, exercised end to end: an attendant frozen to nothing,
/// saved, read back out of the document, and rebuilt from that entry.
///
/// This is the whole point of the change. Before it, a freeze over an empty
/// capture could not be written at all — the allow list naming nothing was
/// read back as no filter — so there was no way to say "this attendant has
/// no tools and no skills", and the panel refused the freeze instead.
#[rstest::rstest]
#[test]
fn an_attendant_frozen_to_nothing_saves_with_no_tools_and_no_skills() {
    // Given a titled attendant with nothing offered to freeze: no tool
    // registry at all, and no discovered skills — the shape a freshly
    // created attendant is in until its own scans land.
    let mut fx = SaveFixture::new(Some("hollow"));
    fx.state
        .session
        .get_mut(&fx.attendant_id)
        .expect("attendant")
        .set_attendant_is_prepping(false);
    fx.open();

    // When both set rows are frozen and the attendant is saved. Composition
    // ends first so the cursor is free to walk the whole form.
    fx.press("attendant-properties-field-next");
    fx.press("attendant-properties-field-next");
    fx.press("attendant-properties-field-next");
    fx.press("attendant-properties-pick-right");
    fx.press("attendant-properties-field-next");
    fx.press("attendant-properties-pick-right");
    fx.press("attendant-properties-save");

    // Then the saved entry carries a present allow filter over no names for
    // both resources.
    let entry = fx.saved().first().cloned().expect("one entry saved");
    for filter in [entry.tool_filter.as_ref(), entry.skill_filter.as_ref()] {
        let filter = filter.expect("the freeze was written");
        assert_eq!(
            filter.mode,
            FilterMode::Allow,
            "a frozen set commits as an allow list"
        );
        assert!(
            filter.names.is_empty(),
            "an empty capture is a capture, not a refusal"
        );
    }

    // And an attendant rebuilt from that entry has no tools and no skills.
    let parent = ChatSessionState::new();
    let restored = crate::saved_create::build(&entry, &parent);
    for name in ["read", "bash", "mcp__github__create_pr"] {
        assert!(
            !restored.is_tool_enabled(name),
            "a frozen-to-nothing attendant must refuse {name}"
        );
    }
    for name in ["web-coder", "dataviz", "anything-at-all"] {
        assert!(
            !restored.is_skill_enabled(name),
            "a frozen-to-nothing attendant must refuse {name}"
        );
    }
}

/// The mirror case, and the one that regressed before: thawing must leave
/// the field absent, not write an empty filter over it. An empty allow
/// filter written by a thaw would leave the attendant frozen to nothing
/// while the panel claimed the set was live.
#[rstest::rstest]
#[test]
fn thawing_a_frozen_set_leaves_the_attendant_with_no_filter() {
    // Given a titled attendant whose tool set is frozen.
    let mut fx = SaveFixture::new(Some("thawed"));
    fx.state
        .session
        .get_mut(&fx.attendant_id)
        .expect("attendant")
        .set_attendant_is_prepping(false);
    fx.open();
    fx.press("attendant-properties-field-next");
    fx.press("attendant-properties-field-next");
    fx.press("attendant-properties-field-next");
    fx.press("attendant-properties-pick-right");
    fx.press("attendant-properties-apply");

    // When the row is thawed and applied again.
    fx.press("attendant-properties-pick-left");
    fx.press("attendant-properties-apply");

    // Then the session holds no filter at all, so it inherits again and
    // admits every tool.
    assert!(
        fx.state
            .session
            .get(&fx.attendant_id)
            .expect("attendant")
            .tool_filter()
            .is_none(),
        "a thaw writes absence, not an empty filter"
    );
    assert!(
        fx.state
            .session
            .get(&fx.attendant_id)
            .expect("attendant")
            .is_tool_enabled("bash")
    );
}

/// An absent filter must stay absent through a save, or "inherit" would
/// quietly become "pinned to today's configuration".
#[rstest::rstest]
#[test]
fn a_save_of_an_unconstrained_attendant_records_no_filter() {
    // Given a titled attendant with no filter for either resource.
    let mut fx = SaveFixture::new(Some("bare"));
    fx.open();

    // When saving without touching either set row.
    fx.press("attendant-properties-save");

    // Then the entry carries neither.
    let entry = fx.saved().first().cloned().expect("one entry saved");
    assert!(entry.tool_filter.is_none());
    assert!(entry.skill_filter.is_none());
}

/// A created attendant takes its parent's profile — filters included — but
/// none of the parent's discovered resources. Under the old lenient rule
/// that combination was harmless; under the strict one, a parent carrying
/// an allow filter produces a child holding that filter and no resources,
/// which is why freezing to nothing had to become expressible at all.
#[rstest::rstest]
#[test]
fn a_created_attendant_inherits_the_parents_empty_allow_filter_as_nothing_allowed() {
    // Given a parent whose allow filter names no tools.
    let mut parent = ChatSessionState::new();
    parent.set_tool_filter(Some(NameFilter {
        mode: FilterMode::Allow,
        names: BTreeSet::new(),
    }));

    // When an attendant is created from an entry that configured nothing.
    let entry = AttendantEntryConfig {
        name: "child".to_owned(),
        ..AttendantEntryConfig::default()
    };
    let child = crate::saved_create::build(&entry, &parent);

    // Then it inherits nothing-allowed rather than inheriting nothing at all.
    assert!(!child.is_tool_enabled("read"));
}
