//! The `jinn.tool` capability: register an MCP tool from a serve-mode script.
//!
//! One host function, [`inject`]'s `jinn.tool(spec, fn)`:
//!
//! ```lua
//! jinn.tool({ name = "greet", description = "Say hi",
//!             input = { who = "string" } }, function(args)
//!   return { greeting = "hi " .. args.who }
//! end)
//! ```
//!
//! ## Registration state machine
//!
//! The function appends to a shared [`Registration`] while it is `Idle`
//! (the script body is executing at load time). Serve activation seals the
//! table ([`seal`]); after that, any further `jinn.tool` call is a Lua
//! error. Outside serve mode the registration is never sealed, so the
//! plain-run failure ("pass the script to the serve subcommand") fires
//! instead — loud, not silent: a silently dead registration reads exactly
//! like a bug from the tool author's chair.
//!
//! ## Input schema dialect (the `input` field)
//!
//! - **Map form** — every key becomes a property of the given JSON-schema
//!   primitive type name and **every field is required**. Predictable
//!   requiredness; an author wanting optional fields passes the raw form.
//! - **Raw form** — a table carrying a top-level `type` or `properties`
//!   key passes through verbatim as a JSON schema (arrays, nested
//!   constraints, `$defs` all ride).
//! - **Absent** — the tool takes no arguments (an empty object schema).
//!
//! Conversion errors (missing name/description, duplicate names,
//! non-function handlers, unknown type names) are **registration-time
//! Lua errors** — the server never starts on a partially-loaded script.

use mlua::Lua;
use mlua::Table;
use mlua::Value;

/// The JSON-schema primitive type names the map form accepts.
const PRIMITIVE_TYPES: [&str; 6] = ["string", "number", "integer", "boolean", "array", "object"];

/// The shared registration table: what the script built and whether the
/// serve layer has taken it over.
#[derive(Debug, Default)]
pub struct Registration {
    /// The tools collected so far, in declaration order.
    tools: Vec<RegisteredTool>,
    /// Set by serve activation; a sealed table refuses new entries.
    sealed: bool,
    /// The inert run/eval default: every registration fails loudly
    /// naming the serve subcommand. Serve builds its own table.
    plain_run: bool,
}

impl Registration {
    /// A fresh, unsealed registration (serve mode's shape).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The inert run/eval default: `jinn.tool` fails loudly naming the
    /// serve subcommand, instead of silently collecting tools nobody
    /// will ever serve.
    #[must_use]
    pub fn plain_run() -> Self {
        Self {
            plain_run: true,
            ..Self::default()
        }
    }

    /// The tools in declaration order; serve activation consumes this.
    #[must_use]
    pub fn tools(&self) -> &[RegisteredTool] {
        &self.tools
    }

    /// Drains the collected tools, sealing the table: later `jinn.tool`
    /// calls error instead of appending. Serve activation calls this once.
    pub fn seal(&mut self) -> Vec<RegisteredTool> {
        self.sealed = true;
        std::mem::take(&mut self.tools)
    }

    /// Whether the table has been sealed by serve activation.
    #[must_use]
    pub fn is_sealed(&self) -> bool {
        self.sealed
    }

    /// Appends one tool. A sealed table or the inert plain-run default
    /// refuses.
    fn push(&mut self, tool: RegisteredTool) -> Result<(), String> {
        if self.plain_run {
            return Err(plain_run_error().to_string());
        }
        if self.sealed {
            return Err(
                "jinn.tool cannot register a tool after the server has started serving; \
                 all registrations must run while the script loads"
                    .to_owned(),
            );
        }
        self.tools.push(tool);
        Ok(())
    }
}

/// One collected `jinn.tool` registration.
#[derive(Debug, Clone)]
pub struct RegisteredTool {
    /// The tool's MCP name.
    pub name: String,
    /// The tool's MCP description.
    pub description: String,
    /// The input schema, as JSON (map form converted, raw form verbatim).
    pub input_schema: serde_json::Value,
    /// The declaration position, for the serve layer's stable ordering.
    pub index: usize,
    /// The handler the serve layer invokes per `tools/call`. Lives only
    /// on the VM thread; the wire layer never touches it.
    pub handler: mlua::Function,
}

/// Registers `jinn.tool(spec, fn)` on the `jinn` table.
///
/// The registration state rides a `Rc<RefCell<Registration>>` shared by
/// the closure and the serve layer (script execution is single-threaded;
/// the serve layer reads the table only on the same VM thread, so no
/// locking ever crosses a thread).
///
/// # Errors
///
/// Propagates any registration failure (harness-side, not script-side).
pub fn inject(
    lua: &Lua,
    jinn: &Table,
    registration: &std::rc::Rc<std::cell::RefCell<Registration>>,
) -> mlua::Result<()> {
    let state = std::rc::Rc::clone(registration);
    let tool = lua.create_function(move |_lua, (spec, handler): (Table, Value)| {
        let name = required_string(&spec, "name")?;
        let description = required_string(&spec, "description")?;
        let input = spec.get::<Option<Table>>("input")?;
        let input_schema = input_schema(input, &name)?;
        let Value::Function(handler) = handler else {
            return Err(mlua::Error::runtime(format!(
                "jinn.tool('{name}') requires a function as its second argument"
            )));
        };

        let mut registration = state.borrow_mut();
        if registration.tools.iter().any(|t| t.name == name) {
            return Err(mlua::Error::runtime(format!(
                "jinn.tool('{name}') registers a duplicate tool name; tool names must be unique"
            )));
        }
        let index = registration.tools.len();
        registration
            .push(RegisteredTool {
                name,
                description,
                input_schema,
                index,
                handler,
            })
            .map_err(mlua::Error::runtime)
    })?;
    jinn.raw_set("tool", tool)?;
    Ok(())
}

/// A `jinn.tool` call outside serve mode: the registration was never
/// sealed and never will be, so the plain-run failure fires.
///
/// # Errors
///
/// Returns the loud plain-run error naming the serve subcommand.
pub fn plain_run_error() -> mlua::Error {
    mlua::Error::runtime(
        "jinn.tool registers tools for 'jinn luau serve'; pass the script to the serve subcommand",
    )
}

/// Reads a required string field from a spec table, erroring on absence
/// or wrong type with the tool name where known.
fn required_string(spec: &Table, key: &str) -> mlua::Result<String> {
    let value: Option<Value> = spec.get(key)?;
    match value {
        Some(Value::String(s)) => Ok(s.to_string_lossy()),
        Some(other) => Err(mlua::Error::runtime(format!(
            "jinn.tool spec field `{key}` must be a string, got {}",
            type_name(&other)
        ))),
        None => Err(mlua::Error::runtime(format!(
            "jinn.tool spec is missing the required `{key}` field"
        ))),
    }
}

/// The readable Luau type name of a value, for error messages.
fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Nil => "nil",
        Value::Boolean(_) => "boolean",
        Value::Integer(_) | Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Table(_) => "table",
        Value::Function(_) => "function",
        Value::Thread(_) => "thread",
        _ => "userdata",
    }
}

/// Converts a spec's `input` table into a JSON schema.
///
/// Absent → no-argument schema; a top-level `type`/`properties` key →
/// raw passthrough; otherwise the all-required map form.
///
/// # Errors
///
/// Errors name the tool (`tool_name`) and the offending field.
fn input_schema(input: Option<Table>, tool_name: &str) -> mlua::Result<serde_json::Value> {
    let Some(table) = input else {
        return Ok(no_args_schema());
    };

    // Raw form: a top-level `type` or `properties` key means the author
    // wrote a JSON schema; pass it through verbatim.
    let top_type: Option<Value> = table.get("type")?;
    let properties: Option<Value> = table.get("properties")?;
    if top_type.is_some() || properties.is_some() {
        return raw_schema(&table, tool_name);
    }

    // Map form: every key a required property of the given type name.
    let mut properties_map = serde_json::Map::new();
    let mut required = Vec::new();
    for pair in table.pairs::<String, Value>() {
        let (field, value) = pair?;
        let Value::String(type_name_value) = value else {
            return Err(mlua::Error::runtime(format!(
                "jinn.tool('{tool_name}') input field `{field}` must map to a type name string, \
                 got {} — for per-field options pass a raw JSON schema with a `properties` key",
                type_name(&value)
            )));
        };
        let type_name_str = type_name_value.to_string_lossy();
        if !PRIMITIVE_TYPES.contains(&type_name_str.as_ref()) {
            return Err(mlua::Error::runtime(format!(
                "jinn.tool('{tool_name}') input field `{field}` names the unknown type \
                 `{type_name_str}`; the map form accepts one of: {}",
                PRIMITIVE_TYPES.join(", ")
            )));
        }
        let mut property = serde_json::Map::new();
        property.insert("type".to_owned(), serde_json::Value::String(type_name_str));
        properties_map.insert(field.clone(), serde_json::Value::Object(property));
        required.push(serde_json::Value::String(field));
    }

    let mut schema = serde_json::Map::new();
    schema.insert(
        "type".to_owned(),
        serde_json::Value::String("object".into()),
    );
    schema.insert(
        "properties".to_owned(),
        serde_json::Value::Object(properties_map),
    );
    schema.insert("required".to_owned(), serde_json::Value::Array(required));
    Ok(serde_json::Value::Object(schema))
}

/// Serializes a raw-form schema table through the json converter.
///
/// A table the converter cannot render (a function value, say) is a
/// registration error naming the tool.
fn raw_schema(table: &Table, tool_name: &str) -> mlua::Result<serde_json::Value> {
    // The json family's converter is the single array/object policy; call
    // it through the module rather than re-implementing the heuristic.
    let _ = tool_name;
    crate::caps::json::to_json(&Value::Table(table.clone())).map_err(|error| {
        mlua::Error::runtime(format!(
            "jinn.tool('{tool_name}') input is not a valid JSON schema table: {error}"
        ))
    })
}

/// The schema for a tool that takes no arguments.
fn no_args_schema() -> serde_json::Value {
    let mut schema = serde_json::Map::new();
    schema.insert(
        "type".to_owned(),
        serde_json::Value::String("object".into()),
    );
    schema.insert(
        "properties".to_owned(),
        serde_json::Value::Object(serde_json::Map::new()),
    );
    serde_json::Value::Object(schema)
}

#[cfg(test)]
#[path = "tool_tests.rs"]
mod tool_tests;
