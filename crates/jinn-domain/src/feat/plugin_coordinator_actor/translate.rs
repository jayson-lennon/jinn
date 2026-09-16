//! Translation from plugin wire types to core domain types.
//!
//! The wire types ([`jinn_plugin_api::PersonaDef`]) are frozen public
//! contract; the domain `Persona` refactors freely. This module is the
//! only place the two meet.

use jinn_plugin_api::PersonaDef;

use crate::feat::persona::Persona;

/// Translates a batch of contributed persona definitions into domain
/// personas.
///
/// Definitions with an empty or whitespace-only `name` are dropped
/// individually — never the whole batch, never the host. An absent wire
/// description becomes the empty string (the domain persona's picker
/// contract). The output preserves input order.
#[must_use]
pub fn personas(defs: &[PersonaDef]) -> Vec<Persona> {
    defs.iter()
        .filter(|d| !d.name.trim().is_empty())
        .map(|d| Persona {
            name: d.name.clone(),
            description: d.description.clone().unwrap_or_default(),
            body: d.body.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::*;

    fn def(name: &str, description: Option<&str>) -> PersonaDef {
        PersonaDef {
            name: name.to_owned(),
            description: description.map(str::to_owned),
            body: "Body text.".to_owned(),
        }
    }

    #[rstest::rstest]
    fn personas_translates_fields() {
        // Given a contributed persona definition with a description.
        // When translating.
        let personas = personas(&[def("coder", Some("Expert coder"))]);

        // Then the domain persona carries the wire fields.
        assert_eq!(personas.len(), 1);
        assert_eq!(personas[0].name, "coder");
        assert_eq!(personas[0].description, "Expert coder");
        assert_eq!(personas[0].body, "Body text.");
    }

    #[rstest::rstest]
    fn personas_maps_missing_description_to_empty() {
        // Given a contributed persona definition without a description.
        // When translating.
        let personas = personas(&[def("minimal", None)]);

        // Then the description is the empty string.
        assert_eq!(personas[0].description, "");
    }

    #[rstest::rstest]
    fn personas_drops_empty_names_individually() {
        // Given a batch with one empty-name and one whitespace-name def.
        // When translating.
        let personas = personas(&[def("", None), def("   ", None), def("kept", None)]);

        // Then only the named def survives, in order.
        assert_eq!(personas.len(), 1);
        assert_eq!(personas[0].name, "kept");
    }

    #[rstest::rstest]
    fn personas_preserves_input_order() {
        // Given an unsorted batch.
        // When translating.
        let personas = personas(&[def("zeta", None), def("alpha", None)]);

        // Then the output order matches the input.
        assert_eq!(personas[0].name, "zeta");
        assert_eq!(personas[1].name, "alpha");
    }
}
