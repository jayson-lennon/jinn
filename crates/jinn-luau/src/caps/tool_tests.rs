//! Tests for the `jinn.tool` registration family.
//!
//! Every test drives the real inject path on a fresh VM and asserts on
//! the observable registration state — the same surface the serve layer
//! reads.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used,
    reason = "test assertions need expect/panic/indexing to state what went wrong"
)]

use super::*;

/// A VM with the tool family injected, returning the shared registration.
fn tool_vm() -> (
    Lua,
    mlua::Function,
    std::rc::Rc<std::cell::RefCell<Registration>>,
) {
    let lua = Lua::new();
    let jinn = lua.create_table().expect("table");
    let registration = std::rc::Rc::new(std::cell::RefCell::new(Registration::new()));
    inject(&lua, &jinn, &registration).expect("injects");
    let tool: mlua::Function = jinn.get("tool").expect("tool function");
    (lua, tool, registration)
}

/// Builds a spec table from string key/value pairs.
fn spec(lua: &Lua, fields: &[(&str, String)]) -> Table {
    let table = lua.create_table().expect("table");
    for (key, value) in fields {
        table.raw_set(*key, value.as_str()).expect("set");
    }
    table
}

#[rstest::rstest]
#[test]
fn a_registration_collects_name_description_handler_and_schema() {
    // Given a VM with the tool family and a valid spec + handler.
    let (lua, tool, registration) = tool_vm();
    let spec_table = spec(
        &lua,
        &[("name", "greet".into()), ("description", "Say hi".into())],
    );
    let handler = lua
        .load("return function(args) return { hi = args.who } end")
        .eval::<mlua::Function>()
        .expect("handler");
    let input = lua.create_table().expect("table");
    input.raw_set("who", "string").expect("set");
    spec_table.raw_set("input", input).expect("set");

    // When registering.
    tool.call::<()>((spec_table, handler)).expect("registers");

    // Then one tool is collected with every field.
    let tools = registration.borrow().tools().to_vec();
    assert_eq!(tools.len(), 1);
    let registered = &tools[0];
    assert_eq!(registered.name, "greet");
    assert_eq!(registered.description, "Say hi");
    assert_eq!(registered.index, 0);
    assert_eq!(
        registered.input_schema["properties"]["who"]["type"],
        "string"
    );
}

#[rstest::rstest]
#[test]
fn a_map_input_makes_every_field_required() {
    // Given a registration whose input is the map form with two fields.
    let (lua, tool, registration) = tool_vm();
    let spec_table = spec(&lua, &[("name", "t".into()), ("description", "d".into())]);
    let handler = lua
        .load("return function() return {} end")
        .eval::<mlua::Function>()
        .expect("handler");
    let input = lua.create_table().expect("table");
    input.raw_set("who", "string").expect("set");
    input.raw_set("times", "integer").expect("set");
    spec_table.raw_set("input", input).expect("set");

    // When registering.
    tool.call::<()>((spec_table, handler)).expect("registers");

    // Then both fields are properties and both are required.
    let schema = registration.borrow().tools()[0].input_schema.clone();
    assert_eq!(schema["type"], "object");
    let required: Vec<&str> = schema["required"]
        .as_array()
        .expect("required array")
        .iter()
        .map(|v| v.as_str().expect("string"))
        .collect();
    assert_eq!(required, ["times", "who"]);
    assert!(schema["properties"]["who"].get("type").is_some());
    assert!(schema["properties"]["times"].get("type").is_some());
}

#[rstest::rstest]
#[test]
fn a_table_with_a_top_level_type_passes_through_as_a_raw_schema() {
    // Given a raw-form input (top-level `type` key) with an optional field.
    let (lua, tool, registration) = tool_vm();
    let spec_table = spec(&lua, &[("name", "t".into()), ("description", "d".into())]);
    let handler = lua
        .load("return function() return {} end")
        .eval::<mlua::Function>()
        .expect("handler");
    let input = lua
        .load(r#"return { type = "object", properties = { who = { type = "string" } }, required = {} }"#)
        .eval::<mlua::Table>()
        .expect("raw schema");
    spec_table.raw_set("input", input).expect("set");

    // When registering.
    tool.call::<()>((spec_table, handler)).expect("registers");

    // Then the schema rides through verbatim — no synthetic required list.
    // (The author's empty `required` table serializes as an empty object
    // under the json family's empty-table-is-object policy; the point is
    // that nothing was synthesized onto it.)
    let schema = registration.borrow().tools()[0].input_schema.clone();
    assert_eq!(schema["type"], "object");
    assert_eq!(schema["properties"]["who"]["type"], "string");
    assert_eq!(schema.get("required"), Some(&serde_json::json!({})));
}

#[rstest::rstest]
#[test]
fn an_absent_input_yields_an_empty_object_schema() {
    // Given a registration with no `input` field.
    let (lua, tool, registration) = tool_vm();
    let spec_table = spec(&lua, &[("name", "t".into()), ("description", "d".into())]);
    let handler = lua
        .load("return function() return {} end")
        .eval::<mlua::Function>()
        .expect("handler");

    // When registering.
    tool.call::<()>((spec_table, handler)).expect("registers");

    // Then the schema is the no-argument object schema.
    let schema = registration.borrow().tools()[0].input_schema.clone();
    assert_eq!(schema["type"], "object");
    assert_eq!(
        schema["properties"],
        serde_json::Value::Object(serde_json::Map::new())
    );
}

#[rstest::rstest]
#[test]
fn a_missing_description_fails_at_registration() {
    // Given a spec with no description.
    let (lua, tool, _registration) = tool_vm();
    let spec_table = spec(&lua, &[("name", "t".into())]);
    let handler = lua
        .load("return function() return {} end")
        .eval::<mlua::Function>()
        .expect("handler");

    // When registering.
    let result = tool.call::<()>((spec_table, handler));

    // Then the registration errors naming the missing field.
    let error = result.expect_err("missing description");
    assert!(
        format!("{error}").contains("missing the required `description`"),
        "got {error}"
    );
}

#[rstest::rstest]
#[test]
fn a_non_function_handler_fails_at_registration() {
    // Given a second argument that is a table, not a function.
    let (lua, tool, _registration) = tool_vm();
    let spec_table = spec(&lua, &[("name", "t".into()), ("description", "d".into())]);
    let not_a_handler = lua.create_table().expect("table");

    // When registering.
    let result = tool.call::<()>((spec_table, not_a_handler));

    // Then the registration errors naming the function requirement.
    let error = result.expect_err("table is not a function");
    assert!(
        format!("{error}").contains("requires a function"),
        "got {error}"
    );
}

#[rstest::rstest]
#[test]
fn an_unknown_type_name_in_a_map_input_fails_at_registration() {
    // Given a map input naming a type the map form does not accept.
    let (lua, tool, _registration) = tool_vm();
    let spec_table = spec(&lua, &[("name", "t".into()), ("description", "d".into())]);
    let handler = lua
        .load("return function() return {} end")
        .eval::<mlua::Function>()
        .expect("handler");
    let input = lua.create_table().expect("table");
    input.raw_set("who", "sting").expect("set");
    spec_table.raw_set("input", input).expect("set");

    // When registering.
    let result = tool.call::<()>((spec_table, handler));

    // Then the error names the field and the accepted types.
    let error = result.expect_err("typo'd type name");
    let message = format!("{error}");
    assert!(message.contains("`who`"), "got {message}");
    assert!(message.contains("`sting`"), "got {message}");
    assert!(message.contains("string, number"), "got {message}");
}

#[rstest::rstest]
#[test]
fn a_duplicate_tool_name_fails_at_registration() {
    // Given one registered tool.
    let (lua, tool, _registration) = tool_vm();
    let handler = lua
        .load("return function() return {} end")
        .eval::<mlua::Function>()
        .expect("handler");
    let first = spec(&lua, &[("name", "t".into()), ("description", "d".into())]);
    tool.call::<()>((first, handler.clone())).expect("first");

    // When registering a second tool under the same name.
    let second = spec(&lua, &[("name", "t".into()), ("description", "d2".into())]);
    let result = tool.call::<()>((second, handler));

    // Then the duplicate is rejected.
    let error = result.expect_err("duplicate name");
    assert!(
        format!("{error}").contains("duplicate tool name"),
        "got {error}"
    );
}

#[rstest::rstest]
#[test]
fn tools_keep_their_declaration_order_in_the_index() {
    // Given two registrations made in a known order.
    let (lua, tool, registration) = tool_vm();
    let handler = lua
        .load("return function() return {} end")
        .eval::<mlua::Function>()
        .expect("handler");
    let first = spec(
        &lua,
        &[("name", "alpha".into()), ("description", "d".into())],
    );
    let second = spec(
        &lua,
        &[("name", "beta".into()), ("description", "d".into())],
    );

    // When registering alpha then beta.
    tool.call::<()>((first, handler.clone())).expect("alpha");
    tool.call::<()>((second, handler)).expect("beta");

    // Then the indices run in declaration order.
    let tools = registration.borrow().tools().to_vec();
    assert_eq!(
        tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        ["alpha", "beta"]
    );
    assert_eq!(tools[0].index, 0);
    assert_eq!(tools[1].index, 1);
}

#[rstest::rstest]
#[test]
fn a_sealed_registration_refuses_further_tools() {
    // Given a sealed registration (serve activation has drained it).
    let (lua, tool, registration) = tool_vm();
    let handler = lua
        .load("return function() return {} end")
        .eval::<mlua::Function>()
        .expect("handler");
    let first = spec(
        &lua,
        &[("name", "alpha".into()), ("description", "d".into())],
    );
    tool.call::<()>((first, handler.clone())).expect("first");
    let _drained = registration.borrow_mut().seal();

    // When registering another tool.
    let second = spec(
        &lua,
        &[("name", "beta".into()), ("description", "d".into())],
    );
    let result = tool.call::<()>((second, handler));

    // Then the sealed table refuses with the late-registration sentence.
    let error = result.expect_err("sealed");
    assert!(
        format!("{error}").contains("after the server has started serving"),
        "got {error}"
    );
}

#[rstest::rstest]
#[test]
fn plain_run_error_names_the_serve_subcommand() {
    // Given the plain-run failure constructor.
    // When rendering it.
    let error = plain_run_error();

    // Then it names the serve subcommand path.
    let message = format!("{error}");
    assert!(message.contains("jinn luau serve"), "got {message}");
}
