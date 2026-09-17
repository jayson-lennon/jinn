//! Vocabulary strings injected into tool outputs as model-facing notices.

/// Injected into `interactive_term` tool output when the user takes control
/// of the terminal overlay. This is a soft decision: it instructs the model
/// to stop and wait — there is no programmatic enforcement.
pub const USER_HAS_CONTROL_NOTICE: &str = "NOTE: The user has taken control of this terminal. \
     Stop your current response and wait for the user to finish; \
     they will hand the terminal back with a screen update.";
