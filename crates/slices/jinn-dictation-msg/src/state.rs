//! The dictation state and meter-level payloads.
//!
//! [`DictationState`] is the one state machine fact the whole UI reads:
//! the phase, the model name, the fetch progress, and — in
//! [`DictationPhase::Error`] — the fault description. [`DictationLevel`]
//! is the recording meter's live input level.

use jinn_slices::SliceScopeId;
use jinn_slices::SlotKey;

/// The dictation slice's dynamic focus scope.
///
/// The slice registers no overlay and pushes no scope today (alt-i works
/// from any scope as a global toggle); the scope id is the slice's
/// identity for its route rows and dynamic intents.
#[must_use]
pub fn dictation_scope() -> SliceScopeId {
    SliceScopeId::new("dictation", "main")
}

/// The dictation state cell's slot key.
#[must_use]
pub fn dictation_state_slot() -> SlotKey {
    SlotKey::builtin("dictation", "state")
}

/// The dictation meter-level cell's slot key.
#[must_use]
pub fn dictation_level_slot() -> SlotKey {
    SlotKey::builtin("dictation", "level")
}

/// The dictation lifecycle's phases.
///
/// The actor is the sole writer; the phase graph is linear except for
/// the two entry points into [`DictationPhase::Error`], which is
/// terminal for the process run (no retry, no auto-recovery). Arming
/// is the honest gap between the toggle press and a live microphone:
/// [`DictationPhase::Recording`] means the capture stream actually
/// plays, not merely that a press asked for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DictationPhase {
    /// Not started: `enabled = false`, or init has not run yet.
    #[default]
    Offline,
    /// The model is downloading; `fetch_progress_percent` advances.
    Fetching,
    /// The model is reading from cache into memory.
    Loading,
    /// Resident and idle; arming is possible.
    Ready,
    /// A toggle press asked for the microphone; the device open is
    /// running. A refusal returns to Ready with the reason; the stream
    /// playing moves on to Recording.
    Arming,
    /// The microphone is open and audio is feeding the backend.
    Recording,
    /// Terminal for the process run; `error` carries the fault text.
    Error,
}

/// The dictation slice's state-machine cell payload.
///
/// The dictation actor is the sole writer; the route action (kernel-side)
/// and the meter draw function resolve read handles through the registry.
#[derive(Debug, Clone, Default)]
pub struct DictationState {
    /// Where the lifecycle sits.
    pub phase: DictationPhase,
    /// The backend's model display name (hints and the dashboard row).
    pub model_name: String,
    /// Fetch progress as a whole percent (0–100); `None` while the total
    /// is unknown or no fetch is running.
    pub fetch_progress_percent: Option<u8>,
    /// The fault description, populated in [`DictationPhase::Error`].
    pub error: Option<String>,
}

impl DictationState {
    /// The hint text an alt-i press reports while the lifecycle is
    /// busy or unavailable.
    ///
    /// `None` for [`DictationPhase::Ready`] and
    /// [`DictationPhase::Recording`]: arming and disarming push no
    /// hint — the meter is the signal.
    #[must_use]
    pub fn press_hint(&self) -> Option<String> {
        match self.phase {
            DictationPhase::Offline => {
                Some("dictation disabled — set [dictation] enabled = true and restart".to_owned())
            }
            DictationPhase::Fetching => {
                let model = display_name(&self.model_name);
                Some(format!("downloading {model}"))
            }
            DictationPhase::Loading => {
                let model = display_name(&self.model_name);
                Some(format!("loading {model}"))
            }
            DictationPhase::Arming => {
                let model = display_name(&self.model_name);
                Some(format!("arming {model}"))
            }
            DictationPhase::Error => Some(format!(
                "dictation failed: {}",
                self.error
                    .clone()
                    .unwrap_or_else(|| "unknown error".to_owned())
            )),
            DictationPhase::Ready | DictationPhase::Recording => None,
        }
    }
}

/// The model name for display, with a fallback when init has not run.
fn display_name(model_name: &str) -> &str {
    if model_name.is_empty() {
        "dictation model"
    } else {
        model_name
    }
}

/// The recording meter's live input level.
///
/// Written by the dictation actor from capture-thread reports; read by
/// the meter draw function every frame. `level` is the smoothed peak
/// amplitude in 0.0..=1.0.
#[derive(Debug, Clone, Default)]
pub struct DictationLevel {
    /// The smoothed input level in 0.0..=1.0.
    pub level: f32,
}

impl DictationLevel {
    /// Clamps a raw peak amplitude into the payload's range.
    ///
    /// Negative samples arrive as magnitude; anything outside 0.0..=1.0
    /// (float noise, a full-scale clip) clamps at the boundary.
    #[must_use]
    pub fn from_amplitude(value: f32) -> Self {
        Self {
            level: value.clamp(0.0, 1.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DictationLevel;
    use super::DictationPhase;
    use super::DictationState;

    #[rstest::rstest]
    #[test]
    fn default_state_reads_offline() {
        // Given a fresh dictation state.
        let state = DictationState::default();

        // When reading the phase.
        // Then it is Offline with no error.
        assert_eq!(state.phase, DictationPhase::Offline);
        assert_eq!(state.error, None);
    }

    #[rstest::rstest]
    #[test]
    fn offline_press_hint_names_the_config_gate() {
        // Given a state still Offline.
        let state = DictationState::default();

        // When reading the press hint.
        // Then it tells the user how to enable dictation.
        assert_eq!(
            state.press_hint().as_deref(),
            Some("dictation disabled — set [dictation] enabled = true and restart")
        );
    }

    #[rstest::rstest]
    #[test]
    fn fetching_press_hint_names_the_model() {
        // Given a state mid-download with a model name.
        let state = DictationState {
            phase: DictationPhase::Fetching,
            model_name: "parakeet-tdt-0.6b-v2".to_owned(),
            ..DictationState::default()
        };

        // When reading the press hint.
        // Then it names the model being downloaded.
        assert_eq!(
            state.press_hint().as_deref(),
            Some("downloading parakeet-tdt-0.6b-v2")
        );
    }

    #[rstest::rstest]
    #[test]
    fn loading_press_hint_names_the_model() {
        // Given a state mid-load with a model name.
        let state = DictationState {
            phase: DictationPhase::Loading,
            model_name: "parakeet-tdt-0.6b-v2".to_owned(),
            ..DictationState::default()
        };

        // When reading the press hint.
        // Then it names the model being loaded.
        assert_eq!(
            state.press_hint().as_deref(),
            Some("loading parakeet-tdt-0.6b-v2")
        );
    }

    #[rstest::rstest]
    #[test]
    fn error_press_hint_carries_the_fault() {
        // Given a state in Error with a fault description.
        let state = DictationState {
            phase: DictationPhase::Error,
            error: Some("no capture device named 'usb-mic'".to_owned()),
            ..DictationState::default()
        };

        // When reading the press hint.
        // Then the fault text rides along.
        assert_eq!(
            state.press_hint().as_deref(),
            Some("dictation failed: no capture device named 'usb-mic'")
        );
    }

    #[rstest::rstest]
    #[test]
    fn ready_and_recording_push_no_hint() {
        // Given the two armed-cycle phases.
        let ready = DictationState {
            phase: DictationPhase::Ready,
            ..DictationState::default()
        };
        let recording = DictationState {
            phase: DictationPhase::Recording,
            ..DictationState::default()
        };

        // When reading the press hints.
        // Then both are None: the meter is the signal on arm and disarm.
        assert_eq!(ready.press_hint(), None);
        assert_eq!(recording.press_hint(), None);
    }

    #[rstest::rstest]
    #[test]
    fn amplitude_clamps_into_the_level_range() {
        // Given raw amplitudes below zero, above one, and inside the range.
        let negative = DictationLevel::from_amplitude(-0.4);
        let over = DictationLevel::from_amplitude(1.7);
        let normal = DictationLevel::from_amplitude(0.3);

        // When clamping each into a level payload.
        // Then the out-of-range values clamp at the boundaries and the
        // in-range value survives.
        assert_eq!(negative.level, 0.0);
        assert_eq!(over.level, 1.0);
        assert_eq!(normal.level, 0.3);
    }
}
