//! What a hook's run produces — messages to publish plus an optional close.

use jinn_slices::PublishClosure;
use jinn_slices::RouteResult;

/// The outcome of a picker lifecycle hook or bind action.
///
/// Messages are erased publish closures in the exact shape the kernel's
/// drain task already consumes ([`PublishClosure`], minted through
/// [`RouteResult`]'s constructors so this crate needs no kameo dependency).
/// `close` pops the picker's scope after the messages publish.
#[derive(Default)]
pub struct PickerOutcome {
    /// Typed message closures to publish to the kameo bus.
    pub messages: Vec<PublishClosure>,
    /// Type names of the messages, for test inspection.
    pub message_names: Vec<&'static str>,
    /// Whether the picker closes after this hook runs.
    pub close: bool,
}

impl std::fmt::Debug for PickerOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PickerOutcome")
            .field("messages", &self.messages.len())
            .field("message_names", &self.message_names)
            .field("close", &self.close)
            .finish()
    }
}

impl PickerOutcome {
    /// An outcome with no messages and no close.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// An outcome carrying one typed message, keeping the picker open.
    #[must_use]
    pub fn new_message<M>(msg: M) -> Self
    where
        M: Clone + Send + 'static,
    {
        Self::from_route_result(RouteResult::new_message(msg))
    }

    /// Wraps a [`RouteResult`]'s messages into a picker outcome
    /// (open — no scope signal is carried).
    #[must_use]
    pub fn from_route_result(result: RouteResult) -> Self {
        Self {
            messages: result.messages,
            message_names: result.message_names,
            close: false,
        }
    }

    /// Appends a typed message, returning self for chaining.
    #[must_use]
    pub fn with_message<M: Clone + Send + 'static>(mut self, msg: M) -> Self {
        let extra = Self::new_message(msg);
        self.messages.extend(extra.messages);
        self.message_names.extend(extra.message_names);
        self
    }

    /// Marks the picker to close after this hook's messages publish.
    #[must_use]
    pub fn close(mut self) -> Self {
        self.close = true;
        self
    }

    /// Merges another outcome's messages into this one (close wins if
    /// either outcome requests it).
    #[must_use]
    pub fn merge(mut self, other: Self) -> Self {
        self.messages.extend(other.messages);
        self.message_names.extend(other.message_names);
        self.close |= other.close;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[rstest::rstest]
    #[test]
    fn new_message_records_the_message_type() {
        // Given a message outcome.
        let outcome = PickerOutcome::new_message(String::from("hello"));

        // When inspecting the recorded names.
        // Then the message type name is recorded for test inspection.
        assert_eq!(outcome.message_names, ["alloc::string::String"]);
        assert_eq!(outcome.messages.len(), 1);
        assert!(!outcome.close);
    }

    #[rstest::rstest]
    #[test]
    fn merge_combines_messages_and_close_wins() {
        // Given an open outcome and a closing outcome.
        let open = PickerOutcome::new_message(String::from("a"));
        let closing = PickerOutcome::empty().close();

        // When merging them.
        let merged = open.merge(closing);

        // Then the messages combine and the close wins.
        assert_eq!(merged.message_names.len(), 1);
        assert!(merged.close);
    }
}
