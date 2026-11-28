//! The inbound dispatch — one `match` over every `ClientMsg`.
//!
//! The browser speaks only `ClientMsg`s; this module is the one place
//! they are interpreted. Each arm delegates to a named handler in
//! `actions.rs`, and there is deliberately no wildcard arm: a new variant
//! is a compile error until it has an arm here.
//!
//! A handler answers with [`Reply`] — the `ServerMsg`s to send the page
//! that asked, in order. Facts every open page must see are not replies;
//! the outbound actor pushes those to every connection as they happen.

use std::sync::{Arc, atomic::AtomicBool};

use jinn_webui2_msg::ClientMsg;

use crate::{actions, outbound::OutboundHandle};

/// What a handler wants sent back to the page that asked.
#[derive(Debug, Default)]
pub struct Reply {
    /// The frames to send, in order.
    pub frames: Vec<jinn_webui2_msg::ServerMsg>,
    /// Set on a jump's reply, and on no other: the connection's reader
    /// checks it before sending each frame and drops the rest once it is
    /// set, so a cancel stops the pages the page has not yet received.
    /// `None` means the reply drains whole, as every reply but a jump's does.
    pub cancel_flag: Option<Arc<AtomicBool>>,
}

impl Reply {
    /// An empty reply: nothing to send.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// A reply carrying one frame.
    #[must_use]
    pub fn one(frame: jinn_webui2_msg::ServerMsg) -> Self {
        Self {
            frames: vec![frame],
            cancel_flag: None,
        }
    }
}

/// Interprets one client message.
///
/// The outbound handle carries the state every handler reads, the
/// connection registry a snapshot reply is written to, and the
/// revisions a resume is decided by. The cancel flag is the asking
/// connection's own: a jump's reply is stamped with it, and a cancel
/// sets it, so a cancel on one page never reaches another page's jump.
#[must_use]
pub async fn dispatch(
    msg: ClientMsg,
    out: &OutboundHandle,
    jump_cancelled: &Arc<AtomicBool>,
) -> Reply {
    match msg {
        ClientMsg::Hello {
            last_entry_id,
            revision,
        } => actions::hello(out, last_entry_id.as_ref(), revision),
        ClientMsg::MoreEntries { before } => actions::more_entries(out, &before),
        ClientMsg::DashOpen => actions::dash_open(out),
        ClientMsg::DashClose => actions::dash_close(out),
        ClientMsg::JumpToEntry { entry_id } => {
            actions::jump_to_entry(out, &entry_id, jump_cancelled)
        }
        ClientMsg::CancelJump => actions::cancel_jump(jump_cancelled),
        ClientMsg::Send { text, images } => actions::send(out, text, images).await,
        ClientMsg::Queue { text, images } => actions::queue(out, text, images).await,
        ClientMsg::Cancel => actions::cancel(out).await,
        ClientMsg::ActivateSession { session_id } => {
            actions::activate_session(out, session_id).await
        }
        ClientMsg::SwitchModel { provider_id } => actions::switch_model(out, provider_id).await,
        ClientMsg::Rename { title } => actions::rename(out, title).await,
        ClientMsg::PickerRows { filter } => actions::picker_rows(out, filter).await,
        ClientMsg::ModelRows { filter } => actions::model_rows(out, filter),
        ClientMsg::MarketOpen { view } => crate::market::open(out, view).await,
        ClientMsg::MarketRows { view } => crate::market::rows(out, view),
        ClientMsg::MarketDetail { model_id, view } => {
            crate::market::detail(out, model_id, view).await
        }
        ClientMsg::MarketUse { model_id, tag } => {
            crate::market::use_model(out, model_id, tag).await
        }
        ClientMsg::MarketRefresh => crate::market::refresh(out).await,
        ClientMsg::SkillRows { filter } => actions::skill_rows(out, filter),
        ClientMsg::ToggleSkill { skill } => actions::toggle_skill(out, &skill),
        ClientMsg::LoadSkill { skill } => actions::load_skill(out, &skill).await,
        ClientMsg::ToolRows { filter } => actions::tool_rows(out, filter),
        ClientMsg::ToggleTool { tool } => actions::toggle_tool(out, &tool),
        ClientMsg::McpRows { filter } => actions::mcp_rows(out, filter),
        ClientMsg::ToggleMcp { server } => actions::toggle_mcp(out, &server).await,
        ClientMsg::PersonaRows { filter } => actions::persona_rows(out, filter),
        ClientMsg::SwitchPersona { persona } => actions::switch_persona(out, &persona).await,
        ClientMsg::TogglePin { entry_id } => actions::toggle_pin(out, entry_id).await,
        ClientMsg::ToggleContext { entry_id } => actions::toggle_context(out, entry_id).await,
        ClientMsg::Fork { entry_id } => actions::fork(out, entry_id).await,
        ClientMsg::AppletRows { filter } => actions::applet_rows(out, filter).await,
        ClientMsg::RefreshModels => actions::refresh_models(out).await,
        ClientMsg::SetTheme { theme } => actions::set_theme(out, theme).await,
        ClientMsg::SetLibrary {
            session_id,
            in_library,
        } => actions::set_library(out, session_id, in_library).await,
        ClientMsg::ArchiveSession { session_id } => actions::archive_session(out, session_id).await,
        ClientMsg::TeardownSession { session_id } => {
            actions::teardown_session(out, session_id).await
        }
        ClientMsg::PushSubscribe(subscription) => {
            actions::push_subscribe(out, subscription.endpoint, subscription.keys)
        }
        ClientMsg::PushUnsubscribe { endpoint } => actions::push_unsubscribe(out, &endpoint),
        ClientMsg::NotificationOpened { session_id } => {
            actions::notification_opened(out, session_id).await
        }
        ClientMsg::NewSessionOptions => actions::new_session_options(out),
        ClientMsg::CreateSession {
            project,
            lifecycle,
            args,
        } => actions::create_session(out, project, lifecycle, args).await,
    }
}
