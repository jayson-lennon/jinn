//! The capture-mode key hook — every unbound key in `term:control`
//! forwards to the terminal.
//!
//! Composition binds this hook as the scope's *catch-all*: it is consulted
//! only when no explicit binding matched, so the configured control-toggle
//! row keeps priority (bindings beat catch-alls) and capture stays
//! hermetic — no composition chrome pierces the mode because key-hook
//! scopes are excluded from the global-toggle spread and the typing
//! carve-out.
//!
//! The hook encodes each key to the bytes a PTY program expects and
//! wraps them in the `send-key` dynamic intent; the route table delivers
//! the intent to its row, which publishes `SendTermKey` with the bytes.
//! Unencodable keys encode to nothing and are dropped.

use jinn_slices::SliceScopeId;
use jinn_slices::route::{DynamicIntent, KeyHook, KeyRoutes};
use jinn_term_msg::control_scope;

/// Builds the capture hook for `term:control`.
///
/// Encodes the raw [`KeyEvent`] via the shared PTY encoder and returns
/// the byte-carrying `send-key` intent; `None` when the key encodes to
/// nothing (a typo'd key never sends garbage).
#[must_use]
pub fn send_key_hook() -> KeyHook {
    std::sync::Arc::new(|event: &jinn_slices::KeyEvent| {
        let bytes = jinn_term_msg::settle::encode_key_event(event);
        if bytes.is_empty() {
            return None;
        }
        Some(DynamicIntent::with_bytes(
            control_scope(),
            "send-key",
            "send key to terminal",
            bytes,
        ))
    })
}

/// Registers the capture hook under the control scope on `routes`.
pub fn register(routes: &KeyRoutes) {
    routes.register_key_hook(&control_scope_id(), send_key_hook());
}

/// The scope id the hook serves (re-exported for wiring/tests).
#[must_use]
pub fn control_scope_id() -> SliceScopeId {
    control_scope()
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

    use super::control_scope_id;
    use super::register;
    use super::send_key_hook;
    use jinn_slices::Key;
    use jinn_slices::KeyEvent;
    use jinn_slices::KeyRoutes;
    use jinn_slices::Modifiers;

    #[rstest::rstest]
    #[case(Key::Char('x'), Modifiers::none(), b"x"[..].to_vec())]
    #[case(Key::Char('c'), Modifiers::ctrl(), vec![0x03])]
    #[case(Key::Enter, Modifiers::none(), b"\r".to_vec())]
    #[case(Key::F(4), Modifiers::none(), b"\x1bOS".to_vec())]
    fn hook_encodes_keys_to_pty_bytes(
        #[case] key: Key,
        #[case] modifiers: Modifiers,
        #[case] expected: Vec<u8>,
    ) {
        // Given the capture hook.
        let hook = send_key_hook();

        // When encoding a raw key event.
        let intent = hook(&KeyEvent { key, modifiers }).expect("encodable keys produce an intent");

        // Then the intent targets the control scope's send-key action and
        // carries the PTY bytes.
        assert_eq!(intent.slice, control_scope_id());
        assert_eq!(intent.action, "send-key");
        assert_eq!(intent.bytes, expected);
    }

    #[rstest::rstest]
    fn hook_drops_unencodable_keys() {
        // Given the capture hook and a key the encoder produces nothing
        // for (f-key numbers outside 1..=12).
        let hook = send_key_hook();
        let unencodable = KeyEvent {
            key: Key::F(13),
            modifiers: Modifiers::none(),
        };
        assert!(jinn_term_msg::settle::encode_key_event(&unencodable).is_empty());

        // When encoding it.
        let intent = hook(&unencodable);

        // Then the key is dropped, not mangled.
        assert!(intent.is_none());
    }

    #[rstest::rstest]
    fn register_attaches_the_hook_under_the_control_scope() {
        // Given an empty route table.
        let routes = KeyRoutes::new();

        // When registering the capture hook.
        register(&routes);

        // Then the hook resolves by the navigation scope id — no string
        // roundtrip (the previous attempt's blocker).
        let hook = routes
            .key_hook(&control_scope_id())
            .expect("hook registered for the control scope");
        let intent = hook(&KeyEvent {
            key: Key::Char('a'),
            modifiers: Modifiers::none(),
        });
        assert!(intent.is_some());
        // And the control scope is enumerated as a key-hook scope.
        assert_eq!(routes.key_hook_scopes(), vec![control_scope_id()]);
    }
}
