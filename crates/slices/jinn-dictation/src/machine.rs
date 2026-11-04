//! The dictation lifecycle state machine — pure, platform-blind logic.
//!
//! [`DictationMachine`] is the whole lifecycle in one testable type. It
//! holds state only — no backend, no I/O — and produces a [`Step`] per
//! event: the new state plus what the owner should do next. The owner
//! (the actor in production, a test in the suite) performs the backend
//! calls where it chooses and reports the results back; the split is
//! what keeps every transition testable without an audio device or a
//! network, on every platform.
//!
//! Phase graph: Offline → Fetching → Loading → Ready → Arming →
//! Recording ⇄ Ready, plus Error — terminal for the process run (no
//! retry, no auto-recovery), reachable from init (unknown model,
//! backend fault) and from the fetch/load completions. Arming is the
//! press-asked-but-no-mic-yet phase: Recording is written only once
//! the capture stream actually plays.

use jinn_dictation_backend::FetchProgress;
use jinn_dictation_backend::Utterance;
use jinn_dictation_msg::DictationPhase;
use jinn_dictation_msg::DictationState;

/// One step's outcome: the new state plus what the owner should do.
#[derive(Debug)]
pub struct Step {
    /// The state after this step; the owner writes it to the cell.
    pub state: DictationState,
    /// What the owner should do next.
    pub action: StepAction,
    /// The utterance a disarm's force-finalize closed, when one was
    /// open. The owner appends its text to the focused chat input.
    pub last_flushed: Option<Utterance>,
}

/// The owner's next action after a machine step.
#[derive(Debug, PartialEq, Eq)]
pub enum StepAction {
    /// Nothing to do beyond writing the state.
    Idle,
    /// Run the backend fetch (report progress back through
    /// [`DictationMachine::fetch_progress`]).
    Fetch,
    /// Run the backend load (report the result through
    /// [`DictationMachine::load_finished`]).
    Load,
    /// Open the microphone capture.
    Arm,
    /// Close the microphone capture (the flush already ran).
    Disarm,
}

/// The dictation lifecycle state machine.
pub struct DictationMachine {
    state: DictationState,
    /// Whether the capture is believed open — the owner's mic truth, so
    /// the Recording ⇄ Ready edge is honest about what was done.
    capture_open: bool,
    /// The `[dictation] enabled` config value, read at activation.
    enabled: bool,
}

impl DictationMachine {
    /// Builds a machine starting Offline.
    ///
    /// `enabled` gates everything: a disabled machine stays Offline
    /// forever — init refuses to run, and the state says so.
    #[must_use]
    pub fn new(model_name: &str, enabled: bool) -> Self {
        Self {
            state: DictationState {
                phase: DictationPhase::Offline,
                model_name: model_name.to_owned(),
                fetch_progress_percent: None,
                error: None,
            },
            capture_open: false,
            enabled,
        }
    }

    /// The current state (a clone; callers write the cell from it).
    #[must_use]
    pub fn state(&self) -> &DictationState {
        &self.state
    }

    /// The init step: validate the model and pick the entry path.
    ///
    /// A disabled machine stays Offline with no work. A model name the
    /// `supported` set does not carry fails here, before any download
    /// starts — a config typo is an Error-state fact naming the value
    /// and the supported set, not a silent fallback. `cached` is the
    /// backend's cache check: cached models enter at Loading, uncached
    /// at Fetching.
    pub fn init(&mut self, supported: &[String], cached: bool) -> Step {
        if !self.enabled {
            return self.step(StepAction::Idle);
        }

        let name = self.state.model_name.clone();
        if !supported.contains(&name) {
            let known = supported.join(", ");
            return self.fail(
                "unknown dictation model",
                &format!("{name:?} (supported: {known})"),
            );
        }

        let action = if cached {
            self.enter_loading()
        } else {
            self.enter_fetching()
        };
        self.step(action)
    }

    /// Fetch progress arrived: update the percent in place.
    pub fn fetch_progress(&mut self, progress: FetchProgress) {
        if self.state.phase == DictationPhase::Fetching {
            self.state.fetch_progress_percent = progress.percent();
        }
    }

    /// The fetch finished: enter Loading (or Error with the fault).
    pub fn fetch_finished(&mut self, fault: Option<String>) -> Step {
        if let Some(detail) = fault {
            return self.fail("fetching", &detail);
        }
        self.enter_loading();
        self.step(StepAction::Load)
    }

    /// The load finished: enter Ready (or Error with the fault).
    pub fn load_finished(&mut self, fault: Option<String>) -> Step {
        if let Some(detail) = fault {
            return self.fail("loading", &detail);
        }
        self.state.phase = DictationPhase::Ready;
        self.state.fetch_progress_percent = None;
        self.step(StepAction::Idle)
    }

    /// A toggle press while Ready: arm the microphone.
    ///
    /// The machine moves to Arming — the honest in-between: the press
    /// asked for the microphone, but nothing is open yet. The owner's
    /// device open decides the outcome: a playing stream reports
    /// [`DictationMachine::arm_opened`] (Recording), a refusal reports
    /// [`DictationMachine::arm_failed`] (back to Ready with the
    /// refusal as the dashboard note). Recording is never written
    /// before the device exists.
    pub fn arm(&mut self) -> Step {
        debug_assert_eq!(
            self.state.phase,
            DictationPhase::Ready,
            "arm only applies from Ready"
        );
        self.state.phase = DictationPhase::Arming;
        self.state.error = None;
        self.step(StepAction::Arm)
    }

    /// The capture stream plays: Recording, for real.
    ///
    /// The owner calls this only after the device open succeeded and
    /// the stream started — the phase now names a live microphone.
    pub fn arm_opened(&mut self) -> Step {
        debug_assert_eq!(
            self.state.phase,
            DictationPhase::Arming,
            "arm_opened only applies from Arming"
        );
        self.state.phase = DictationPhase::Recording;
        self.state.error = None;
        self.capture_open = true;
        self.step(StepAction::Idle)
    }

    /// The device open refused (from Arming), or the capture closed
    /// without a toggle (from Recording): back to Ready, refusal noted.
    ///
    /// Arming never half-happens: the state returns to Ready so the next
    /// press tries again, and the refusal lands on the dashboard row.
    /// This is deliberately not the terminal Error phase — a missing or
    /// misnamed device is fixable without a restart.
    pub fn arm_failed(&mut self, reason: &str) -> Step {
        self.capture_open = false;
        self.state.phase = DictationPhase::Ready;
        self.state.error = Some(reason.to_owned());
        self.step(StepAction::Idle)
    }

    /// A toggle press while Recording: disarm with a force-finalize.
    ///
    /// The owner runs the backend flush first and reports the result
    /// here, so the returned utterance travels with the state the owner
    /// writes; the owner appends whatever [`Step::last_flushed`] carries
    /// before the user can type again. A flush fault is terminal: the
    /// backend's transcript state is unknown, and guessing would insert
    /// wrong text.
    pub fn disarm(&mut self, flushed: Option<Utterance>, fault: Option<String>) -> Step {
        debug_assert_eq!(
            self.state.phase,
            DictationPhase::Recording,
            "disarm only applies while Recording"
        );
        if let Some(detail) = fault {
            return self.fail("disarm", &detail);
        }
        self.capture_open = false;
        self.state.phase = DictationPhase::Ready;
        let mut step = self.step(StepAction::Disarm);
        step.last_flushed = flushed;
        step
    }

    /// The capture closed without a toggle (device vanished, stream
    /// error): back to Ready, refusal note.
    pub fn capture_lost(&mut self, reason: &str) -> Step {
        if self.state.phase != DictationPhase::Recording {
            return self.step(StepAction::Idle);
        }
        self.arm_failed(reason)
    }

    /// A terminal fault: Error phase with a description naming the
    /// stage. Error is terminal for the process run — no retry, no
    /// auto-recovery. The description shows on the dashboard row and on
    /// alt-i.
    pub fn fail(&mut self, stage: &str, detail: &str) -> Step {
        self.state.phase = DictationPhase::Error;
        self.state.fetch_progress_percent = None;
        self.state.error = Some(format!("{stage}: {detail}"));
        self.step(StepAction::Idle)
    }

    fn enter_fetching(&mut self) -> StepAction {
        self.state.phase = DictationPhase::Fetching;
        self.state.fetch_progress_percent = None;
        StepAction::Fetch
    }

    fn enter_loading(&mut self) -> StepAction {
        self.state.phase = DictationPhase::Loading;
        StepAction::Load
    }

    fn step(&self, action: StepAction) -> Step {
        Step {
            state: self.state.clone(),
            action,
            last_flushed: None,
        }
    }
}
