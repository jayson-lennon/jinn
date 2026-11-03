//! The dashboard row's note text — one spelling, tested.
//!
//! The actor publishes through the bus; this pure function is what the
//! tests and the actor share, so the row's phrasing has one home.
//! Fetch progress rides in the note ("downloading … 12%") so the row
//! updates in place as the percent advances.

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
        DictationPhase::Ready => format!("ready ({model})"),
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
