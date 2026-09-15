//! Picker kind — shared vocabulary re-exported from `jinn-slices`.

pub use jinn_slices::picker_kind::PickerKind;

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, reason = "test code")]
    use super::*;

    #[rstest::rstest]
    #[test]
    fn reasoning_effort_displays_as_reasoning_effort() {
        // Given the ReasoningEffort picker kind.
        // When displaying.
        // Then it renders as 'reasoning effort'.
        assert_eq!(PickerKind::ReasoningEffort.to_string(), "reasoning effort");
    }

    #[rstest::rstest]
    #[test]
    fn endpoint_displays_as_endpoints() {
        // Given the Endpoint picker kind.
        // When displaying.
        // Then it renders as 'endpoints'.
        assert_eq!(PickerKind::Endpoint.to_string(), "endpoints");
    }

    #[rstest::rstest]
    #[test]
    fn plugin_displays_as_plugins() {
        // Given the Plugin picker kind.
        // When displaying.
        // Then it renders as 'plugins'.
        assert_eq!(PickerKind::Plugin.to_string(), "plugins");
    }
}
