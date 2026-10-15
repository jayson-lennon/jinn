//! The work-time monitor actor — the only writer of working-time intervals.
//!
//! Folds [`WorkStateChanged`] into the wall-clock intervals held in the
//! work-time slice's cell. It deliberately never reads a session's phase: the
//! phase writers announce the boundary, and a monitor that derived the fact
//! itself would need the same writes the announcers already make, giving two
//! places that disagree about when a turn started.
//!
//! The forced `Idle → Idle` publish for a cancel-consumed-by-frontend race
//! reaches this actor as `working: false` with no interval open, and
//! `end_working` reports no close — so it is a no-op rather than a boundary.

use jinn_session_msg::WorkStateChanged;
use jinn_slices::cell::TypedCell;
use jinn_work_time_msg::{RestoreWorkingTime, WorkingTimeState};
use trouper::actor::ActorPath;
use trouper::actor::{MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

/// The monitor actor's static trouper path.
pub const WORK_TIME_MONITOR_PATH: &str = "work-time-monitor";

/// The working-time monitor.
///
/// Holds the one write handle to the interval cell. Every mutation of a
/// working interval in the application happens here.
pub struct WorkTimeMonitorActor {
    state: TypedCell<WorkingTimeState>,
}

impl ServiceActor for WorkTimeMonitorActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "trait contract: start is never called (spawn uses start_with)"
    )]
    async fn start(
        _args: &trouper::json::Json,
    ) -> Result<Self, error_stack::Report<RegistryError>> {
        // Never called: the spawn helper injects the cell via `start_with`.
        Err(
            error_stack::IntoReport::into_report(RegistryError::InvalidSpec)
                .attach("WorkTimeMonitorActor is spawned via start_with"),
        )
    }
}

impl WorkTimeMonitorActor {
    /// Spawns the actor at its static path. The caller subscribes the
    /// returned path to the work-time topic (composition's
    /// `SliceHost::subscribe_service`) — subscribe is the readiness point, so
    /// it must follow this call before any publish.
    ///
    pub fn spawn(system: &ActorSystem, state: TypedCell<WorkingTimeState>) -> ActorPath {
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(ActorPath::new(WORK_TIME_MONITOR_PATH))
            .start_with(move || Box::pin(async move { Ok(Self { state }) }))
            .handles::<WorkStateChanged>()
            .handles::<RestoreWorkingTime>()
            .start()
    }

    /// Folds one work-state change into the session's intervals.
    pub fn handle_work_state_changed(&self, event: &WorkStateChanged) {
        self.state.update(|working| {
            if event.working {
                working.begin_working(event.session_id.clone(), event.at);
            } else {
                working.end_working(&event.session_id, event.at);
            }
        });
    }

    /// Puts a loaded session's recorded intervals back into the cell.
    ///
    /// An interval the previous process left open is closed at the snapshot's
    /// own last-update time — the last moment we know it was alive. Closing at
    /// load instead would bill the whole period the app spent closed, and the
    /// work that was billing is long gone.
    pub fn handle_restore(&self, event: &RestoreWorkingTime) {
        self.state.update(|working| {
            working.restore(event.session_id.clone(), event.intervals.clone());
            working.end_working(&event.session_id, event.last_active_at);
        });
    }
}

impl MsgHandler<WorkStateChanged> for WorkTimeMonitorActor {
    async fn handle(&mut self, msg: &WorkStateChanged, _ctx: &mut MsgCtx<'_>) {
        self.handle_work_state_changed(msg);
    }
}

impl MsgHandler<RestoreWorkingTime> for WorkTimeMonitorActor {
    async fn handle(&mut self, msg: &RestoreWorkingTime, _ctx: &mut MsgCtx<'_>) {
        self.handle_restore(msg);
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        reason = "test code"
    )]
    use super::*;
    use jiff::{SignedDuration, Timestamp};
    use jinn_core_types::{SessionId, WorkingInterval};

    use jinn_work_time_msg::work_time_slot;

    const START: i64 = 1_000_000_000;

    fn at(offset_secs: i64) -> Timestamp {
        Timestamp::from_second(START + offset_secs).expect("valid offset")
    }

    fn working(id: &SessionId, at: Timestamp) -> WorkStateChanged {
        WorkStateChanged {
            session_id: id.clone(),
            working: true,
            at,
        }
    }

    fn idle(id: &SessionId, at: Timestamp) -> WorkStateChanged {
        WorkStateChanged {
            session_id: id.clone(),
            working: false,
            at,
        }
    }

    /// A monitor over a cell registered the way production registers it.
    fn monitor() -> (WorkTimeMonitorActor, TypedCell<WorkingTimeState>, SessionId) {
        let slices = jinn_slices::Slices::new();
        let cell = slices
            .register(work_time_slot(), WorkingTimeState::default())
            .expect("a fresh registry never has the work-time slot taken");
        let actor = WorkTimeMonitorActor {
            state: cell.clone(),
        };
        (actor, cell, SessionId::new())
    }

    #[rstest::rstest]
    fn starting_work_opens_an_interval() {
        // Given a monitor over a fresh cell.
        let (actor, cell, id) = monitor();

        // When the session announces it started working.
        actor.handle_work_state_changed(&working(&id, at(0)));

        // Then the session is recorded as working.
        assert!(cell.read().is_working(&id));
    }

    #[rstest::rstest]
    fn a_full_turn_records_its_span() {
        // Given a monitor over a fresh cell.
        let (actor, cell, id) = monitor();

        // When a ten-second turn starts and then stops.
        actor.handle_work_state_changed(&working(&id, at(0)));
        actor.handle_work_state_changed(&idle(&id, at(10)));

        // Then the session worked for ten seconds and is now idle.
        assert!(!cell.read().is_working(&id));
        assert_eq!(
            cell.read().working_time(&id, at(100)),
            SignedDuration::from_secs(10)
        );
    }

    #[rstest::rstest]
    fn a_stop_without_a_start_records_nothing() {
        // Given a cell that never saw this session work.
        let (actor, cell, id) = monitor();

        // When a stop arrives — the shape a forced Idle -> Idle publish takes.
        actor.handle_work_state_changed(&idle(&id, at(10)));

        // Then no interval was invented for it.
        assert_eq!(cell.read().intervals(&id), []);
    }

    #[rstest::rstest]
    fn a_start_while_already_working_is_not_billed_twice() {
        // Given a session already working.
        let (actor, cell, id) = monitor();
        actor.handle_work_state_changed(&working(&id, at(0)));

        // When a second start arrives.
        actor.handle_work_state_changed(&working(&id, at(5)));

        // Then exactly one interval exists, so the overlap is not double-billed.
        assert_eq!(cell.read().intervals(&id).len(), 1);
    }

    fn restore(
        id: &SessionId,
        intervals: Vec<WorkingInterval>,
        last_active_at: Timestamp,
    ) -> RestoreWorkingTime {
        RestoreWorkingTime {
            session_id: id.clone(),
            intervals,
            last_active_at,
        }
    }

    #[rstest::rstest]
    fn loading_a_session_closes_the_interval_it_left_open() {
        // Given a monitor whose cell is empty, and a session killed mid-turn.
        let (actor, cell, id) = monitor();

        // When the session is loaded, carrying the open interval.
        actor.handle_restore(&restore(&id, vec![WorkingInterval::open(at(0))], at(10)));

        // Then the interval is closed, so the session is not working on load.
        assert!(!cell.read().is_working(&id));
    }

    #[rstest::rstest]
    fn loading_bills_only_up_to_the_persisted_stamp() {
        // Given a session killed mid-turn at 10, and loaded much later.
        let (actor, cell, id) = monitor();
        let reloaded_at = at(50_000);

        // When it is loaded with the open interval and its last-update stamp.
        actor.handle_restore(&restore(&id, vec![WorkingInterval::open(at(0))], at(10)));

        // Then the total is the 10 seconds it actually worked, not the 50,000
        // seconds the app spent closed.
        assert_eq!(
            cell.read().working_time(&id, reloaded_at),
            SignedDuration::from_secs(10)
        );
    }

    #[rstest::rstest]
    fn loading_restores_recorded_working_time() {
        // Given a session that recorded ten seconds of closed work.
        let (actor, cell, id) = monitor();

        // When it is loaded.
        actor.handle_restore(&restore(
            &id,
            vec![WorkingInterval::closed(at(0), at(10))],
            at(10),
        ));

        // Then its recorded time comes back intact.
        assert_eq!(
            cell.read().working_time(&id, at(1000)),
            SignedDuration::from_secs(10)
        );
    }

    #[rstest::rstest]
    fn loading_leaves_a_fully_closed_session_untouched() {
        // Given a session whose intervals are all closed.
        let (actor, cell, id) = monitor();

        // When it is loaded with a stamp later than its last interval.
        actor.handle_restore(&restore(
            &id,
            vec![WorkingInterval::closed(at(0), at(10))],
            at(20),
        ));

        // Then the total is unchanged: there was nothing open to close, and the
        // later stamp must not extend a boundary that already happened.
        assert_eq!(
            cell.read().working_time(&id, at(1000)),
            SignedDuration::from_secs(10)
        );
    }

    #[rstest::rstest]
    fn loading_a_session_that_never_worked_records_nothing() {
        // Given a snapshot written before working time was recorded.
        let (actor, cell, id) = monitor();

        // When it is loaded with no intervals.
        actor.handle_restore(&restore(&id, Vec::new(), at(20)));

        // Then it starts at zero rather than at the load moment.
        assert_eq!(
            cell.read().working_time(&id, at(1000)),
            SignedDuration::ZERO
        );
    }

    #[rstest::rstest]
    fn a_restored_session_can_work_again_from_its_own_boundary() {
        // Given a session loaded with a closed interval ending at 10.
        let (actor, cell, id) = monitor();
        actor.handle_restore(&restore(
            &id,
            vec![WorkingInterval::closed(at(0), at(10))],
            at(10),
        ));

        // When it starts working again at 20.
        actor.handle_work_state_changed(&working(&id, at(20)));

        // Then the new stretch is separate, and both are counted.
        assert_eq!(cell.read().intervals(&id).len(), 2);
        assert!(cell.read().is_working(&id));
    }
}
