//! The quake bar's canvas actor — the log writer on trouper.
//!
//! The kameo counterpart of this actor was the first port to the
//! the `trouper` runtime ([`ServiceActor`] tier: stateless
//! side-effectful fold, no journaling). It subscribes to the
//! `jinn.quake-bar` trouper topic — fed by the kameo→trouper bridge
//! (`jinn.quake-bar`) — and appends each
//! [`SubmitQuakeBarCommand`] to the slice cell's log, exactly as the
//! kameo actor did. The cell handle cannot ride the runtime's JSON
//! start args, so it is injected through the builder's
//! [`start_with`](trouper::builder::ServiceBuilder::start_with)
//! override.

use trouper::actor::ActorPath;
use trouper::actor::{MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

use jinn_slices::TypedCell;

use crate::command::SubmitQuakeBarCommand;
use crate::state::QuakeBarState;

/// The quake bar actor on the canvas runtime.
///
/// The single consumer of [`SubmitQuakeBarCommand`]; the only writer
/// of the log field of the slice cell.
pub struct QuakeBarCanvasActor {
    /// The slice cell (the one write handle, shared by clone).
    cell: TypedCell<QuakeBarState>,
}

impl ServiceActor for QuakeBarCanvasActor {
    // Never invoked: the spawn helper injects the cell via `start_with`,
    // and the trait-required signature has nothing to await.
    async fn start(_args: &serde_json::Value) -> Result<Self, error_stack::Report<RegistryError>> {
        async {}.await;
        Err(
            error_stack::IntoReport::into_report(RegistryError::InvalidSpec).attach(
                "QuakeBarCanvasActor is spawned via start_with; start requires the typed cell",
            ),
        )
    }
}

impl MsgHandler<SubmitQuakeBarCommand> for QuakeBarCanvasActor {
    async fn handle(&mut self, msg: SubmitQuakeBarCommand, _ctx: &mut MsgCtx<'_>) {
        self.apply_submit(msg);
    }
}

impl QuakeBarCanvasActor {
    /// Spawns the actor at `quake-bar` and subscribes it to the
    /// quake-bar topic.
    ///
    /// A successful [`ActorSystem::subscribe`] is the ordering guarantee:
    /// the topic cursor is registered, so every later publish reaches
    /// the actor's inbox.
    /// # Panics
    ///
    /// Panics if the topic subscription fails — a broken actor system;
    /// the activation ordering relies on the cursor being registered.
    pub fn spawn(system: &ActorSystem, cell: &TypedCell<QuakeBarState>) -> ActorPath {
        let path = trouper::builder::spawn_service_builder::<Self>(system)
            .at(ActorPath::new("quake-bar"))
            .start_with({
                let cell = cell.clone();
                move || Box::pin(async move { Ok(Self { cell }) })
            })
            .handles::<SubmitQuakeBarCommand>()
            .start();
        #[expect(
            clippy::expect_used,
            reason = "subscription failure is a broken actor system, not a caller bug"
        )]
        system
            .subscribe(&path, &crate::command::quake_bar_topic(), None)
            .expect("quake-bar actor subscribes to its topic");
        path
    }

    /// Appends the submitted text to the command log.
    fn apply_submit(&self, msg: SubmitQuakeBarCommand) {
        self.cell.update(|s| s.log.push(msg.text));
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]

    use crate::command::SubmitQuakeBarCommand;
    use crate::state::QuakeBarState;
    use crate::state::quake_bar_slot;
    use jinn_slices::Slices;

    use super::QuakeBarCanvasActor;

    /// Polls `check` until it passes or the bounded retry budget runs out.
    async fn wait_for(check: impl Fn() -> bool) {
        for _ in 0..200 {
            if check() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("condition never held within the retry budget");
    }

    #[rstest::rstest]
    #[test]
    fn submit_command_appends_text_to_log() {
        // Given a quake bar actor over its slice cell.
        let slices = Slices::new();
        let cell = slices
            .register(quake_bar_slot(), QuakeBarState::default())
            .expect("fresh registry");
        let actor = QuakeBarCanvasActor { cell: cell.clone() };

        // When applying a SubmitQuakeBarCommand.
        actor.apply_submit(SubmitQuakeBarCommand {
            text: "hello".to_owned(),
        });

        // Then the text appears in the command log.
        let guard = cell.read();
        assert_eq!(guard.log.len(), 1);
        assert_eq!(guard.log.visible_lines(5), &["hello".to_owned()]);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn topic_published_submit_command_reaches_cell_log_through_canvas() {
        // Given a fabric with the quake-bar canvas actor subscribed.
        let fabric = jinn_testutil::TestFabric::new();
        let slices = Slices::new();
        let cell = slices
            .register(quake_bar_slot(), QuakeBarState::default())
            .expect("fresh registry");
        QuakeBarCanvasActor::spawn(fabric.system(), &cell);

        // When a SubmitQuakeBarCommand envelope lands on the topic.
        fabric
            .send_to_topic(
                &SubmitQuakeBarCommand {
                    text: "hello".to_owned(),
                },
                &crate::command::quake_bar_topic(),
            )
            .await;

        // Then the command reaches the cell log through the canvas.
        wait_for(|| cell.read().log.len() == 1).await;
        let guard = cell.read();
        assert_eq!(guard.log.visible_lines(5), &["hello".to_owned()]);
    }
}
