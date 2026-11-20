//! The new-session region — the menu item, its creation modal, and
//! the create POST's server half.
//!
//! Parts declared: GET pull route only. No settle render (the modal
//! is pull-opened, never part of a page render), no SSE event (the
//! push-side visibility rides the fan-out's `SessionCreated` /
//! `SessionSetupCompleted` subscriptions on existing kinds), and no
//! page fill (the modal lives in the page shell's host span, outside
//! every filled region).
//!
//! The pull renders the form fragment from the config layer:
//! one `<option>` per `[[project.entry]]` entry (the raw configured
//! path string is the value, so the create POST round-trips exactly
//! what the list offered) and the lifecycle list with the blank row
//! first. The `?lifecycle=` query re-selects — a lifecycle change
//! re-pulls the fragment wholesale (the model modal's filter-re-pull
//! pattern), and the fragment renders the args input only when the
//! selected lifecycle's setup declares parameters. With no project
//! entries configured the pull renders the explanatory refusal
//! wrapper instead of a usable form.
//!
//! Creation itself is [`crate::delivery::handle_create`]: this region
//! only renders. The POST route is assembled in `server.rs` (the
//! file that owns every action POST); the region trait carries GET
//! pulls only.
//!
//! Browser half: the modal's Datastar bindings (`session_new.html`) — the
//! item opens the modal, a lifecycle change re-pulls, submit posts
//! and swaps the answered whole page (the activation navigation).

use axum::extract::Query as AxQuery;
use axum::extract::State as AxumState;
use jinn_preferences_config::schemas::{ProjectConfig, SessionLifecycle};
use jinn_session_lifecycle_msg::command_template::CommandTemplate;
use jinn_session_lifecycle_msg::command_template::Param;
use jinn_webui_msg::SseKind;

use crate::region::RegionParts;
use crate::region::RegionPull;
use crate::region::WebRegion;
use crate::templates::SessionNewOption;

/// The new-session region.
pub struct SessionNewRegion;

impl WebRegion for SessionNewRegion {
    fn id(&self) -> &'static str {
        "session-new"
    }

    fn parts(&self) -> RegionParts {
        RegionParts {
            settle_render: false,
            pull_route: true,
            sse_event: false,
            page_fill: false,
        }
    }

    fn event_kinds(&self) -> &'static [SseKind] {
        &[]
    }

    fn pull(&self) -> Option<RegionPull> {
        Some(RegionPull {
            pattern: crate::contract::SESSION_NEW_ROUTE,
            handler: {
                use axum::routing::get;
                get(pull_handler)
            },
        })
    }

    fn container_id(&self) -> Option<&'static str> {
        // The modal swaps into the page shell's host span, not into a
        // region-owned container.
        None
    }

    fn css(&self) -> &'static str {
        SESSION_NEW_CSS
    }

    fn render(
        &self,
        _session_id: &jinn_core_types::SessionId,
        _guard: &jinn_kernel::common::state::StateReadGuard<'_>,
    ) -> String {
        // Declared without the settle-render part: the modal is
        // pull-opened from the menu item and never appears in a page
        // render.
        String::new()
    }
}

/// The pull's query: the optional lifecycle re-selection.
#[derive(Debug, Clone, serde::Deserialize, Default)]
pub struct SessionNewPullQuery {
    /// The lifecycle the form renders selected; absent or empty is
    /// the blank row.
    #[serde(default)]
    pub lifecycle: Option<String>,
}

/// `GET /web/session-new?lifecycle=…` — the new-session form
/// fragment (or the refusal wrapper when no projects are configured).
async fn pull_handler(
    AxumState(shared): AxumState<crate::server::ServerShared>,
    AxQuery(query): AxQuery<SessionNewPullQuery>,
) -> Result<crate::server::Fragment, crate::server::HandlerError> {
    let lifecycle = query.lifecycle.unwrap_or_default();
    form_fragment(shared.config(), &lifecycle).map(|html| {
        crate::server::Fragment::new(html)
            .with_swap(crate::contract::SESSION_NEW_MODAL_HOST_SELECTOR, "inner")
    })
}

/// One lifecycle's form-facing view: name and its setup template.
struct LifecycleOption {
    name: String,
    template: Option<CommandTemplate>,
}

impl LifecycleOption {
    /// The setup template's minimum arg count. A lifecycle whose
    /// setup is a `Builtin` command carries no template; the TUI
    /// picker omits such rows from the arg path, and the web form
    /// treats them the same way (no args input, no minimum) while
    /// still listing them.
    fn param_count(&self) -> usize {
        self.template
            .as_ref()
            .map_or(0, CommandTemplate::param_count)
    }
}

/// The configured lifecycle entries with their setup templates.
fn lifecycle_options(config: &jinn_slices::ConfigLayer) -> Vec<LifecycleOption> {
    config
        .get_list::<SessionLifecycle>()
        .unwrap_or_default()
        .into_iter()
        .map(|lifecycle| LifecycleOption {
            name: lifecycle.name,
            template: lifecycle.setup.as_ref().and_then(|command| match command {
                jinn_preferences_config::schemas::LifecycleCommand::Shell(shell) => {
                    Some(CommandTemplate::parse(shell))
                }
                jinn_preferences_config::schemas::LifecycleCommand::Builtin(_) => None,
            }),
        })
        .collect()
}

/// The form fragment: the modal (or refusal wrapper) HTML.
///
/// An explicit re-selection must name a configured lifecycle — the
/// 400 refuses the re-pull rather than rendering a form that lies
/// about what will run.
///
/// # Errors
///
/// Returns the render error (mapped to 500) or
/// [`crate::server::HandlerError`] with status 400 for an unknown
/// lifecycle re-selection.
pub(crate) fn form_fragment(
    config: &jinn_slices::ConfigLayer,
    lifecycle: &str,
) -> Result<String, crate::server::HandlerError> {
    let projects: Vec<SessionNewOption> = config
        .get_list::<ProjectConfig>()
        .unwrap_or_default()
        .into_iter()
        .map(|entry| {
            let raw = entry.path.to_string_lossy().into_owned();
            SessionNewOption {
                value: raw.clone(),
                label: raw,
                param_count: None,
            }
        })
        .collect();

    // No configured projects: the explanatory refusal, not a form
    // that cannot succeed.
    if projects.is_empty() {
        return crate::templates::render(&crate::templates::SessionNewTemplate::with_contract(
            Vec::new(),
            Vec::new(),
            String::new(),
            false,
            String::new(),
        ))
        .map_err(|e| crate::server::map_render_error(&e));
    }

    let lifecycles = lifecycle_options(config);
    // An explicit re-selection must name a configured lifecycle; the
    // blank row (empty string) is always valid.
    let selected = if lifecycle.is_empty() {
        String::new()
    } else if lifecycles.iter().any(|l| l.name == lifecycle) {
        lifecycle.to_owned()
    } else {
        return Err(crate::server::fail(
            400,
            format!("unknown lifecycle {lifecycle}"),
        ));
    };

    let selected_option = lifecycles.iter().find(|l| l.name == selected);
    let param_count = selected_option.map_or(0, LifecycleOption::param_count);
    let show_args = param_count > 0;
    let arg_tokens = selected_option
        .and_then(|l| l.template.as_ref())
        .map_or_else(String::new, arg_display_tokens);

    let mut rendered = Vec::with_capacity(lifecycles.len() + 1);
    rendered.push(SessionNewOption {
        value: String::new(),
        label: "blank — new empty session".to_owned(),
        param_count: None,
    });
    for option in &lifecycles {
        rendered.push(SessionNewOption {
            value: option.name.clone(),
            label: option.name.clone(),
            param_count: (option.param_count() > 0).then_some(option.param_count()),
        });
    }

    crate::templates::render(&crate::templates::SessionNewTemplate::with_contract(
        projects, rendered, selected, show_args, arg_tokens,
    ))
    .map_err(|e| crate::server::map_render_error(&e))
}

/// The template's replacement tokens in display form, space-joined —
/// `<1> <branch> <args>` — what the setup script asks the user to
/// fill, the same token list the Discord prompt shows (`Please
/// enter: …`).
fn arg_display_tokens(template: &CommandTemplate) -> String {
    template
        .params()
        .iter()
        .map(|p| match p {
            Param::Named(name) => format!("<{name}>"),
            Param::Positional(n) => format!("<{n}>"),
            Param::Splat => "<args>".to_owned(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The new-session modal's stylesheet: overlay, card, field rows,
/// the args hint, and the refusal wrapper.
pub const SESSION_NEW_CSS: &str = r#"
.session-new-modal { position: fixed; inset: 0; background: rgba(0,0,0,.55);
                     display: flex; align-items: flex-start;
                     justify-content: center; padding-top: 10vh; z-index: 50; }
.session-new-modal[hidden] { display: none; }
.session-new-modal-inner { background: var(--panel); border: 1px solid var(--border);
                           border-radius: 8px; width: min(520px, 92vw);
                           padding: 16px; display: flex; flex-direction: column; gap: 12px; }
.session-new-title { margin: 0; font-size: 1.05rem; }
.session-new-field { display: flex; flex-direction: column; gap: 4px; }
.session-new-field label { color: var(--muted); font-size: .85rem; }
.session-new-field select, .session-new-field input { background: var(--bg);
                       color: var(--fg); border: 1px solid var(--border);
                       border-radius: 6px; padding: 6px 8px; font: inherit; }
.session-new-hint { margin: 0; color: var(--muted); font-size: .8rem; }
.session-new-actions { display: flex; justify-content: flex-end; }
.session-new-submit { background: var(--accent); color: var(--bg);
                      border: 1px solid var(--accent); border-radius: 6px;
                      padding: 6px 14px; font: inherit; cursor: pointer; }
.session-new-submit:hover { filter: brightness(1.1); }
.session-new-empty p { color: var(--fg); margin: 0 0 8px; }
.session-new-empty code { background: var(--bg); border-radius: 4px; padding: 1px 4px; }
"#;
