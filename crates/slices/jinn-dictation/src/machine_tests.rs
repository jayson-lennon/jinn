//! The state-machine suite — platform-blind, stub-driven.
//!
#![allow(clippy::expect_used, clippy::assert_is_empty, reason = "test code")]

//! Every matrix transition runs against [`StubBackend`], which
//! simulates the lifecycle without any native dependency: fetchable or
//! pre-cached, loading or failing, and one utterance per feed. No test
//! downloads a model or opens an audio device, and the whole suite
//! compiles and passes on every platform.

use jinn_dictation_backend::BackendError;
use jinn_dictation_backend::DictationBackend;
use jinn_dictation_backend::FetchProgress;
use jinn_dictation_backend::Utterance;
use jinn_dictation_msg::DictationPhase;
use jinn_dictation_msg::DictationState;

use crate::machine::DictationMachine;
use crate::machine::StepAction;

use error_stack::Report;

/// A backend the tests script: cached or not, loadable or not, and one
/// scripted utterance out of `flush`. Crate-visible so the actor
/// suite can script the same lifecycle. `resets` counts `reset` calls
/// — clone the Arc before the backend is boxed and moved, and the
/// counter survives behind the trait object.
#[derive(Default)]
pub(crate) struct StubBackend {
    pub cached: bool,
    pub load_fails: bool,
    pub fetch_fails: bool,
    pub flushed: Option<Utterance>,
    pub resets: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl DictationBackend for StubBackend {
    fn model_name(&self) -> &'static str {
        "stub-model"
    }

    fn is_cached(&self) -> bool {
        self.cached
    }

    fn is_loaded(&self) -> bool {
        false
    }

    fn fetch(
        &mut self,
        on_progress: &mut dyn FnMut(FetchProgress),
    ) -> Result<(), Report<BackendError>> {
        if self.fetch_fails {
            return Err(Report::new(BackendError).attach("stub fetch failure"));
        }
        on_progress(FetchProgress::new(500, Some(1000)));
        Ok(())
    }

    fn load(&mut self) -> Result<(), Report<BackendError>> {
        if self.load_fails {
            return Err(Report::new(BackendError).attach("stub load failure"));
        }
        Ok(())
    }

    fn feed(
        &mut self,
        _audio: jinn_dictation_backend::DecodableAudio,
    ) -> Result<Vec<Utterance>, Report<BackendError>> {
        Ok(Vec::new())
    }

    fn flush(&mut self) -> Result<Option<Utterance>, Report<BackendError>> {
        Ok(self.flushed.take())
    }

    fn reset(&mut self) {
        self.resets
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

const SUPPORTED: &[&str] = &["stub-model"];

fn supported_list() -> Vec<String> {
    SUPPORTED.iter().map(|model| (*model).to_owned()).collect()
}

#[rstest::rstest]
#[test]
fn init_stays_offline_when_disabled() {
    // Given a machine built with dictation disabled.
    let mut machine = DictationMachine::new("stub-model", false);

    // When init runs.
    let step = machine.init(&supported_list(), true);

    // Then the phase stays Offline with nothing to do.
    assert_eq!(step.state.phase, DictationPhase::Offline);
    assert_eq!(step.action, StepAction::Idle);
}

#[rstest::rstest]
#[test]
fn init_of_an_uncached_model_enters_fetching() {
    // Given an enabled machine whose backend reports a cold cache.
    let mut machine = DictationMachine::new("stub-model", true);

    // When init runs with `cached = false`.
    let step = machine.init(&supported_list(), false);

    // Then the machine enters Fetching and asks for a fetch.
    assert_eq!(step.state.phase, DictationPhase::Fetching);
    assert_eq!(step.action, StepAction::Fetch);
}

#[rstest::rstest]
#[test]
fn init_of_a_cached_model_enters_loading() {
    // Given an enabled machine whose backend reports a warm cache.
    let mut machine = DictationMachine::new("stub-model", true);

    // When init runs with `cached = true`.
    let step = machine.init(&supported_list(), true);

    // Then the machine enters Loading and asks for a load: fetching is
    // skipped.
    assert_eq!(step.state.phase, DictationPhase::Loading);
    assert_eq!(step.action, StepAction::Load);
}

#[rstest::rstest]
#[test]
fn init_names_an_unknown_model_and_the_supported_set_in_error() {
    // Given an enabled machine configured for a model nothing supports.
    let mut machine = DictationMachine::new("whisper-tiny", true);

    // When init runs against the stub's supported set.
    let step = machine.init(&supported_list(), true);

    // Then the machine is in Error, naming the value and the set.
    assert_eq!(step.state.phase, DictationPhase::Error);
    assert_eq!(
        step.state.error.as_deref(),
        Some("unknown dictation model: \"whisper-tiny\" (supported: stub-model)")
    );
    assert_eq!(step.action, StepAction::Idle);
}

#[rstest::rstest]
#[test]
fn fetch_progress_updates_the_percent_in_place() {
    // Given a machine mid-fetch.
    let mut machine = DictationMachine::new("stub-model", true);
    let _ = machine.init(&supported_list(), false);

    // When fetch progress at 50% arrives.
    machine.fetch_progress(FetchProgress::new(500, Some(1000)));

    // Then the state's percent reads 50.
    assert_eq!(machine.state().fetch_progress_percent, Some(50));
}

#[rstest::rstest]
#[test]
fn fetch_progress_outside_fetching_is_ignored() {
    // Given a machine still Offline.
    let mut machine = DictationMachine::new("stub-model", true);

    // When a stray progress report arrives.
    machine.fetch_progress(FetchProgress::new(500, Some(1000)));

    // Then no percent was recorded: the report belonged to nothing.
    assert_eq!(machine.state().fetch_progress_percent, None);
}

#[rstest::rstest]
#[test]
fn a_finished_fetch_enters_loading() {
    // Given a machine mid-fetch.
    let mut machine = DictationMachine::new("stub-model", true);
    let _ = machine.init(&supported_list(), false);

    // When the fetch completes cleanly.
    let step = machine.fetch_finished(None);

    // Then the machine enters Loading and asks for the load.
    assert_eq!(step.state.phase, DictationPhase::Loading);
    assert_eq!(step.action, StepAction::Load);
}

#[rstest::rstest]
#[test]
fn a_failed_fetch_is_terminal_error() {
    // Given a machine mid-fetch.
    let mut machine = DictationMachine::new("stub-model", true);
    let _ = machine.init(&supported_list(), false);

    // When the fetch completes with a fault.
    let step = machine.fetch_finished(Some("disk full".to_owned()));

    // Then the machine is in Error naming the stage, and there is no
    // next action: Error is terminal for the process run.
    assert_eq!(step.state.phase, DictationPhase::Error);
    assert_eq!(step.state.error.as_deref(), Some("fetching: disk full"));
    assert_eq!(step.action, StepAction::Idle);
}

#[rstest::rstest]
#[test]
fn a_finished_load_enters_ready() {
    // Given a machine mid-load.
    let mut machine = DictationMachine::new("stub-model", true);
    let _ = machine.init(&supported_list(), true);

    // When the load completes cleanly.
    let step = machine.load_finished(None);

    // Then the machine is Ready with no next action.
    assert_eq!(step.state.phase, DictationPhase::Ready);
    assert_eq!(step.action, StepAction::Idle);
    assert_eq!(step.state.fetch_progress_percent, None);
}

#[rstest::rstest]
#[test]
fn a_failed_load_is_terminal_error() {
    // Given a machine mid-load.
    let mut machine = DictationMachine::new("stub-model", true);
    let _ = machine.init(&supported_list(), true);

    // When the load completes with a fault.
    let step = machine.load_finished(Some("corrupt model file".to_owned()));

    // Then the machine is in Error naming the stage.
    assert_eq!(step.state.phase, DictationPhase::Error);
    assert_eq!(
        step.state.error.as_deref(),
        Some("loading: corrupt model file")
    );
}

#[rstest::rstest]
#[test]
fn arm_moves_ready_to_arming() {
    // Given a Ready machine.
    let mut machine = DictationMachine::new("stub-model", true);
    let _ = machine.init(&supported_list(), true);
    let _ = machine.load_finished(None);

    // When the toggle arm lands.
    let step = machine.arm();

    // Then the machine is Arming — the device is not open yet, and the
    // machine asks for the capture.
    assert_eq!(step.state.phase, DictationPhase::Arming);
    assert_eq!(step.action, StepAction::Arm);
}

#[rstest::rstest]
#[test]
fn arm_opened_moves_arming_to_recording() {
    // Given an Arming machine (a press asked for the microphone).
    let mut machine = DictationMachine::new("stub-model", true);
    let _ = machine.init(&supported_list(), true);
    let _ = machine.load_finished(None);
    let _ = machine.arm();

    // When the capture stream plays.
    let step = machine.arm_opened();

    // Then the machine is Recording — for real, the stream is live.
    assert_eq!(step.state.phase, DictationPhase::Recording);
    assert_eq!(step.action, StepAction::Idle);
}

#[rstest::rstest]
#[test]
fn a_refused_arm_returns_to_ready_with_the_note() {
    // Given a machine that just armed.
    let mut machine = DictationMachine::new("stub-model", true);
    let _ = machine.init(&supported_list(), true);
    let _ = machine.load_finished(None);
    let _ = machine.arm();

    // When the device open refuses.
    let step = machine.arm_failed("no capture device available for usb-mic");

    // Then the machine is back in Ready carrying the refusal note —
    // Ready, not the terminal Error: a missing device is fixable
    // without a restart.
    assert_eq!(step.state.phase, DictationPhase::Ready);
    assert_eq!(
        step.state.error.as_deref(),
        Some("no capture device available for usb-mic")
    );
}

#[rstest::rstest]
#[test]
fn disarming_returns_to_ready_and_carries_the_flushed_utterance() {
    // Given a Recording machine.
    let mut machine = DictationMachine::new("stub-model", true);
    let _ = machine.init(&supported_list(), true);
    let _ = machine.load_finished(None);
    let _ = machine.arm();
    let _ = machine.arm_opened();

    // When the disarm lands with a flushed trailing utterance.
    let step = machine.disarm(Some(Utterance::new("trailing words".to_owned())), None);

    // Then the machine is back in Ready and the utterance travels with
    // the step for the commit path.
    assert_eq!(step.state.phase, DictationPhase::Ready);
    assert_eq!(step.action, StepAction::Disarm);
    assert_eq!(
        step.last_flushed.as_ref().map(|u| u.text.as_str()),
        Some("trailing words")
    );
}

#[rstest::rstest]
#[test]
fn disarming_without_an_open_utterance_commits_nothing() {
    // Given a Recording machine.
    let mut machine = DictationMachine::new("stub-model", true);
    let _ = machine.init(&supported_list(), true);
    let _ = machine.load_finished(None);
    let _ = machine.arm();
    let _ = machine.arm_opened();

    // When the disarm lands with nothing flushed (no open utterance).
    let step = machine.disarm(None, None);

    // Then the machine is Ready and the step carries no utterance.
    assert_eq!(step.state.phase, DictationPhase::Ready);
    assert_eq!(step.last_flushed, None);
}

#[rstest::rstest]
#[test]
fn a_failed_flush_on_disarm_is_terminal() {
    // Given a Recording machine.
    let mut machine = DictationMachine::new("stub-model", true);
    let _ = machine.init(&supported_list(), true);
    let _ = machine.load_finished(None);
    let _ = machine.arm();
    let _ = machine.arm_opened();

    // When the disarm's flush faults.
    let step = machine.disarm(None, Some("backend wedged".to_owned()));

    // Then the machine is in Error: the transcript state is unknown,
    // and guessing would insert wrong text.
    assert_eq!(step.state.phase, DictationPhase::Error);
    assert_eq!(step.state.error.as_deref(), Some("disarm: backend wedged"));
    assert_eq!(step.last_flushed, None);
}

#[rstest::rstest]
#[test]
fn a_lost_capture_while_recording_returns_to_ready_with_the_note() {
    // Given a Recording machine.
    let mut machine = DictationMachine::new("stub-model", true);
    let _ = machine.init(&supported_list(), true);
    let _ = machine.load_finished(None);
    let _ = machine.arm();
    let _ = machine.arm_opened();

    // When the capture reports it vanished without a toggle.
    let step = machine.capture_lost("device unplugged");

    // Then the machine is back in Ready carrying the reason.
    assert_eq!(step.state.phase, DictationPhase::Ready);
    assert_eq!(step.state.error.as_deref(), Some("device unplugged"));
}

#[rstest::rstest]
#[test]
fn a_lost_capture_report_outside_recording_changes_nothing() {
    // Given a machine mid-arming — the device open is running, the
    // stream never played, so there is no session to lose.
    let mut machine = DictationMachine::new("stub-model", true);
    let _ = machine.init(&supported_list(), true);
    let _ = machine.load_finished(None);
    let _ = machine.arm();

    // When a stray capture-lost report arrives.
    let step = machine.capture_lost("device unplugged");

    // Then the state is untouched: the report belonged to no session.
    assert_eq!(step.state.phase, DictationPhase::Arming);
    assert_eq!(step.state.error, None);
}

#[rstest::rstest]
#[test]
fn press_hint_reads_per_phase_exactly_as_the_row_expects() {
    // Given states in every phase.
    let offline = DictationState::default();
    let fetching = DictationState {
        phase: DictationPhase::Fetching,
        model_name: "stub-model".to_owned(),
        ..DictationState::default()
    };

    // When reading the press hints.
    // Then the disabled gate names the config fix and the busy phase
    // names the model.
    assert_eq!(
        offline.press_hint().as_deref(),
        Some("dictation disabled — set [dictation] enabled = true and restart")
    );
    assert_eq!(
        fetching.press_hint().as_deref(),
        Some("downloading stub-model")
    );
}

#[rstest::rstest]
#[test]
fn stub_backend_serves_the_full_trait_surface() {
    // Given a stub backend with a warm cache and a scripted flush.
    let mut backend = StubBackend {
        cached: true,
        flushed: Some(Utterance::new("trailing".to_owned())),
        ..StubBackend::default()
    };

    // When every trait method runs.
    // Then the scripted answers come back: cached, uncached fetch
    // progress, no feed output, and the flush hands over its utterance
    // exactly once.
    assert!(backend.is_cached());
    assert!(!backend.is_loaded());
    let mut last_percent = None;
    backend
        .fetch(&mut |progress| last_percent = progress.percent())
        .expect("stub fetch succeeds");
    assert_eq!(last_percent, Some(50));
    assert!(
        backend
            .feed(
                jinn_dictation_backend::DecodableAudio::new(vec![0.0; 512])
                    .expect("512 is decodable")
            )
            .expect("stub feed succeeds")
            .is_empty()
    );
    let flushed = backend.flush().expect("stub flush succeeds");
    assert_eq!(
        flushed.map(|utterance| utterance.text),
        Some("trailing".to_owned())
    );
    // And a second flush finds nothing: the utterance was taken.
    assert!(backend.flush().expect("stub flush succeeds").is_none());
}
