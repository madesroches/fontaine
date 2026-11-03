use crate::{App, Ui};
use fontaine_core::config::EditorMode;
use fontaine_core::user_config;
use toml_edit::Value;

impl<R: FnMut(&mut App)> Ui<R> {
    /// `/vim`: toggles the input box's editing mode, saves the choice to the user file and
    /// applies it to the running session, where the mode is read on each keypress.
    pub(crate) fn vim(&mut self) {
        let new_mode = match self.app.input.mode {
            EditorMode::Emacs => EditorMode::Vim,
            EditorMode::Vim => EditorMode::Emacs,
        };
        self.app.input.mode = new_mode;
        self.app.input.vim = Default::default();
        let value = Value::from(new_mode.as_str());
        let persist_error = match &self.user_config {
            Some(path) => user_config::set_config_value(path, "tui.editor_mode", Some(value))
                .err()
                .map(|e| {
                    format!(
                        "could not save it to {}: {}",
                        path.display(),
                        fontaine_tools::error_chain::chain(&e)
                    )
                }),
            None => Some("there is no user settings file to save it to".to_string()),
        };
        let message = match new_mode {
            EditorMode::Vim => {
                "vim mode on (esc leaves insert, i returns; the status bar shows the state)"
            }
            EditorMode::Emacs => "emacs mode on (readline-style editing)",
        }
        .to_string();
        self.finish_switch(message, persist_error, None);
    }
}
