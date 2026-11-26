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

use jinn_webui2_msg::ClientMsg;

use crate::{actions, outbound::OutboundHandle};

/// What a handler wants sent back to the page that asked.
#[derive(Debug, Default)]
pub struct Reply {
    /// The frames to send, in order.
    pub frames: Vec<jinn_webui2_msg::ServerMsg>,
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
        }
    }
}

/// Interprets one client message.
///
/// The outbound handle carries the state every handler reads, the
/// connection registry a snapshot reply is written to, and the
/// revisions a resume is decided by.
#[must_use]
pub async fn dispatch(msg: ClientMsg, out: &OutboundHandle) -> Reply {
    match msg {
        ClientMsg::Hello {
            last_entry_id,
            revision,
        } => actions::hello(out, last_entry_id.as_ref(), revision),
        ClientMsg::Send { text } => actions::send(out, text).await,
        ClientMsg::Queue { text } => actions::queue(out, text).await,
        ClientMsg::Cancel => actions::cancel(out).await,
        ClientMsg::ActivateSession { session_id } => {
            actions::activate_session(out, session_id).await
        }
        ClientMsg::SwitchModel { provider_id } => actions::switch_model(out, provider_id).await,
        ClientMsg::Rename { title } => actions::rename(out, title).await,
        ClientMsg::PickerRows { filter } => actions::picker_rows(out, filter).await,
        ClientMsg::ModelRows { filter } => actions::model_rows(out, filter),
        ClientMsg::SkillRows { filter } => actions::skill_rows(out, filter),
        ClientMsg::ToggleSkill { skill } => actions::toggle_skill(out, &skill),
        ClientMsg::LoadSkill { skill } => actions::load_skill(out, &skill).await,
        ClientMsg::ToolRows { filter } => actions::tool_rows(out, filter),
        ClientMsg::ToggleTool { tool } => actions::toggle_tool(out, &tool),
        ClientMsg::McpRows { filter } => actions::mcp_rows(out, filter),
        ClientMsg::ToggleMcp { server } => actions::toggle_mcp(out, &server).await,
        ClientMsg::PersonaRows { filter } => actions::persona_rows(out, filter),
        ClientMsg::SwitchPersona { persona } => actions::switch_persona(out, &persona).await,
    }
}
