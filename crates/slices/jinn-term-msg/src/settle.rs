//! Output settle detection + key encoding (shared with the kernel's
//! `interactive_term*` tools).

use std::time::Duration;

/// Default quiet window: no output for this long ⇒ treat as settled.
pub const DEFAULT_QUIET_MS: u64 = 400;

/// Default hard cap on the settle wait, even with continuous output.
pub const DEFAULT_MAX_WAIT_MS: u64 = 3000;

/// A `Duration` for the default quiet window.
#[must_use]
pub fn default_quiet() -> Duration {
    Duration::from_millis(DEFAULT_QUIET_MS)
}

/// A `Duration` for the default settle cap.
#[must_use]
pub fn default_max_wait() -> Duration {
    Duration::from_millis(DEFAULT_MAX_WAIT_MS)
}

/// Whether the settle condition is met.
///
/// `quiet_for` is how long no output arrived; `waited` is the total time in
/// this settle wait. Settled when either bound is reached.
#[must_use]
pub fn should_settle(
    quiet_for: Duration,
    waited: Duration,
    quiet: Duration,
    cap: Duration,
) -> bool {
    quiet_for >= quiet || waited >= cap
}

/// The deadline for the quiet window, given the last output instant.
///
/// Encapsulated so the actor's select loop and tests share one definition of
/// "quiet deadline" instead of each recomputing it.
#[must_use]
pub fn quiet_deadline(last_output_at: std::time::Instant, quiet: Duration) -> std::time::Instant {
    last_output_at + quiet
}

/// How input arguments encode to pty bytes.
///
/// Emitted bytes are ordered: `text` verbatim, then each named key, then the
/// trailing `enter`. This mirrors the tool's argument semantics
/// (`interactive_term_send {text?, keys?, enter?}`).
#[must_use]
pub fn encode_input(text: Option<&str>, keys: &[String], enter: bool) -> Vec<u8> {
    let mut out = Vec::new();
    if let Some(text) = text {
        out.extend_from_slice(text.as_bytes());
    }
    for key in keys {
        out.extend_from_slice(&encode_key(key));
    }
    if enter {
        out.push(b'\r');
    }
    out
}

/// Encodes the legacy-xterm byte sequence for a function key `1..=12`.
///
/// F1–F4 use the short SS3 form (`ESC O P`…`ESC O S`); F5+ use `CSI n ~`
/// (xterm codes 15, 17, 18, 19, 20, 21, 23, 24). Numbers outside `1..=12`
/// encode to nothing.
#[must_use]
pub fn fkey_bytes(n: u8) -> Vec<u8> {
    match n {
        1 => vec![0x1b, b'O', b'P'],
        2 => vec![0x1b, b'O', b'Q'],
        3 => vec![0x1b, b'O', b'R'],
        4 => vec![0x1b, b'O', b'S'],
        5..=12 => {
            let code = match n {
                5 => 15,
                6 => 17,
                7 => 18,
                8 => 19,
                9 => 20,
                10 => 21,
                11 => 23,
                _ => 24,
            };
            let mut out = b"\x1b[".to_vec();
            out.extend_from_slice(code.to_string().as_bytes());
            out.push(b'~');
            out
        }
        _ => Vec::new(),
    }
}

/// Encodes one named key to its legacy-xterm byte sequence.
///
/// Recognized names (case-insensitive; `"c-"` prefixes for control):
/// `enter`/`return`, `esc`/`escape`, `tab`, `backspace`, `delete`/`del`,
/// `space`, `up`, `down`, `left`, `right`, `home`, `end`, `pageup`,
/// `pagedown`, `f1`–`f12`, `ctrl+<letter>`/`c-<letter>`,
/// `alt+<key>`/`m-<key>` (ESC prefix), and any single printable character
/// verbatim. Unknown names encode to nothing (empty bytes) so a typo'd key
/// never sends garbage.
#[must_use]
pub fn encode_key(name: &str) -> Vec<u8> {
    let lower = name.trim().to_ascii_lowercase();
    let bytes: &[u8] = match lower.as_str() {
        "enter" | "return" | "\\n" | "\\r" => b"\r",
        "esc" | "escape" => b"\x1b",
        "tab" | "\\t" => b"\t",
        "backspace" => b"\x7f",
        "delete" | "del" => b"\x1b[3~",
        "space" => b" ",
        "up" | "uparrow" => b"\x1b[A",
        "down" | "downarrow" => b"\x1b[B",
        "right" | "rightarrow" => b"\x1b[C",
        "left" | "leftarrow" => b"\x1b[D",
        "home" => b"\x1b[H",
        "end" => b"\x1b[F",
        "pageup" => b"\x1b[5~",
        "pagedown" => b"\x1b[6~",
        _ if lower.starts_with("ctrl+") || lower.starts_with("c-") => return encode_ctrl(&lower),
        _ if lower.starts_with("alt+") || lower.starts_with("m-") => return encode_alt(&lower),
        _ if lower.strip_prefix('f').is_some_and(|digits| {
            !digits.is_empty() && digits.as_bytes().iter().all(u8::is_ascii_digit)
        }) =>
        {
            return {
                let digits = lower.strip_prefix('f').unwrap_or_default();
                digits
                    .bytes()
                    .try_fold(0u32, |acc, d| {
                        acc.checked_mul(10)?.checked_add(u32::from(d - b'0'))
                    })
                    .and_then(|n| u8::try_from(n).ok())
                    .map_or_else(Vec::new, fkey_bytes)
            };
        }
        // Single printable character, sent verbatim (case preserved —
        // `B` must reach a case-sensitive program as capital B).
        _ => {
            let trimmed = name.trim();
            let mut chars = trimmed.chars();
            match (chars.next(), chars.next()) {
                (Some(_), None) if !trimmed.starts_with('\\') => {
                    return trimmed.as_bytes().to_vec();
                }
                _ => {
                    // Literal newline/escape spellings already matched above;
                    // anything else multi-char is unknown → no bytes.
                    return Vec::new();
                }
            }
        }
    };
    bytes.to_vec()
}

/// Encodes `ctrl+<key>` / `c-<key>`: legacy C0 `key & 0x1F`.
fn encode_ctrl(spec: &str) -> Vec<u8> {
    let base = spec
        .strip_prefix("ctrl+")
        .or_else(|| spec.strip_prefix("c-"))
        .unwrap_or(spec);
    let mut chars = base.chars();
    match (chars.next(), chars.next()) {
        (Some(ch), None)
            if ch.is_ascii_alphabetic()
                || ch == '@'
                || ch == '['
                || ch == ']'
                || ch == '\\'
                || ch == '^'
                || ch == '_' =>
        {
            vec![(ch.to_ascii_uppercase() as u8) & 0x1f]
        }
        _ => Vec::new(),
    }
}

/// Encodes `alt+<key>` / `m-<key>`: ESC prefix followed by the key bytes.
fn encode_alt(spec: &str) -> Vec<u8> {
    let base = spec
        .strip_prefix("alt+")
        .or_else(|| spec.strip_prefix("m-"))
        .unwrap_or(spec);
    let mut out = vec![0x1b];
    out.extend_from_slice(&encode_key(base));
    out
}

/// Encodes a [`KeyEvent`] into the bytes a pty program expects.
///
/// The event-path twin of [`encode_key`]: printable characters, C0
/// controls for ctrl-modified letters, the ESC prefix for alt, and the
/// same named-key sequences.
#[must_use]
pub fn encode_key_event(event: &jinn_slices::KeyEvent) -> Vec<u8> {
    use jinn_slices::Key;

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
    use super::{encode_key, encode_key_event};
    use jinn_slices::{Key, KeyEvent, Modifiers};

    /// The user-takeover path and the agent path must agree on f-keys —
    /// both wrap [`fkey_bytes`].
    #[rstest::rstest]
    #[case(1)]
    #[case(4)]
    #[case(5)]
    #[case(12)]
    fn encode_key_event_matches_encode_key_for_f_keys(#[case] n: u8) {
        // Given the same function key on both paths.
        let event = KeyEvent {
            key: Key::F(n),
            modifiers: Modifiers::none(),
        };

        // When encoding via the event path and the name path.
        let from_event = encode_key_event(&event);
        let from_name = encode_key(&format!("f{n}"));

        // Then the byte sequences are identical.
        assert_eq!(from_event, from_name);
        assert!(!from_event.is_empty());
    }

    #[rstest::rstest]
    #[case(Key::Enter, Modifiers::none(), &b"\r"[..])]
    #[case(Key::Esc, Modifiers::none(), b"\x1b")]
    #[case(Key::Up, Modifiers::none(), b"\x1b[A")]
    #[case(Key::F(5), Modifiers::none(), b"\x1b[15~")]
    #[case(Key::F(1), Modifiers::none(), b"\x1bOP")]
    fn encodes_plain_keys_from_events(
        #[case] key: Key,
        #[case] modifiers: Modifiers,
        #[case] expected: &[u8],
    ) {
        // Given a key event.
        let event = KeyEvent { key, modifiers };

        // When encoding it for the pty.
        let bytes = encode_key_event(&event);

        // Then it matches the byte sequence a real terminal sends.
        assert_eq!(bytes, expected);
    }

    #[rstest::rstest]
    fn encodes_ctrl_char_as_c0_control() {
        // Given Ctrl+C.
        let event = KeyEvent {
            key: Key::Char('c'),
            modifiers: Modifiers::ctrl(),
        };

        // When encoding it.
        let bytes = encode_key_event(&event);

        // Then it is the C0 ETX byte.
        assert_eq!(bytes, vec![0x03]);
    }

    #[rstest::rstest]
    fn encodes_alt_char_with_esc_prefix() {
        // Given Alt+X.
        let event = KeyEvent {
            key: Key::Char('x'),
            modifiers: Modifiers::alt(),
        };

        // When encoding it.
        let bytes = encode_key_event(&event);

        // Then it is ESC followed by the key byte.
        assert_eq!(bytes, vec![0x1b, b'x']);
    }

    #[rstest::rstest]
    fn encodes_shift_char_as_uppercase() {
        // Given Shift+G (already normalized to 'G' by the TUI in practice).
        let event = KeyEvent {
            key: Key::Char('g'),
            modifiers: Modifiers::shift(),
        };

        // When encoding it.
        let bytes = encode_key_event(&event);

        // Then the byte is uppercase.
        assert_eq!(bytes, b"G");
    }
}
