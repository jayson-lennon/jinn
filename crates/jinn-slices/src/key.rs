//! Key representation for keyboard events.
//!
//! Backend-agnostic key types that decouple key handling from any
//! specific terminal library. These are the vocabulary of every keybind
//! surface: static scopes, slice route rows, and slice key hooks all
//! speak [`KeyEvent`].

use serde::{Deserialize, Serialize};

/// Keyboard key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Key {
    /// A character key.
    Char(char),
    /// Enter key.
    Enter,
    /// Escape key.
    Esc,
    /// Tab key.
    Tab,
    /// Backspace key.
    Backspace,
    /// Up arrow.
    Up,
    /// Down arrow.
    Down,
    /// Left arrow.
    Left,
    /// Right arrow.
    Right,
    /// Home key.
    Home,
    /// End key.
    End,
    /// Page up key.
    PageUp,
    /// Page down key.
    PageDown,
    /// Delete key (forward delete).
    Delete,
    /// Function key (F1–F12).
    F(u8),
}

impl ratatui_which_key::Key for KeyEvent {
    fn display(&self) -> String {
        if self.modifiers.ctrl
            && let Key::Char(c) = self.key
        {
            return format!("<C-{}>", c.to_ascii_lowercase());
        }

        if self.modifiers.alt
            && let Key::Char(c) = self.key
        {
            return format!("<M-{}>", c.to_ascii_lowercase());
        }

        let base = match self.key {
            Key::Char(' ') => "Space".to_owned(),
            Key::Char(c) => c.to_string(),
            Key::Tab => "Tab".to_owned(),
            Key::Enter => "Enter".to_owned(),
            Key::Backspace => "Backspace".to_owned(),
            Key::Esc => "Esc".to_owned(),
            Key::Up => "↑".to_owned(),
            Key::Down => "↓".to_owned(),
            Key::Left => "←".to_owned(),
            Key::Right => "→".to_owned(),
            Key::Home => "Home".to_owned(),
            Key::End => "End".to_owned(),
            Key::PageUp => "PageUp".to_owned(),
            Key::PageDown => "PageDown".to_owned(),
            Key::Delete => "Delete".to_owned(),
            Key::F(n) => format!("F{n}"),
        };

        match (
            self.modifiers.shift,
            self.modifiers.ctrl,
            self.modifiers.alt,
        ) {
            (true, false, false) => format!("S-{base}"),
            (false, true, false) => format!("C-{base}"),
            (true, true, false) => format!("C-S-{base}"),
            (false, false, true) => format!("M-{base}"),
            _ => base,
        }
    }

    fn is_backspace(&self) -> bool {
        matches!(self.key, Key::Backspace)
    }

    fn space() -> Self {
        KeyEvent {
            key: Key::Char(' '),
            modifiers: Modifiers::none(),
        }
    }

    fn from_char(c: char) -> Option<Self> {
        Some(KeyEvent {
            key: Key::Char(c),
            modifiers: Modifiers::none(),
        })
    }

    fn from_special_name(name: &str) -> Option<Self> {
        Self::parse_notation(name)
    }
}

impl KeyEvent {
    /// Parse a key notation string into a `KeyEvent`.
    ///
    /// Supports modifier-prefixed forms: `c-` for Ctrl, `s-` for Shift, `m-` for Meta/Alt.
    /// Modifiers apply to both named keys and single characters.
    ///
    /// Named keys: `"tab"`, `"enter"`, `"escape"`, arrow keys,
    /// function keys (`"f1"`–`"f12"`), and symbolic aliases (`"lt"` → `<`, `"gt"` → `>`).
    ///
    /// Matching is case-insensitive.
    ///
    /// # Examples
    ///
    /// - `"c-x"` → Ctrl+X
    /// - `"s-enter"` → Shift+Enter
    /// - `"c-enter"` → Ctrl+Enter
    /// - `"tab"` → Tab
    /// - `"f5"` → F5
    /// - `"lt"` → <
    pub fn parse_notation(name: &str) -> Option<Self> {
        let lower = name.to_ascii_lowercase();

        let (modifiers, rest) = if let Some(stripped) = lower.strip_prefix("s-") {
            (Modifiers::shift(), stripped)
        } else if let Some(stripped) = lower.strip_prefix("m-") {
            (Modifiers::alt(), stripped)
        } else if let Some(stripped) = lower.strip_prefix("c-") {
            (Modifiers::ctrl(), stripped)
        } else {
            (Modifiers::none(), lower.as_str())
        };

        let key = parse_key_name(rest)?;

        Some(KeyEvent { key, modifiers })
    }
}

/// Parse a lower-case key name string into a [`Key`].
///
/// Handles named keys (`"tab"`, `"enter"`, …), function keys (`"f1"`–`"f12"`),
/// symbolic aliases (`"lt"`, `"gt"`, `"space"`), and bare single characters.
fn parse_key_name(name: &str) -> Option<Key> {
    match name {
        "tab" => Some(Key::Tab),
        "enter" => Some(Key::Enter),
        "bs" | "backspace" => Some(Key::Backspace),
        "esc" | "escape" => Some(Key::Esc),
        "up" => Some(Key::Up),
        "down" => Some(Key::Down),
        "left" => Some(Key::Left),
        "right" => Some(Key::Right),
        "home" => Some(Key::Home),
        "end" => Some(Key::End),
        "pgup" | "pageup" => Some(Key::PageUp),
        "pgdn" | "pagedown" => Some(Key::PageDown),
        "delete" | "del" => Some(Key::Delete),
        "space" => Some(Key::Char(' ')),
        "lt" => Some(Key::Char('<')),
        "gt" => Some(Key::Char('>')),
        s if s.starts_with('f') && s.len() > 1 => {
            let num: u8 = s.get(1..)?.parse().ok()?;
            (1..=12).contains(&num).then_some(Key::F(num))
        }
        s if s.len() == 1 => Some(Key::Char(s.chars().next()?)),
        _ => None,
    }
}

/// Keyboard modifier flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Modifiers {
    /// Control key held.
    pub ctrl: bool,
    /// Alt key held.
    pub alt: bool,
    /// Shift key held.
    pub shift: bool,
}

impl Modifiers {
    /// Create a modifiers with no flags set.
    #[must_use]
    pub fn none() -> Self {
        Self {
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    /// Create a modifiers with only ctrl set.
    #[must_use]
    pub fn ctrl() -> Self {
        Self {
            ctrl: true,
            alt: false,
            shift: false,
        }
    }

    /// Create a modifiers with only alt set.
    #[must_use]
    pub fn alt() -> Self {
        Self {
            ctrl: false,
            alt: true,
            shift: false,
        }
    }

    /// Create a modifiers with only shift set.
    #[must_use]
    pub fn shift() -> Self {
        Self {
            ctrl: false,
            alt: false,
            shift: true,
        }
    }

    /// Returns `true` if no modifier flags are set.
    #[must_use]
    pub fn is_none(&self) -> bool {
        !self.ctrl && !self.alt && !self.shift
    }
}

/// A keyboard event with key and modifiers.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct KeyEvent {
    /// The key that was pressed.
    pub key: Key,
    /// Modifier keys held at the time.
    pub modifiers: Modifiers,
}

#[cfg(test)]
mod tests {
    use super::{Key, KeyEvent, Modifiers};
    use ratatui_which_key::Key as _;

    #[rstest::rstest]
    #[case::ctrl(Modifiers::none(), "ctrl", false)]
    #[case::alt(Modifiers::none(), "alt", false)]
    #[case::shift(Modifiers::none(), "shift", false)]
    fn modifiers_none_flag_is_false(
        #[case] mods: Modifiers,
        #[case] flag: &str,
        #[case] expected: bool,
    ) {
        // Given modifiers created with none().
        // When inspecting each flag.
        // Then each individual flag is false.
        let actual = match flag {
            "ctrl" => mods.ctrl,
            "alt" => mods.alt,
            "shift" => mods.shift,
            _ => panic!("unknown flag: {flag}"),
        };
        assert_eq!(actual, expected);
    }

    #[rstest::rstest]
    fn parse_notation_s_enter_returns_shift_enter() {
        // Given the notation "s-enter".
        let result = KeyEvent::parse_notation("s-enter");

        // When parsing.
        let key_event = result.expect("should parse");

        // Then it is Shift+Enter.
        assert_eq!(key_event.key, Key::Enter);
        assert!(key_event.modifiers.shift);
        assert!(!key_event.modifiers.ctrl);
    }

    #[rstest::rstest]
    fn parse_notation_c_enter_returns_ctrl_enter() {
        // Given the notation "c-enter".
        let result = KeyEvent::parse_notation("c-enter");

        // When parsing.
        let key_event = result.expect("should parse");

        // Then it is Ctrl+Enter.
        assert_eq!(key_event.key, Key::Enter);
        assert!(key_event.modifiers.ctrl);
        assert!(!key_event.modifiers.shift);
    }

    #[rstest::rstest]
    fn parse_notation_enter_returns_unmodified() {
        // Given the notation "enter".
        let result = KeyEvent::parse_notation("enter");

        // When parsing.
        let key_event = result.expect("should parse");

        // Then it is plain Enter with no modifiers.
        assert_eq!(key_event.key, Key::Enter);
        assert!(key_event.modifiers.is_none());
    }

    #[rstest::rstest]
    #[case::tab("tab", Key::Tab)]
    #[case::enter("enter", Key::Enter)]
    #[case::bs("bs", Key::Backspace)]
    #[case::backspace("backspace", Key::Backspace)]
    #[case::esc("esc", Key::Esc)]
    #[case::escape("escape", Key::Esc)]
    #[case::up("up", Key::Up)]
    #[case::down("down", Key::Down)]
    #[case::left("left", Key::Left)]
    #[case::right("right", Key::Right)]
    #[case::home("home", Key::Home)]
    #[case::end("end", Key::End)]
    #[case::pgup("pgup", Key::PageUp)]
    #[case::pageup("pageup", Key::PageUp)]
    #[case::pgdn("pgdn", Key::PageDown)]
    #[case::pagedown("pagedown", Key::PageDown)]
    #[case::delete("delete", Key::Delete)]
    #[case::del("del", Key::Delete)]
    #[case::space("space", Key::Char(' '))]
    #[case::lt("lt", Key::Char('<'))]
    #[case::gt("gt", Key::Char('>'))]
    fn parse_notation_named_keys(#[case] input: &str, #[case] expected: Key) {
        // Given a notation string for a named key.
        let result = KeyEvent::parse_notation(input);

        // When parsing.
        let key_event = result.expect("should parse");

        // Then the key matches with no modifiers.
        assert_eq!(key_event.key, expected);
        assert!(key_event.modifiers.is_none());
    }

    #[rstest::rstest]
    #[case::f1("f1", 1)]
    #[case::f6("f6", 6)]
    #[case::f12("f12", 12)]
    fn parse_notation_function_keys(#[case] input: &str, #[case] num: u8) {
        // Given a function key notation.
        let result = KeyEvent::parse_notation(input);

        // When parsing.
        let key_event = result.expect("should parse");

        // Then it is the correct function key.
        assert_eq!(key_event.key, Key::F(num));
        assert!(key_event.modifiers.is_none());
    }

    #[rstest::rstest]
    #[case::f0("f0")]
    #[case::f13("f13")]
    fn parse_notation_rejects_out_of_range_function_keys(#[case] input: &str) {
        // Given an out-of-range function key notation.
        let result = KeyEvent::parse_notation(input);

        // When parsing.

        // Then it returns None.
        assert!(result.is_none());
    }

    #[rstest::rstest]
    fn parse_notation_single_char_returns_key_event() {
        // Given a single-character notation.
        let result = KeyEvent::parse_notation("a");

        // When parsing.
        let key_event = result.expect("should parse");

        // Then it is Char('a') with no modifiers.
        assert_eq!(key_event.key, Key::Char('a'));
        assert!(key_event.modifiers.is_none());
    }

    #[rstest::rstest]
    fn parse_notation_ctrl_single_char() {
        // Given a ctrl-modified single-char notation.
        let result = KeyEvent::parse_notation("c-x");

        // When parsing.
        let key_event = result.expect("should parse");

        // Then it is Ctrl+Char('x').
        assert_eq!(key_event.key, Key::Char('x'));
        assert!(key_event.modifiers.ctrl);
        assert!(!key_event.modifiers.shift);
    }

    #[rstest::rstest]
    fn parse_notation_shift_single_char() {
        // Given a shift-modified single-char notation.
        let result = KeyEvent::parse_notation("s-a");

        // When parsing.
        let key_event = result.expect("should parse");

        // Then it is Shift+Char('a').
        assert_eq!(key_event.key, Key::Char('a'));
        assert!(!key_event.modifiers.ctrl);
        assert!(key_event.modifiers.shift);
    }

    #[rstest::rstest]
    fn parse_notation_case_insensitive() {
        // Given a notation with mixed case.
        let result = KeyEvent::parse_notation("ENTER");

        // When parsing.
        let key_event = result.expect("should parse");

        // Then it still resolves correctly.
        assert_eq!(key_event.key, Key::Enter);
    }

    #[rstest::rstest]
    #[case::empty("")]
    #[case::unknown("foobar")]
    #[case::bare_ctrl("c-")]
    #[case::bare_shift("s-")]
    #[case::bare_meta("m-")]
    fn parse_notation_rejects_invalid_inputs(#[case] input: &str) {
        // Given an invalid notation.
        let result = KeyEvent::parse_notation(input);

        // When parsing.

        // Then it returns None.
        assert!(result.is_none());
    }

    #[rstest::rstest]
    fn parse_notation_m_s_returns_alt_s() {
        // Given the notation "m-s".
        let result = KeyEvent::parse_notation("m-s");

        // When parsing.
        let key_event = result.expect("should parse");

        // Then it is Alt+Char('s').
        assert_eq!(key_event.key, Key::Char('s'));
        assert!(key_event.modifiers.alt);
        assert!(!key_event.modifiers.ctrl);
        assert!(!key_event.modifiers.shift);
    }

    #[rstest::rstest]
    fn parse_notation_m_enter_returns_alt_enter() {
        // Given the notation "m-enter".
        let result = KeyEvent::parse_notation("m-enter");

        // When parsing.
        let key_event = result.expect("should parse");

        // Then it is Alt+Enter.
        assert_eq!(key_event.key, Key::Enter);
        assert!(key_event.modifiers.alt);
        assert!(!key_event.modifiers.ctrl);
        assert!(!key_event.modifiers.shift);
    }

    #[rstest::rstest]
    fn display_alt_char_shows_m_notation() {
        // Given a KeyEvent with Alt+Char('s').
        let key_event = KeyEvent {
            key: Key::Char('s'),
            modifiers: Modifiers::alt(),
        };

        // When displaying.
        let display = key_event.display();

        // Then it shows "<M-s>".
        assert_eq!(display, "<M-s>");
    }

    #[rstest::rstest]
    fn display_alt_named_key_shows_m_prefix() {
        // Given a KeyEvent with Alt+Enter.
        let key_event = KeyEvent {
            key: Key::Enter,
            modifiers: Modifiers::alt(),
        };

        // When displaying.
        let display = key_event.display();

        // Then it shows "M-Enter".
        assert_eq!(display, "M-Enter");
    }
}
