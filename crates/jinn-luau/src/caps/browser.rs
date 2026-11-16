//! The `jinn.browser` capability: the browser daemon's eleven-tool
//! surface for agent-written scripts.
//!
//! Every function is a thin daemon-client call — discovery/auto-start is
//! the shared client crate's job, the HTTP transport is the same
//! blocking client `jinn.http` uses, and the results are ordinary Lua
//! tables (the daemon's JSON envelope decoded through `jinn.json`'s
//! converter policy). A daemon failure raises a Lua error carrying the
//! daemon's message; there is no "current tab" — every tab-taking
//! function requires its `tab` field, exactly like the MCP tools.

use jinn_browser::client::DaemonClient;
use mlua::Lua;
use mlua::Table;
use mlua::Value;
use serde_json::Value as Json;

use crate::caps::Caps;
use crate::caps::json::from_json;
use std::rc::Rc;
use std::sync::Arc;

/// Registers the eleven `jinn.browser.*` functions.
///
/// # Errors
///
/// Propagates any registration failure (harness-side, not script-side).
///
/// The `caps` parameter is the family-injection convention (every
/// capability family takes it); this family's client needs nothing from
/// it, so the parameter goes unread.
#[expect(
    clippy::too_many_lines,
    reason = "eleven sibling registrations differing only in name and endpoint; a table-driven form would obscure each function's schema in macro indirection"
)]
pub fn inject(lua: &Lua, jinn: &Table, _caps: &Rc<Caps>) -> mlua::Result<()> {
    // The client is cheap to clone and owns no Chrome connection.
    // Script execution is single-threaded, so the client lives behind
    // an Arc the closures share.
    let client = Arc::new(DaemonClient::new());
    let family = lua.create_table()?;

    macro_rules! browser_fn {
        ($name:literal, $caps:ident, $body:expr) => {{
            let client = Arc::clone(&client);
            family.raw_set(
                $name,
                lua.create_function(move |lua, arg: Option<Table>| {
                    let arg = arg.unwrap_or_else(|| lua.create_table().expect("empty table"));
                    run_call(lua, &client, &arg, $body)
                })?,
            )?;
        }};
    }

    browser_fn!("open_tab", caps, |lua, client, arg| {
        let url: Option<String> = arg.get("url").ok();
        let tab = client.open_tab(url.as_deref()).map_err(|e| to_lua(&e))?;
        let json = serde_json::to_value(tab).map_err(mlua::Error::external)?;
        from_json(lua, &json)
    });
    browser_fn!("list_tabs", caps, |lua, client, _arg| {
        let tabs = client.list_tabs().map_err(|e| to_lua(&e))?;
        let json = serde_json::to_value(tabs).map_err(mlua::Error::external)?;
        from_json(lua, &json)
    });
    browser_fn!("navigate", caps, |lua, client, arg| {
        let tab: String = required_field(arg, "tab")?;
        let url: String = required_field(arg, "url")?;
        let info = client.navigate(&tab, &url).map_err(|e| to_lua(&e))?;
        let json = serde_json::to_value(info).map_err(mlua::Error::external)?;
        from_json(lua, &json)
    });
    browser_fn!("snapshot", caps, |lua, client, arg| {
        let tab: String = required_field(arg, "tab")?;
        let snapshot = client.snapshot(&tab).map_err(|e| to_lua(&e))?;
        let json = serde_json::to_value(snapshot).map_err(mlua::Error::external)?;
        from_json(lua, &json)
    });
    browser_fn!("click", caps, |lua, client, arg| {
        let tab: String = required_field(arg, "tab")?;
        let reference: String = required_field(arg, "ref")?;
        client.click(&tab, &reference).map_err(|e| to_lua(&e))?;
        from_json(lua, &Json::Null)
    });
    browser_fn!("click_at", caps, |lua, client, arg| {
        let tab: String = required_field(arg, "tab")?;
        let x: f64 = required_number(arg, "x")?;
        let y: f64 = required_number(arg, "y")?;
        client.click_at(&tab, x, y).map_err(|e| to_lua(&e))?;
        from_json(lua, &Json::Null)
    });
    browser_fn!("type", caps, |lua, client, arg| {
        let tab: String = required_field(arg, "tab")?;
        let reference: String = required_field(arg, "ref")?;
        let text: String = required_field(arg, "text")?;
        client
            .type_text(&tab, &reference, &text)
            .map_err(|e| to_lua(&e))?;
        from_json(lua, &Json::Null)
    });
    browser_fn!("press", caps, |lua, client, arg| {
        let tab: String = required_field(arg, "tab")?;
        let key: String = required_field(arg, "key")?;
        client.press(&tab, &key).map_err(|e| to_lua(&e))?;
        from_json(lua, &Json::Null)
    });
    browser_fn!("evaluate", caps, |lua, client, arg| {
        let tab: String = required_field(arg, "tab")?;
        let script: String = required_field(arg, "script")?;
        // Optional cross-origin frame (`g1` from the latest snapshot):
        // the script evaluates inside that frame's own session.
        let frame: Option<String> = match arg.get("frame") {
            Ok(mlua::Value::Nil) | Err(_) => None,
            Ok(mlua::Value::String(s)) => Some(s.to_str()?.to_owned()),
            Ok(_) => {
                return Err(mlua::Error::runtime("frame must be a string (e.g. \"g1\")"));
            }
        };
        let value = match frame.as_deref() {
            Some(frame) => client
                .evaluate_in_frame(&tab, frame, &script)
                .map_err(|e| to_lua(&e))?,
            None => client.evaluate(&tab, &script).map_err(|e| to_lua(&e))?,
        };
        from_json(lua, &value.unwrap_or(Json::Null))
    });
    browser_fn!("screenshot", caps, |lua, client, arg| {
        let tab: String = required_field(arg, "tab")?;
        let path = client.screenshot(&tab).map_err(|e| to_lua(&e))?;
        let result = lua.create_table()?;
        result.raw_set("path", path.to_string_lossy().into_owned())?;
        Ok(mlua::Value::Table(result))
    });
    browser_fn!("wait", caps, |lua, client, arg| {
        let tab: String = required_field(arg, "tab")?;
        let condition: jinn_browser::WaitCondition = wait_condition(arg)?;
        client.wait(&tab, &condition).map_err(|e| to_lua(&e))?;
        from_json(lua, &Json::Null)
    });
    browser_fn!("close_tab", caps, |lua, client, arg| {
        let tab: String = required_field(arg, "tab")?;
        client.close_tab(&tab).map_err(|e| to_lua(&e))?;
        from_json(lua, &Json::Null)
    });

    jinn.raw_set("browser", family)?;
    Ok(())
}

/// Runs one tool call against the daemon. The body signature is
/// `(lua, client, args) -> mlua::Result<Value>`; a daemon-side failure
/// raises a Lua error naming the daemon's message (the same text the
/// MCP tools carry).
fn run_call<F>(lua: &Lua, client: &DaemonClient, arg: &Table, body: F) -> mlua::Result<Value>
where
    F: FnOnce(&Lua, &DaemonClient, &Table) -> mlua::Result<Value>,
{
    body(lua, client, arg)
}

/// Reads a required string field, raising a Lua error naming the field
/// when absent — the schema-required rule, restated at the family
/// boundary.
fn required_field(arg: &Table, field: &'static str) -> mlua::Result<String> {
    arg.get::<Option<String>>(field)?
        .ok_or_else(|| mlua::Error::runtime(format!("missing required field `{field}`")))
}

/// A required numeric field (`click_at`'s coordinates).
fn required_number(arg: &Table, field: &'static str) -> mlua::Result<f64> {
    arg.get::<Option<f64>>(field)?
        .ok_or_else(|| mlua::Error::runtime(format!("missing required field `{field}`")))
}

/// Converts a client error into a Lua error carrying its message.
fn to_lua(report: &error_stack::Report<jinn_browser::client::ClientError>) -> mlua::Error {
    mlua::Error::runtime(report.current_context().to_string())
}

/// Reads the wait tool's `condition` field into the backend's shape:
/// `"navigation"`, `"network_idle"`, `{text = "…"}`, or `{ms = n}`.
fn wait_condition(arg: &Table) -> mlua::Result<jinn_browser::WaitCondition> {
    let value: mlua::Value = arg.get("condition")?;
    match value {
        mlua::Value::String(s) => {
            let owned = s.to_str().map_err(mlua::Error::runtime)?.to_owned();
            match owned.as_str() {
                "navigation" => Ok(jinn_browser::WaitCondition::Navigation),
                "network_idle" => Ok(jinn_browser::WaitCondition::NetworkIdle),
                other => Err(mlua::Error::runtime(format!(
                    "`condition` string must be \"navigation\" or \"network_idle\"; got \"{other}\""
                ))),
            }
        }
        mlua::Value::Table(t) => {
            if let Ok(Some(text)) = t.get::<Option<String>>("text") {
                return Ok(jinn_browser::WaitCondition::Text(text));
            }
            if let Ok(Some(ms)) = t.get::<Option<u64>>("ms") {
                return Ok(jinn_browser::WaitCondition::Ms(ms));
            }
            Err(mlua::Error::runtime(
                "`condition` table must carry exactly one of `text` or `ms`",
            ))
        }
        _ => Err(mlua::Error::runtime(
            "`condition` must be a string or a table with `text`/`ms`",
        )),
    }
}
