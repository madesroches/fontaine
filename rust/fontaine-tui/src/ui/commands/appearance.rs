use crate::theme::Theme;
use crate::{App, Ui};
use fontaine_core::agent::AgentRuntime;
use fontaine_core::config::ThemeName;
use fontaine_core::user_config;

impl<R: FnMut(&mut App)> Ui<R> {
    /// `/theme`: lists the themes, or switches to one and saves the choice.
    pub(crate) fn theme(&mut self, name: &str) {
        let current = ThemeName::ALL
            .into_iter()
            .find(|t| Theme::preset(*t) == self.app.theme);
        if name.is_empty() {
            let names: Vec<String> = ThemeName::ALL
                .iter()
                .map(|t| {
                    if Some(*t) == current {
                        format!("{} (current)", t.as_str())
                    } else {
                        t.as_str().to_string()
                    }
                })
                .collect();
            let saved = match &self.user_config {
                Some(path) => format!("saved to {}", path.display()),
                None => "not saved: there is no user settings file".to_string(),
            };
            self.app.transcript.notice(format!(
                "themes: {} · /theme <name> switches; the choice is {saved}",
                names.join(", ")
            ));
            return;
        }
        let Some(chosen) = ThemeName::parse(name) else {
            let valid: Vec<&str> = ThemeName::ALL.iter().map(|t| t.as_str()).collect();
            self.app.transcript.error(format!(
                "unknown theme `{name}`; the themes are {}",
                valid.join(", ")
            ));
            return;
        };
        self.app.theme = Theme::preset(chosen);
        let persist_error = match &self.user_config {
            Some(path) => user_config::set_theme(path, chosen).err().map(|e| {
                format!(
                    "could not save it to {}: {}",
                    path.display(),
                    fontaine_tools::error_chain::chain(&e)
                )
            }),
            None => Some("there is no user settings file to save it to".to_string()),
        };
        self.finish_switch(format!("theme is now {}", chosen.as_str()), persist_error);
    }

    /// `/output-style`: shows the styles, or switches to one for the next turn.
    pub(crate) fn output_style(&mut self, runtime: &mut AgentRuntime, name: &str) {
        if name.is_empty() {
            let known: Vec<String> = runtime
                .extensions()
                .output_styles
                .iter()
                .map(|s| format!("{} ({})", s.name, s.description))
                .collect();
            let current = runtime.output_style().unwrap_or("none");
            self.app.transcript.notice(format!(
                "output style: {current} · available: {}",
                if known.is_empty() {
                    "none".to_string()
                } else {
                    known.join(", ")
                }
            ));
            return;
        }
        let choice = (name != "none").then_some(name);
        match runtime.set_output_style(choice) {
            Ok(()) => self
                .app
                .transcript
                .notice(format!("output style is now {}", choice.unwrap_or("none"))),
            Err(e) => self.app.transcript.error(format!("/output-style: {e}")),
        }
    }
}
