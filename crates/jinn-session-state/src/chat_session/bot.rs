//! Bot-session API surface: behavior, trigger, and model settings, plus report access.

use super::*;

impl ChatSessionState {
    /// Whether this session is a bot of another session.
    #[must_use]
    pub fn is_bot(&self) -> bool {
        self.core.identity.origin == SessionOrigin::Bot
    }

    /// What a run in this session sees of the conversation.
    #[must_use]
    pub fn bot_behavior(&self) -> BotBehavior {
        self.core.bot.behavior
    }

    /// Set what a run in this session sees of the conversation.
    pub fn set_bot_behavior(&mut self, behavior: BotBehavior) {
        self.core.bot.behavior = behavior;
    }

    /// Whether this bot is still being composed.
    ///
    /// Composition is the only configuration that stops a dispatch, and it
    /// says so on its own: a manual trigger stops a *fire*, not the user
    /// pressing `R`, so conflating the two would mark a perfectly runnable
    /// bot as stopped. Everything else runs on its own terms.
    ///
    /// Only meaningful for a bot — an ordinary session carries the
    /// same default fields and would answer `true`.
    #[must_use]
    pub fn bot_is_prepping(&self) -> bool {
        self.core.bot.prep_mode
    }

    /// Set whether this bot is still being composed.
    pub fn set_bot_is_prepping(&mut self, prep_mode: bool) {
        self.core.bot.prep_mode = prep_mode;
    }

    /// Whether this bot runs on its parent's completion.
    ///
    /// The sidebar marks this with its own glyph. It is a fact about the
    /// configuration rather than a derived state, so it is asked of the
    /// session rather than re-derived by each renderer — a marker whose
    /// meaning is a few fields away is a marker the two surfaces can
    /// disagree about.
    #[must_use]
    pub fn bot_fires_on_parent_completion(&self) -> bool {
        self.bot_trigger() == BotTrigger::ParentCompleted
    }

    /// The condition that causes an automatic re-run.
    #[must_use]
    pub fn bot_trigger(&self) -> BotTrigger {
        self.core.bot.trigger
    }

    /// Set the condition that causes an automatic re-run.
    pub fn set_bot_trigger(&mut self, trigger: BotTrigger) {
        self.core.bot.trigger = trigger;
    }

    /// The user-editable seed text injected ahead of the prior report.
    #[must_use]
    pub fn seed_template(&self) -> &str {
        &self.core.bot.seed_template
    }

    /// Set the user-editable seed text injected ahead of the prior report.
    pub fn set_seed_template(&mut self, template: String) {
        self.core.bot.seed_template = template;
    }

    /// Whether this bot's model is its own or the one it inherited.
    ///
    /// A fact about the bot's configuration rather than a reading of
    /// its model: the model is always concrete, and this says whether the
    /// bot claimed it. Never infer one from the other.
    #[must_use]
    pub fn bot_model_setting(&self) -> BotModelSetting {
        self.core.bot.model_setting
    }

    /// Set whether this bot's model is its own or inherited.
    pub fn set_bot_model_setting(&mut self, setting: BotModelSetting) {
        self.core.bot.model_setting = setting;
    }

    /// The bot's append-only report log, oldest first.
    #[must_use]
    pub fn bot_reports(&self) -> &[BotReport] {
        &self.core.bot.reports
    }

    /// Append one report to this bot's log.
    ///
    /// Append-only by design: the harness never removes or edits a report,
    /// and the next run is seeded from the most recent entry.
    pub fn append_bot_report(&mut self, body: String) -> BotReport {
        let reports = &mut self.core.bot.reports;
        let report = BotReport {
            run: reports.len() + 1,
            published_at: Timestamp::now(),
            body,
        };
        reports.push(report.clone());
        report
    }

    /// The most recent report, if this bot has ever reported.
    #[must_use]
    pub fn latest_bot_report(&self) -> Option<&BotReport> {
        self.core.bot.reports.last()
    }

    /// The workflow-local TOML file this bot was loaded from, when
    /// it belongs to a workflow. `None` for personal bots.
    #[must_use]
    pub fn workflow_source(&self) -> Option<&jinn_core_types::BotSourcePath> {
        self.core.bot.workflow_source.as_ref()
    }

    /// Stamp this bot as loaded from the workflow-local file `source`.
    pub fn set_workflow_source(&mut self, source: jinn_core_types::BotSourcePath) {
        self.core.bot.workflow_source = Some(source);
    }
}
