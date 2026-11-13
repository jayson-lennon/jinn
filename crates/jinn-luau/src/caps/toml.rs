//! The `jinn.toml` capability: encode/decode between Lua and TOML.
//!
//! Decode goes through `toml::Value` and is total: every TOML document
//! maps to Lua tables (arrays 1-indexed), and TOML has no null, so decode
//! never produces nil.
//!
//! Encode rejects nil-bearing tables with a message naming the key —
//! TOML cannot represent null, and silently dropping keys (the JSON
//! behavior) would corrupt documents. Datetimes decode as their rendered
//! string and encode as strings, the ordinary scripting shape.

use mlua::Lua;
use mlua::Table;
use mlua::Value;

/// Registers `jinn.toml.encode/decode`.
///
/// # Errors
///
/// Propagates any registration failure (harness-side, not script-side).
pub fn inject(lua: &Lua, jinn: &Table) -> mlua::Result<()> {
    let family = lua.create_table()?;

    family.raw_set(
        "encode",
        lua.create_function(|_lua, value: Value| {
            let doc = to_toml(&value)?;
            toml::to_string(&doc)
                .map_err(|error| mlua::Error::runtime(format!("cannot encode TOML: {error}")))
        })?,
    )?;

    family.raw_set(
        "decode",
        lua.create_function(|lua, text: String| {
            let parsed: toml::Value = text
                .parse()
                .map_err(|error| mlua::Error::runtime(format!("cannot decode TOML: {error}")))?;
            from_toml(lua, &parsed)
        })?,
    )?;

    jinn.raw_set("toml", family)?;
    Ok(())
}

/// Converts a Lua value into a TOML value; nil is rejected, naming the key.
fn to_toml(value: &Value) -> mlua::Result<toml::Value> {
    let doc = match value {
        Value::Boolean(b) => toml::Value::Boolean(*b),
        Value::Integer(i) => toml::Value::Integer(*i),
        Value::Number(n) => toml::Value::Float(*n),
        Value::String(s) => toml::Value::String(s.to_string_lossy()),
        Value::Table(table) => table_to_toml(table)?,
        other => {
            return Err(mlua::Error::runtime(format!(
                "cannot encode a {} as TOML",
                type_name(other)
            )));
        }
    };
    Ok(doc)
}

/// Converts one Lua table; a nil table value is a named-key error.
fn table_to_toml(table: &Table) -> mlua::Result<toml::Value> {
    let mut array_items: Vec<toml::Value> = Vec::new();
    let mut array_len = 0 as mlua::Integer;
    let mut is_array = true;

    for item in table.sequence_values::<Value>() {
        let item = item?;
        if item == Value::Nil {
            return Err(mlua::Error::runtime(
                "cannot encode TOML: the array carries a nil value",
            ));
        }
        array_items.push(to_toml(&item)?);
        array_len += 1;
    }

    for pair in table.pairs::<Value, Value>() {
        let (key, _) = pair?;
        match key {
            Value::Integer(i) if i >= 1 && i <= array_len => {}
            _ => {
                is_array = false;
            }
        }
    }

    if is_array && array_len > 0 && array_items.len() as mlua::Integer == array_len {
        return Ok(toml::Value::Array(array_items));
    }

    let mut map = toml::map::Map::new();
    for pair in table.pairs::<Value, Value>() {
        let (key, value) = pair?;
        let name = match &key {
            Value::String(s) => s.to_string_lossy(),
            Value::Integer(i) => i.to_string(),
            other => {
                return Err(mlua::Error::runtime(format!(
                    "cannot use a {} as a TOML key",
                    type_name(other)
                )));
            }
        };
        if value == Value::Nil {
            return Err(mlua::Error::runtime(format!(
                "cannot encode TOML: the key `{name}` is nil and TOML has no null"
            )));
        }
        map.insert(name, to_toml(&value)?);
    }
    Ok(toml::Value::Table(map))
}

/// Converts a TOML value into a Lua value (arrays 1-indexed; no nil).
fn from_toml(lua: &Lua, doc: &toml::Value) -> mlua::Result<Value> {
    let value = match doc {
        toml::Value::Boolean(b) => Value::Boolean(*b),
        toml::Value::Integer(i) => Value::Integer(*i),
        toml::Value::Float(f) => Value::Number(*f),
        toml::Value::String(s) => Value::String(lua.create_string(s)?),
        toml::Value::Datetime(dt) => Value::String(lua.create_string(dt.to_string())?),
        toml::Value::Array(items) => {
            let table = lua.create_table()?;
            for (index, item) in items.iter().enumerate() {
                table.raw_set(index as mlua::Integer + 1, from_toml(lua, item)?)?;
            }
            Value::Table(table)
        }
        toml::Value::Table(map) => {
            let table = lua.create_table()?;
            for (name, item) in map {
                table.raw_set(name.as_str(), from_toml(lua, item)?)?;
            }
            Value::Table(table)
        }
    };
    Ok(value)
}

/// The readable name of a non-encodable value, for the error message.
fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Function(_) => "function",
        Value::Thread(_) => "thread",
        Value::UserData(_) | Value::LightUserData(_) => "userdata",
        _ => "value",
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unwrap_used,
        reason = "test assertions need expect/panic to state what went wrong"
    )]

    use super::*;

    /// A VM with the toml family registered.
    fn toml_vm() -> (Lua, mlua::Function, mlua::Function) {
        let lua = Lua::new();
        let jinn = lua.create_table().expect("table");
        inject(&lua, &jinn).expect("injects");
        let family: mlua::Table = jinn.get("toml").expect("toml family");
        let encode: mlua::Function = family.get("encode").expect("encode");
        let decode: mlua::Function = family.get("decode").expect("decode");
        (lua, encode, decode)
    }

    #[rstest::rstest]
    #[test]
    fn a_string_keyed_table_encodes_as_a_document() {
        // Given the toml capability and a string-keyed table.
        let (lua, encode, _decode) = toml_vm();
        let table = lua.create_table().expect("table");
        table.raw_set("name", "jinn").expect("set");
        table.raw_set("port", 8080).expect("set");

        // When encoding it.
        let encoded: String = encode.call(Value::Table(table)).expect("encodes");

        // Then TOML key = value lines come back.
        assert!(encoded.contains("name = \"jinn\""), "got {encoded}");
        assert!(encoded.contains("port = 8080"), "got {encoded}");
    }

    #[rstest::rstest]
    #[test]
    fn a_sequential_table_encodes_as_an_array() {
        // Given the toml capability and a 1..n table.
        let (lua, encode, _decode) = toml_vm();
        let table = lua.create_table().expect("table");
        table.raw_set(1, "a").expect("set");
        table.raw_set(2, "b").expect("set");

        // When encoding the array-wrapped table.
        let wrapper = lua.create_table().expect("table");
        wrapper.raw_set("items", table).expect("set");
        let encoded: String = encode.call(Value::Table(wrapper)).expect("encodes");

        // Then a TOML array section comes back (TOML spells top-level
        // arrays only inside a document key).
        assert!(encoded.contains("items = [\"a\", \"b\"]"), "got {encoded}");
    }

    #[rstest::rstest]
    #[test]
    fn a_function_value_is_rejected_as_unencodable() {
        // Given the toml capability and a table holding a function value
        // (the shape a nil would produce via Luau's key-erasure anyway).
        let (lua, encode, _decode) = toml_vm();
        let table = lua.create_table().expect("table");
        table.raw_set("present", 1).expect("set");
        let f = lua.create_function(|_, ()| Ok(())).expect("function");
        table.raw_set("odd", Value::Function(f)).expect("set");

        // When encoding it.
        let result: mlua::Result<String> = encode.call(Value::Table(table));

        // Then the error is the unencodable-value script error.
        let error = result.expect_err("function is unencodable");
        assert!(error.to_string().contains("function"), "got {error}");
    }

    #[rstest::rstest]
    #[test]
    fn decode_maps_a_document_to_tables() {
        // Given the toml capability.
        let (_lua, _encode, decode) = toml_vm();

        // When decoding a small document.
        let value: Table = decode
            .call("name = \"jinn\"\nports = [80, 443]\n")
            .expect("decodes");

        // Then the fields read back: string, and a 1-indexed array.
        let name: String = value.get("name").expect("name");
        let ports: Vec<mlua::Integer> = value
            .get::<Table>("ports")
            .expect("ports")
            .sequence_values()
            .collect::<mlua::Result<_>>()
            .expect("sequence");
        assert_eq!(name, "jinn");
        assert_eq!(ports, vec![80, 443]);
    }

    #[rstest::rstest]
    #[test]
    fn encode_decode_roundtrips() {
        // Given the toml capability and a Lua table.
        let (lua, encode, decode) = toml_vm();
        let table = lua.create_table().expect("table");
        table.raw_set("enabled", true).expect("set");
        table.raw_set("label", "probe").expect("set");

        // When round-tripping through a string.
        let encoded: String = encode.call(Value::Table(table)).expect("encodes");
        let decoded: Table = decode.call(encoded.as_str()).expect("decodes");

        // Then the fields read back.
        let enabled: bool = decoded.get("enabled").expect("enabled");
        let label: String = decoded.get("label").expect("label");
        assert!(enabled);
        assert_eq!(label, "probe");
    }

    #[rstest::rstest]
    #[test]
    fn malformed_toml_is_a_script_error() {
        // Given the toml capability.
        let (_lua, _encode, decode) = toml_vm();

        // When decoding garbage.
        let result: mlua::Result<Value> = decode.call("= not toml =");

        // Then a script-visible error comes back, not a panic.
        assert!(result.is_err());
    }
}
