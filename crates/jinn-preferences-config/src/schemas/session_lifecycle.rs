//! Session lifecycle configuration schema — the `jinn.toml`
//! `[[session_lifecycle]]` entries plus the [`LifecycleCommand`] serde
//! shell-or-builtin encoding.
//!
//! Pure serde data: the *handler registry* ([`BuiltinRegistry`] in the
//! kernel) that resolves [`BuiltinId`] at run time stays in the kernel's
//! session-lifecycle feature — command parsing needs no registry lookup,
//! so the data/behavior split is safe.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// Identifies a compiled-in lifecycle handler.
///
/// Used as the discriminant in [`LifecycleCommand::Builtin`] to look up
/// a registered handler in the builtin registry.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BuiltinId(pub String);

impl fmt::Display for BuiltinId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A lifecycle command - either a shell string or a builtin handler reference.
///
/// # Serde behavior
///
/// - `Shell("echo /tmp")` serializes as `"echo /tmp"` (bare string)
/// - `Builtin(BuiltinId("hello-world"))` serializes as `{ builtin = "hello-world" }`
///
/// This ensures backward compatibility with existing `jinn.toml` configs
/// that use bare strings for `setup_command` and `teardown_command`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LifecycleCommand {
    /// A shell command to execute via `$SHELL -c`.
    Shell(String),
    /// A reference to a compiled-in lifecycle handler.
    Builtin(BuiltinId),
}

impl Serialize for LifecycleCommand {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Shell(s) => serializer.serialize_str(s),
            Self::Builtin(id) => {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("builtin", &id.0)?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for LifecycleCommand {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de::{self, MapAccess, Visitor};

        /// Helper struct for deserializing the `{ builtin = "name" }` form.
        #[expect(dead_code, reason = "field accessed by serde deserialization")]
        #[derive(Deserialize)]
        struct BuiltinForm {
            builtin: String,
        }

        enum Field {
            Builtin,
        }

        impl<'de> Deserialize<'de> for Field {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                struct FieldVisitor;

                impl Visitor<'_> for FieldVisitor {
                    type Value = Field;

                    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                        formatter.write_str("`builtin`")
                    }

                    fn visit_str<E>(self, v: &str) -> Result<Field, E>
                    where
                        E: de::Error,
                    {
                        match v {
                            "builtin" => Ok(Field::Builtin),
                            _ => Err(de::Error::unknown_field(v, &["builtin"])),
                        }
                    }
                }

                deserializer.deserialize_identifier(FieldVisitor)
            }
        }

        struct LifecycleCommandVisitor;

        impl<'de> Visitor<'de> for LifecycleCommandVisitor {
            type Value = LifecycleCommand;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a string or { builtin = \"name\" }")
            }

            fn visit_str<E>(self, v: &str) -> Result<LifecycleCommand, E>
            where
                E: de::Error,
            {
                Ok(LifecycleCommand::Shell(v.to_owned()))
            }

            fn visit_string<E>(self, v: String) -> Result<LifecycleCommand, E>
            where
                E: de::Error,
            {
                Ok(LifecycleCommand::Shell(v))
            }

            fn visit_map<A>(self, mut map: A) -> Result<LifecycleCommand, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut builtin_id: Option<String> = None;
                while let Some(key) = map.next_key::<Field>()? {
                    match key {
                        Field::Builtin => {
                            if builtin_id.is_some() {
                                return Err(de::Error::duplicate_field("builtin"));
                            }
                            builtin_id = Some(map.next_value()?);
                        }
                    }
                }
                let id = builtin_id.ok_or_else(|| de::Error::missing_field("builtin"))?;
                Ok(LifecycleCommand::Builtin(BuiltinId(id)))
            }
        }

        deserializer.deserialize_any(LifecycleCommandVisitor)
    }
}

/// A named session lifecycle recipe — paired setup and teardown commands.
///
/// Defined in `jinn.toml` under `[[session_lifecycle]]`. The setup command
/// runs when creating a new session; the teardown command runs when closing it.
/// Commands may contain positional parameters (`$1`, `$2`) that are collected
/// from the user before execution.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionLifecycle {
    /// Human-readable name shown in the lifecycle picker.
    pub name: String,
    /// Optional description shown below the name in the picker.
    #[serde(default)]
    pub description: Option<String>,
    /// Command to run when creating a session. Last line of stdout becomes the CWD.
    /// May contain `$1`, `$2` positional args. `None` means no setup (blank lifecycle).
    ///
    /// Supports both shell commands and builtin handlers.
    /// See [`LifecycleCommand`] for details.
    #[serde(rename = "setup_command", default)]
    pub setup: Option<LifecycleCommand>,
    /// Command to run when closing a session. Receives the same args as setup.
    /// `None` means no teardown needed.
    ///
    /// Supports both shell commands and builtin handlers.
    /// See [`LifecycleCommand`] for details.
    #[serde(rename = "teardown_command", default)]
    pub teardown: Option<LifecycleCommand>,
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        reason = "test code"
    )]

    use super::*;

    #[derive(Serialize, Deserialize)]
    struct CommandWrapper {
        cmd: LifecycleCommand,
    }

    #[derive(Deserialize)]
    struct SetupCommandWrapper {
        setup_command: LifecycleCommand,
    }

    #[rstest::rstest]
    #[test]
    fn shell_serializes_as_bare_string() {
        // Given a Shell command.
        let cmd = LifecycleCommand::Shell("echo /tmp".to_owned());

        // When serializing to TOML (through a wrapper for valid TOML document).
        let toml_str = toml::to_string(&CommandWrapper { cmd }).expect("serialize");

        // Then it's a bare string.
        assert!(toml_str.contains("echo /tmp"));
    }

    #[rstest::rstest]
    #[test]
    fn builtin_serializes_as_map() {
        // Given a Builtin command.
        let cmd = LifecycleCommand::Builtin(BuiltinId("hello-world".to_owned()));

        // When serializing to TOML (through a wrapper for valid TOML document).
        let toml_str = toml::to_string(&CommandWrapper { cmd }).expect("serialize");

        // Then it's { builtin = "hello-world" }.
        assert!(toml_str.contains("builtin"));
        assert!(toml_str.contains("hello-world"));
    }

    #[rstest::rstest]
    #[test]
    fn bare_string_deserializes_as_shell() {
        // Given a bare string TOML value (wrapped in a table for valid TOML).
        let toml_str = r#"setup_command = "echo /tmp""#;

        // When deserializing through a wrapper.
        let wrapper: SetupCommandWrapper = toml::from_str(toml_str).expect("deserialize");

        // Then it's a Shell variant.
        assert_eq!(
            wrapper.setup_command,
            LifecycleCommand::Shell("echo /tmp".to_owned())
        );
    }

    #[rstest::rstest]
    #[test]
    fn builtin_map_deserializes_as_builtin() {
        // Given a { builtin = "name" } TOML value (wrapped in a table for valid TOML).
        let toml_str = r#"setup_command = { builtin = "hello-world" }"#;

        // When deserializing through a wrapper.
        let wrapper: SetupCommandWrapper = toml::from_str(toml_str).expect("deserialize");

        // Then it's a Builtin variant.
        assert_eq!(
            wrapper.setup_command,
            LifecycleCommand::Builtin(BuiltinId("hello-world".to_owned()))
        );
    }

    #[rstest::rstest]
    #[test]
    fn roundtrip_shell() {
        // Given a Shell command.
        let original = LifecycleCommand::Shell("echo /tmp".to_owned());

        // When serializing and deserializing through a wrapper.
        let toml_str = toml::to_string(&CommandWrapper {
            cmd: original.clone(),
        })
        .expect("serialize");
        let restored: CommandWrapper = toml::from_str(&toml_str).expect("deserialize");

        // Then it matches the original.
        assert_eq!(restored.cmd, original);
    }

    #[rstest::rstest]
    #[test]
    fn roundtrip_builtin() {
        // Given a Builtin command.
        let original = LifecycleCommand::Builtin(BuiltinId("hello-world".to_owned()));

        // When serializing and deserializing through a wrapper.
        let toml_str = toml::to_string(&CommandWrapper {
            cmd: original.clone(),
        })
        .expect("serialize");
        let restored: CommandWrapper = toml::from_str(&toml_str).expect("deserialize");

        // Then it matches the original.
        assert_eq!(restored.cmd, original);
    }

    #[rstest::rstest]
    #[test]
    fn builtin_id_display_outputs_inner_string() {
        // Given a BuiltinId.
        let id = BuiltinId("hello-world".to_owned());

        // When displaying.
        let displayed = format!("{id}");

        // Then the inner string is shown (not empty).
        assert_eq!(displayed, "hello-world");
    }

    #[rstest::rstest]
    #[test]
    fn deserialize_invalid_field_returns_error() {
        // Given TOML with an unknown field in a builtin map.
        let toml_str = r#"setup_command = { unknown = "hello" }"#;

        // When deserializing.
        let result: Result<SetupCommandWrapper, _> = toml::from_str(toml_str);

        // Then an error is returned (triggers the `expecting` message in FieldVisitor).
        assert!(result.is_err());
    }

    #[rstest::rstest]
    #[test]
    fn deserialize_non_string_non_map_returns_error() {
        // Given TOML where the command is a number (not a string or map).
        let toml_str = "setup_command = 42";

        // When deserializing.
        let result: Result<SetupCommandWrapper, _> = toml::from_str(toml_str);

        // Then an error is returned (triggers the `expecting` message in LifecycleCommandVisitor).
        assert!(result.is_err());
    }
}
