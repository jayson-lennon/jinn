//! The dictation slice's shared cell vocabulary: the lifecycle state and
//! the live meter level, each beside its slot key.
//!
//! Both payloads live in this crossing crate rather than in the slice
//! crate for the same reason every other cell payload does: the shared
//! cell catalog names the slots without depending on a slice
//! implementation, and the kernel-resident route action reads the state
//! cell (to decide what the next alt-i press means) without pulling in
//! the actor.
//!
//! Writers: the dictation actor owns the state machine and is the sole
//! writer of both cells. The route action reads the state cell; the meter
//! draw function reads both.

pub mod message;
pub mod state;

pub use message::DictationLifecycleTick;
pub use message::DictationToggle;
pub use state::{DictationLevel, DictationPhase, DictationState, dictation_level_slot, dictation_state_slot, dictation_scope};
