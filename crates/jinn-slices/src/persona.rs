//! Persona vocabulary shared across slices and the kernel.
//!
//! [`Persona`] is a plain parsed-prompt record with no cell or slot of its own.
//! It is read by the context assembler, the session actor, the persona picker
//! spec, the install seeder, and the preferences config -- crates in four
//! different families -- so it lives in shared vocabulary rather than in the
//! persona slice's own message crate, which the context assembler must not
//! depend on.
//!
//! The persona slice's cell payload ([`Personas`], [`Personas::normalize_name`],
//! [`personas_slot`]) is genuinely slice state and stays in `jinn-persona-msg`.

/// A parsed persona ready for use in the system prompt.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Persona {
    /// Unique persona name (from frontmatter).
    pub name: String,
    /// Short description for the picker UI.
    pub description: String,
    /// The persona body - the actual system prompt text.
    pub body: String,
}

#[cfg(test)]
mod tests {
    use super::Persona;

    #[rstest::rstest]
    fn persona_roundtrips_through_serde() {
        // Given a persona.
        let original = Persona {
            name: "reviewer".to_owned(),
            description: "reviews code".to_owned(),
            body: "You review code.".to_owned(),
        };

        // When it is serialized and read back.
        let json = serde_json::to_string(&original).expect("serializes");
        let restored: Persona = serde_json::from_str(&json).expect("deserializes");

        // Then every field survives the roundtrip.
        assert_eq!(restored.name, "reviewer");
        assert_eq!(restored.description, "reviews code");
        assert_eq!(restored.body, "You review code.");
    }
}
