//! Bot properties popup — the edit state for its seven fields.
//!
//! The popup is a two-phase vim-style form over a bot's trigger,
//! behavior, prep mode, tool set, skill set, model, and seed template.
//! `j`/`k` move the form cursor between fields, `h`/`l` act within the
//! focused field, and `i` opens the seed-template editor on its own scope.
//! Every edit stays pending — the popup never touches the session — until
//! the user applies every field at once. The values the session had at open
//! are snapshotted so leaving restores them exactly.
//!
//! Prep mode cages the two rows above it. While the bot is being
//! composed nothing runs, so its trigger and behavior are values the user
//! has written but that do not apply; they stay on screen, dimmed, and the
//! cursor cannot reach them, which is a stronger statement than letting the
//! cursor arrive and having the pick do nothing. The tool and skill rows sit
//! *below* the prep row and are never caged: a bot being composed
//! still has a tool budget, and a frozen set is what the bot is being
//! composed *for*. The model row is never caged for the same reason: which
//! provider a bot will run under is settled while composing it, not
//! after.
//!
//! Choice rows carry their display order and labels here ([`TRIGGER_CHOICES`],
//! [`BEHAVIOR_CHOICES`], [`MODEL_CHOICES`]) so the renderer and the pick
//! helpers share one source of truth; nothing downstream hardcodes a choice
//! string.

use std::collections::BTreeSet;

use crate::{BotBehavior, BotModelSetting, BotTrigger};
use jinn_core_types::{FilterMode, NameFilter};
use jinn_slices::LineInput;
use jinn_slices::SlotKey;

/// The `bot/properties` slot: the properties popup's single edit state.
///
/// The seed-template editor shares this slot — it is only ever open while a
/// properties edit is pending, so a second cell would model nothing.
#[must_use]
pub fn bot_properties_slot() -> SlotKey {
    SlotKey::builtin("bot", "properties")
}

/// The properties popup's scope: navigation-only.
///
/// `j`/`k`/`h`/`l`/`i` drive the form. The scope captures no input — typed
/// characters never land here; the seed template edits through
/// [`bot_seed_template_scope`].
#[must_use]
pub fn bot_properties_scope() -> jinn_slices::slice_scope::SliceScopeId {
    jinn_slices::slice_scope::SliceScopeId::navigation("bot", "properties")
}

/// The seed-template editor's scope: input-capturing, rename-popup parity.
///
/// Pushed by the properties popup's `i` row and popped by the editor's
/// keep/restore/clear rows. Capturing is what generates the editing keys
/// and the printable-character catch-all for the editor's input hook.
#[must_use]
pub fn bot_seed_template_scope() -> jinn_slices::slice_scope::SliceScopeId {
    jinn_slices::slice_scope::SliceScopeId::new("bot", "seed-template")
}

/// The trigger's choices in display order: first shown leftmost.
pub const TRIGGER_CHOICES: &[(BotTrigger, &str)] = &[
    (BotTrigger::ParentCompleted, "parent-completed"),
    (BotTrigger::Manual, "manual"),
];

/// The behavior's choices in display order: first shown leftmost.
pub const BEHAVIOR_CHOICES: &[(BotBehavior, &str)] = &[
    (BotBehavior::Reset, "reset"),
    (BotBehavior::Preserve, "preserve"),
];

/// The tool-set and skill-set rows' choices in display order: first shown
/// leftmost.
///
/// Declared here beside the trigger's and the behavior's so the renderer
/// reads every choice row from one place, and so the set rows are ordered
/// the same way as the rest of the form rather than as a bespoke pair of
/// spans.
pub const SET_MODE_CHOICES: &[(SetMode, &str)] =
    &[(SetMode::Live, "live"), (SetMode::Frozen, "frozen")];

/// The model row's choices in display order: first shown leftmost.
///
/// `inherit` is first because it is the default and the row is read far more
/// often as "this bot has no model of its own" than as "and it does".
/// The labels are the settings' own names, so the row, the session blob, and
/// the choice all say one word rather than two that have to be kept in step.
pub const MODEL_CHOICES: &[(BotModelSetting, &str)] = &[
    (BotModelSetting::Inherit, "inherit"),
    (BotModelSetting::Fixed, "fixed"),
];

/// Whether a bot's tools or skills are held fixed or keep growing.
///
/// The two rows this names are the only place a bot's capability set
/// can be pinned to what it is. A blocklist — the shape the pickers write —
/// can only say "never these"; it says nothing about the tools a server
/// starts contributing next week, so every bot silently gains them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SetMode {
    /// The bot inherits whatever its parent has, including resources
    /// discovered after this bot was saved. The default: a bot
    /// nobody constrained follows the user's configuration as it grows.
    #[default]
    Live,
    /// The bot is restricted to the names it had at the moment it was
    /// frozen. Anything discovered afterward is refused.
    Frozen,
}

/// One field of the properties form, in display order.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PropertyField {
    /// When the bot re-runs on its own. A choice field (`h`/`l`).
    #[default]
    Trigger,
    /// What the bot's context looks like per run. A choice field.
    Behavior,
    /// Whether the bot is still being composed. A two-state field:
    /// `h`/`l` cycle it, and while it is on, the two rows above do not
    /// apply.
    PrepMode,
    /// Whether new tools are admitted automatically or refused. A two-state
    /// field; freezing captures the tools the bot currently permits.
    ToolSet,
    /// Whether new skills are admitted automatically or refused. A two-state
    /// field, over the same shape as the tool set.
    SkillSet,
    /// Whether the session's model belongs to this bot or was inherited
    /// with the parent's. A choice field, over two choices that are not
    /// peers — the one on screen is what the session already holds.
    Model,
    /// The seed text, edited through the template editor (`i`).
    SeedTemplate,
}

impl PropertyField {
    /// The next field in display order, stopping at the last.
    ///
    /// While the bot is being composed the two rows above the prep
    /// row are unreachable, so moving *up* off the prep row stays put
    /// rather than landing on a row that governs nothing. The rows are
    /// still rendered — a hidden row is not a disabled one, and the value
    /// the user wrote has to be visible to be worth keeping. The rows below
    /// it are never gated: they still apply to a bot being composed.
    #[must_use]
    pub fn next(self, prep_mode: bool) -> Self {
        let stepped = match self {
            Self::Trigger => Self::Behavior,
            Self::Behavior => Self::PrepMode,
            Self::PrepMode => Self::ToolSet,
            Self::ToolSet => Self::SkillSet,
            Self::SkillSet => Self::Model,
            Self::Model | Self::SeedTemplate => Self::SeedTemplate,
        };
        match stepped {
            Self::Trigger | Self::Behavior if prep_mode => self,
            _ => stepped,
        }
    }

    /// The previous field in display order, stopping at the first reachable.
    ///
    /// The step is the plain one row up, then clamped to the first row the
    /// cursor may rest on. While the bot is being composed that floor
    /// is the prep row, so moving up off it stays put rather than landing on
    /// one of the two rows that do not apply. Clamping *after* stepping is
    /// what keeps a single `k` meaning "the row above me" — computing the
    /// target from the floor instead would make one press skip a row as
    /// soon as composition ended, because the floor had moved.
    #[must_use]
    pub fn previous(self, prep_mode: bool) -> Self {
        let floor = Self::first_reachable(prep_mode);
        match self {
            Self::Trigger => floor,
            Self::Behavior => Self::Trigger,
            Self::PrepMode => Self::Behavior,
            Self::ToolSet => Self::PrepMode,
            Self::SkillSet => Self::ToolSet,
            Self::Model => Self::SkillSet,
            Self::SeedTemplate => Self::Model,
        }
        .clamp_to(floor)
    }

    /// The nearest field at or above this one that the cursor may rest on.
    fn clamp_to(self, floor: Self) -> Self {
        if rank(self) < rank(floor) {
            floor
        } else {
            self
        }
    }

    /// The first field the cursor may rest on, given the prep state.
    #[must_use]
    pub fn first_reachable(prep_mode: bool) -> Self {
        if prep_mode {
            Self::PrepMode
        } else {
            Self::Trigger
        }
    }

    /// The field the form cursor rests on when the popup opens.
    ///
    /// Prep mode is where a composing bot's cursor belongs: it is the
    /// one row above which two inapplicable rows sit, so it is the only row
    /// a user looking at a fresh bot can usefully act on. A bot
    /// that is not being composed opens on the first row as before.
    #[must_use]
    pub fn opening_focus(prep_mode: bool) -> Self {
        Self::first_reachable(prep_mode)
    }

    /// Whether this field applies while the bot is being composed.
    ///
    /// The seed template survives: pins are the whole point of composing,
    /// so a template written during prep is exactly what the first run
    /// should inject. So do the two set rows — a frozen tool or skill set is
    /// precisely the constraint a user composes a bot *under*, so
    /// caging them would make the row useless for as long as it is needed.
    /// And so does the model row: it says whether the bot's model is
    /// its own, which is one of the first things a bot is being
    /// composed *with* — the provider a future run will use.
    #[must_use]
    pub fn applies_while_prepping(self) -> bool {
        !matches!(self, Self::Trigger | Self::Behavior)
    }

    /// The field's label as shown in the popup.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Trigger => "trigger",
            Self::Behavior => "behavior",
            Self::PrepMode => "prep mode",
            Self::ToolSet => "tool set",
            Self::SkillSet => "skill set",
            Self::Model => "model",
            Self::SeedTemplate => "seed template",
        }
    }
}

/// A field's position in display order, for comparing two of them.
///
/// Declared after the enum rather than as a `rank` on it because a field's
/// place in the *form* is the form's business: the enum names seven settings,
/// and nothing about the settings themselves says which row they sit on.
fn rank(field: PropertyField) -> u8 {
    match field {
        PropertyField::Trigger => 0,
        PropertyField::Behavior => 1,
        PropertyField::PrepMode => 2,
        PropertyField::ToolSet => 3,
        PropertyField::SkillSet => 4,
        PropertyField::Model => 5,
        PropertyField::SeedTemplate => 6,
    }
}

/// Which way an `h`/`l` pick moves through a choice row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickDirection {
    /// `h` — toward the first (leftmost) choice.
    Left,
    /// `l` — toward the last (rightmost) choice.
    Right,
}

/// Picks the choice adjacent to `current`, clamped at the row's ends.
///
/// Derives the position from the value itself, so the pending value is the
/// only stored state and cannot desync from an index.
fn pick_choice<'a, T>(
    choices: &'a [(T, &'static str)],
    current: &T,
    direction: PickDirection,
) -> Option<&'a T>
where
    T: PartialEq,
{
    if choices.is_empty() {
        return None;
    }
    let index = choices.iter().position(|(value, _)| value == current)?;
    let next = match direction {
        PickDirection::Left => index.saturating_sub(1),
        PickDirection::Right => (index + 1).min(choices.len() - 1),
    };
    choices.get(next).map(|(value, _)| value)
}

/// Picks the adjacent trigger choice, clamped at the row's ends.
#[must_use]
pub fn pick_trigger(current: BotTrigger, direction: PickDirection) -> BotTrigger {
    pick_choice(TRIGGER_CHOICES, &current, direction)
        .copied()
        .unwrap_or(current)
}

/// Picks the adjacent behavior choice, clamped at the row's ends.
#[must_use]
pub fn pick_behavior(current: BotBehavior, direction: PickDirection) -> BotBehavior {
    pick_choice(BEHAVIOR_CHOICES, &current, direction)
        .copied()
        .unwrap_or(current)
}

/// Picks the adjacent model choice, clamped at the row's ends.
#[must_use]
pub fn pick_model_setting(current: BotModelSetting, direction: PickDirection) -> BotModelSetting {
    pick_choice(MODEL_CHOICES, &current, direction)
        .copied()
        .unwrap_or(current)
}

/// The values a bot had when the properties popup opened.
///
/// Leaving the popup restores these; applying replaces them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OriginalValues {
    /// The trigger at open time.
    pub trigger: BotTrigger,
    /// The behavior at open time.
    pub behavior: BotBehavior,
    /// Whether the bot was being composed at open time.
    pub prep_mode: bool,
    /// The frozen tool set at open time.
    ///
    /// Carried as a whole filter rather than a bare [`SetMode`] because
    /// opening the popup is what *reads* the bot's filter, and the
    /// filter is where the row's meaning lives. Absent means Live, and so
    /// does a deny filter — only a present allow-mode filter says the set has
    /// been pinned, including one naming nothing.
    pub tool_set: Option<NameFilter>,
    /// The frozen skill set at open time, as above.
    pub skill_set: Option<NameFilter>,
    /// Whether the bot's model was its own at open time.
    ///
    /// Held as the setting rather than as the model it decides about: the
    /// setting is the fact, and the model is the value every session already
    /// has whether or not it owns it.
    pub model_setting: BotModelSetting,
    /// The seed template at open time.
    pub template: String,
}

impl OriginalValues {
    /// The set mode a filter of this shape represents.
    ///
    /// Only an allow-mode filter is a frozen set. A deny filter is a
    /// blocklist — "never these" — which says nothing about the names
    /// nobody thought to withhold, so a bot carrying one is still
    /// growing with its parent's configuration.
    ///
    /// This is the one reading of a filter both the popup's opener and its
    /// restore path use, so a row can never mean one thing when it opens
    /// and another when `<esc>` puts it back.
    #[must_use]
    pub fn mode_of(filter: Option<&NameFilter>) -> SetMode {
        if filter.is_some_and(|filter| filter.mode == FilterMode::Allow) {
            SetMode::Frozen
        } else {
            SetMode::Live
        }
    }

    /// The names an already-frozen filter would commit as, mode included.
    ///
    /// The filter's own patterns rather than a set re-derived from them: a
    /// hand-written allow list's globs are already in the correct mode, and
    /// flattening them into names would quietly narrow what the user wrote.
    ///
    /// `Some` with an empty set is a real answer — a set frozen to nothing —
    /// and is distinct from `None`, which says the row is Live.
    #[must_use]
    pub fn names_of(filter: Option<&NameFilter>) -> Option<BTreeSet<String>> {
        (Self::mode_of(filter) == SetMode::Frozen)
            .then(|| filter.map_or_default(|filter| filter.names.clone()))
    }
}

/// State for the bot properties popup.
///
/// Opened from the sessions section with `P`; every field targets the
/// highlighted bot session. Nothing here reaches the session — the
/// popup holds pending edits until the apply row commits them together.
#[derive(Debug, Clone, Default)]
pub struct BotPropertiesState {
    /// The session the popup is editing. `None` while the popup is closed.
    pub session_id: Option<jinn_core_types::SessionId>,
    /// The field the form cursor is on (`j`/`k`).
    pub focus: PropertyField,
    /// What the behavior `<enter>` would apply right now.
    pub pending_behavior: BotBehavior,
    /// The trigger `<enter>` would apply right now.
    pub pending_trigger: BotTrigger,
    /// Whether `<enter>` would leave the bot in prep mode.
    pub pending_prep_mode: bool,
    /// Whether `<enter>` would pin the bot's tools.
    pub pending_tool_set: SetMode,
    /// Whether the user has acted on the tool row this popup session.
    ///
    /// Distinct from the mode, and the reason: the mode is a reading of the
    /// bot's own filter, so an untouched row on a hand-written
    /// blocklist commits back that blocklist and changes nothing. What the
    /// commit has to act on is whether the *user* moved the row, because a
    /// thaw has to write the unconfigured filter — leaving the allow list
    /// in place is what pinned a thawed bot to its old set forever.
    pub tool_set_touched: bool,
    /// The tool names a Frozen tool set would be pinned to.
    ///
    /// Captured at the moment the row was flipped, never recomputed: the
    /// capture is a statement of what the bot had *then*, and reading
    /// the live filter again at commit time would capture whatever the
    /// configuration has since become. `None` while the row is Live.
    pub frozen_tools: Option<BTreeSet<String>>,
    /// Whether `<enter>` would pin the bot's skills.
    pub pending_skill_set: SetMode,
    /// Whether the user has acted on the skill row, as for the tool row.
    pub skill_set_touched: bool,
    /// The skill names a Frozen skill set would be pinned to, as above.
    pub frozen_skills: Option<BTreeSet<String>>,
    /// Whether `<enter>` would store the bot's model into its entry.
    pub pending_model_setting: BotModelSetting,
    /// Whether the bot being edited has a parent session.
    ///
    /// A parentless bot is standalone: its trigger can never fire, because
    /// every trigger but `manual` means "when the parent's turn
    /// completes" and there is no parent. The flag is read at open time
    /// and never edited; it dims the trigger row and makes `h`/`l` on it a
    /// no-op, the same treatment the prep cage gives that row on a
    /// composing bot.
    pub has_parent: bool,
    /// The values the session had at open; `<esc>`/`<c-c>` restore them.
    /// `None` while the popup is closed.
    pub original: Option<OriginalValues>,
    /// The editable seed-template text + cursor.
    pub seed_template: LineInput,
    /// The template text captured when `i` opened the editor; `<esc>` in
    /// the editor restores it. `None` while the editor is closed.
    pub editor_original: Option<String>,
    /// Whether the help overlay is showing. Toggled with `?`; it targets
    /// whichever field the form cursor is on, so it is never stale.
    pub help_visible: bool,
    /// Whether a second `<c-s>` would overwrite a same-named saved
    /// bot.
    ///
    /// Saving over an existing entry destroys whatever that entry
    /// currently holds, so it is armed by the first stroke and committed by
    /// the second; a first save of a new name needs no second stroke,
    /// because there is nothing to destroy. The flag is popup-local and
    /// cleared on every path that leaves the popup (apply, `<esc>`,
    /// `<c-c>`, a committed save), so an armed popup can never leak an
    /// armed state into the next one.
    pub save_armed: bool,
    /// The popup's one-line status, shown under the form.
    ///
    /// Holds what the last key did: a save that wrote, a save that was
    /// refused, an overwrite waiting on its second stroke, a mode change
    /// that had to drop a pattern. The form has no other way to answer a
    /// key — the session is only written by `<enter>`, and a flip that
    /// silently discarded a glob would otherwise be invisible.
    ///
    /// There is no message for a freeze that captured nothing, because there
    /// is nothing to report: an allow list over no names is a filter like
    /// any other, and it is written.
    ///
    /// Cleared by the next keystroke, so the line always describes the most
    /// recent thing that happened rather than accumulating a history of
    /// things that did.
    pub status: Option<PopupStatus>,
}

/// Which of the two set rows a flip or a capture concerns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetField {
    /// The tool-set row, over the session's tool filter.
    Tool,
    /// The skill-set row, over the session's skill filter.
    Skill,
}

impl SetField {
    /// The resource's name, as the status line reports it.
    #[must_use]
    pub fn resource(self) -> &'static str {
        match self {
            Self::Tool => "tool",
            Self::Skill => "skill",
        }
    }
}

/// What the popup's status line reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PopupStatus {
    /// An overwrite is armed; the next `<c-s>` replaces the entry.
    OverwriteArmed {
        /// The name whose entry is about to be replaced.
        name: String,
    },
    /// A save wrote the bot to `jinn.toml`.
    Saved {
        /// The name the bot was saved under.
        name: String,
    },
    /// A save did not happen, and this is why.
    SaveFailed {
        /// The reason, in the user's terms.
        reason: String,
    },
    /// A mode change dropped one or more glob patterns from the filter.
    GlobDropped {
        /// Which set row changed: the tool set or the skill set.
        field: SetField,
    },
}

impl BotPropertiesState {
    /// Moves the form cursor to the next field, stopping at the last.
    ///
    /// While the bot is being composed the cursor cannot pass up
    /// through the prep row, so the two rows above are unreachable.
    pub fn focus_next(&mut self) {
        self.focus = self.focus.next(self.pending_prep_mode);
    }

    /// Moves the form cursor to the previous field, stopping at the first.
    pub fn focus_previous(&mut self) {
        self.focus = self.focus.previous(self.pending_prep_mode);
    }

    /// Acts on the focused field: picks a choice, or cycles prep mode.
    ///
    /// `h` and `l` are the same key on a two-state field — there is no
    /// previous or next, only the other value — so a two-state cycle is
    /// what the caller gets whichever key it pressed.
    ///
    /// A no-op on the seed-template field: it is not a choice row, it edits
    /// through the template editor. A no-op on a choice row while the
    /// bot is being composed, which the cage already makes
    /// unreachable; the guard is here so a caller that reaches the row
    /// some other way cannot move a value that governs nothing.
    ///
    /// A no-op on either set row. Freezing one has to read the bot's
    /// live capability set, which this cell cannot reach; [`Self::set_mode`]
    /// is that operation, and a bare cycle here would freeze the row
    /// against nothing.
    ///
    /// The model row *is* pickable: it says whether the bot owns the
    /// model the session already holds, and every answer to that is decided
    /// here — nothing outside the panel has to be read to make it.
    pub fn pick(&mut self, direction: PickDirection) {
        match self.focus {
            PropertyField::Trigger | PropertyField::Behavior if self.pending_prep_mode => {}
            // A standalone bot's trigger is locked: `manual` is the only
            // value that means anything without a parent, so the choice
            // cannot be moved even though the cursor may rest on the row.
            PropertyField::Trigger if !self.has_parent => {}
            PropertyField::Trigger => {
                self.pending_trigger = pick_trigger(self.pending_trigger, direction);
            }
            PropertyField::Behavior => {
                self.pending_behavior = pick_behavior(self.pending_behavior, direction);
            }
            PropertyField::PrepMode => {
                self.pending_prep_mode = !self.pending_prep_mode;
            }
            PropertyField::Model => {
                self.pending_model_setting =
                    pick_model_setting(self.pending_model_setting, direction);
            }
            PropertyField::ToolSet | PropertyField::SkillSet | PropertyField::SeedTemplate => {}
        }
    }

    /// Whether the user has moved this popup's cursor onto a set row and
    /// acted on it, as opposed to the popup having merely opened over an
    /// bot that already carried a filter.
    #[must_use]
    pub fn set_touched(&self, field: SetField) -> bool {
        match field {
            SetField::Tool => self.tool_set_touched,
            SetField::Skill => self.skill_set_touched,
        }
    }

    /// The pending mode of one of the two set rows.
    #[must_use]
    pub fn set_mode_of(&self, field: SetField) -> SetMode {
        match field {
            SetField::Tool => self.pending_tool_set,
            SetField::Skill => self.pending_skill_set,
        }
    }

    /// The names the pending mode of `field` would be committed as.
    ///
    /// `None` while the row is Live, which is the signal to leave the
    /// bot's filter for that resource alone and let it inherit.
    #[must_use]
    pub fn pending_set(&self, field: SetField) -> Option<&BTreeSet<String>> {
        match field {
            SetField::Tool => self.frozen_tools.as_ref(),
            SetField::Skill => self.frozen_skills.as_ref(),
        }
    }

    /// Sets a set row to `mode`, from the capabilities `permitted` names.
    ///
    /// Freezing records what the bot has *now*, as an allow list of
    /// the names it currently permits. Capturing at the moment of the flip
    /// rather than at commit is the whole point: the popup buffers edits so
    /// `<esc>` can withdraw them, and a capture taken later would be a
    /// reading of a configuration the user never looked at.
    ///
    /// The effective set is preserved exactly, so nothing in either picker
    /// changes its mark when the bot is frozen — only names that
    /// arrive *later* are refused, and those appear unchecked.
    ///
    /// A capture that permits nothing is a no-op: it stores no names and
    /// leaves the row Live. An empty allow list is indistinguishable from no
    /// filter at all — `NameFilter::permits` reads an empty set as
    /// "everything" in both modes — so writing one would look like a frozen
    /// set in `jinn.toml` and behave as an inheriting one, which is exactly
    /// the leak freezing exists to stop.
    ///
    /// Flipping back to Live discards the capture outright rather than
    /// caching it. Nothing was written when the row was frozen, so a
    /// round trip has nothing to restore, and holding a second copy of every
    /// filter on the panel would be state no one reads.
    pub fn set_mode(&mut self, field: SetField, mode: SetMode, permitted: &BTreeSet<String>) {
        let (frozen, next) = match mode {
            SetMode::Live => (SetMode::Live, None),
            // An empty capture is recorded as Frozen with no names, and the
            // commit is what declines to write it. Refusing the mode here
            // instead would make the row unselectable on a bot that
            // has discovered nothing yet, which is the *default* state of a
            // fresh bot — the user could reach Frozen on no row but
            // that one, and would see the key do nothing at all.
            SetMode::Frozen => (SetMode::Frozen, Some(permitted.clone())),
        };
        match field {
            SetField::Tool => {
                self.pending_tool_set = frozen;
                self.frozen_tools = next;
                self.tool_set_touched = true;
            }
            SetField::Skill => {
                self.pending_skill_set = frozen;
                self.frozen_skills = next;
                self.skill_set_touched = true;
            }
        }
    }

    /// Restores every field to the values captured at open, discarding all
    /// pending edits and any editor draft.
    ///
    /// A no-op without a snapshot (the popup was never opened). Also
    /// disarms the save: this is the `<esc>`/`<c-c>` close path, and an
    /// armed overwrite must not survive into the next popup session.
    ///
    /// The set rows go back to what the bot's filters said, captures
    /// included. Nothing was written while they were being flipped, so this
    /// is what makes leaving the popup byte-for-byte invisible to the
    /// session.
    pub fn restore_original(&mut self) {
        self.save_armed = false;
        let Some(original) = self.original.clone() else {
            return;
        };
        let cursor_pos = original.template.len();
        self.pending_behavior = original.behavior;
        self.pending_trigger = original.trigger;
        self.pending_prep_mode = original.prep_mode;
        self.restore_set(SetField::Tool, original.tool_set.as_ref());
        self.restore_set(SetField::Skill, original.skill_set.as_ref());
        self.pending_model_setting = original.model_setting;
        self.seed_template = LineInput {
            input: original.template,
            cursor_pos,
        };
        self.editor_original = None;
    }

    /// Puts one set row back to the open-time filter, capture included.
    ///
    /// A filter that was already a frozen set keeps its own names rather
    /// than a capture: the user did not ask for this row to change, and
    /// re-deriving the names from a filter the user did not author would
    /// silently drop the patterns it contains.
    fn restore_set(&mut self, field: SetField, filter: Option<&NameFilter>) {
        let mode = OriginalValues::mode_of(filter);
        let captured = OriginalValues::names_of(filter);
        match field {
            SetField::Tool => {
                self.pending_tool_set = mode;
                self.frozen_tools = captured;
            }
            SetField::Skill => {
                self.pending_skill_set = mode;
                self.frozen_skills = captured;
            }
        }
    }

    /// Captures the current template text as the editor's restore point.
    pub fn begin_template_edit(&mut self) {
        self.editor_original = Some(self.seed_template.input.clone());
    }

    /// Restores the pre-editor template text and closes the editor state.
    pub fn cancel_template_edit(&mut self) {
        if let Some(original) = self.editor_original.take() {
            let cursor_pos = original.len();
            self.seed_template = LineInput {
                input: original,
                cursor_pos,
            };
        }
    }

    /// Accepts the edited text and closes the editor state.
    pub fn keep_template_edit(&mut self) {
        self.editor_original = None;
    }

    /// Arms an overwrite of the named saved bot.
    ///
    /// The arming is what a second `<c-s>` confirms: the entry that exists
    /// under this name is about to be replaced by this session's
    /// configuration, and a user who did not mean to replace it can still
    /// change the name before pressing again.
    pub fn arm_save(&mut self) {
        self.save_armed = true;
    }

    /// Disarms the overwrite, after a committed save.
    pub fn disarm_save(&mut self) {
        self.save_armed = false;
    }

    /// Accepts the popup's current values as the restore point.
    ///
    /// Called once a save has been written. `<esc>` reverts to
    /// [`Self::original`], so leaving that frozen at its open-time value
    /// would make a close undo a save the user was just told had
    /// succeeded. The baseline moves to what was written instead.
    ///
    /// A save means wanting these settings, so the pending values are what
    /// the session now holds and what the file now says; both are the same
    /// thing here, and this makes the popup agree with them.
    ///
    /// The set rows are recorded in the shape a filter takes rather than as
    /// bare captures, so the next `<esc>` puts back the file's own contents
    /// — patterns included — instead of a set re-derived from them.
    pub fn commit_as_original(&mut self) {
        // A captured set commits as an allow filter over exactly those names
        // — including no names, which is a set frozen to nothing. A row with
        // no capture commits as no filter at all: that is a thaw, and the
        // bot inherits from here.
        let committed = |names: Option<&BTreeSet<String>>| {
            names.map(|names| NameFilter {
                mode: FilterMode::Allow,
                names: names.clone(),
            })
        };
        // An untouched row keeps the filter the bot already had,
        // which is what the commit wrote too -- recording the row's Live
        // reading here instead would make `<esc>` after a save "undo" a
        // freeze the save actually persisted.
        let settled = |touched: bool,
                       captured: Option<&BTreeSet<String>>,
                       was: Option<&NameFilter>| match (touched, was) {
            (false, Some(was)) => Some(was.clone()),
            _ => committed(captured),
        };
        self.original = Some(OriginalValues {
            trigger: self.pending_trigger,
            behavior: self.pending_behavior,
            prep_mode: self.pending_prep_mode,
            tool_set: settled(
                self.tool_set_touched,
                self.frozen_tools.as_ref(),
                self.original.as_ref().and_then(|o| o.tool_set.as_ref()),
            ),
            skill_set: settled(
                self.skill_set_touched,
                self.frozen_skills.as_ref(),
                self.original.as_ref().and_then(|o| o.skill_set.as_ref()),
            ),
            model_setting: self.pending_model_setting,
            template: self.seed_template.input.clone(),
        });
        self.editor_original = None;
    }

    /// Replaces the status line's message.
    pub fn report(&mut self, status: PopupStatus) {
        self.status = Some(status);
    }

    /// Clears the status line, and disarms an armed overwrite.
    ///
    /// Called on every keystroke that is not itself a status-producing one:
    /// the line describes the most recent key, so a message about an armed
    /// overwrite must not still be claiming the next `<c-s>` is waiting
    /// after the user has moved on.
    ///
    /// The arm goes with the message. Hiding the prompt while leaving the
    /// save armed is the one sequence that silently destroys an entry: the
    /// user presses some other key, the "Overwrite …?" line disappears, and
    /// the `<c-s>` they press next — the one they were told to press to
    /// confirm — overwrites with no confirmation ever having been on
    /// screen. The prompt and the arm are one piece of state: the save
    /// that follows an arming is the save the user was shown, so
    /// everything that hides the prompt also withdraws the offer.
    pub fn clear_status(&mut self) {
        self.status = None;
        self.save_armed = false;
    }
}
