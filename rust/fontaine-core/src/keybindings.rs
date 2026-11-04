//! Keystroke parsing and the remappable TUI actions. This module lives in `fontaine-core` so
//! config validation can check keybindings without a TUI dependency.

use std::fmt;
use std::str::FromStr;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A parsed keystroke with crossterm's native types. `Ord` orders by the rendered form, since
/// crossterm's types carry no ordering of their own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Keystroke {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl PartialOrd for Keystroke {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Keystroke {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.format().cmp(&other.format())
    }
}

impl Keystroke {
    /// Renders the keystroke in the config syntax: lowercase modifiers first, then the key,
    /// with `shift+tab` for the `BackTab` code so the default bindings read as users type them.
    pub fn format(&self) -> String {
        let mut out = String::new();
        if self.modifiers.contains(KeyModifiers::CONTROL) {
            out.push_str("ctrl+");
        }
        if self.modifiers.contains(KeyModifiers::ALT) {
            out.push_str("alt+");
        }
        if self.modifiers.contains(KeyModifiers::SHIFT) {
            out.push_str("shift+");
        }
        match self.code {
            KeyCode::BackTab => out.push_str("shift+tab"),
            KeyCode::Tab => out.push_str("tab"),
            KeyCode::Enter => out.push_str("enter"),
            KeyCode::Esc => out.push_str("esc"),
            KeyCode::Backspace => out.push_str("backspace"),
            KeyCode::Delete => out.push_str("delete"),
            KeyCode::Up => out.push_str("up"),
            KeyCode::Down => out.push_str("down"),
            KeyCode::Left => out.push_str("left"),
            KeyCode::Right => out.push_str("right"),
            KeyCode::Home => out.push_str("home"),
            KeyCode::End => out.push_str("end"),
            KeyCode::PageUp => out.push_str("pageup"),
            KeyCode::PageDown => out.push_str("pagedown"),
            KeyCode::Insert => out.push_str("insert"),
            KeyCode::F(n) => {
                out.push('f');
                out.push_str(&n.to_string());
            }
            KeyCode::Char(' ') => out.push_str("space"),
            KeyCode::Char(c) => out.push(c.to_ascii_lowercase()),
            _ => out.push_str("unknown"),
        }
        out
    }
}

impl fmt::Display for Keystroke {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.format())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeystrokeError {
    Empty,
    UnknownModifier(String),
    MissingKey,
    UnknownKey(String),
    ChordNotSupported,
}

impl fmt::Display for KeystrokeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeystrokeError::Empty => f.write_str("keystroke is empty"),
            KeystrokeError::UnknownModifier(m) => write!(f, "unknown modifier `{m}`"),
            KeystrokeError::MissingKey => f.write_str("keystroke has a modifier but no key"),
            KeystrokeError::UnknownKey(k) => write!(f, "unknown key `{k}`"),
            KeystrokeError::ChordNotSupported => {
                f.write_str("chords are not supported; use a single keystroke")
            }
        }
    }
}

impl std::error::Error for KeystrokeError {}

/// Parses a keystroke string such as `ctrl+shift+t` or `esc`. One keystroke only: a
/// space-separated sequence is a chord, which this iteration does not support.
pub fn parse_keystroke(text: &str) -> Result<Keystroke, KeystrokeError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(KeystrokeError::Empty);
    }
    if text.contains(char::is_whitespace) {
        return Err(KeystrokeError::ChordNotSupported);
    }
    let lower = text.to_lowercase();
    if matches!(
        lower.as_str(),
        "ctrl" | "control" | "alt" | "meta" | "shift"
    ) {
        return Err(KeystrokeError::MissingKey);
    }
    // Everything before the last `+` is a modifier; a trailing `+` names the `+` key itself
    // (the spelled-out form `plus` exists for configs that find `ctrl++` unreadable).
    let mut segments: Vec<&str> = lower.split('+').collect();
    let key_name = if segments.last() == Some(&"") {
        // The final `+` names the plus key itself; only the segments before it are modifiers.
        segments.pop();
        if segments.last() == Some(&"") {
            // `ctrl++`: the empty middle segment is the plus key, preceded by modifiers.
            segments.pop();
        }
        if segments.iter().any(|segment| segment.is_empty()) {
            return Err(KeystrokeError::MissingKey);
        }
        "+"
    } else {
        segments.pop().expect("split yields at least one segment")
    };
    let mut modifiers = KeyModifiers::NONE;
    for segment in segments {
        match segment {
            "ctrl" | "control" => modifiers |= KeyModifiers::CONTROL,
            "alt" | "meta" => modifiers |= KeyModifiers::ALT,
            "shift" => modifiers |= KeyModifiers::SHIFT,
            other => return Err(KeystrokeError::UnknownModifier(other.to_string())),
        }
    }
    let code = match key_name {
        "backtab" => KeyCode::BackTab,
        "tab" => KeyCode::Tab,
        "enter" | "return" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "backspace" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        "insert" | "ins" => KeyCode::Insert,
        name if name.len() > 1
            && name.starts_with('f')
            && name[1..].bytes().all(|b| b.is_ascii_digit()) =>
        {
            let n: u8 = name[1..]
                .parse()
                .map_err(|_| KeystrokeError::UnknownKey(key_name.to_string()))?;
            if !(1..=24).contains(&n) {
                return Err(KeystrokeError::UnknownKey(key_name.to_string()));
            }
            KeyCode::F(n)
        }
        "space" => KeyCode::Char(' '),
        "plus" => KeyCode::Char('+'),
        s if s.chars().count() == 1 => {
            let c = s.chars().next().expect("one char");
            KeyCode::Char(c)
        }
        _ => return Err(KeystrokeError::UnknownKey(key_name.to_string())),
    };
    // `shift+tab` parses to `BackTab` so the default `cycle_mode` binding round-trips; the
    // shift stays implied by the code, not in the modifier set. The same holds for an
    // explicit `backtab`, which `matches` also delivers without SHIFT.
    if code == KeyCode::Tab && modifiers.contains(KeyModifiers::SHIFT) {
        return Ok(Keystroke {
            code: KeyCode::BackTab,
            modifiers: modifiers - KeyModifiers::SHIFT,
        });
    }
    if code == KeyCode::BackTab {
        return Ok(Keystroke {
            code,
            modifiers: modifiers - KeyModifiers::SHIFT,
        });
    }
    Ok(Keystroke { code, modifiers })
}

/// The remappable TUI actions. Defaults are documented in `docs/tui.md`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TuiAction {
    CycleMode,
    Interrupt,
    Quit,
    AgentsPanel,
    Todos,
    Transcript,
    Reprint,
    AttachClipboard,
    HistorySearch,
    HistoryPrevious,
    HistoryNext,
    Submit,
    Newline,
}

impl TuiAction {
    /// The action's snake_case name in config.
    pub fn as_str(self) -> &'static str {
        match self {
            TuiAction::CycleMode => "cycle_mode",
            TuiAction::Interrupt => "interrupt",
            TuiAction::Quit => "quit",
            TuiAction::AgentsPanel => "agents_panel",
            TuiAction::Todos => "todos",
            TuiAction::Transcript => "transcript",
            TuiAction::Reprint => "reprint",
            TuiAction::AttachClipboard => "attach_clipboard",
            TuiAction::HistorySearch => "history_search",
            TuiAction::HistoryPrevious => "history_previous",
            TuiAction::HistoryNext => "history_next",
            TuiAction::Submit => "submit",
            TuiAction::Newline => "newline",
        }
    }

    /// Every action, in definition order.
    pub const ALL: [TuiAction; 13] = [
        TuiAction::CycleMode,
        TuiAction::Interrupt,
        TuiAction::Quit,
        TuiAction::AgentsPanel,
        TuiAction::Todos,
        TuiAction::Transcript,
        TuiAction::Reprint,
        TuiAction::AttachClipboard,
        TuiAction::HistorySearch,
        TuiAction::HistoryPrevious,
        TuiAction::HistoryNext,
        TuiAction::Submit,
        TuiAction::Newline,
    ];

    /// The default keystroke for this action, `None` for an action with no default key
    /// (`agents_panel`, reached with `/agents`). The TUI's default table adds the two other
    /// `newline` bindings (`shift+enter`, `alt+enter`).
    pub fn default_keystroke(self) -> Option<Keystroke> {
        Some(match self {
            TuiAction::CycleMode => Keystroke {
                code: KeyCode::BackTab,
                modifiers: KeyModifiers::NONE,
            },
            TuiAction::Interrupt => Keystroke {
                code: KeyCode::Char('c'),
                modifiers: KeyModifiers::CONTROL,
            },
            TuiAction::Quit => Keystroke {
                code: KeyCode::Char('d'),
                modifiers: KeyModifiers::CONTROL,
            },
            TuiAction::AgentsPanel => return None,
            TuiAction::Todos => Keystroke {
                code: KeyCode::Char('t'),
                modifiers: KeyModifiers::CONTROL,
            },
            TuiAction::Transcript => Keystroke {
                code: KeyCode::Char('o'),
                modifiers: KeyModifiers::CONTROL,
            },
            TuiAction::Reprint => Keystroke {
                code: KeyCode::Char('l'),
                modifiers: KeyModifiers::CONTROL,
            },
            TuiAction::AttachClipboard => Keystroke {
                code: KeyCode::Char('v'),
                modifiers: KeyModifiers::CONTROL,
            },
            TuiAction::HistorySearch => Keystroke {
                code: KeyCode::Char('r'),
                modifiers: KeyModifiers::CONTROL,
            },
            TuiAction::HistoryPrevious => Keystroke {
                code: KeyCode::Up,
                modifiers: KeyModifiers::NONE,
            },
            TuiAction::HistoryNext => Keystroke {
                code: KeyCode::Down,
                modifiers: KeyModifiers::NONE,
            },
            TuiAction::Submit => Keystroke {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
            },
            TuiAction::Newline => Keystroke {
                code: KeyCode::Char('j'),
                modifiers: KeyModifiers::CONTROL,
            },
        })
    }
}

impl fmt::Display for TuiAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TuiActionError {
    Unknown(String),
}

impl fmt::Display for TuiActionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TuiActionError::Unknown(name) => write!(f, "unknown action `{name}`"),
        }
    }
}

impl std::error::Error for TuiActionError {}

impl FromStr for TuiAction {
    type Err = TuiActionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        TuiAction::ALL
            .into_iter()
            .find(|a| a.as_str() == s)
            .ok_or_else(|| TuiActionError::Unknown(s.to_string()))
    }
}

/// Keystrokes that may not be rebound or unbound because they must always work.
pub const RESERVED_KEYS: &[Keystroke] = &[
    Keystroke {
        code: KeyCode::Char('c'),
        modifiers: KeyModifiers::CONTROL,
    },
    Keystroke {
        code: KeyCode::Char('d'),
        modifiers: KeyModifiers::CONTROL,
    },
];

/// Whether `keystroke` is one of the reserved keys.
pub fn is_reserved(keystroke: Keystroke) -> bool {
    RESERVED_KEYS.contains(&keystroke)
}

/// Parses and validates one `[keybindings]` entry: the keystroke (the map key) and the action
/// (the value, empty to unbind). An entry naming a reserved keystroke, an unknown key or an
/// unknown action fails with the reason as the error text, so the config error can name it.
pub fn parse_binding(
    keystroke: &str,
    action: &str,
) -> Result<(Keystroke, Option<TuiAction>), String> {
    let key = parse_keystroke(keystroke).map_err(|e| e.to_string())?;
    if is_reserved(key) {
        return Err(format!(
            "`{keystroke}` is reserved and cannot be rebound or unbound"
        ));
    }
    if action.is_empty() {
        return Ok((key, None));
    }
    let action = TuiAction::from_str(action).map_err(|e| e.to_string())?;
    Ok((key, Some(action)))
}

/// Matches a crossterm event against a parsed keystroke. Terminals disagree on how they
/// deliver Shift+Tab (`BackTab`, or `Tab` carrying `SHIFT`), and on whether `BackTab` itself
/// carries `SHIFT`, so both deliveries normalize before the comparison; character codes
/// compare case-insensitively because a shifted letter event arrives uppercase.
pub fn matches(event: KeyEvent, keystroke: Keystroke) -> bool {
    let mut code = event.code;
    let mut modifiers = event.modifiers;
    if code == KeyCode::Tab && modifiers.contains(KeyModifiers::SHIFT) {
        code = KeyCode::BackTab;
    }
    if code == KeyCode::BackTab {
        modifiers -= KeyModifiers::SHIFT;
    }
    let code_matches = match (code, keystroke.code) {
        (KeyCode::Char(a), KeyCode::Char(b)) => a.eq_ignore_ascii_case(&b),
        (a, b) => a == b,
    };
    code_matches && modifiers == keystroke.modifiers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsing_is_case_insensitive() {
        let a = parse_keystroke("Ctrl+Shift+T").unwrap();
        let b = parse_keystroke("ctrl+shift+t").unwrap();
        assert_eq!(a, b);
        assert_eq!(a.code, KeyCode::Char('t'));
        assert!(
            a.modifiers
                .contains(KeyModifiers::CONTROL | KeyModifiers::SHIFT)
        );
    }

    #[test]
    fn letter_f_is_a_key_not_a_function_key() {
        let k = parse_keystroke("ctrl+f").unwrap();
        assert_eq!(k.code, KeyCode::Char('f'));
        assert_eq!(parse_keystroke("f5").unwrap().code, KeyCode::F(5));
    }

    #[test]
    fn shift_tab_normalizes_to_backtab() {
        let k = parse_keystroke("shift+tab").unwrap();
        assert_eq!(k.code, KeyCode::BackTab);
        assert!(!k.modifiers.contains(KeyModifiers::SHIFT));
        assert_eq!(k.format(), "shift+tab");
    }

    #[test]
    fn shift_backtab_normalizes_like_shift_tab() {
        let k = parse_keystroke("shift+backtab").unwrap();
        assert_eq!(k.code, KeyCode::BackTab);
        assert!(!k.modifiers.contains(KeyModifiers::SHIFT));
        assert!(matches(
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE),
            k
        ));
        assert!(matches(
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
            k
        ));
    }

    #[test]
    fn aliases_and_special_keys_parse() {
        assert_eq!(parse_keystroke("esc").unwrap().code, KeyCode::Esc);
        assert_eq!(parse_keystroke("return").unwrap().code, KeyCode::Enter);
        assert_eq!(
            parse_keystroke("meta+b").unwrap().modifiers,
            KeyModifiers::ALT
        );
        assert_eq!(parse_keystroke("space").unwrap().code, KeyCode::Char(' '));
        assert_eq!(parse_keystroke("plus").unwrap().code, KeyCode::Char('+'));
        assert_eq!(parse_keystroke("ctrl++").unwrap().code, KeyCode::Char('+'));
        assert_eq!(parse_keystroke("+").unwrap().code, KeyCode::Char('+'));
        assert_eq!(parse_keystroke("f2").unwrap().code, KeyCode::F(2));
        assert_eq!(parse_keystroke("F12").unwrap().code, KeyCode::F(12));
        assert!(parse_keystroke("f25").is_err());
        assert!(parse_keystroke("fn").is_err());
    }

    #[test]
    fn unknown_modifier_action_and_key_fail() {
        assert!(matches!(
            parse_keystroke("win+t"),
            Err(KeystrokeError::UnknownModifier(_))
        ));
        assert!(matches!(
            parse_keystroke("ctrl"),
            Err(KeystrokeError::MissingKey)
        ));
        assert!(matches!(
            parse_keystroke("shift"),
            Err(KeystrokeError::MissingKey)
        ));
        assert!(matches!(
            parse_keystroke("ctrl+fn"),
            Err(KeystrokeError::UnknownKey(_))
        ));
    }

    #[test]
    fn chords_are_rejected() {
        assert!(matches!(
            parse_keystroke("ctrl+k ctrl+c"),
            Err(KeystrokeError::ChordNotSupported)
        ));
    }

    #[test]
    fn action_parsing_round_trips() {
        for action in TuiAction::ALL {
            assert_eq!(TuiAction::from_str(action.as_str()).unwrap(), action);
        }
    }

    #[test]
    fn only_the_agents_panel_has_no_default_key() {
        for action in TuiAction::ALL {
            assert_eq!(
                action.default_keystroke().is_none(),
                action == TuiAction::AgentsPanel,
                "{action}"
            );
        }
    }

    #[test]
    fn unknown_action_errors() {
        assert!(matches!(
            TuiAction::from_str("nope"),
            Err(TuiActionError::Unknown(_))
        ));
    }

    #[test]
    fn reserved_keys_are_ctrl_c_and_ctrl_d() {
        assert!(is_reserved(parse_keystroke("ctrl+c").unwrap()));
        assert!(is_reserved(parse_keystroke("ctrl+d").unwrap()));
        assert!(!is_reserved(parse_keystroke("ctrl+t").unwrap()));
    }

    #[test]
    fn binding_parsing_accepts_unbind_and_rejects_reserved() {
        let (k, a) = parse_binding("ctrl+t", "agents_panel").unwrap();
        assert_eq!(k.code, KeyCode::Char('t'));
        assert_eq!(a, Some(TuiAction::AgentsPanel));

        let (k, a) = parse_binding("ctrl+t", "").unwrap();
        assert_eq!(a, None);
        assert_eq!(k.code, KeyCode::Char('t'));

        assert!(parse_binding("ctrl+c", "quit").is_err());
        assert!(parse_binding("ctrl+d", "").is_err());
        assert!(parse_binding("ctrl+t", "nope").is_err());
    }

    #[test]
    fn event_matching_normalizes_shift_tab_and_shifted_letters() {
        let k = parse_keystroke("shift+tab").unwrap();
        assert!(matches(KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT), k));
        assert!(matches(
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE),
            k
        ));
        assert!(matches(
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
            k
        ));

        let t = parse_keystroke("ctrl+t").unwrap();
        assert!(!matches(
            KeyEvent::new(
                KeyCode::Char('T'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            ),
            t
        ));
        let t = parse_keystroke("shift+t").unwrap();
        assert!(matches(
            KeyEvent::new(KeyCode::Char('T'), KeyModifiers::SHIFT),
            t
        ));
    }
}
