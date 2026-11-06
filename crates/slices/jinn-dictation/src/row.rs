//! The dashboard row's note text — one spelling, tested.
//!
//! The actor publishes through the bus; this pure function is what the
//! tests and the actor share, so the row's phrasing has one home.
//! Fetch progress rides in the note ("downloading … 12%") so the row
//! updates in place as the percent advances. A Ready-with-error row is
//! a refused arm: the note spells out the refusal.

use jinn_dictation_msg::DictationPhase;
use jinn_dictation_msg::DictationState;
use jinn_slices::NoteTone;
use jinn_slices::ServiceStatusUpdate;
use jinn_slices::TypedCell;

/// Publishes the dictation row from the current cell state.
///
/// The row's note tone follows the phase: error tone in Error, muted
/// otherwise. The row name is the actor's trouper path — the same key
/// the runtime census announces the actor under.
pub async fn publish_row(
    bus: &jinn_slices::bus::BusService,
    state_cell: &TypedCell<DictationState>,
) {
    let state = state_cell.read().clone();
    let note_tone = match state.phase {
        DictationPhase::Error => Some(NoteTone::Error),
        _ => None,
    };
    bus.publish(ServiceStatusUpdate {
        name: crate::dictation_actor::DICTATION_PATH.to_owned(),
        description: None,
        status_message: Some(row_note(&state)),
        note_tone,
    })
    .await;
}

/// Publishes a pending-change note on the dictation row.
///
/// The live-reload path's visible half: a deferred or refused
/// `[dictation]` edit must never be silent, and the row is where the
/// actor can say so. Warning tone — attention, not failure.
pub async fn publish_pending_note(bus: &jinn_slices::bus::BusService, note: &str) {
    bus.publish(ServiceStatusUpdate {
        name: crate::dictation_actor::DICTATION_PATH.to_owned(),
        description: None,
        status_message: Some(note.to_owned()),
        note_tone: Some(NoteTone::Warning),
    })
    .await;
}

/// The dashboard row's note for a state.
#[must_use]
pub fn row_note(state: &DictationState) -> String {
    let model = if state.model_name.is_empty() {
        "dictation"
    } else {
        &state.model_name
    };
    match state.phase {
        DictationPhase::Offline => "dictation disabled".to_owned(),
        DictationPhase::Fetching => match state.fetch_progress_percent {
            Some(percent) => format!("downloading {model} — {percent}%"),
            None => format!("downloading {model}"),
        },
        DictationPhase::Loading => format!("loading {model}"),
        DictationPhase::Arming => format!("arming {model}"),
        // The refusal note rides here: a refused arm lands in Ready
        // with `error` set, and the dashboard is where the actor can
        // say why the microphone never opened.
        DictationPhase::Ready => match state.error.as_deref() {
            Some(refusal) => format!("ready ({model}) — {refusal}"),
            None => format!("ready ({model})"),
        },
        DictationPhase::Recording => "recording".to_owned(),
        DictationPhase::Error => format!(
            "failed: {}",
            state
                .error
                .clone()
                .unwrap_or_else(|| "unknown error".to_owned())
        ),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test code, panics are acceptable")]

    use super::row_note;
    use jinn_dictation_msg::DictationPhase;
    use jinn_dictation_msg::DictationState;

    #[rstest::rstest]
    #[test]
    fn row_note_reads_arming() {
        // Given a state mid-arm with a model name.
        let state = DictationState {
            phase: DictationPhase::Arming,
            model_name: "stub-model".to_owned(),
            ..DictationState::default()
        };

        // When reading the row note.
        // Then it names the model being armed.
        assert_eq!(row_note(&state), "arming stub-model");
    }

    #[rstest::rstest]
    #[test]
    fn row_note_surfaces_the_refusal_on_ready() {
        // Given a Ready state carrying a refused arm's note.
        let state = DictationState {
            phase: DictationPhase::Ready,
            model_name: "stub-model".to_owned(),
            error: Some(
                "no capture device available for usb-mic; available inputs: builtin".to_owned(),
            ),
            ..DictationState::default()
        };

        // When reading the row note.
        // Then the refusal rides after the ready note — the dashboard
        // is where the refused arm explains itself.
        assert_eq!(
            row_note(&state),
            "ready (stub-model) — no capture device available for usb-mic; available inputs: builtin"
        );
    }

    #[rstest::rstest]
    #[test]
    fn row_note_ready_without_an_error_stays_plain() {
        // Given a Ready state with no error.
        let state = DictationState {
            phase: DictationPhase::Ready,
            model_name: "stub-model".to_owned(),
            ..DictationState::default()
        };

        // When reading the row note.
        // Then it is the plain ready note — no trailing separator.
        assert_eq!(row_note(&state), "ready (stub-model)");
    }
}
