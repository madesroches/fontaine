use std::fmt::Write as _;

use crate::{App, Ui};

impl<R: FnMut(&mut App)> Ui<R> {
    /// `/keybindings`: the effective keystroke→action table, marking the user's overrides and
    /// naming the file to edit. Viewing only; there is no in-app editor.
    pub(crate) fn keybindings(&mut self) {
        let mut out = String::from("keybindings (keystroke = action):");
        for (action, keys, overridden) in self.app.keymap.rows() {
            let keys = if keys.is_empty() {
                "unbound".to_string()
            } else {
                keys.iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let marker = if overridden { "overridden" } else { "default" };
            let _ = writeln!(out, "\n  {action:<16} {keys:<24} {marker}");
        }
        match &self.user_config {
            Some(path) => {
                let _ = write!(
                    out,
                    "\nedit them in {} under [keybindings]; changes apply on the next launch",
                    path.display()
                );
            }
            None => {
                out.push_str(
                    "\nthere is no user settings file; start one with a [keybindings] table",
                );
            }
        }
        self.app.transcript.notice(out);
    }
}
