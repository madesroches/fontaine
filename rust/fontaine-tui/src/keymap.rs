//! The effective keystroke→action table the App dispatches on, built from the defaults plus
//! the user's `[keybindings]` overrides. Parsing and validation live in `fontaine-core`; this
//! module only resolves and answers lookups.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use fontaine_core::keybindings::{Keystroke, TuiAction, matches, parse_binding};
use ratatui::crossterm::event::KeyEvent;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeymapError {
    pub message: String,
}

impl fmt::Display for KeymapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for KeymapError {}

/// The keystroke→action table in force. Config validation has already parsed and scope-checked
/// every entry, so a resolve failure here means the table was built outside `config::load`
/// (a test, a hand-made map) and the error names the offending entry.
#[derive(Clone, Debug)]
pub struct Keymap {
    bindings: BTreeMap<Keystroke, TuiAction>,
    /// The keystrokes the user supplied, which win over defaults and mark their action
    /// "overridden" in the `/keybindings` report.
    overridden: BTreeSet<Keystroke>,
}

/// The built-in table: one entry per default keystroke. `newline` answers to three keys.
fn defaults() -> Vec<(Keystroke, TuiAction)> {
    use fontaine_core::keybindings::parse_keystroke as k;
    [
        ("shift+tab", TuiAction::CycleMode),
        ("ctrl+c", TuiAction::Interrupt),
        ("ctrl+d", TuiAction::Quit),
        ("ctrl+t", TuiAction::AgentsPanel),
        ("ctrl+o", TuiAction::Todos),
        ("ctrl+l", TuiAction::Reprint),
        ("ctrl+v", TuiAction::AttachClipboard),
        ("ctrl+r", TuiAction::HistorySearch),
        ("up", TuiAction::HistoryPrevious),
        ("down", TuiAction::HistoryNext),
        ("enter", TuiAction::Submit),
        ("shift+enter", TuiAction::Newline),
        ("alt+enter", TuiAction::Newline),
        ("ctrl+j", TuiAction::Newline),
    ]
    .into_iter()
    .map(|(stroke, action)| (k(stroke).expect("default keystrokes parse"), action))
    .collect()
}

impl Keymap {
    /// The default table, with `overrides` applied: a keystroke names the action it now
    /// triggers (replacing any action it had), and an empty action value unbinds it.
    pub fn resolve(overrides: &BTreeMap<String, String>) -> Result<Keymap, KeymapError> {
        let mut bindings: BTreeMap<Keystroke, TuiAction> = defaults().into_iter().collect();
        let mut overridden = BTreeSet::new();
        for (keystroke, action) in overrides {
            let (key, action) =
                parse_binding(keystroke, action).map_err(|message| KeymapError {
                    message: format!("keybindings.{keystroke}: {message}"),
                })?;
            match action {
                Some(action) => {
                    bindings.insert(key, action);
                }
                None => {
                    bindings.remove(&key);
                }
            }
            overridden.insert(key);
        }
        Ok(Keymap {
            bindings,
            overridden,
        })
    }

    /// The action `key` triggers, if any.
    pub fn lookup(&self, key: KeyEvent) -> Option<TuiAction> {
        self.bindings
            .iter()
            .find(|(stroke, _)| matches(key, **stroke))
            .map(|(_, action)| *action)
    }

    /// The keystrokes bound to `action`, for the hint strings; empty when unbound.
    pub fn keys_for(&self, action: TuiAction) -> Vec<Keystroke> {
        self.bindings
            .iter()
            .filter(|(_, bound)| **bound == action)
            .map(|(stroke, _)| *stroke)
            .collect()
    }

    /// Whether `action`'s binding came from the user rather than the defaults.
    pub fn is_overridden(&self, action: TuiAction) -> bool {
        self.keys_for(action)
            .iter()
            .any(|stroke| self.overridden.contains(stroke))
    }

    /// One row per action with any binding, in action order: the action, its keystrokes as
    /// they would be written in the config, and whether the binding is the user's.
    pub fn rows(&self) -> Vec<(TuiAction, Vec<Keystroke>, bool)> {
        TuiAction::ALL
            .into_iter()
            .map(|action| {
                let keys = self.keys_for(action);
                let overridden = self.is_overridden(action);
                (action, keys, overridden)
            })
            .collect()
    }
}

impl Default for Keymap {
    fn default() -> Keymap {
        Keymap {
            bindings: defaults().into_iter().collect(),
            overridden: BTreeSet::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fontaine_core::keybindings::parse_keystroke;
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    fn map(entries: &[(&str, &str)]) -> Keymap {
        Keymap::resolve(
            &entries
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
        )
        .unwrap()
    }

    #[test]
    fn defaults_cover_every_action() {
        let keymap = Keymap::default();
        for action in TuiAction::ALL {
            assert!(!keymap.keys_for(action).is_empty(), "{action}");
        }
        assert_eq!(
            keymap.lookup(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE)),
            Some(TuiAction::CycleMode)
        );
        assert_eq!(
            keymap.lookup(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL)),
            Some(TuiAction::Newline)
        );
        assert_eq!(
            keymap.lookup(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)),
            Some(TuiAction::Newline)
        );
    }

    #[test]
    fn an_override_replaces_the_action_of_its_keystroke() {
        let keymap = map(&[("ctrl+t", "todos")]);
        assert_eq!(
            keymap.lookup(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL)),
            Some(TuiAction::Todos)
        );
        // The action the keystroke had is left without a key.
        assert!(keymap.keys_for(TuiAction::AgentsPanel).is_empty());
        assert!(keymap.is_overridden(TuiAction::Todos));
    }

    #[test]
    fn an_empty_value_unbinds_the_keystroke() {
        let keymap = map(&[("ctrl+t", "")]);
        assert_eq!(
            keymap.lookup(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL)),
            None
        );
    }

    #[test]
    fn the_default_cycle_key_survives_shift_tab_variants() {
        let keymap = Keymap::default();
        for event in [
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE),
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
            KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT),
        ] {
            assert_eq!(keymap.lookup(event), Some(TuiAction::CycleMode));
        }
    }

    #[test]
    fn a_reserved_or_unknown_entry_is_an_error() {
        assert!(
            Keymap::resolve(
                &[("ctrl+c".to_string(), "quit".to_string())]
                    .into_iter()
                    .collect()
            )
            .is_err()
        );
        assert!(
            Keymap::resolve(
                &[("ctrl+t".to_string(), "nope".to_string())]
                    .into_iter()
                    .collect()
            )
            .is_err()
        );
    }

    #[test]
    fn keys_for_renders_as_config_syntax() {
        let keymap = map(&[("f2", "agents_panel"), ("ctrl+t", "")]);
        let keys = keymap.keys_for(TuiAction::AgentsPanel);
        assert_eq!(keys, vec![parse_keystroke("f2").unwrap()]);
        assert_eq!(keys[0].to_string(), "f2");
        assert_eq!(
            parse_keystroke("shift+tab").unwrap().to_string(),
            "shift+tab"
        );
    }
}
