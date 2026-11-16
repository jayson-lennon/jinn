//! The `jinn.json` capability: encode/decode between Lua and JSON values.
//!
//! Hand-written converters in both directions (no serde round-trip
//! through a Lua-value adapter), because the array/object ambiguity is a
//! policy decision, not a serialization detail: a Lua table with
//! sequential `1..n` integer keys encodes as a JSON array, anything else
//! as an object; an empty table is `{}` (the ordinary scripting choice —
//! an empty array is spellable as `jinn.json.encode({n=0})`-style only if
//! the schema demands it, so `{}` wins); a `nil` value inside a table
//! erases its key, which is Luau table semantics, not this module's.
//!
//! Decoding maps JSON arrays to 1-indexed tables, objects to string-keyed
//! tables, and null to `nil` — so a decoded document is ordinary Lua to
//! traverse.

use mlua::Lua;
use mlua::Table;
use mlua::Value;

/// Registers `jinn.json.encode/decode`.
///
/// # Errors
///
/// Propagates any registration failure (harness-side, not script-side).
pub fn inject(lua: &Lua, jinn: &Table) -> mlua::Result<()> {
    let family = lua.create_table()?;

    family.raw_set(
        "encode",
        lua.create_function(|_lua, value: Value| {
            serde_json::to_string(&to_json(&value)?)
                .map_err(|error| mlua::Error::runtime(format!("cannot encode JSON: {error}")))
        })?,
    )?;

    family.raw_set(
        "decode",
        lua.create_function(|lua, text: String| {
            let parsed: serde_json::Value = serde_json::from_str(&text)
                .map_err(|error| mlua::Error::runtime(format!("cannot decode JSON: {error}")))?;
            from_json(lua, &parsed)
        })?,
    )?;

    jinn.raw_set("json", family)?;
    Ok(())
}

/// Converts a Lua value into a JSON value, applying the array heuristic.
fn to_json(value: &Value) -> mlua::Result<serde_json::Value> {
    let json = match value {
        Value::Nil => serde_json::Value::Null,
        Value::Boolean(b) => serde_json::Value::Bool(*b),
        Value::Integer(i) => serde_json::Value::from(*i),
        Value::Number(n) => serde_json::Value::from(*n),
        Value::String(s) => serde_json::Value::String(s.to_string_lossy()),
        Value::Table(table) => table_to_json(table)?,
        other => {
            return Err(mlua::Error::runtime(format!(
                "cannot encode a {} as JSON",
                value_type_name(other)
            )));
        }
    };
    Ok(json)
}

/// Converts one Lua table, deciding array-vs-object by its keys.
fn table_to_json(table: &Table) -> mlua::Result<serde_json::Value> {
    let mut sequence = Vec::new();
    let mut is_array = true;
    let mut index = 1 as mlua::Integer;

    for pair in table.sequence_values::<Value>() {
        let value = pair?;
        sequence.push(to_json(&value)?);
        index += 1;
    }

    // A table is an array iff every key 1..=len is present and nothing
    // beyond the sequence exists. Anything else — string keys, gaps,
    // extra keys — is an object.
    for pair in table.pairs::<Value, Value>() {
        let (key, _) = pair?;
        match key {
            Value::Integer(i) if i == index => {}
            Value::Integer(i) if i >= 1 && i < index => {}
            _ => {
                is_array = false;
                break;
            }
        }
    }

    if is_array && index > 1 {
        return Ok(serde_json::Value::Array(sequence));
    }

    let mut object = serde_json::Map::new();
    for pair in table.pairs::<Value, Value>() {
        let (key, value) = pair?;
        let name = match key {
            Value::String(s) => s.to_string_lossy(),
            Value::Integer(i) => i.to_string(),
            other => {
                return Err(mlua::Error::runtime(format!(
                    "cannot use a {} as a JSON object key",
                    value_type_name(&other)
                )));
            }
        };
        object.insert(name, to_json(&value)?);
    }
    Ok(serde_json::Value::Object(object))
}

/// Converts a JSON value into a Lua value (arrays 1-indexed, null nil).
pub(crate) fn from_json(lua: &Lua, json: &serde_json::Value) -> mlua::Result<Value> {
    let value = match json {
        serde_json::Value::Null => Value::Nil,
        serde_json::Value::Bool(b) => Value::Boolean(*b),
        serde_json::Value::Number(n) => n.as_i64().map_or_else(
            || n.as_f64().map_or(Value::Nil, Value::Number),
            Value::Integer,
        ),
        serde_json::Value::String(s) => Value::String(lua.create_string(s)?),
        serde_json::Value::Array(items) => {
            let table = lua.create_table()?;
            for (index, item) in items.iter().enumerate() {
                table.raw_set(index as mlua::Integer + 1, from_json(lua, item)?)?;
            }
            Value::Table(table)
        }
        serde_json::Value::Object(map) => {
            let table = lua.create_table()?;
            for (name, item) in map {
                table.raw_set(name.as_str(), from_json(lua, item)?)?;
            }
            Value::Table(table)
        }
    };
    Ok(value)
}

/// The readable name of a non-encodable value, for the error message.
fn value_type_name(value: &Value) -> &'static str {
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

    /// A VM with the json family registered.
    fn json_vm() -> (Lua, mlua::Function, mlua::Function) {
        let lua = Lua::new();
        let jinn = lua.create_table().expect("table");
        inject(&lua, &jinn).expect("injects");
        let family: mlua::Table = jinn.get("json").expect("json family");
        let encode: mlua::Function = family.get("encode").expect("encode");
        let decode: mlua::Function = family.get("decode").expect("decode");
        (lua, encode, decode)
    }

    #[rstest::rstest]
    #[test]
    fn a_sequential_table_encodes_as_an_array() {
        // Given the json capability and a 1..n table.
        let (lua, encode, _decode) = json_vm();
        let table = lua.create_table().expect("table");
        table.raw_set(1, "a").expect("set");
        table.raw_set(2, "b").expect("set");
        table.raw_set(3, "c").expect("set");

        // When encoding it.
        let encoded: String = encode.call(Value::Table(table)).expect("encodes");

        // Then a JSON array comes back.
        assert_eq!(encoded, r#"["a","b","c"]"#);
    }

    #[rstest::rstest]
    #[test]
    fn a_string_keyed_table_encodes_as_an_object() {
        // Given the json capability and a string-keyed table.
        let (lua, encode, _decode) = json_vm();
        let table = lua.create_table().expect("table");
        table.raw_set("name", "jinn").expect("set");
        table.raw_set("count", 3).expect("set");

        // When encoding it.
        let encoded: String = encode.call(Value::Table(table)).expect("encodes");

        // Then a JSON object comes back with both keys.
        assert!(encoded.contains(r#""name":"jinn""#), "got {encoded}");
        assert!(encoded.contains(r#""count":3"#), "got {encoded}");
    }

    #[rstest::rstest]
    #[test]
    fn an_empty_table_encodes_as_an_empty_object() {
        // Given the json capability and an empty table.
        let (lua, encode, _decode) = json_vm();
        let table = lua.create_table().expect("table");

        // When encoding it.
        let encoded: String = encode.call(Value::Table(table)).expect("encodes");

        // Then an empty object comes back.
        assert_eq!(encoded, "{}");
    }

    #[rstest::rstest]
    #[test]
    fn a_gappy_table_encodes_as_an_object() {
        // Given a table with keys 1 and 3 (a gap at 2).
        let (lua, encode, _decode) = json_vm();
        let table = lua.create_table().expect("table");
        table.raw_set(1, "a").expect("set");
        table.raw_set(3, "c").expect("set");

        // When encoding it.
        let encoded: String = encode.call(Value::Table(table)).expect("encodes");

        // Then an object with stringified keys comes back, not an array.
        assert!(encoded.contains(r#""1":"a""#), "got {encoded}");
        assert!(encoded.contains(r#""3":"c""#), "got {encoded}");
    }

    #[rstest::rstest]
    #[test]
    fn scalars_encode_directly() {
        // Given the json capability.
        let (lua, encode, _decode) = json_vm();

        // When encoding each scalar.
        let int: String = encode.call(Value::Integer(42)).expect("encodes");
        let float: String = encode.call(Value::Number(1.5)).expect("encodes");
        let boolean: String = encode.call(Value::Boolean(true)).expect("encodes");
        let text: String = encode
            .call(Value::String(lua.create_string("hi").expect("str")))
            .expect("encodes");

        // Then each renders as JSON.
        assert_eq!(int, "42");
        assert_eq!(float, "1.5");
        assert_eq!(boolean, "true");
        assert_eq!(text, r#""hi""#);
    }

    #[rstest::rstest]
    #[test]
    fn decode_arrays_are_one_indexed_tables() {
        // Given the json capability.
        let (_lua, _encode, decode) = json_vm();

        // When decoding a JSON array.
        let value: Table = decode.call(r#"["x","y"]"#).expect("decodes");

        // Then the items sit at 1 and 2.
        let first: String = value.get(1).expect("first");
        let second: String = value.get(2).expect("second");
        assert_eq!((first.as_str(), second.as_str()), ("x", "y"));
    }

    #[rstest::rstest]
    #[test]
    fn decode_null_becomes_nil() {
        // Given the json capability.
        let (_lua, _encode, decode) = json_vm();

        // When decoding an object carrying null.
        let value: Table = decode.call(r#"{"a":null,"b":1}"#).expect("decodes");

        // Then the null key reads back nil and the other key survives.
        let a: Option<mlua::Integer> = value.get("a").expect("a");
        let b: mlua::Integer = value.get("b").expect("b");
        assert!(a.is_none());
        assert_eq!(b, 1);
    }

    #[rstest::rstest]
    #[test]
    fn encode_decode_roundtrips_a_nested_document() {
        // Given the json capability and a nested Lua document.
        let (lua, encode, decode) = json_vm();
        let inner = lua.create_table().expect("table");
        inner.raw_set("deep", true).expect("set");
        let outer = lua.create_table().expect("table");
        let _ = outer.raw_set("list", [1, 2, 3].to_vec());
        outer.raw_set("name", "jinn").expect("set");
        outer.raw_set("child", Value::Table(inner)).expect("set");

        // When round-tripping through a string.
        let encoded: String = encode.call(Value::Table(outer)).expect("encodes");
        let decoded: Table = decode.call(encoded.as_str()).expect("decodes");

        // Then the fields read back.
        let name: String = decoded.get("name").expect("name");
        let list: Vec<mlua::Integer> = decoded
            .get::<Table>("list")
            .expect("list")
            .sequence_values()
            .collect::<mlua::Result<_>>()
            .expect("sequence");
        let child: Table = decoded.get("child").expect("child");
        let deep: bool = child.get("deep").expect("deep");
        assert_eq!(name, "jinn");
        assert_eq!(list, vec![1, 2, 3]);
        assert!(deep);
    }

    #[rstest::rstest]
    #[test]
    fn malformed_json_is_a_script_error() {
        // Given the json capability.
        let (_lua, _encode, decode) = json_vm();

        // When decoding garbage.
        let result: mlua::Result<Value> = decode.call("{not json");

        // Then a script-visible error comes back, not a panic.
        assert!(result.is_err());
    }
}
