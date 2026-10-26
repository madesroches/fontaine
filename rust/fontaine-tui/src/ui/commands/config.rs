use crate::config_panel::{ConfigKey, ConfigPanel, value_label};
use crate::{App, Ui};
use fontaine_core::agent::AgentRuntime;
use fontaine_core::config::{Config, Slot, ThemeName};
use fontaine_core::config_catalog::{self, Applies, ModelSlot, Running};
use fontaine_core::user_config;

/// The scope that outranks the user file for `path`, whose value therefore wins from the next
/// launch whatever a save to the user file says.
pub(crate) fn overriding_scope(config: &Config, path: &str) -> Option<Slot> {
    config_catalog::winning_slot(config, path).filter(|slot| *slot != Slot::User)
}

impl<R: FnMut(&mut App)> Ui<R> {
    /// `/config`: opens the settings panel.
    pub(crate) fn open_config(&mut self, runtime: &AgentRuntime) {
        let theme = self.current_theme().unwrap_or(runtime.config.tui.theme);
        let main = runtime.models.main.reference.to_string();
        let guardian = runtime.state.guardian();
        let running = Running {
            theme: theme.as_str(),
            main_model: &main,
            guardian_model: Some(guardian.label()).filter(|label| !label.is_empty()),
        };
        self.app.config = Some(ConfigPanel::new(
            &runtime.config,
            &running,
            self.user_config.as_deref(),
            &self.config_pending,
        ));
    }

    /// The panel's picker rows hand over to the picker the matching command opens.
    pub(crate) async fn open_config_picker(&mut self, runtime: &mut AgentRuntime, slot: ModelSlot) {
        match slot {
            ModelSlot::Main => self.open_model_picker(runtime, None).await,
            ModelSlot::Guardian => self.open_guardian_picker(runtime),
        }
    }

    /// Writes a change the panel asked for to the user file, then tells the panel how it went.
    pub(crate) fn config_change(&mut self, runtime: &AgentRuntime, change: ConfigKey) {
        let (path, value) = match change {
            ConfigKey::Save { path, value } => (path, Some(value)),
            ConfigKey::Reset(path) => (path, None),
            ConfigKey::None | ConfigKey::Close | ConfigKey::OpenPicker(_) => return,
        };
        let Some(file) = self.user_config.clone() else {
            if let Some(panel) = &mut self.app.config {
                panel.failed("there is no user settings file");
            }
            return;
        };
        if let Err(e) = user_config::set_config_value(&file, path, value.clone()) {
            if let Some(panel) = &mut self.app.config {
                panel.failed(&fontaine_tools::error_chain::chain(&e));
            }
            return;
        }
        let Some(row) = config_catalog::find(path) else {
            return;
        };
        let label = value
            .as_ref()
            .map_or_else(|| "unset".to_string(), value_label);
        let live = (row.applies == Applies::Live && path == "tui.theme").then(|| {
            let theme = value
                .as_ref()
                .and_then(|v| v.as_str())
                .and_then(ThemeName::parse)
                .unwrap_or_default();
            self.apply_theme(theme);
            theme.as_str()
        });
        let overridden = overriding_scope(&runtime.config, path);
        let Some(panel) = &mut self.app.config else {
            return;
        };
        match panel.saved(path, &label, live, overridden) {
            Some(pending) => {
                self.config_pending.insert(path.to_string(), pending);
            }
            None => {
                self.config_pending.remove(path);
            }
        }
    }
}
