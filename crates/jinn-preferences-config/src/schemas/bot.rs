//! The `[[bot.entry]]` list — saved bots, restorable by name.
//!
//! A saved bot is a snapshot of one session's run configuration taken
//! when the user saves it from the properties popup: how it fires, what it
//! does to its context, the model and persona it runs under, and the pins
//! that carry its standing instructions. Creating from an entry reproduces
//! the configuration verbatim and inherits only what a saved file cannot
//! know — the creating session's cwd, project, and the rest of its
//! environment.
//!
//! The entry stores the *session-state* types (`BotBehavior`,
//! `BotTrigger`, `ModelSelection`, `ReasoningEffort`, `Endpoint`) via
//! serde rather than TOML-native stand-ins, so restore means "deserialize"
//! and never "translate field by field" — a divergence between the entry
//! shape and the session shape cannot compile.

use jinn_bot_msg::{BotBehavior, BotModelSetting, BotTrigger};
use jinn_core_types::{Endpoint, ModelSelection, NameFilter, ReasoningEffort};
use serde::{Deserialize, Serialize};

impl jinn_config::ConfigList for BotEntryConfig {
    const KEY: &'static str = "bot.entry";
    /// The key saved bots carried before the attendant→bot rename. Read
    /// only; a save writes the live key.
    const LEGACY_KEY: Option<&'static str> = Some("attendant.entry");
    const ENTRY_KEY: &'static str = "name";
    const ENTRY_FIELDS: &'static [&'static str] = &[
        "name",
        "behavior",
        "trigger",
        "prep_mode",
        "seed_template",
        "model",
        "persona_name",
        "tool_filter",
        "skill_filter",
        "reasoning_effort",
        "endpoint",
        "pins",
    ];
}

/// One saved bot, defined in `jinn.toml` under `[[bot.entry]]`.
///
/// # Behavior: Snapshot Read.
///
/// The file is read when a bot is created (and by bot-management
/// actions), so an edit applies at the next bot creation; a running bot
/// is not reconfigured mid-flight.
///
/// The `name` field is the array key the patcher matches entries by, so
/// saving over an existing name replaces that entry in place — comments on
/// its siblings survive.
///
/// "Not configured" means absent: a field that is `None` (or an empty
/// disablement list) is inherited from the creating session at create time;
/// a field that is present overrides. A model serialized as the
/// no-provider placeholder counts as not configured and inherits too —
/// [`Self::configured_model`].
///
/// Every field type is `PartialEq`, so equality is derived from the struct
/// rather than compared through a serialized form. That was not always so:
/// `pins` used to hold whole chat entries, which have no `PartialEq`, and
/// [`same_entry`] compared the two documents instead.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BotEntryConfig {
    /// The entry's identity, and the title the created bot gets.
    pub name: String,
    /// What a run in the bot sees of the conversation.
    #[serde(default)]
    pub behavior: BotBehavior,
    /// The condition that causes an automatic re-run.
    #[serde(default)]
    pub trigger: BotTrigger,
    /// Whether the bot is still being composed, and so does not run.
    ///
    /// Saved like any other run setting: a bot saved in prep mode is
    /// a half-written one, and restoring it as a running bot would
    /// dispatch instructions the user never finished.
    #[serde(default = "default_prep_mode")]
    pub prep_mode: bool,
    /// The text injected ahead of each run's prior report.
    #[serde(default = "default_seed_template")]
    pub seed_template: String,
    /// The model selection, absent when the saved session had no provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelSelection>,
    /// The persona name, absent when the saved session ran the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona_name: Option<String>,
    /// Which tools this bot may use.
    ///
    /// Same mode-plus-glob filter as `[tools]`. Stored as an optional filter
    /// rather than a plain list: an allow-mode bot must survive a
    /// round trip, and a bare list could not say which mode it meant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_filter: Option<NameFilter>,
    /// Which skills this bot may load, as above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill_filter: Option<NameFilter>,
    /// Reasoning effort, absent when the saved session set none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Pinned OpenRouter routing endpoint, absent when none was pinned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<Endpoint>,
    /// The session's pinned instructions at save time, in history order.
    ///
    /// A saved pin is a role and a text — see [`BotPinConfig`] for
    /// why nothing else is stored. Order is the whole point: a bot's
    /// standing instructions are a *sequence*, so restore rebuilds them as
    /// pinned history in this order with fresh entry ids.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pins: Vec<BotPinConfig>,
}

/// One standing instruction a saved bot is spawned with.
///
/// A persisted pin is only the instruction text and the role the model reads
/// it as. Everything else a chat entry carries is either derived when the
/// bot is created or deliberately left out because a restored
/// bot is a new session and the old values would be lies: entry ids
/// and timestamps describe the session this was saved from, and a persisted
/// token count is never recomputed — it stays stale forever.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotPinConfig {
    /// Whether the model reads this instruction as something the user said
    /// or something the agent produced.
    ///
    /// Required, with no `#[serde(default)]`. A user instruction and an
    /// agent result with identical text are *different messages* to the
    /// model, and nothing in the text distinguishes them. Defaulting a
    /// missing role to `user` would restore the bot's own prior turns
    /// as facts it was told, so a malformed pin must fail loudly instead.
    pub role: BotPinRole,

    /// The instruction itself.
    pub text: String,
}

/// The entry kinds a saved bot can carry.
///
/// These are exactly the kinds `entries_to_messages` turns into a message.
/// System and actor entries are excluded from context by default, and a tool
/// result is a snapshot of a file as it was — re-injected into a newly
/// spawned bot, it asserts stale contents as current fact.
///
/// If a future change makes another kind reach the prompt, the save-side
/// filter must follow it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BotPinRole {
    /// An instruction the user gave the bot.
    User,
    /// A result the agent produced that the bot should keep seeing.
    Assistant,
}

impl BotPinConfig {
    /// This instruction as a live entry, or `None` for a kind the model
    /// never reads.
    ///
    /// `None` is what keeps a tool result, system line, or actor notice out
    /// of the spawn schema — see [`BotPinRole`] for why. An
    /// `Intersession` captures as a `User`-role pin: a parent→bot
    /// dispatch rides the prompt as a plain user turn, so a pinned dispatch
    /// is a standing instruction the model reads as the user's.
    #[must_use]
    pub fn from_entry(entry: &jinn_core_types::ChatEntry) -> Option<Self> {
        let role = match &entry.kind {
            jinn_core_types::ChatEntryKind::User { .. }
            | jinn_core_types::ChatEntryKind::Intersession { .. } => BotPinRole::User,
            jinn_core_types::ChatEntryKind::Assistant(_) => BotPinRole::Assistant,
            _ => return None,
        };
        Some(Self {
            role,
            text: entry.text().clone(),
        })
    }

    /// This instruction as a live entry, for a fresh session to pin.
    ///
    /// The two roles construct differently: a user entry carries a
    /// display/expanded split and an assistant entry a single string. Both
    /// fields get the same text here because nothing in the tree ever makes
    /// them differ, and filling both keeps the round trip honest if that
    /// ever changes.
    ///
    /// Pin position is deliberately *not* set here — restore decides, and
    /// the reason it picks `Relative` belongs to the restore path.
    #[must_use]
    pub fn to_entry(&self) -> jinn_core_types::ChatEntry {
        match self.role {
            BotPinRole::User => {
                jinn_core_types::ChatEntry::user_expanded(self.text.clone(), self.text.clone())
            }
            BotPinRole::Assistant => jinn_core_types::ChatEntry::assistant(&self.text),
        }
    }
}

/// Serde default for the seed template, matching the session state's own
/// default so an entry written without the field seeds like a fresh
/// bot does.
fn default_seed_template() -> String {
    jinn_bot_msg::default_seed_template()
}

/// Prep mode's default in a config file.
///
/// True, matching the session default: an entry that does not say it is
/// composing says it is composing, the same way a session blob that omits
/// the key does.
const fn default_prep_mode() -> bool {
    true
}

impl BotEntryConfig {
    /// Captures a save-ready entry from a session's live configuration.
    ///
    /// `name` is the caller's decision (the session's title); everything
    /// else reads off the session state types the fields mirror. Pin
    /// entries pass through as-is — serialization drops what TOML cannot
    /// carry and restore regenerates IDs.
    ///
    /// `model_setting` is the bot's *claim* to its model, not the model
    /// itself: both are passed, and only the setting decides whether the entry
    /// keeps a `model` key. A session with no provider configured is a
    /// separate reason to store nothing — there is no model to record as its
    /// own — and the two do not collapse into one another.
    #[must_use]
    pub fn from_parts(
        name: String,
        behavior: BotBehavior,
        trigger: BotTrigger,
        prep_mode: bool,
        seed_template: String,
        model_setting: BotModelSetting,
        model: &ModelSelection,
        persona_name: &str,
        tool_filter: Option<&NameFilter>,
        skill_filter: Option<&NameFilter>,
        reasoning_effort: Option<ReasoningEffort>,
        endpoint: Option<&Endpoint>,
        pins: Vec<BotPinConfig>,
    ) -> Self {
        Self {
            name,
            behavior,
            trigger,
            prep_mode,
            seed_template,
            // The bot's own model, stored whole. An inheriting
            // bot stores nothing: without this test a hand-written
            // entry with no `model` key would acquire a pinned one the first
            // time it was saved from the panel. A session with no provider
            // stores nothing either — it would refuse to dispatch, so there
            // is no model to record as its own.
            model: (model_setting.is_fixed() && !model.is_no_provider()).then(|| model.clone()),
            // The default persona is the default on restore too; storing
            // it would only pin the entry to today's default value.
            persona_name: (!jinn_core_types::DEFAULT_PERSONA_NAME.eq(persona_name))
                .then(|| persona_name.to_owned()),
            // Absence is the filter's own: a session carrying no filter
            // stores none, so create inherits the parent's exactly as an
            // absent disablement list used to. A session carrying a present
            // filter stores that filter whole, including one naming nothing —
            // a bot frozen to no tools has to survive the round trip
            // as the bot that has no tools.
            tool_filter: tool_filter.cloned(),
            skill_filter: skill_filter.cloned(),
            reasoning_effort,
            endpoint: endpoint.cloned(),
            pins,
        }
    }

    /// The entry's configured model, `None` when the entry carries none.
    ///
    /// The deserialized form of "no provider" is treated as absent, so a
    /// hand-written entry that names the placeholder inherits like an
    /// absent field rather than configuring the placeholder.
    #[must_use]
    pub fn configured_model(&self) -> Option<&ModelSelection> {
        self.model.as_ref().filter(|model| !model.is_no_provider())
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use std::collections::BTreeSet;

    use std::sync::Arc;

    use jinn_bot_msg::{BotBehavior, BotModelSetting, BotTrigger};
    use jinn_config::{ConfigLayer, InMemoryConfigStorage};
    use jinn_core_types::{
        Endpoint, FilterMode, ModelSelection, NO_PROVIDER_ID, NameFilter, ReasoningEffort,
    };

    use super::{BotEntryConfig, BotPinConfig, BotPinRole};

    /// A layer over a document given as a string.
    fn layer_over(body: &str) -> ConfigLayer {
        let doc = body.parse().expect("test TOML parses");
        ConfigLayer::load(Arc::new(InMemoryConfigStorage::new(doc))).expect("layer loads")
    }

    /// The plainest entry that still serializes: no model, no persona, no
    /// filters, so a test's field assertions measure only what it set.
    fn entry_with_pins(pins: Vec<BotPinConfig>) -> BotEntryConfig {
        BotEntryConfig::from_parts(
            "pinned".to_owned(),
            BotBehavior::Reset,
            BotTrigger::Manual,
            false,
            jinn_bot_msg::default_seed_template(),
            BotModelSetting::Inherit,
            &ModelSelection::Single(NO_PROVIDER_ID.to_owned()),
            jinn_core_types::DEFAULT_PERSONA_NAME,
            None,
            None,
            None,
            None,
            pins,
        )
    }

    /// A fully-populated entry, the shape `entries_round_trip_through_the_
    /// config_layer` reads back.
    ///
    /// Every field is set, so a round-trip failure names the field that did
    /// not survive rather than one that was never in the fixture.
    fn entry_with_filters(
        tool_filter: Option<&NameFilter>,
        skill_filter: Option<&NameFilter>,
    ) -> BotEntryConfig {
        let endpoint = Endpoint {
            tag: "zai".to_owned(),
            provider_name: "ZAI".to_owned(),
        };
        BotEntryConfig::from_parts(
            "reviewer".to_owned(),
            BotBehavior::Reset,
            BotTrigger::ParentCompleted,
            false,
            "prior: <prior_report>".to_owned(),
            BotModelSetting::Fixed,
            &ModelSelection::Single("zai/glm-4.7".to_owned()),
            "reviewer",
            tool_filter,
            skill_filter,
            Some(ReasoningEffort::High),
            Some(&endpoint),
            Vec::new(),
        )
    }

    #[rstest::rstest]
    #[test]
    fn entries_round_trip_through_the_config_layer() {
        // Given a document carrying one full bot entry.
        let layer = layer_over(
            r#"
            [[bot.entry]]
            name = "reviewer"
            behavior = "reset"
            trigger = "parent_completed"
            prep_mode = false
            seed_template = "prior: <prior_report>"
            model = { single = "zai/glm-4.7" }
            persona_name = "reviewer"
            reasoning_effort = "high"

            [bot.entry.tool_filter]
            mode = "deny"
            names = ["write"]

            [bot.entry.skill_filter]
            mode = "deny"
            names = ["bash", "edit"]

            [bot.entry.endpoint]
            tag = "zai"
            provider_name = "ZAI"
        "#,
        );

        // When reading the list.
        let entries = layer.get_list::<BotEntryConfig>().expect("list reads");

        // Then the entry reads back field for field, filters included.
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0],
            entry_with_filters(
                Some(&NameFilter::deny(["write".to_owned()])),
                Some(&NameFilter::deny(["bash".to_owned(), "edit".to_owned()])),
            ),
            "round trip lost fields: {}",
            layer.document_text()
        );
    }

    #[rstest::rstest]
    #[test]
    fn an_allow_mode_filter_survives_the_document() {
        // Given a document whose bot is restricted to two tools.
        let layer = layer_over(
            r#"
            [[bot.entry]]
            name = "narrow"

            [bot.entry.tool_filter]
            mode = "allow"
            names = ["read", "mcp__github__*"]
        "#,
        );

        // When reading the list.
        let entries = layer.get_list::<BotEntryConfig>().expect("list reads");

        // Then the mode is not lost — a plain list could not carry it, so
        // this is the case that made the field a filter rather than names.
        let filter = entries[0].tool_filter.as_ref().expect("filter present");
        assert_eq!(filter.mode, FilterMode::Allow);
        assert!(filter.permits("mcp__github__create_pr"));
        assert!(!filter.permits("bash"));
    }

    #[rstest::rstest]
    #[test]
    fn an_absent_filter_is_stored_as_absent() {
        // Given a session carrying no filter for either resource.
        let entry = entry_with_filters(None, None);

        // When reading the stored filters.
        // Then both are absent, so create inherits the parent's.
        assert!(entry.tool_filter.is_none());
        assert!(entry.skill_filter.is_none());
    }

    /// The distinction the field exists to carry: a present allow list over
    /// nothing is a filter that permits nothing, and storing it must not
    /// collapse into the absent case.
    #[rstest::rstest]
    #[test]
    fn a_present_empty_allow_filter_is_stored_as_present() {
        // Given a session whose filter permits no tool at all.
        let filter = NameFilter {
            mode: FilterMode::Allow,
            names: BTreeSet::default(),
        };
        let entry = entry_with_filters(Some(&filter), None);

        // When reading the stored filter.
        // Then it is present, and still permits nothing.
        assert_eq!(entry.tool_filter, Some(filter));
    }

    #[rstest::rstest]
    #[test]
    fn clearing_a_filter_removes_it_from_the_document() {
        // Given a document whose entry carries a tool filter. The header and
        // the name must both match what is written back, or the patcher finds
        // no entry to update and the test would pass for the wrong reason.
        let storage = Arc::new(InMemoryConfigStorage::new(
            "[[bot.entry]]\nname = \"reviewer\"\n\
             \n[bot.entry.tool_filter]\nmode = \"deny\"\nnames = [\"write\"]\n"
                .parse()
                .expect("parses"),
        ));
        let layer = ConfigLayer::load(storage.clone()).expect("load");

        // When the entry is re-saved with no filter configured.
        layer
            .put_list::<BotEntryConfig>(&[entry_with_filters(None, None)])
            .expect("list writes");

        // Then the key is gone from the document. This is what `ENTRY_FIELDS`
        // buys: without `tool_filter` in that list the patcher has no
        // declaration for the key, leaves the stale block in place, and the
        // filter survives a save that should have cleared it.
        let text = storage.text();
        assert!(
            !text.contains("tool_filter"),
            "a cleared filter survived the save:\n{text}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn an_absent_bot_list_reads_empty() {
        // Given a document with no bot umbrella at all.
        let layer = layer_over("[tools]\ndefault_timeout_secs = 60");

        // When reading the list.
        let entries = layer.get_list::<BotEntryConfig>().expect("list reads");

        // Then it is empty rather than an error — first-save needs no
        // special case.
        assert_eq!(entries, [] as [BotEntryConfig; 0]);
    }

    #[rstest::rstest]
    #[test]
    fn a_minimal_entry_defaults_its_bot_fields() {
        // Given an entry naming only what it must.
        let layer = layer_over("[[bot.entry]]\nname = \"bare\"\n");

        // When reading the list.
        let entries = layer.get_list::<BotEntryConfig>().expect("list reads");

        // Then the entry is a composing manual bot with the shared
        // seed template and nothing configured. A bare entry does not run:
        // the same default `N` gives the session it came from.
        let e = &entries[0];
        assert_eq!(e.behavior, BotBehavior::Reset);
        assert_eq!(e.trigger, BotTrigger::Manual);
        assert!(e.prep_mode);
        assert_eq!(e.seed_template, jinn_bot_msg::default_seed_template());
        assert_eq!(e.configured_model(), None);
        assert_eq!(e.persona_name, None);
        assert_eq!(e.pins, [] as [BotPinConfig; 0]);
    }

    #[rstest::rstest]
    #[test]
    fn overwriting_one_entry_preserves_sibling_comments() {
        // Given a document whose two entries carry user comments.
        let doc = r#"
            # the nightly reviewer
            [[bot.entry]]
            name = "reviewer"
            trigger = "parent_completed"

            # runs on every parent turn
            [[bot.entry]]
            name = "watcher"
        "#
        .parse()
        .expect("test TOML parses");
        let storage = Arc::new(InMemoryConfigStorage::new(doc));
        let layer = ConfigLayer::load(storage.clone()).expect("load");

        // When rewriting the whole list with the first entry changed.
        let mut entries = layer.get_list::<BotEntryConfig>().expect("read");
        entries[0].behavior = BotBehavior::Reset;
        layer
            .put_list::<BotEntryConfig>(&entries)
            .expect("list writes");

        // Then both comments survive and the edit landed.
        let text = storage.text();
        assert!(text.contains("# the nightly reviewer"), "lost:\n{text}");
        assert!(
            text.contains("# runs on every parent turn"),
            "lost:\n{text}"
        );
        let reread = layer.get_list::<BotEntryConfig>().expect("reread");
        assert_eq!(reread[0].behavior, BotBehavior::Reset);
        assert_eq!(reread[0].trigger, BotTrigger::ParentCompleted);
    }

    #[rstest::rstest]
    #[test]
    fn two_saves_of_an_unchanged_entry_are_byte_identical() {
        // Given a layer over an otherwise-commented document.
        let doc = "# user header\n".parse().expect("test TOML parses");
        let storage = Arc::new(InMemoryConfigStorage::new(doc));
        let layer = ConfigLayer::load(storage.clone()).expect("load");
        // With nested values present, because that is where the two save
        // paths used to disagree. An entry with no pins and no model passes
        // trivially: an empty array already rendered inline, so the fixture
        // never reached the oscillation.
        let saved = entry_with_filters(
            Some(&NameFilter::deny(["write".to_owned()])),
            Some(&NameFilter::deny(["bash".to_owned()])),
        );
        let mut with_pins = saved.clone();
        with_pins.pins = vec![
            BotPinConfig {
                role: BotPinRole::User,
                text: "always in context".to_owned(),
            },
            BotPinConfig {
                role: BotPinRole::Assistant,
                text: "the agreed verdict".to_owned(),
            },
        ];

        // When saving the same entry twice (second save is an overwrite).
        layer
            .put_list::<BotEntryConfig>(std::slice::from_ref(&with_pins))
            .expect("first save");
        let once = storage.text();
        layer
            .put_list::<BotEntryConfig>(std::slice::from_ref(&with_pins))
            .expect("second save");
        let twice = storage.text();

        // Then the document did not move between saves: the filter's names
        // are a `BTreeSet`, so serialization order cannot churn the user's
        // file between runs.
        assert_eq!(
            once, twice,
            "document moved:\nonce:\n{once}\ntwice:\n{twice}"
        );
    }

    /// The rendered `pins` line, with spacing around `=` normalized away.
    ///
    /// The patcher preserves the whitespace the old line carried, so a file
    /// hand-edited to `pins= [...]` stays tight. Asserting on the shape
    /// rather than the spacing keeps these tests about inline-vs-header.
    fn pins_line(text: &str) -> Option<String> {
        text.lines()
            .find(|line| line.trim_start().starts_with("pins"))
            .map(|line| line.replace("pins=", "pins ="))
    }

    /// An entry with both nested-value kinds present, so a rendering
    /// assertion can cover a table and an array at once.
    fn entry_with_pins_and_model() -> BotEntryConfig {
        let mut e = entry_with_pins(vec![BotPinConfig {
            role: BotPinRole::User,
            text: "always in context".to_owned(),
        }]);
        e.model = Some(ModelSelection::Single("zai/glm-4.7".to_owned()));
        e
    }

    #[rstest::rstest]
    #[test]
    fn an_entries_values_all_render_as_key_value_pairs() {
        // Given a bot carrying both kinds of nested value.
        let storage = Arc::new(InMemoryConfigStorage::new(
            "[tools]\ndefault_timeout_secs = 60\n"
                .parse()
                .expect("parses"),
        ));
        let layer = ConfigLayer::load(storage.clone()).expect("load");
        layer
            .put_list::<BotEntryConfig>(&[entry_with_pins_and_model()])
            .expect("write");

        // When reading the rendered document.
        let text = storage.text();

        // Then the entry is one block and every value sits on it as
        // `key = value`. A nested `[[bot.entry.pins]]` is a *sibling*
        // of its entry in TOML's grammar, not a child — it reads as the next
        // bot and can bind to the wrong one.
        assert!(
            text.contains("model = { single = \"zai/glm-4.7\" }"),
            "model not inline:\n{text}"
        );
        assert_eq!(
            pins_line(&text).as_deref(),
            Some("pins = [{ role = \"user\", text = \"always in context\" }]"),
            "pins not inline:\n{text}"
        );
        assert!(
            !text.contains("[bot.entry.model]"),
            "model rendered as a sub-table:\n{text}"
        );
        assert!(
            !text.contains("[[bot.entry.pins]]"),
            "pins rendered as a sub-list:\n{text}"
        );
        assert_eq!(
            text.matches("[[bot.entry]]").count(),
            1,
            "expected exactly one entry block:\n{text}"
        );
    }

    #[rstest::rstest]
    #[case::no_entry_yet("")]
    #[case::already_inline(
        "[[bot.entry]]\nname = \"pinned\"\npins = [{ role = \"user\", text = \"a\" }]\n"
    )]
    #[case::left_in_header_form(
        "[[bot.entry]]\nname = \"pinned\"\n[[bot.entry.pins]]\nrole = \"user\"\ntext = \"a\"\n"
    )]
    fn an_entry_renders_the_same_way_from_any_starting_shape(#[case] body: &str) {
        // Given a document in one of the shapes a save can be handed.
        let storage = Arc::new(InMemoryConfigStorage::new(
            body.parse().expect("test TOML parses"),
        ));
        let layer = ConfigLayer::load(storage.clone()).expect("load");
        let saved = entry_with_pins(vec![BotPinConfig {
            role: BotPinRole::User,
            text: "a".to_owned(),
        }]);

        // When saving it.
        layer
            .put_list::<BotEntryConfig>(std::slice::from_ref(&saved))
            .expect("write");

        // Then the pins render inline regardless of how the document already
        // held them — the rendered form must not depend on whether the key
        // was already present, which is what made consecutive saves disagree.
        let text = storage.text();
        assert_eq!(
            pins_line(&text).as_deref(),
            Some("pins = [{ role = \"user\", text = \"a\" }]"),
            "pins not inline from this starting shape:\n{text}"
        );
        assert!(
            !text.contains("[[bot.entry.pins]]"),
            "pins left in header form:\n{text}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn two_entries_keep_their_own_pins() {
        // Given two bots pinned to different instructions.
        let storage = Arc::new(InMemoryConfigStorage::new(
            "[tools]\nx = 1\n".parse().expect("parses"),
        ));
        let layer = ConfigLayer::load(storage.clone()).expect("load");
        let mut first = entry_with_pins(vec![BotPinConfig {
            role: BotPinRole::User,
            text: "one".to_owned(),
        }]);
        first.name = "first".to_owned();
        let mut second = entry_with_pins(vec![
            BotPinConfig {
                role: BotPinRole::User,
                text: "two-a".to_owned(),
            },
            BotPinConfig {
                role: BotPinRole::User,
                text: "two-b".to_owned(),
            },
        ]);
        second.name = "second".to_owned();

        // When saving both and reading them back.
        layer
            .put_list::<BotEntryConfig>(&[first, second])
            .expect("write");
        let read = layer.get_list::<BotEntryConfig>().expect("read");

        // Then each entry holds exactly its own pins — a nested list must
        // never bind to the neighbouring entry.
        assert_eq!(read[0].name, "first");
        assert_eq!(read[0].pins.len(), 1);
        assert_eq!(read[0].pins[0].text, "one");
        assert_eq!(read[1].name, "second");
        assert_eq!(read[1].pins.len(), 2);
        assert_eq!(read[1].pins[0].text, "two-a");
        assert_eq!(read[1].pins[1].text, "two-b");
    }

    #[rstest::rstest]
    #[test]
    fn a_comment_above_an_entry_survives_rewriting_its_pins() {
        // Given a document whose bot entry carries a user comment.
        let storage = Arc::new(InMemoryConfigStorage::new(
            "# nightly reviewer\n[tools]\nx = 1\n"
                .parse()
                .expect("parses"),
        ));
        let layer = ConfigLayer::load(storage.clone()).expect("load");

        // When saving a bot with pins.
        layer
            .put_list::<BotEntryConfig>(&[entry_with_pins(vec![BotPinConfig {
                role: BotPinRole::User,
                text: "always in context".to_owned(),
            }])])
            .expect("write");

        // Then the comment survives — inline rendering must not cost the
        // user their notes.
        assert!(
            storage.text().contains("# nightly reviewer"),
            "lost:\n{}",
            storage.text()
        );
    }

    #[rstest::rstest]
    #[test]
    fn a_fixed_bot_stores_its_model() {
        // Given a session that owns its model.
        let entry = BotEntryConfig::from_parts(
            "reviewer".to_owned(),
            BotBehavior::Reset,
            BotTrigger::Manual,
            false,
            "prior: <prior_report>".to_owned(),
            BotModelSetting::Fixed,
            &ModelSelection::Single("zai/glm-4.7".to_owned()),
            jinn_core_types::DEFAULT_PERSONA_NAME,
            None,
            None,
            None,
            None,
            Vec::new(),
        );

        // When reading its configured model.
        let configured = entry.configured_model();

        // Then the model is stored, so recreating the bot from this
        // entry runs on the same provider.
        assert_eq!(
            configured,
            Some(&ModelSelection::Single("zai/glm-4.7".to_owned()))
        );
    }

    #[rstest::rstest]
    #[test]
    fn an_inheriting_bot_stores_no_model() {
        // Given a session holding a concrete model but not claiming it.
        let entry = BotEntryConfig::from_parts(
            "reviewer".to_owned(),
            BotBehavior::Reset,
            BotTrigger::Manual,
            false,
            "prior: <prior_report>".to_owned(),
            BotModelSetting::Inherit,
            &ModelSelection::Single("zai/glm-4.7".to_owned()),
            jinn_core_types::DEFAULT_PERSONA_NAME,
            None,
            None,
            None,
            None,
            Vec::new(),
        );

        // When reading its configured model.
        let configured = entry.configured_model();

        // Then it stores none. The session always holds a model, so storing
        // whatever it holds is what would silently pin an inheriting
        // bot on its first save.
        assert_eq!(configured, None);
    }

    #[rstest::rstest]
    #[test]
    fn a_fixed_bot_with_no_provider_stores_no_model() {
        // Given a session that owns its model but has no provider.
        let entry = BotEntryConfig::from_parts(
            "reviewer".to_owned(),
            BotBehavior::Reset,
            BotTrigger::Manual,
            false,
            "prior: <prior_report>".to_owned(),
            BotModelSetting::Fixed,
            &ModelSelection::Single(NO_PROVIDER_ID.to_owned()),
            jinn_core_types::DEFAULT_PERSONA_NAME,
            None,
            None,
            None,
            None,
            Vec::new(),
        );

        // When reading its configured model.
        let configured = entry.configured_model();

        // Then it stores none — for a different reason than an inheriting
        // bot. There is no model here to record as its own, and the
        // placeholder must never be written into the file.
        assert_eq!(configured, None);
    }

    #[rstest::rstest]
    #[test]
    fn saving_an_inheriting_bot_leaves_a_hand_written_entry_without_a_model() {
        // Given a document holding a hand-written entry that names no model.
        let storage = Arc::new(InMemoryConfigStorage::new(
            "[[bot.entry]]\nname = \"reviewer\"\nbehavior = \"reset\"\n"
                .parse()
                .expect("parses"),
        ));
        let layer = ConfigLayer::load(storage.clone()).expect("load");
        let mut entry = layer.get_list::<BotEntryConfig>().expect("read").remove(0);
        // The entry inherits, by the absence of the key.
        entry.model = None;

        // When it is saved back from the panel.
        layer
            .put_list::<BotEntryConfig>(&[entry])
            .expect("list writes");

        // Then no `model` key appears. This is the whole point of the
        // setting: the panel writes the entry's own configuration, and an
        // entry that never claimed a model must not acquire one.
        let text = storage.text();
        assert!(
            !text.contains("model"),
            "an inheriting entry gained a model on save:\n{text}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn switching_the_setting_touches_only_the_model_key() {
        // Given a document holding a complete entry whose model is its own.
        // Every declared key is present, so the save's only possible
        // difference is the one this test is about.
        let storage = Arc::new(InMemoryConfigStorage::new(
            "[[bot.entry]]\nname = \"reviewer\"\nbehavior = \"reset\"\n\
             trigger = \"manual\"\nprep_mode = false\n\
             seed_template = \"prior: <prior_report>\"\n\
             model = { single = \"zai/glm-4.7\" }\n"
                .parse()
                .expect("parses"),
        ));
        let layer = ConfigLayer::load(storage.clone()).expect("load");
        let mut entry = layer.get_list::<BotEntryConfig>().expect("read").remove(0);
        let before = storage.text();

        // When the user moves the panel's model row to inherit and saves.
        entry.model = None;
        layer
            .put_list::<BotEntryConfig>(&[entry])
            .expect("list writes");

        // Then the only difference is the model line — the rest of the
        // entry, and the user's own comment above it, are untouched.
        let after = storage.text();
        assert!(!after.contains("model"), "the model key survived:\n{after}");
        assert_eq!(
            before.replace("model = { single = \"zai/glm-4.7\" }\n", ""),
            after,
            "something other than the model key changed:\nbefore:\n{before}\nafter:\n{after}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn two_saves_of_a_fixed_entry_are_byte_identical() {
        // Given a layer holding a fixed entry with a model and a pin.
        let storage = Arc::new(InMemoryConfigStorage::new(
            "# user header\n".parse().expect("parses"),
        ));
        let layer = ConfigLayer::load(storage.clone()).expect("load");
        let mut entry = entry_with_filters(None, None);
        entry.pins = vec![BotPinConfig {
            role: BotPinRole::User,
            text: "always in context".to_owned(),
        }];

        // When saving it twice.
        layer
            .put_list::<BotEntryConfig>(std::slice::from_ref(&entry))
            .expect("first save");
        let once = storage.text();
        layer
            .put_list::<BotEntryConfig>(std::slice::from_ref(&entry))
            .expect("second save");
        let twice = storage.text();

        // Then the document did not move, and the model key is in it — the
        // inline rendering of a `model` has to be stable for a fixed
        // bot, or every save would churn the user's file.
        assert_eq!(
            once, twice,
            "document moved:\nonce:\n{once}\ntwice:\n{twice}"
        );
        assert!(
            once.contains("model = { single = \"zai/glm-4.7\" }"),
            "a fixed bot must write its model:\n{once}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn a_no_provider_model_is_never_configured() {
        // Given an entry whose model deserialized to the no-provider
        // placeholder (as a hand-written entry naming it would).
        let layer = layer_over(&format!(
            "[[bot.entry]]\nname = \"x\"\nmodel = {{ single = \"{NO_PROVIDER_ID}\" }}\n"
        ));

        // When reading its configured model.
        let entries = layer.get_list::<BotEntryConfig>().expect("read");

        // Then it counts as not configured — create inherits the parent's
        // model instead of configuring the placeholder.
        assert!(entries[0].configured_model().is_none());
    }

    #[rstest::rstest]
    #[test]
    fn pinned_instructions_round_trip_through_the_document() {
        // Given a document carrying one user instruction and one the agent
        // produced.
        let saved = entry_with_pins(vec![
            BotPinConfig {
                role: BotPinRole::User,
                text: "always in context".to_owned(),
            },
            BotPinConfig {
                role: BotPinRole::Assistant,
                text: "the agreed verdict".to_owned(),
            },
        ]);
        let storage = Arc::new(InMemoryConfigStorage::new(
            "# header\n".parse().expect("parses"),
        ));
        let layer = ConfigLayer::load(storage.clone()).expect("load");
        layer
            .put_list::<BotEntryConfig>(std::slice::from_ref(&saved))
            .expect("write");

        // When the list is read back.
        let read = layer.get_list::<BotEntryConfig>().expect("read");

        // Then both pins survive, in order, with their roles intact — a
        // user instruction must not come back as an agent result.
        assert_eq!(read[0].pins, saved.pins, "document:\n{}", storage.text());
    }

    #[rstest::rstest]
    #[test]
    fn a_pinned_intersession_dispatch_captures_as_a_user_role_pin_and_restores() {
        // Given a pinned parent→bot dispatch entry.
        let source = jinn_core_types::SessionId::new();
        let target = jinn_core_types::SessionId::new();
        let dispatch =
            jinn_core_types::ChatEntry::intersession(source, None, target, "always check tests");

        // When capturing it for a saved bot.
        let pin = BotPinConfig::from_entry(&dispatch).expect("dispatch captures");

        // Then it is a User-role pin — the dispatch rides the prompt as a
        // plain user turn — and restores as a pinned user entry.
        assert_eq!(pin.role, BotPinRole::User);
        assert_eq!(pin.text, "always check tests");
        let restored = pin.to_entry();
        assert!(
            matches!(restored.kind, jinn_core_types::ChatEntryKind::User { .. }),
            "a captured dispatch restores as a user entry"
        );
    }

    #[rstest::rstest]
    #[test]
    fn a_pin_carries_a_role_and_a_text_and_nothing_else() {
        // Given an entry carrying one instruction.
        let saved = entry_with_pins(vec![BotPinConfig {
            role: BotPinRole::User,
            text: "always in context".to_owned(),
        }]);

        // When it is serialized.
        let text = toml::to_string(&saved).expect("serializes");

        // Then no field a restored session derives — or would be lied to by
        // — reaches the file: no id, timing, token count, pin position, or
        // the display/expanded split. A leak serializes as a TOML *key*,
        // so each name is matched at line start: the default seed
        // template's prose legitimately says "id" mid-sentence, and prose
        // is not a field.
        for leaked in [
            "id",
            "timing",
            "token_count",
            "pin_position",
            "display",
            "expanded",
            "context_override",
            "context_history",
        ] {
            let key = format!("\n{leaked} =");
            assert!(
                !text.contains(&key),
                "a derived field leaked into the file as {leaked}:\n{text}"
            );
        }
    }

    #[rstest::rstest]
    #[test]
    fn a_pin_without_a_role_fails_to_read_rather_than_defaulting() {
        // Given a hand-written entry whose pin names a text but no role.
        let doc = "[[bot.entry]]\nname = \"x\"\npins = [{ text = \"hi\" }]\n";

        // When reading the list.
        let layer = layer_over(doc);

        // Then the read fails. Defaulting the role to user would restore an
        // agent result as an instruction the user gave.
        let result = layer.get_list::<BotEntryConfig>();

        assert!(result.is_err(), "a roleless pin was accepted");
    }

    #[rstest::rstest]
    #[case(BotPinRole::User, "user")]
    #[case(BotPinRole::Assistant, "assistant")]
    fn a_pin_role_writes_its_kind_name(#[case] role: BotPinRole, #[case] wire: &str) {
        // Given an entry carrying a pin of this role.
        let saved = entry_with_pins(vec![BotPinConfig {
            role,
            text: "hi".to_owned(),
        }]);

        // When it is serialized.
        let text = toml::to_string(&saved).expect("serializes");

        // Then the role is spelled the way `ChatEntry::kind_str` spells it,
        // so the file reads in one vocabulary.
        assert!(
            text.contains(&format!("role = \"{wire}\"")),
            "expected role = \"{wire}\":\n{text}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn a_long_instruction_shrinks_to_a_fraction_of_a_whole_entry() {
        // Given an instruction long enough that a whole chat entry's
        // derived fields would dominate its line.
        let instruction = "keep the module boundary intact ".repeat(40);

        // When an entry carries it as a role and text.
        let now = toml::to_string(&entry_with_pins(vec![BotPinConfig {
            role: BotPinRole::User,
            text: instruction.clone(),
        }]))
        .expect("serializes")
        .len();

        // Then it takes roughly half what the same entry took when `pins`
        // held whole chat entries. Both sides are whole entries, so the
        // shared fields cancel out and the difference is the pin's cost
        // alone. The old shape is measured, not assumed — it still
        // serializes, so this compares real bytes.
        let old = toml::to_string(&legacy_entry_with_pin(&instruction))
            .expect("serializes")
            .len();
        let ratio = f64::from(u32::try_from(now).expect("length fits")) / old as f64;
        assert!(
            (0.45..0.65).contains(&ratio),
            "expected roughly a half, got {ratio:.2} ({now} vs {old})"
        );
    }

    /// An entry shaped as `pins` used to be: whole pinned chat entries.
    ///
    /// Retained only to measure what the role-and-text schema saves. Nothing
    /// in the tree reads this shape any more — the struct exists solely so
    /// the size assertion compares against the real old bytes rather than a
    /// mirrored guess that could drift from them.
    #[derive(serde::Serialize)]
    struct LegacyEntryConfig {
        name: String,
        behavior: BotBehavior,
        trigger: BotTrigger,
        prep_mode: bool,
        seed_template: String,
        pins: Vec<jinn_core_types::ChatEntry>,
    }

    fn legacy_entry_with_pin(instruction: &str) -> LegacyEntryConfig {
        LegacyEntryConfig {
            name: "pinned".to_owned(),
            behavior: BotBehavior::Reset,
            trigger: BotTrigger::Manual,
            prep_mode: false,
            seed_template: jinn_bot_msg::default_seed_template(),
            pins: vec![jinn_core_types::ChatEntry {
                pin_position: Some(jinn_core_types::PinPosition::Relative),
                ..jinn_core_types::ChatEntry::user(instruction)
            }],
        }
    }
}
