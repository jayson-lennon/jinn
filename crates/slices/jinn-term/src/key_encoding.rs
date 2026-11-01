//! Key encoding: turning named keys and key events into the byte sequences a
//! terminal expects.
//!
//! This is a terminfo-family concern, not a message concern. It encodes a
//! function key three ways depending on what the terminal advertises — `SS3`
//! for the unmodified navigational keys, a `CSI` sequence with a modifier
//! parameter for anything shifted or combined, and a literal control byte for
//! the ASCII controls — and the choice between them has to agree with the
//! terminal's advertised capability, which the tests check against a real
//! `terminfo` entry where one is available.
//!
//! It lives here rather than beside the message types that travel to the actor
//! because encoding needs to know what the pty in this process actually is,
//! and that is the one thing a contract crate cannot see.

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
/// Recognized names (case-insensitive): `enter`/`return`, `esc`/`escape`,
/// `tab`, `backspace`, `delete`/`del`, `space`, `up`, `down`, `left`,
/// `right`, `home`, `end`, `pageup`, `pagedown`, `f1`–`f12`, any single
/// printable character verbatim, and any of the above with `s-`/`shift+`,
/// `c-`/`ctrl+` and `m-`/`alt+` prefixes (repeatable and combinable).
/// Unknown names encode to nothing (empty bytes) so a typo'd key never sends
/// garbage.
#[must_use]
pub fn encode_key(name: &str) -> Vec<u8> {
    let trimmed = name.trim();
    // Modifiers peel off the *original* spelling so the remaining character
    // keeps its case: `B` must reach a case-sensitive program as capital B.
    let (modifiers, base) = peel_modifiers(trimmed);
    let lower = base.to_ascii_lowercase();

    // Tab carries its own fixed shift form (`CSI Z`) rather than a modifier
    // parameter, and a lone literal tab must stay a horizontal tab.
    if matches!(lower.as_str(), "tab" | "\\t") {
        return if modifiers.shift {
            b"\x1b[Z".to_vec()
        } else {
            b"\t".to_vec()
        };
    }

    if let Some(special) = special_by_name(&lower) {
        return special.encode(modifiers);
    }

    let bytes: &[u8] = match lower.as_str() {
        "enter" | "return" | "\\n" | "\\r" => b"\r",
        "esc" | "escape" => b"\x1b",
        "backspace" => b"\x7f",
        "space" => b" ",
        _ if lower.strip_prefix('f').is_some_and(|digits| {
            !digits.is_empty() && digits.as_bytes().iter().all(u8::is_ascii_digit)
        }) =>
        {
            let digits = lower.strip_prefix('f').unwrap_or_default();
            return digits
                .bytes()
                .try_fold(0u32, |acc, d| {
                    acc.checked_mul(10)?.checked_add(u32::from(d - b'0'))
                })
                .and_then(|n| u8::try_from(n).ok())
                .map_or_else(Vec::new, fkey_bytes);
        }
        // Single printable character, sent verbatim with its case preserved.
        _ => {
            let mut chars = base.chars();
            return match (chars.next(), chars.next()) {
                (Some(c), None) if !base.starts_with('\\') => {
                    // Ctrl on a character is a C0 mask, and the mask is only
                    // defined for the keys xterm maps to one. A name naming
                    // any other character with ctrl yields no bytes rather
                    // than a control byte the program cannot interpret.
                    if modifiers.ctrl && !is_c0_maskable(c) {
                        return Vec::new();
                    }
                    char_bytes(c, modifiers)
                }
                _ => {
                    // Literal newline/escape spellings already matched above;
                    // anything else multi-char is unknown → no bytes.
                    Vec::new()
                }
            };
        }
    };
    bytes.to_vec()
}

/// Whether `c` has a defined C0 control under Ctrl.
///
/// Letters fold to `A..=Z & 0x1F`; `@ [ ] \ ^ _` are the historical
/// escapes. Everything else (digits, punctuation, non-ASCII) has no
/// unambiguous control byte.
fn is_c0_maskable(c: char) -> bool {
    c.is_ascii_alphabetic() || matches!(c, '@' | '[' | ']' | '\\' | '^' | '_')
}

/// One of the `s-`/`shift+`, `c-`/`ctrl+`, `m-`/`alt+` modifier prefixes.
#[derive(Clone, Copy)]
enum ModifierFlag {
    Shift,
    Ctrl,
    Alt,
}

/// Peels leading modifier prefixes, accumulating the flags they name.
///
/// Prefixes are repeatable and order-independent, so `c-s-up` and
/// `s-c-up` both name Ctrl+Shift+Up. Returns the accumulated modifiers and
/// the remaining key name.
fn peel_modifiers(spec: &str) -> (jinn_slices::Modifiers, &str) {
    let mut modifiers = jinn_slices::Modifiers::none();
    let mut rest = spec;

    while let Some((flag, remainder)) = split_modifier_prefix(rest) {
        match flag {
            ModifierFlag::Shift => modifiers.shift = true,
            ModifierFlag::Ctrl => modifiers.ctrl = true,
            ModifierFlag::Alt => modifiers.alt = true,
        }
        rest = remainder;
    }

    (modifiers, rest)
}

/// Splits one leading modifier prefix off `spec`, if present.
fn split_modifier_prefix(spec: &str) -> Option<(ModifierFlag, &str)> {
    let prefix = [
        ("s-", ModifierFlag::Shift),
        ("shift+", ModifierFlag::Shift),
        ("c-", ModifierFlag::Ctrl),
        ("ctrl+", ModifierFlag::Ctrl),
        ("m-", ModifierFlag::Alt),
        ("alt+", ModifierFlag::Alt),
    ];

    prefix
        .iter()
        .find_map(|(text, flag)| spec.strip_prefix(text).map(|rest| (*flag, rest)))
}

/// Resolves a special key by its agent-facing name, or `None` when the name
/// is not a special key. The twin of [`SpecialKey::resolve`].
fn special_by_name(name: &str) -> Option<SpecialKey> {
    Some(match name {
        "up" | "uparrow" => SpecialKey::UP,
        "down" | "downarrow" => SpecialKey::DOWN,
        "right" | "rightarrow" => SpecialKey::RIGHT,
        "left" | "leftarrow" => SpecialKey::LEFT,
        "home" => SpecialKey::HOME,
        "end" => SpecialKey::END,
        "delete" | "del" => SpecialKey::DELETE,
        "pageup" => SpecialKey::PAGE_UP,
        "pagedown" => SpecialKey::PAGE_DOWN,
        _ => return None,
    })
}

/// The bytes a character key sends.
///
/// Ctrl on a character is a C0 mask and Alt on a character is an ESC prefix —
/// the exact opposite of the modifier-parameter encoding the special keys use,
/// which is why characters never route through [`SpecialKey`]. Shared by
/// [`encode_key`] and [`encode_key_event`] so the two paths cannot drift.
fn char_bytes(c: char, modifiers: jinn_slices::Modifiers) -> Vec<u8> {
    let mut bytes = c.to_string().into_bytes();
    if modifiers.ctrl {
        // Ctrl produces C0 controls; letters map A..=Z & 0x1F.
        if let Some(b) = bytes.first_mut() {
            *b = b.to_ascii_uppercase() & 0x1f;
        }
    } else if modifiers.shift {
        for b in &mut bytes {
            *b = b.to_ascii_uppercase();
        }
    } else {
        // No modifier: the character is sent as typed.
    }
    if modifiers.alt {
        let mut out = vec![0x1b];
        out.extend_from_slice(&bytes);
        return out;
    }
    bytes
}

/// The xterm-256color terminfo entry jinn advertises to every child pty.
///
/// A child program's terminfo lookups are keyed off this value, so the key
/// encoder is a hand-written table of *this* entry's capabilities rather
/// than a runtime terminfo query. That keeps a child's key bytes identical on
/// every platform regardless of whether a terminfo database is installed.
#[must_use]
pub fn advertised_term() -> &'static str {
    "xterm-256color"
}

/// A non-character key, resolved to the two forms terminfo uses for it.
///
/// xterm encodes these keys in two distinct families and a modified key
/// *never* uses the unmodified key's family:
///
/// - **Unmodified** — arrows, Home and End use SS3 (`ESC O <final>`), while
///   Delete, PageUp and PageDown use the `CSI <code> ~` form.
/// - **Modified** — every key in this set uses the `CSI`-parameter family
///   with a modifier parameter: `CSI 1;<mod><final>` for the SS3-family keys
///   and `CSI <code>;<mod>~` for the `~`-family. Alt and Ctrl are carried in
///   that parameter, *not* as an ESC prefix or a C0 mask (those remain
///   correct only for [`jinn_slices::Key::Char`]).
///
/// Keeping both forms on one descriptor is what stops the two from drifting
/// apart: a key cannot have a base form without also having a rule for its
/// modified form.
#[derive(Clone, Copy, PartialEq, Eq)]
struct SpecialKey {
    /// The bytes terminfo specifies for this key with no modifier held.
    plain: &'static [u8],
    /// Leading parameter of the modified form, as ASCII digits.
    code: &'static str,
    /// Terminator of the modified form: `~`, or the SS3 final letter.
    final_byte: u8,
}

impl SpecialKey {
    const UP: Self = Self {
        plain: b"\x1bOA",
        code: "1",
        final_byte: b'A',
    };
    const DOWN: Self = Self {
        plain: b"\x1bOB",
        code: "1",
        final_byte: b'B',
    };
    const RIGHT: Self = Self {
        plain: b"\x1bOC",
        code: "1",
        final_byte: b'C',
    };
    const LEFT: Self = Self {
        plain: b"\x1bOD",
        code: "1",
        final_byte: b'D',
    };
    const HOME: Self = Self {
        plain: b"\x1bOH",
        code: "1",
        final_byte: b'H',
    };
    const END: Self = Self {
        plain: b"\x1bOF",
        code: "1",
        final_byte: b'F',
    };
    const DELETE: Self = Self {
        plain: b"\x1b[3~",
        code: "3",
        final_byte: b'~',
    };
    const PAGE_UP: Self = Self {
        plain: b"\x1b[5~",
        code: "5",
        final_byte: b'~',
    };
    const PAGE_DOWN: Self = Self {
        plain: b"\x1b[6~",
        code: "6",
        final_byte: b'~',
    };

    /// Resolves a non-character key, or `None` when it has no special form.
    fn resolve(key: &jinn_slices::Key) -> Option<Self> {
        use jinn_slices::Key;

        Some(match key {
            Key::Up => Self::UP,
            Key::Down => Self::DOWN,
            Key::Right => Self::RIGHT,
            Key::Left => Self::LEFT,
            Key::Home => Self::HOME,
            Key::End => Self::END,
            Key::Delete => Self::DELETE,
            Key::PageUp => Self::PAGE_UP,
            Key::PageDown => Self::PAGE_DOWN,
            // Char, Enter, Esc, Tab, Backspace and F(n) are handled by
            // their own paths — Tab has a dedicated shift form and F(n) has
            // its own numbering.
            _ => return None,
        })
    }

    /// The bytes for this key with `modifiers` held.
    ///
    /// No modifier emits the base form byte-for-byte; any modifier switches
    /// to the `CSI`-parameter family. The two families differ only in their
    /// leading parameter and their terminator — both put a `;` before the
    /// modifier parameter:
    ///
    /// ```text
    /// arrows/Home/End   ESC [ 1 ; <mod> <final>   e.g. ESC [ 1;2A
    /// Delete/PgUp/PgDn  ESC [ <code> ; <mod> ~     e.g. ESC [ 3;2~
    /// ```
    fn encode(&self, modifiers: jinn_slices::Modifiers) -> Vec<u8> {
        if modifiers.is_none() {
            return self.plain.to_vec();
        }

        let mut out = b"\x1b[".to_vec();
        out.extend_from_slice(self.code.as_bytes());
        out.push(b';');
        out.extend_from_slice(modifier_parameter(modifiers).to_string().as_bytes());
        out.push(self.final_byte);
        out
    }
}

/// The `xterm` modifier parameter for a held modifier combination.
///
/// xterm encodes the modifier state as `1 +` a bitmask of shift (1), alt
/// (2), ctrl (4) and meta (8), giving the familiar 2=shift, 3=alt, 4=alt+shift,
/// 5=ctrl, 6=ctrl+shift, 7=ctrl+alt, 8=ctrl+alt+shift.
///
/// Only the shift row is corroborated by the `xterm-256color` entry itself
/// (`kri=\E[1;2A`, `khome=\E[1;2H`, …); the remaining rows are absent from
/// every terminfo entry on this system and are derived from the documented
/// xterm convention. The parity test therefore asserts only where a real
/// capability exists.
fn modifier_parameter(modifiers: jinn_slices::Modifiers) -> u8 {
    let mut mask = 0;
    if modifiers.shift {
        mask |= 1;
    }
    if modifiers.alt {
        mask |= 2;
    }
    if modifiers.ctrl {
        mask |= 4;
    }
    1 + mask
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

    let plain: &[u8] = match &event.key {
        // Characters never take the special-key path: Ctrl on a character is a
        // C0 mask and Alt on a character is an ESC prefix, which is the
        // opposite of the modifier-parameter encoding the special keys use.
        Key::Char(c) => return char_bytes(*c, m),
        // Shift+Tab is `CSI Z`; plain Tab is HT.
        Key::Tab if m.shift => return b"\x1b[Z".to_vec(),
        Key::Tab => b"\t",
        Key::Enter => b"\r",
        Key::Esc => b"\x1b",
        Key::Backspace => b"\x7f",
        Key::F(n) => {
            // F1–F4 use the short SS3 form; F5+ use `CSI n ~`.
            return fkey_bytes(*n);
        }
        // Arrows, Home, End, Delete, PageUp and PageDown: the base form with
        // no modifier, the modifier-parameter form with any modifier.
        key => {
            let Some(special) = SpecialKey::resolve(key) else {
                return Vec::new();
            };
            return special.encode(m);
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
    // The terminfo parity test reports its own skip to the captured test
    // output; printing is the only way to surface it there.
    #![allow(clippy::print_stderr, reason = "skip notes go to captured test output")]
    // TERMINFO/TERMINFO_DIRS roots come from the environment and may be
    // relative; the probe reads through raw std paths only.
    #![expect(
        clippy::disallowed_types,
        reason = "env-supplied TERMINFO/TERMINFO_DIRS roots may be relative; test-only probe reads through raw std paths"
    )]
    use super::{advertised_term, encode_key, encode_key_event};
    use jinn_slices::{Key, KeyEvent, Modifiers};
    use std::path::PathBuf;

    /// Locates the compiled terminfo entry for `term`, or `None` when the
    /// system has no terminfo database.
    ///
    /// Test-only by design: production code must never depend on a terminfo
    /// database being installed, which is exactly why the encoder table is
    /// hand-written. This reader exists so the table can be *checked* against
    /// the real thing on a machine that has one.
    fn terminfo_entry_path(term: &str) -> Option<PathBuf> {
        // `TERMINFO` and every `TERMINFO_DIRS` entry are directory *roots*;
        // the entry itself lives at `<root>/<first-char>/<term>`.
        let mut roots: Vec<PathBuf> = std::env::var_os("TERMINFO")
            .map(PathBuf::from)
            .into_iter()
            .collect();
        if let Some(dirs) = std::env::var_os("TERMINFO_DIRS") {
            roots.extend(std::env::split_paths(&dirs));
        }
        roots.extend(
            [
                "/usr/share/terminfo",
                "/lib/terminfo",
                "/etc/terminfo",
                "/usr/lib/terminfo",
            ]
            .into_iter()
            .map(PathBuf::from),
        );

        // Entries are stored under the first character of the terminal name,
        // in both its literal and lowercase-hex form.
        let first = term.chars().next()?;
        let hex = format!("{:x}", first as u32);

        roots
            .into_iter()
            .flat_map(move |root| {
                [root.join(&hex), root.join(first.to_string())]
                    .into_iter()
                    .map(move |dir| dir.join(term))
            })
            .find(|candidate| candidate.is_file())
    }

    /// Reads one string capability's bytes, unescaping the terminfo `\E`
    /// notation into a real 0x1b.
    fn cap_bytes(terminfo: &[u8], name: &str) -> Option<Vec<u8>> {
        // The names section holds `key=value\0` pairs.
        let text = std::str::from_utf8(terminfo).ok()?;
        let raw = text
            .split('\0')
            .find_map(|entry| entry.strip_prefix(&format!("{name}=")))?;

        let mut out = Vec::new();
        let mut chars = raw.chars();
        while let Some(c) = chars.next() {
            match c {
                '\\' => match chars.next()? {
                    'E' => out.push(0x1b),
                    'n' => out.push(b'\n'),
                    'r' => out.push(b'\r'),
                    't' => out.push(b'\t'),
                    '\\' => out.push(b'\\'),
                    'b' => out.push(0x08),
                    'f' => out.push(0x0c),
                    '0' => out.push(0),
                    '1'..='9' => {}
                    other => out.push(u8::try_from(other).ok()?),
                },
                _ => {
                    // Multi-byte capability characters are pushed as their
                    // UTF-8 bytes so the comparison stays byte-exact.
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                }
            }
        }
        Some(out)
    }

    /// Reads the compiled terminfo entry for the advertised terminal, or
    /// `None` when this machine has no terminfo database to compare against.
    fn read_advertised_terminfo_entry() -> Option<Vec<u8>> {
        let path = terminfo_entry_path(advertised_term())?;
        Some(std::fs::read(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}")))
    }

    /// The encoder table must match the terminfo entry jinn advertises.
    ///
    /// This is the build-time guard that lets the encoder skip a runtime
    /// terminfo lookup: if the advertised `TERM` ever changes, these tests go
    /// red instead of the key encoder silently drifting from it. Only
    /// capabilities the database actually defines are asserted — ctrl- and
    /// alt-modified arrow capabilities exist in no entry, so their values are
    /// pinned by the unit tests above rather than claimed to be verified.
    #[rstest::rstest]
    fn unmodified_keys_match_the_advertised_terminfo_entry() {
        // Given the terminal identity jinn advertises to child ptys, the real
        // compiled entry for it, and the unmodified key/capability pairs both
        // encoder paths are checked against.
        let term = advertised_term();
        let Some(terminfo) = read_advertised_terminfo_entry() else {
            // A missing terminfo database must never break the build, so
            // the parity check is skipped rather than failed.
            eprintln!("skipping terminfo parity check: no compiled entry for {term}");
            return;
        };
        let cases: &[(Key, &str, &str)] = &[
            (Key::Up, "up", "kcuu1"),
            (Key::Down, "down", "kcud1"),
            (Key::Right, "right", "kcuf1"),
            (Key::Left, "left", "kcub1"),
            (Key::Home, "home", "khome"),
            (Key::End, "end", "kend"),
            (Key::Delete, "delete", "kdch1"),
            (Key::PageUp, "pageup", "kpp"),
            (Key::PageDown, "pagedown", "knp"),
            (Key::F(1), "f1", "kf1"),
            (Key::F(2), "f2", "kf2"),
            (Key::F(3), "f3", "kf3"),
            (Key::F(4), "f4", "kf4"),
            (Key::F(5), "f5", "kf5"),
            (Key::F(6), "f6", "kf6"),
            (Key::F(7), "f7", "kf7"),
            (Key::F(8), "f8", "kf8"),
            (Key::F(9), "f9", "kf9"),
            (Key::F(10), "f10", "kf10"),
            (Key::F(11), "f11", "kf11"),
            (Key::F(12), "f12", "kf12"),
        ];

        for (key, name, cap) in cases {
            let Some(expected) = cap_bytes(&terminfo, cap) else {
                // A capability this entry omits cannot be asserted against.
                continue;
            };

            // When encoding the key through both encoder paths.
            let event = KeyEvent {
                key: key.clone(),
                modifiers: Modifiers::none(),
            };

            // Then both paths emit exactly the capability's bytes.
            assert_eq!(
                encode_key_event(&event),
                expected,
                "{name} ({cap}) must match the {term} entry"
            );
            assert_eq!(encode_key(name), expected, "{name} via the name path");
        }
    }

    #[rstest::rstest]
    fn shift_modified_keys_match_the_advertised_terminfo_entry() {
        // Given the terminal identity jinn advertises to child ptys, the real
        // compiled entry for it, and the shift-modified key/capability pairs
        // both encoder paths are checked against.
        let term = advertised_term();
        let Some(terminfo) = read_advertised_terminfo_entry() else {
            // A missing terminfo database must never break the build, so
            // the parity check is skipped rather than failed.
            eprintln!("skipping terminfo parity check: no compiled entry for {term}");
            return;
        };
        let cases: &[(Key, &str, &str)] = &[
            (Key::Up, "s-up", "kri"),
            (Key::Down, "s-down", "kind"),
            (Key::Right, "s-right", "kRIT"),
            (Key::Left, "s-left", "kLFT"),
            (Key::Home, "s-home", "kHOM"),
            (Key::End, "s-end", "kEND"),
            (Key::Delete, "s-delete", "kDC"),
        ];

        for (key, name, cap) in cases {
            let Some(expected) = cap_bytes(&terminfo, cap) else {
                // A capability this entry omits cannot be asserted against.
                continue;
            };

            // When encoding the key through both encoder paths.
            let event = KeyEvent {
                key: key.clone(),
                modifiers: Modifiers::shift(),
            };

            // Then both paths emit exactly the capability's bytes.
            assert_eq!(
                encode_key_event(&event),
                expected,
                "{name} ({cap}) must match the {term} entry"
            );
            assert_eq!(encode_key(name), expected, "{name} via the name path");
        }
    }

    /// The user-takeover path and the agent path must agree on every key they
    /// both handle. These are two entry points onto one table precisely
    /// because independent copies drift — the user path emitting the CSI
    /// arrow form while the agent path emitted the SS3 form for the same
    /// logical key.
    #[rstest::rstest]
    #[case(Key::Up, "up")]
    #[case(Key::Down, "down")]
    #[case(Key::Left, "left")]
    #[case(Key::Right, "right")]
    #[case(Key::Home, "home")]
    #[case(Key::End, "end")]
    #[case(Key::Delete, "delete")]
    #[case(Key::PageUp, "pageup")]
    #[case(Key::PageDown, "pagedown")]
    #[case(Key::F(1), "f1")]
    #[case(Key::F(4), "f4")]
    #[case(Key::F(5), "f5")]
    #[case(Key::F(12), "f12")]
    fn encode_key_event_matches_encode_key_for_special_keys(#[case] key: Key, #[case] name: &str) {
        // Given the same special key on both paths.
        let event = KeyEvent {
            key,
            modifiers: Modifiers::none(),
        };

        // When encoding via the event path and the name path.
        let from_event = encode_key_event(&event);
        let from_name = encode_key(name);

        // Then the byte sequences are identical.
        assert_eq!(from_event, from_name);
        assert!(!from_event.is_empty());
    }

    /// The same agreement must hold once modifiers are involved: a modified
    /// special key is a modifier-parameter sequence on both paths, never the
    /// character-oriented ESC prefix or C0 mask the name path applies to
    /// characters.
    #[rstest::rstest]
    #[case("s-up", Key::Up, Modifiers::shift())]
    #[case("c-left", Key::Left, Modifiers::ctrl())]
    #[case("m-home", Key::Home, Modifiers::alt())]
    #[case("s-delete", Key::Delete, Modifiers::shift())]
    #[case("s-tab", Key::Tab, Modifiers::shift())]
    fn encode_key_event_matches_encode_key_for_modified_special_keys(
        #[case] name: &str,
        #[case] key: Key,
        #[case] modifiers: Modifiers,
    ) {
        // Given the same modified special key on both paths.
        let event = KeyEvent { key, modifiers };

        // When encoding via the event path and the name path.
        let from_event = encode_key_event(&event);
        let from_name = encode_key(name);

        // Then the byte sequences are identical.
        assert_eq!(from_event, from_name);
    }

    /// Modifier prefixes on the name path are order-independent and
    /// repeatable, so an agent can spell a combination either way round.
    #[rstest::rstest]
    #[case("c-s-up", "s-c-up")]
    #[case("shift+ctrl+up", "ctrl+shift+up")]
    fn modifier_prefixes_combine_in_any_order(#[case] first: &str, #[case] second: &str) {
        // Given the same key combination spelled two ways.

        // When encoding both spellings.
        let a = encode_key(first);
        let b = encode_key(second);

        // Then both spellings produce the same bytes.
        assert_eq!(a, b);
        assert!(!a.is_empty());
    }

    #[rstest::rstest]
    #[case(Key::Enter, Modifiers::none(), &b"\r"[..])]
    #[case(Key::Esc, Modifiers::none(), b"\x1b")]
    #[case(Key::Up, Modifiers::none(), b"\x1bOA")]
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

    /// Arrows, Home and End use the SS3 form (`ESC O <final>`) that the
    /// advertised `xterm-256color` terminfo actually specifies
    /// (`kcuu1=\EOA` … `khome=\EOH`). The alternate CSI form (`ESC [ <final>`)
    /// is *accepted* by xterm but is not what terminfo advertises, so a
    /// program that looks the received bytes up in its terminfo entry — every
    /// ncurses program, i.e. htop, vim, nano — never matches it and silently
    /// discards the keystroke.
    #[rstest::rstest]
    #[case(Key::Up, b"\x1bOA")]
    #[case(Key::Down, b"\x1bOB")]
    #[case(Key::Right, b"\x1bOC")]
    #[case(Key::Left, b"\x1bOD")]
    #[case(Key::Home, b"\x1bOH")]
    #[case(Key::End, b"\x1bOF")]
    fn encodes_navigational_keys_in_the_ss3_form(#[case] key: Key, #[case] expected: &[u8]) {
        // Given an unmodified navigational key.
        let event = KeyEvent {
            key,
            modifiers: Modifiers::none(),
        };

        // When encoding it for the pty.
        let bytes = encode_key_event(&event);

        // Then the SS3 form terminfo advertises is emitted.
        assert_eq!(bytes, expected);
    }

    /// The already-correct keys stay correct: `kdch1=\E[3~`, `kpp=\E[5~`,
    /// `knp=\E[6~`, `kf1=\EOP`…`kf12=\E[24~`, `kbs=^?`. Pinned so a future
    /// edit that "unifies" the families onto one form cannot regress them.
    #[rstest::rstest]
    #[case(Key::Delete, b"\x1b[3~")]
    #[case(Key::PageUp, b"\x1b[5~")]
    #[case(Key::PageDown, b"\x1b[6~")]
    #[case(Key::F(1), b"\x1bOP")]
    #[case(Key::F(4), b"\x1bOS")]
    #[case(Key::F(5), b"\x1b[15~")]
    #[case(Key::F(12), b"\x1b[24~")]
    fn encodes_terminfo_correct_keys_unchanged(#[case] key: Key, #[case] expected: &[u8]) {
        // Given an unmodified key whose terminfo cap jinn already matched.
        let event = KeyEvent {
            key,
            modifiers: Modifiers::none(),
        };

        // When encoding it for the pty.
        let bytes = encode_key_event(&event);

        // Then the same bytes terminfo advertises are emitted.
        assert_eq!(bytes, expected);
    }

    /// **Provenance: verified against the real terminfo database.** Every
    /// expected value here is the literal body of an `xterm-256color`
    /// capability, confirmed with `infocmp -1 xterm-256color`:
    /// `kri=\E[1;2A`, `kind=\E[1;2B`, `kRIT=\E[1;2C`, `kLFT=\E[1;2D`,
    /// `kHOM=\E[1;2H`, `kEND=\E[1;2F`, `kDC=\E[3;2~`.
    #[rstest::rstest]
    #[case(Key::Up, b"\x1b[1;2A")]
    #[case(Key::Down, b"\x1b[1;2B")]
    #[case(Key::Right, b"\x1b[1;2C")]
    #[case(Key::Left, b"\x1b[1;2D")]
    #[case(Key::Home, b"\x1b[1;2H")]
    #[case(Key::End, b"\x1b[1;2F")]
    #[case(Key::Delete, b"\x1b[3;2~")]
    fn encodes_shift_modified_special_keys_per_terminfo(#[case] key: Key, #[case] expected: &[u8]) {
        // Given a special key with shift held.
        let event = KeyEvent {
            key,
            modifiers: Modifiers::shift(),
        };

        // When encoding it for the pty.
        let bytes = encode_key_event(&event);

        // Then the exact sequence the terminfo capability specifies is sent.
        assert_eq!(bytes, expected);
    }

    /// **Provenance: derived, not verified.** No terminfo entry on this
    /// system carries a ctrl- or alt-modified arrow capability, so these
    /// expected values come from the documented xterm convention
    /// (`1 +` shift/alt/ctrl bitmask) rather than from a database read. They
    /// pin the *arithmetic* of the modifier parameter — that ctrl adds 4 and
    /// that combinations accumulate — not a claim that a real entry agrees.
    /// Re-derive this table if that ever changes.
    #[rstest::rstest]
    #[case(Modifiers::alt(), b"\x1b[1;3A")]
    #[case(Modifiers::ctrl(), b"\x1b[1;5A")]
    #[case(Modifiers { ctrl: true, alt: true, shift: false }, b"\x1b[1;7A")]
    #[case(Modifiers { ctrl: true, alt: false, shift: true }, b"\x1b[1;6A")]
    fn encodes_ctrl_and_alt_special_keys_with_the_modifier_parameter(
        #[case] modifiers: Modifiers,
        #[case] expected: &[u8],
    ) {
        // Given Up with a ctrl and/or alt combination held.
        let event = KeyEvent {
            key: Key::Up,
            modifiers,
        };

        // When encoding it for the pty.
        let bytes = encode_key_event(&event);

        // Then the modifier parameter is `1 +` the combination's bitmask.
        assert_eq!(bytes, expected);
    }

    /// A modified special key must never fall back to the SS3 form. The two
    /// families are not interchangeable: `ESC O A` and `ESC [ 1;2A` share
    /// nothing but intent, and emitting SS3 for a modified key looks correct
    /// because the final letter matches.
    #[rstest::rstest]
    #[case(Key::Up, Modifiers::shift())]
    #[case(Key::Home, Modifiers::ctrl())]
    #[case(Key::Delete, Modifiers::alt())]
    #[case(Key::PageUp, Modifiers::shift())]
    fn modified_special_keys_never_encode_as_ss3(#[case] key: Key, #[case] modifiers: Modifiers) {
        // Given a special key with a modifier held.
        let event = KeyEvent { key, modifiers };

        // When encoding it for the pty.
        let bytes = encode_key_event(&event);

        // Then the sequence opens with CSI, not SS3.
        assert_eq!(&bytes[..2], b"\x1b[");
    }

    /// Shift+Tab is `CSI Z` — the one key crossterm reports as its own code
    /// (`KeyCode::BackTab`), converted to `Key::Tab` with the shift modifier
    /// set. It is a fixed sequence, not a member of the modifier-parameter
    /// family, so it is spelled out rather than derived.
    #[rstest::rstest]
    fn encodes_shift_tab_as_csi_z() {
        // Given Tab with shift held.
        let event = KeyEvent {
            key: Key::Tab,
            modifiers: Modifiers::shift(),
        };

        // When encoding it for the pty.
        let bytes = encode_key_event(&event);

        // Then the back-tab sequence is sent, not a horizontal tab.
        assert_eq!(bytes, b"\x1b[Z");
    }

    #[rstest::rstest]
    fn encodes_unmodified_tab_as_horizontal_tab() {
        // Given Tab with no modifier.
        let event = KeyEvent {
            key: Key::Tab,
            modifiers: Modifiers::none(),
        };

        // When encoding it for the pty.
        let bytes = encode_key_event(&event);

        // Then a plain HT is sent — the shift form must not leak in.
        assert_eq!(bytes, b"\t");
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
