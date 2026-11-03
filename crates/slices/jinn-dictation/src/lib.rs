//! The dictation slice — local voice input.
//!
//! One actor ([`DictationActor`]) owns the backend lifecycle
//! (Offline → Fetching → Loading → Ready ⇄ Recording, Error terminal),
//! driving the pure [`DictationMachine`] and writing the two cells the
//! rest of the app reads: the state cell (phase, model, fetch percent,
//! fault) and the meter-level cell. The real Parakeet backend runs on
//! linux; every other platform runs
//! [`jinn_dictation_backend::NullBackend`] through the one gated module
//! ([`platform`]) and the linux target-scoped dependency table in
//! `Cargo.toml` — the lifecycle runs identically everywhere, and only
//! the transcription differs.
//!
//! alt-i ([`routes`]) is a global-toggle route row: busy phases push
//! the state-name hint, Ready arms, Recording disarms with a
//! force-finalized trailing utterance committed through the shared
//! paste-append helper in `jinn-chat-input-msg`.
//!
//! The meter ([`meter`]) renders on the always-painted draw path —
//! invisible idle, green while recording — at the seam between the
//! main column and the sidebar, hidden on full-width tabs.

pub mod capture;
pub mod config;
pub mod dictation_actor;
pub mod machine;
#[cfg(test)]
mod machine_tests;
pub mod meter;
pub mod platform;
pub mod routes;
pub mod row;

pub use dictation_actor::DictationActor;
pub use dictation_actor::DictationActorDeps;
pub use machine::DictationMachine;
pub use platform::fetch_model;
pub use platform::platform_default_model;
pub use platform::platform_supports_real_backend;
pub use row::row_note;

use jinn_slices::SliceHost;

/// Activates the slice: registers the meter's draw slot and attaches
/// the alt-i row.
///
/// The actor spawns here too (it reads nothing another activation
/// mints); the dictation cells came from the shared catalog, resolved
/// read-only is not enough for the actor, so the handles arrive through
/// [`DictationActorDeps`].
///
/// `state` is the shared application state handle (the utterance
/// commits write the focused input buffer through it); `config` is the
/// same `jinn.toml` layer every slice receives, read here at
/// activation — a changed `[dictation]` section takes effect on the
/// next jinn start.
/// # Panics
///
/// Panics if the catalog has not run — the actor would hold no cells and
/// the meter would read nothing, and the alt-i row would dispatch into
/// nothing.
#[expect(
    clippy::expect_used,
    reason = "bootstrap assertion: broken slice wiring must abort launch, not continue degraded"
)]
pub fn activate(
    host: &mut SliceHost<'_, jinn_slices::RenderFacts>,
    state: &jinn_kernel::common::state::State,
    services: &jinn_kernel::Services,
    config: &jinn_config::ConfigLayer,
) {
    let settings = match config::DictationSettings::from_layer(config) {
        Ok(settings) => settings,
        Err(report) => {
            // A malformed `[dictation]` section is a launch-blocking
            // wiring failure, the same severity a malformed `[canvas]`
            // bind carries. The launch-validate pass should have caught
            // it earlier; treat reaching here as a bug and log loudly.
            tracing::error!(
                ?report,
                "failed to read the [dictation] section; dictation disabled for this run"
            );
            config::DictationSettings {
                enabled: false,
                device: None,
                model: platform::DEFAULT_MODEL.to_owned(),
                noise_reduction: true,
            }
        }
    };

    let state_cell = host
        .slices()
        .reader::<jinn_dictation_msg::DictationState>(&jinn_dictation_msg::dictation_state_slot())
        .expect("the cell catalog registers the dictation state cell before any slice activates");
    let level_cell = host
        .slices()
        .reader::<jinn_dictation_msg::DictationLevel>(&jinn_dictation_msg::dictation_level_slot())
        .expect("the cell catalog registers the dictation level cell before any slice activates");

    // The meter's draw slot: always-painted, composition dispatches it
    // on chat frames with the full area; the geometry derives inside.
    host.slices()
        .register_render_slot::<jinn_kernel::common::app_state::AppState>(
            jinn_slices::Region::DictationMeter,
            std::sync::Arc::new(
                |frame: &mut ratatui::Frame<'_>,
                 target: jinn_slices::DrawTarget,
                 ctx: &dyn jinn_slices::DrawContext<jinn_kernel::common::app_state::AppState>,
                 _rects: &mut Vec<ratatui::layout::Rect>| {
                    meter::paint(frame, target, ctx);
                },
            ),
        );

    // The alt-i row.
    routes::attach_all(host.key_routes());

    // The lifecycle actor.
    let backend = platform::build_backend(&settings.model);
    DictationActor::spawn(
        host.system(),
        DictationActorDeps {
            state_cell,
            level_cell,
            bus: services.bus.clone(),
            runtime: tokio::runtime::Handle::current(),
            config: settings,
            backend,
            state: state.clone(),
        },
    );
}
