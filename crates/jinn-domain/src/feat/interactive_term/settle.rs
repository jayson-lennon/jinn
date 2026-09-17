//! The `KeyEvent`-tied key encoder stays kernel-side (it needs the
//! kernel's [`crate::protocol::KeyEvent`]); the rest of settle logic
//! lives in `jinn-term-msg`.

pub use jinn_term_msg::settle::*;

/// Encodes a platform [`KeyEvent`](crate::protocol::key::KeyEvent) into
/// the bytes a pty program expects.
#[must_use]
pub fn encode_key_event(event: &crate::protocol::key::KeyEvent) -> Vec<u8> {
    use crate::protocol::key::Key;

    let m = event.modifiers;
    let byte_for_char = |c: char| -> Vec<u8> {
        let mut bytes = c.to_string().into_bytes();
        match (m.ctrl, m.shift) {
            // Ctrl produces C0 controls; letters map A..=Z & 0x1F.
            (true, _) => {
                if let Some(b) = bytes.first_mut() {
                    *b = b.to_ascii_uppercase() & 0x1f;
                }
            }
            (false, true) => {
                for b in &mut bytes {
                    *b = b.to_ascii_uppercase();
                }
            }
            (false, false) => {}
        }
        if m.alt {
            let mut out = vec![0x1b];
            out.extend_from_slice(&bytes);
            return out;
        }
        bytes
    };

    let plain: &[u8] = match event.key {
        Key::Char(c) => return byte_for_char(c),
        Key::Enter => b"\r",
        Key::Esc => b"\x1b",
        Key::Tab => b"\t",
        Key::Backspace => b"\x7f",
        Key::Delete => b"\x1b[3~",
        Key::Up => b"\x1b[A",
        Key::Down => b"\x1b[B",
        Key::Right => b"\x1b[C",
        Key::Left => b"\x1b[D",
        Key::Home => b"\x1b[H",
        Key::End => b"\x1b[F",
        Key::PageUp => b"\x1b[5~",
        Key::PageDown => b"\x1b[6~",
        Key::F(n) => {
            // F1–F4 use the short SS3 form; F5+ use `CSI n ~`.
            return fkey_bytes(n);
        }
    };
    plain.to_vec()
}

#[cfg(test)]
mod key_event_tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::*;
    use crate::protocol::key::{Key, KeyEvent, Modifiers};
    use jinn_term_msg::settle::{encode_input, encode_key, should_settle};
    use std::time::Duration;

    #[rstest::rstest]
    #[case("enter", b"\r")]
    #[case("return", b"\r")]
    #[case("esc", b"\x1b")]
    #[case("escape", b"\x1b")]
    #[case("tab", b"\t")]
    #[case("backspace", b"\x7f")]
    #[case("delete", b"\x1b[3~")]
    #[case("space", b" ")]
    #[case("up", b"\x1b[A")]
    #[case("down", b"\x1b[B")]
    #[case("right", b"\x1b[C")]
    #[case("left", b"\x1b[D")]
    #[case("home", b"\x1b[H")]
    #[case("end", b"\x1b[F")]
    #[case("pageup", b"\x1b[5~")]
    #[case("pagedown", b"\x1b[6~")]
    #[case("ctrl+c", &[0x03])]
    #[case("ctrl+d", &[0x04])]
    #[case("c-z", &[0x1a])]
    #[case("ctrl+a", &[0x01])]
    #[case("alt+x", b"\x1bx")]
    #[case("m-x", b"\x1bx")]
    #[case("q", b"q")]
    #[case("A", b"A")]
    #[case("a", b"a")]
    fn encode_key_maps_named_keys_to_xterm_bytes(#[case] name: &str, #[case] expected: &[u8]) {
        // Given a named key.
        // When encoding.
        let bytes = encode_key(name);
        // Then the legacy xterm sequence is produced.
        assert_eq!(bytes, expected, "encoding of {name:?}");
    }

    #[rstest::rstest]
    fn encode_key_is_case_insensitive_for_names() {
        // Given an upper-case key name.
        // When encoding.
        let bytes = encode_key("ENTER");
        // Then it matches the lower-case encoding.
        assert_eq!(bytes, b"\r");
    }

    #[rstest::rstest]
    fn unknown_key_name_encodes_to_nothing() {
        // Given a nonsense key name.
        // When encoding.
        let bytes = encode_key("frobnicate");
        // Then no bytes are produced (a typo never sends garbage).
        assert!(bytes.is_empty());
    }

    #[rstest::rstest]
    fn encode_input_orders_text_then_keys_then_enter() {
        // Given text, keys, and the enter flag.
        // When encoding all three.
        let bytes = encode_input(Some("ls -la"), &["tab".to_owned()], true);

        // Then the order is text, key, newline.
        assert_eq!(bytes, b"ls -la\t\r");
    }

    #[rstest::rstest]
    fn encode_input_with_only_text_sends_verbatim() {
        // Given only text.
        let bytes = encode_input(Some("pwd"), &[], false);
        // Then it is sent byte-for-byte.
        assert_eq!(bytes, b"pwd");
    }

    #[rstest::rstest]
    fn encode_input_with_nothing_sends_nothing() {
        // Given no inputs.
        let bytes = encode_input(None, &[], false);
        // Then nothing is sent (pure screen sync).
        assert!(bytes.is_empty());
    }

    #[rstest::rstest]
    fn should_settle_after_quiet_window() {
        // Given output stopped longer than the quiet window.
        let quiet_for = Duration::from_millis(500);
        let quiet = Duration::from_millis(400);
        let cap = Duration::from_secs(10);
        let waited = Duration::from_millis(600);

        // When checking the settle condition.
        // Then it is settled via the quiet bound.
        assert!(should_settle(quiet_for, waited, quiet, cap));
    }

    #[rstest::rstest]
    fn should_not_settle_while_output_is_fresh() {
        // Given output arrived 100ms ago (within the quiet window) and the
        // cap is far away.
        let quiet_for = Duration::from_millis(100);
        let quiet = Duration::from_millis(400);
        let cap = Duration::from_secs(10);
        let waited = Duration::from_millis(200);

        // Then it is not settled.
        assert!(!should_settle(quiet_for, waited, quiet, cap));
    }

    #[rstest::rstest]
    fn cap_wins_against_continuously_animating_program() {
        // Given output arriving constantly (never quiet) but the total wait
        // exceeded the cap (htop-style animation).
        let quiet_for = Duration::ZERO;
        let quiet = Duration::from_millis(400);
        let cap = Duration::from_secs(3);
        let waited = Duration::from_millis(3100);

        // Then it is settled via the cap.
        assert!(should_settle(quiet_for, waited, quiet, cap));
    }

    #[rstest::rstest]
    fn quiet_deadline_is_last_output_plus_quiet() {
        // Given a last-output instant.
        let last = std::time::Instant::now();
        let quiet = Duration::from_millis(400);

        // When computing the quiet deadline.
        let deadline = quiet_deadline(last, quiet);

        // Then it is exactly one quiet window after the last output.
        assert_eq!(deadline.duration_since(last), quiet);
    }

    /// Agent-path function keys must produce the same bytes a real terminal
    /// sends: SS3 for F1–F4, `CSI n ~` with xterm codes for F5–F12. htop's
    /// F4 filter (and every other program) depends on these exact bytes.
    #[rstest::rstest]
    #[case("f1", &b"\x1bOP"[..])]
    #[case("f2", b"\x1bOQ")]
    #[case("f3", b"\x1bOR")]
    #[case("f4", b"\x1bOS")]
    #[case("f5", b"\x1b[15~")]
    #[case("f6", b"\x1b[17~")]
    #[case("f7", b"\x1b[18~")]
    #[case("f8", b"\x1b[19~")]
    #[case("f9", b"\x1b[20~")]
    #[case("f10", b"\x1b[21~")]
    #[case("f11", b"\x1b[23~")]
    #[case("f12", b"\x1b[24~")]
    fn encode_key_sends_function_key_bytes(#[case] name: &str, #[case] expected: &[u8]) {
        // Given the named function key.
        // When encoding it for the pty.
        let bytes = encode_key(name);

        // Then it produces the legacy-xterm sequence.
        assert_eq!(bytes, expected);
    }

    /// The user-takeover path and the agent path must agree on f-keys —
    /// both wrap [`fkey_bytes`].
    #[rstest::rstest]
    #[case(1)]
    #[case(4)]
    #[case(5)]
    #[case(12)]
    fn encode_key_event_matches_encode_key_for_f_keys(#[case] n: u8) {
        // Given the same function key on both paths.
        let event = crate::protocol::key::KeyEvent {
            key: crate::protocol::key::Key::F(n),
            modifiers: crate::protocol::key::Modifiers::none(),
        };

        // When encoding via the event path and the name path.
        let from_event = encode_key_event(&event);
        let from_name = encode_key(&format!("f{n}"));

        // Then the byte sequences are identical.
        assert_eq!(from_event, from_name);
        assert!(!from_event.is_empty());
    }

    /// Out-of-range f-key numbers encode to nothing (never garbage).
    #[rstest::rstest]
    fn encode_key_ignores_out_of_range_function_keys() {
        // Given f-key names outside 1..=12.
        // When encoding them.
        // Then nothing is sent.
        assert!(encode_key("f0").is_empty());
        assert!(encode_key("f13").is_empty());
        assert!(encode_key("f99999999999999999999").is_empty());
    }

    #[rstest::rstest]
    #[case(crate::protocol::key::Key::Enter, crate::protocol::key::Modifiers::none(), &b"\r"[..])]
    #[case(
        crate::protocol::key::Key::Esc,
        crate::protocol::key::Modifiers::none(),
        b"\x1b"
    )]
    #[case(
        crate::protocol::key::Key::Up,
        crate::protocol::key::Modifiers::none(),
        b"\x1b[A"
    )]
    #[case(
        crate::protocol::key::Key::F(5),
        crate::protocol::key::Modifiers::none(),
        b"\x1b[15~"
    )]
    #[case(
        crate::protocol::key::Key::F(1),
        crate::protocol::key::Modifiers::none(),
        b"\x1bOP"
    )]
    fn encodes_plain_keys_from_events(
        #[case] key: crate::protocol::key::Key,
        #[case] modifiers: crate::protocol::key::Modifiers,
        #[case] expected: &[u8],
    ) {
        // Given a key event.
        let event = crate::protocol::key::KeyEvent { key, modifiers };

        // When encoding it for the pty.
        let bytes = encode_key_event(&event);

        // Then it matches the byte sequence a real terminal sends.
        assert_eq!(bytes, expected);
    }

    #[rstest::rstest]
    fn encodes_ctrl_char_as_c0_control() {
        // Given Ctrl+C.
        let event = crate::protocol::key::KeyEvent {
            key: crate::protocol::key::Key::Char('c'),
            modifiers: crate::protocol::key::Modifiers::ctrl(),
        };

        // When encoding it.
        let bytes = encode_key_event(&event);

        // Then it is the C0 ETX byte.
        assert_eq!(bytes, vec![0x03]);
    }

    #[rstest::rstest]
    fn encodes_alt_char_with_esc_prefix() {
        // Given Alt+X.
        let event = crate::protocol::key::KeyEvent {
            key: crate::protocol::key::Key::Char('x'),
            modifiers: crate::protocol::key::Modifiers::alt(),
        };

        // When encoding it.
        let bytes = encode_key_event(&event);

        // Then it is ESC followed by the key byte.
        assert_eq!(bytes, vec![0x1b, b'x']);
    }

    #[rstest::rstest]
    fn encodes_shift_char_as_uppercase() {
        // Given Shift+G (already normalized to 'G' by the TUI in practice).
        let event = crate::protocol::key::KeyEvent {
            key: crate::protocol::key::Key::Char('g'),
            modifiers: crate::protocol::key::Modifiers::shift(),
        };

        // When encoding it.
        let bytes = encode_key_event(&event);

        // Then the byte is uppercase.
        assert_eq!(bytes, b"G");
    }
}
