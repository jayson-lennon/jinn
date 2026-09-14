//! The skill picker's spec — behavior authored once in the builder.
//!
//! Hooks fill in as the skill picker migrates off the legacy per-kind
//! handler arms; the id and widget kind here are the registry's source of
//! truth from the moment of registration.

use jinn_picker::PickerId;
use jinn_picker::PickerSpec;
use jinn_picker::PickerWidget;
use jinn_picker::PreviewSpec;

/// The domain entry wrapped by this picker's items.
#[derive(Debug)]
pub struct SkillEntry {
    /// Display name of the skill.
    pub name: String,
}

/// Builds the skill picker's spec.
#[must_use]
pub fn skill_spec() -> PickerSpec<SkillEntry> {
    PickerSpec::new(PickerId::new(crate::feat::picker::registry::SKILL_ID))
        .title(" Skills ")
        .widget(PickerWidget::Preview(PreviewSpec {
            reset_scroll_on_selection_change: true,
        }))
}

#[cfg(test)]
impl SkillEntry {
    /// A second spec under the same skill id exercising bind dispatch in
    /// integration tests: `<tab>` pushes a transient entry; `<esc>` closes.
    #[must_use]
    pub fn spec_for_tests() -> PickerSpec<Self> {
        use jinn_picker::ActionCtx;
        use jinn_picker::PickerOutcome;

        PickerSpec::new(PickerId::new(crate::feat::picker::registry::SKILL_ID))
            .title(" Skills (test) ")
            .bind("<tab>", "test", |ctx: &mut ActionCtx<'_>| {
                let host = ctx.host();
                let state = host
                    .state_any()
                    .downcast_mut::<crate::common::app_state::AppState>()
                    .expect("domain host lends AppState");
                state
                    .active_session_mut()
                    .push_entry(crate::protocol::ChatEntry::transient("test bind ran"));
                PickerOutcome::empty()
            })
            .bind("<esc>", "close", |_ctx: &mut ActionCtx<'_>| {
                PickerOutcome::empty().close()
            })
    }
}
