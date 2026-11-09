use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::Arc;

use fontaine_core::agent::{AgentRuntime, EnvLookup};
use fontaine_core::plugin_ops::{
    self, Fetch, GitMode, OpError, PreparedInstall, PreparedUpdate, Workdir, commit_install,
    commit_update, prepare_install, prepare_update,
};
use fontaine_core::plugins;
use fontaine_tools::error_chain::chain;

use crate::plugin_prompt::{self, Change, Why};
use crate::{Action, Ui};

const USAGE: &str = "usage: /plugin, or /plugin install <name>@<marketplace>, uninstall <name>, update [<name>|all], enable <name>, disable <name>";

/// Marketplace registration stays on the command line, so the message says where.
const NO_MARKETPLACE: &str = "register a marketplace first with `fontaine plugin marketplace add`";

/// Fetching and committing shell out to git and touch the disk, so they run off the interface's
/// thread. Git never prompts on the terminal here: a credential request fails.
struct Work {
    config_dir: PathBuf,
    env: EnvLookup,
}

impl Work {
    fn of(runtime: &AgentRuntime) -> Work {
        Work {
            config_dir: runtime.config_dir().to_path_buf(),
            env: Arc::clone(runtime.env()),
        }
    }

    async fn run<T: Send + 'static>(
        &self,
        job: impl FnOnce(&Fetch<'_>) -> Result<T, OpError> + Send + 'static,
    ) -> Result<T, OpError> {
        let config_dir = self.config_dir.clone();
        let env = Arc::clone(&self.env);
        tokio::task::spawn_blocking(move || {
            let fetch = Fetch::new(&config_dir, &*env, GitMode::Unattended);
            job(&fetch)
        })
        .await
        .map_err(|e| OpError::caused("the plugin operation did not finish", e))?
    }
}

impl<R: FnMut(&mut crate::App)> Ui<R> {
    /// The command-menu rows follow the skills the session has now.
    fn refresh_menu(&mut self, runtime: &AgentRuntime) {
        let (skill_rows, _hidden) = crate::commands::skill_menu(runtime.slash_skills());
        self.app.menu = crate::commands::builtin_menu();
        self.app.menu.extend(skill_rows);
        self.app.menu.sort_by(|a, b| a.name.cmp(&b.name));
    }

    /// A plugin change runs between turns, with nothing in the background that the changed
    /// toolset could surprise. Says so and returns false when a subagent is still live.
    fn plugins_idle(&mut self, runtime: &AgentRuntime, command: &str) -> bool {
        if runtime.subagents_idle() {
            return true;
        }
        self.app.transcript.error(format!(
            "subagents are still running or have reports, messages or board reminders to deliver; {command} is refused until they finish or are killed"
        ));
        false
    }

    /// Applies what the registry and the extension directories hold now, then tells the command
    /// menu. The report reaches the transcript through the session's notices.
    async fn apply_plugin_changes(&mut self, runtime: &mut AgentRuntime, changed: &[String]) {
        runtime.reload_extensions(changed).await;
        self.refresh_menu(runtime);
    }

    /// `/reload-plugins`: picks up added, edited and removed skills, agents and output styles in
    /// the user, project and plugin directories and applies staged plugin updates, without
    /// losing the conversation.
    pub(crate) async fn reload_plugins(&mut self, runtime: &mut AgentRuntime) {
        if !self.plugins_idle(runtime, "/reload-plugins") {
            return;
        }
        self.apply_plugin_changes(runtime, &[]).await;
    }

    /// `/plugin <subcommand>`: installs, updates, removes, enables and disables plugins, then
    /// loads the result into this session. Marketplace registration stays on the command line.
    pub(crate) async fn plugin_command(
        &mut self,
        runtime: &mut AgentRuntime,
        argument: &str,
    ) -> ControlFlow<()> {
        let mut words = argument.split_whitespace();
        let (verb, name, extra) = (words.next(), words.next(), words.next());
        if extra.is_some() {
            self.app.transcript.error(USAGE);
            return ControlFlow::Continue(());
        }
        let command = format!("/plugin {}", verb.unwrap_or_default());
        match (verb, name) {
            (Some("install"), Some(spec)) => {
                if !self.plugins_idle(runtime, &command) {
                    return ControlFlow::Continue(());
                }
                self.plugin_install(runtime, spec).await
            }
            (Some("update"), name) => {
                if !self.plugins_idle(runtime, &command) {
                    return ControlFlow::Continue(());
                }
                self.plugin_update(runtime, name.filter(|n| *n != "all"))
                    .await
            }
            (Some("uninstall"), Some(name)) => {
                if self.plugins_idle(runtime, &command) {
                    self.plugin_uninstall(runtime, name).await;
                }
                ControlFlow::Continue(())
            }
            (Some(verb @ ("enable" | "disable")), Some(name)) => {
                if self.plugins_idle(runtime, &command) {
                    self.plugin_toggle(runtime, name, verb == "enable").await;
                }
                ControlFlow::Continue(())
            }
            _ => {
                self.app.transcript.error(USAGE);
                ControlFlow::Continue(())
            }
        }
    }

    /// Shows the surface for approval and waits for the answer. `None` means the interface is
    /// gone.
    async fn confirm_plugin(
        &mut self,
        runtime: &AgentRuntime,
        change: Change,
        plugin: &str,
        summary: &[String],
        fingerprint: Option<&str>,
    ) -> Option<bool> {
        // The dialog is cut to fit; the transcript keeps all of it.
        self.app.transcript.notice(summary.join("\n"));
        self.app
            .begin_plugin_prompt(plugin_prompt::new(change, plugin, summary, fingerprint));
        loop {
            self.draw(runtime);
            let Some(input) = self.input.recv().await else {
                self.app.plugin_prompt = None;
                return None;
            };
            match self.app.handle_input(input) {
                Action::PluginApprove(answer) => return Some(answer),
                Action::Quit => return None,
                _ => {}
            }
        }
    }

    async fn plugin_install(&mut self, runtime: &mut AgentRuntime, spec: &str) -> ControlFlow<()> {
        let work = Work::of(runtime);
        let owned = spec.to_string();
        let prepared: PreparedInstall = match work
            .run(move |fx| prepare_install(fx, &owned, Workdir::Auto))
            .await
        {
            Ok(prepared) => prepared,
            Err(e) => {
                let hint = if chain(&e).contains("is registered")
                    && chain(&e).contains("no marketplace")
                {
                    format!(" ({NO_MARKETPLACE})")
                } else {
                    String::new()
                };
                self.app
                    .transcript
                    .error(format!("/plugin install: {}{hint}", chain(&e)));
                return ControlFlow::Continue(());
            }
        };
        for warning in prepared.warnings.iter().chain(&prepared.build.warnings) {
            self.app.transcript.warning(warning.clone(), None);
        }
        if let Some(warning) = &prepared.fingerprint_warning {
            self.app.transcript.warning(warning.clone(), None);
        }
        let name = prepared.name.clone();
        let fingerprint = prepared.fingerprint.clone();
        let summary = prepared.summary.clone();
        let answer = self
            .confirm_plugin(
                runtime,
                Change::Install,
                &name,
                &summary,
                fingerprint.as_deref(),
            )
            .await;
        match answer {
            None => return ControlFlow::Break(()),
            Some(false) => {
                // Dropping the prepared install removes its scratch directory.
                drop(prepared);
                self.app.transcript.notice("install cancelled");
                return ControlFlow::Continue(());
            }
            Some(true) => {}
        }
        match work.run(move |fx| commit_install(fx, prepared)).await {
            Ok(installed) => {
                runtime.state.record_plugin_change(
                    "install",
                    &installed.name,
                    installed.fingerprint.as_deref().unwrap_or(""),
                );
                self.app.transcript.notice(format!(
                    "installed plugin `{}` ({})",
                    installed.name,
                    plugins::short_pin(&installed.pin)
                ));
                self.apply_plugin_changes(runtime, &[]).await;
            }
            Err(e) => self
                .app
                .transcript
                .error(format!("/plugin install: {}", chain(&e))),
        }
        ControlFlow::Continue(())
    }

    async fn plugin_update(
        &mut self,
        runtime: &mut AgentRuntime,
        only: Option<&str>,
    ) -> ControlFlow<()> {
        let names: Vec<String> = match only {
            Some(name) => vec![name.to_string()],
            None => match plugins::read_registry(runtime.config_dir()) {
                Ok(registry) => registry.plugin.into_iter().map(|p| p.name).collect(),
                Err(e) => {
                    self.app
                        .transcript
                        .error(format!("/plugin update: {}", chain(&e)));
                    return ControlFlow::Continue(());
                }
            },
        };
        if names.is_empty() {
            self.app.transcript.notice("no plugins are installed");
            return ControlFlow::Continue(());
        }
        let work = Work::of(runtime);
        for name in names {
            let owned = name.clone();
            let prepared: PreparedUpdate = match work
                .run(move |fx| prepare_update(fx, &owned, Workdir::Auto))
                .await
            {
                Ok(prepared) => prepared,
                Err(e) => {
                    self.app
                        .transcript
                        .error(format!("/plugin update {name}: {}", chain(&e)));
                    continue;
                }
            };
            if prepared.new_pin == prepared.old_pin && !prepared.needs_confirmation() {
                self.app.transcript.notice(format!(
                    "plugin `{name}` is already up to date ({})",
                    plugins::short_pin(&prepared.old_pin)
                ));
                continue;
            }
            for warning in prepared.warnings.iter().chain(&prepared.build.warnings) {
                self.app.transcript.warning(warning.clone(), None);
            }
            if let Some(warning) = &prepared.fingerprint_warning {
                self.app.transcript.warning(warning.clone(), None);
            }
            if prepared.needs_confirmation() {
                let fingerprint = prepared.fingerprint.clone();
                let summary = prepared.summary.clone();
                let why = if fingerprint.is_none() {
                    Why::NoFingerprint
                } else if prepared.recorded.is_none() {
                    Why::NoRecordedApproval
                } else {
                    Why::Differs
                };
                match self
                    .confirm_plugin(
                        runtime,
                        Change::Update(why),
                        &name,
                        &summary,
                        fingerprint.as_deref(),
                    )
                    .await
                {
                    None => return ControlFlow::Break(()),
                    Some(false) => {
                        drop(prepared);
                        self.app
                            .transcript
                            .notice(format!("update of `{name}` cancelled"));
                        continue;
                    }
                    Some(true) => {}
                }
            }
            // Nothing may run from the old checkout while it is replaced.
            runtime.stop_plugin_servers(&name).await;
            let fingerprint = prepared.fingerprint.clone().unwrap_or_default();
            match work.run(move |fx| commit_update(fx, prepared)).await {
                Ok(updated) => {
                    runtime
                        .state
                        .record_plugin_change("update", &name, &fingerprint);
                    self.app.transcript.notice(format!(
                        "plugin `{name}` moved to {}",
                        plugins::short_pin(&updated.new_pin)
                    ));
                }
                Err(e) => self
                    .app
                    .transcript
                    .error(format!("/plugin update {name}: {}", chain(&e))),
            }
            // A failed update reloads too: it starts again what was stopped.
            self.apply_plugin_changes(runtime, std::slice::from_ref(&name))
                .await;
        }
        ControlFlow::Continue(())
    }

    async fn plugin_uninstall(&mut self, runtime: &mut AgentRuntime, name: &str) {
        // Stopped before the checkout goes, so nothing runs from a directory that is deleted.
        runtime.stop_plugin_servers(name).await;
        let config_dir = runtime.config_dir().to_path_buf();
        let owned = name.to_string();
        let removed =
            tokio::task::spawn_blocking(move || plugin_ops::uninstall(&config_dir, &owned))
                .await
                .map_err(|e| OpError::caused("the plugin operation did not finish", e))
                .and_then(|result| result);
        match removed {
            Ok(removed) => {
                for warning in &removed.warnings {
                    self.app.transcript.warning(warning.clone(), None);
                }
                runtime.state.record_plugin_change("uninstall", name, "");
                self.app
                    .transcript
                    .notice(format!("uninstalled plugin `{name}`"));
                self.apply_plugin_changes(runtime, &[]).await;
            }
            Err(e) => {
                self.app
                    .transcript
                    .error(format!("/plugin uninstall: {}", chain(&e)));
                // What was stopped starts again.
                self.apply_plugin_changes(runtime, &[name.to_string()])
                    .await;
            }
        }
    }

    async fn plugin_toggle(&mut self, runtime: &mut AgentRuntime, name: &str, enabled: bool) {
        let verb = if enabled { "enable" } else { "disable" };
        let config_dir = runtime.config_dir().to_path_buf();
        let owned = name.to_string();
        let result = tokio::task::spawn_blocking(move || {
            plugin_ops::set_enabled(&config_dir, &owned, enabled)
        })
        .await
        .map_err(|e| OpError::caused("the plugin operation did not finish", e))
        .and_then(|result| result);
        match result {
            Ok(()) => {
                runtime.state.record_plugin_change(verb, name, "");
                self.app
                    .transcript
                    .notice(format!("plugin `{name}` is now {verb}d"));
                self.apply_plugin_changes(runtime, &[]).await;
            }
            Err(e) => self
                .app
                .transcript
                .error(format!("/plugin {verb}: {}", chain(&e))),
        }
    }
}
