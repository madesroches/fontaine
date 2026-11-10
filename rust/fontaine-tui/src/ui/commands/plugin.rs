use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::Arc;

use fontaine_core::agent::{AgentRuntime, EnvLookup};
use fontaine_core::extensions::{self, Skill};
use fontaine_core::plugin_ops::{
    self, Fetch, GitMode, OpError, PreparedInstall, PreparedUpdate, Workdir, commit_install,
    commit_update, prepare_install, prepare_update,
};
use fontaine_core::plugins;
use fontaine_core::skill_trust;
use fontaine_tools::error_chain::chain;

use crate::plugin_prompt::{self, Change, Why};
use crate::skill_trust_prompt::{self, SkillTrustFooter};
use crate::{Action, Ui};

const USAGE: &str = "usage: /plugin, or /plugin install <name>@<marketplace>, uninstall <name>, update [<name>|all], enable <name>, disable <name>, marketplace add <url-or-path> [--name <name>], marketplace list, marketplace update [<name>], marketplace remove <name>";

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

    /// `/plugin <subcommand>`: installs, updates, removes, enables and disables plugins, and
    /// registers, updates and removes marketplaces, then loads the result into this session.
    pub(crate) async fn plugin_command(
        &mut self,
        runtime: &mut AgentRuntime,
        argument: &str,
    ) -> ControlFlow<()> {
        let words: Vec<&str> = argument.split_whitespace().collect();
        match words.as_slice() {
            ["install", spec] => {
                if !self.plugins_idle(runtime, "/plugin install") {
                    return ControlFlow::Continue(());
                }
                self.plugin_install(runtime, spec).await
            }
            ["update"] | ["update", "all"] => {
                if !self.plugins_idle(runtime, "/plugin update") {
                    return ControlFlow::Continue(());
                }
                self.plugin_update(runtime, None).await
            }
            ["update", name] => {
                if !self.plugins_idle(runtime, "/plugin update") {
                    return ControlFlow::Continue(());
                }
                self.plugin_update(runtime, Some(name)).await
            }
            ["uninstall", name] => {
                if self.plugins_idle(runtime, "/plugin uninstall") {
                    self.plugin_uninstall(runtime, name).await;
                }
                ControlFlow::Continue(())
            }
            [verb @ ("enable" | "disable"), name] => {
                if self.plugins_idle(runtime, &format!("/plugin {verb}")) {
                    self.plugin_toggle(runtime, name, *verb == "enable").await;
                }
                ControlFlow::Continue(())
            }
            ["marketplace", "list"] => {
                self.marketplace_list(runtime);
                ControlFlow::Continue(())
            }
            ["marketplace", "add", source] => {
                if !self.plugins_idle(runtime, "/plugin marketplace add") {
                    return ControlFlow::Continue(());
                }
                self.marketplace_add(runtime, source, None).await
            }
            ["marketplace", "add", source, "--name", name] => {
                if !self.plugins_idle(runtime, "/plugin marketplace add") {
                    return ControlFlow::Continue(());
                }
                self.marketplace_add(runtime, source, Some(name)).await
            }
            ["marketplace", "update"] => {
                if !self.plugins_idle(runtime, "/plugin marketplace update") {
                    return ControlFlow::Continue(());
                }
                self.marketplace_update(runtime, None).await
            }
            ["marketplace", "update", name] => {
                if !self.plugins_idle(runtime, "/plugin marketplace update") {
                    return ControlFlow::Continue(());
                }
                self.marketplace_update(runtime, Some(name)).await
            }
            ["marketplace", "remove", name] => {
                if !self.plugins_idle(runtime, "/plugin marketplace remove") {
                    return ControlFlow::Continue(());
                }
                self.marketplace_remove(runtime, name).await
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

    /// The `/skills trust` question for one skill of an install in progress. `None` means the
    /// interface is gone.
    async fn ask_skill_trust(
        &mut self,
        runtime: &AgentRuntime,
        skill: &Skill,
        previous: Option<&skill_trust::TrustRecord>,
    ) -> Option<bool> {
        self.app.begin_skill_trust(skill_trust_prompt::new(
            skill,
            previous,
            SkillTrustFooter::Install,
        ));
        loop {
            self.draw(runtime);
            let Some(input) = self.input.recv().await else {
                self.app.skill_trust_prompt = None;
                return None;
            };
            match self.app.handle_input(input) {
                Action::SkillTrust(answer) => return Some(answer),
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
                let hint = if e.is_unknown_marketplace() {
                    " (register it first with `/plugin marketplace add <url-or-path>`)".to_string()
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
        let (recorded, unreadable) = skill_trust::load(runtime.config_dir());
        if let Some(message) = unreadable {
            self.app.transcript.warning(message, None);
        }
        let trustable: Vec<Skill> = match extensions::load_plugin_skills(&prepared.build.dir) {
            Ok(skills) => skills
                .into_iter()
                .filter(|skill| !skill.declared_tools.is_empty())
                .filter(|skill| {
                    recorded.state(&skill.name, skill.scope, &skill.digest)
                        != skill_trust::TrustState::Trusted
                })
                .collect(),
            Err(e) => {
                drop(prepared);
                self.app
                    .transcript
                    .error(format!("/plugin install: {}", chain(&e)));
                self.app.transcript.notice("install cancelled");
                return ControlFlow::Continue(());
            }
        };
        let mut accepted: Vec<Skill> = Vec::new();
        let mut declined: Vec<String> = Vec::new();
        for skill in trustable {
            let previous = recorded.record(&skill.name);
            match self.ask_skill_trust(runtime, &skill, previous).await {
                None => {
                    drop(prepared);
                    self.app.transcript.notice("install cancelled");
                    return ControlFlow::Break(());
                }
                Some(true) => accepted.push(skill),
                Some(false) => declined.push(skill.name),
            }
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
                if !accepted.is_empty() {
                    match skill_trust::trust(
                        runtime.config_dir(),
                        accepted.iter().map(skill_trust::record_of).collect(),
                    ) {
                        Ok(()) => {
                            for skill in &accepted {
                                runtime.state.record_skill_trust(&skill.name, &skill.digest);
                                self.app.transcript.notice(format!(
                                    "`{}` trusted: its allowed-tools count from this reload onward",
                                    skill.name
                                ));
                            }
                        }
                        Err(e) => self
                            .app
                            .transcript
                            .error(format!("could not record the trust: {}", chain(&e))),
                    }
                }
                if !declined.is_empty() {
                    self.app.transcript.notice(format!(
                        "left untrusted: {}; `/skills trust <name>` or `fontaine plugin trust {}` honours them later",
                        declined.join(", "),
                        installed.name
                    ));
                }
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
        let mut touched: Vec<String> = Vec::new();
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
                    None => {
                        self.apply_plugin_changes(runtime, &touched).await;
                        return ControlFlow::Break(());
                    }
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
            touched.push(name.clone());
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
        }
        // One reload after every commit; a failed update is in it too, so what was stopped starts
        // again.
        if !touched.is_empty() {
            self.apply_plugin_changes(runtime, &touched).await;
        }
        ControlFlow::Continue(())
    }

    async fn plugin_uninstall(&mut self, runtime: &mut AgentRuntime, name: &str) {
        // Stopped before the checkout goes, so nothing runs from a directory that is deleted.
        let stopped = runtime.stop_plugin_servers(name).await;
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
                // A typo touched nothing: no reload then. A registry entry that is still there may
                // have lost its checkout, and what was stopped starts again.
                let still_installed = plugins::read_registry(runtime.config_dir())
                    .map_or(true, |r| r.plugin.iter().any(|p| p.name == name));
                if !stopped.is_empty() || still_installed {
                    self.apply_plugin_changes(runtime, &[name.to_string()])
                        .await;
                }
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

    /// `/plugin marketplace add`: registers the marketplace so `/plugin install` can name it.
    /// A marketplace loads nothing by itself, so the session is not reloaded.
    async fn marketplace_add(
        &mut self,
        runtime: &mut AgentRuntime,
        source: &str,
        name: Option<&str>,
    ) -> ControlFlow<()> {
        let work = Work::of(runtime);
        let owned = source.to_string();
        let owned_name = name.map(str::to_string);
        let added = work
            .run(move |fx| {
                plugin_ops::add_marketplace(fx, &owned, owned_name.as_deref(), Workdir::Auto)
            })
            .await;
        match added {
            Ok(added) => self.app.transcript.notice(format!(
                "registered marketplace `{}` ({} plugin(s))",
                added.name, added.plugins
            )),
            Err(e) => self
                .app
                .transcript
                .error(format!("/plugin marketplace add: {}", chain(&e))),
        }
        ControlFlow::Continue(())
    }

    /// `/plugin marketplace list`: one line per registered marketplace, like the CLI.
    fn marketplace_list(&mut self, runtime: &AgentRuntime) {
        let registry = match plugins::read_registry(runtime.config_dir()) {
            Ok(registry) => registry,
            Err(e) => {
                self.app
                    .transcript
                    .error(format!("/plugin marketplace list: {}", chain(&e)));
                return;
            }
        };
        if registry.marketplace.is_empty() {
            self.app.transcript.notice("no marketplaces registered");
            return;
        }
        for marketplace in &registry.marketplace {
            let installed = registry
                .plugin
                .iter()
                .filter(|p| p.marketplace == marketplace.name)
                .count();
            let offered = match plugin_ops::read_catalog(&plugins::marketplace_dir(
                runtime.config_dir(),
                &marketplace.name,
            )) {
                Ok(catalog) => Some(catalog.plugins.len()),
                Err(e) => {
                    self.app
                        .transcript
                        .warning(format!("marketplace `{}`: {e}", marketplace.name), None);
                    None
                }
            };
            self.app.transcript.notice(plugin_ops::marketplace_line(
                marketplace,
                offered,
                installed,
            ));
        }
    }

    /// `/plugin marketplace update`: moves the pin to the upstream revision. Installed plugins
    /// are not touched; a path-source plugin follows on its next update.
    async fn marketplace_update(
        &mut self,
        runtime: &mut AgentRuntime,
        name: Option<&str>,
    ) -> ControlFlow<()> {
        let work = Work::of(runtime);
        let owned = name.map(str::to_string);
        let updated = work
            .run(move |fx| plugin_ops::update_marketplace_named(fx, owned.as_deref()))
            .await;
        match updated {
            Ok(plugin_ops::MarketplaceUpdate::UpToDate { name, pin }) => {
                self.app.transcript.notice(format!(
                    "marketplace `{name}` is already up to date ({})",
                    plugins::short_pin(&pin)
                ))
            }
            Ok(plugin_ops::MarketplaceUpdate::Moved { name, old, new }) => {
                self.app.transcript.notice(format!(
                    "marketplace `{name}` moved {} -> {}",
                    plugins::short_pin(&old),
                    plugins::short_pin(&new)
                ))
            }
            Err(e) => self
                .app
                .transcript
                .error(format!("/plugin marketplace update: {}", chain(&e))),
        }
        ControlFlow::Continue(())
    }

    /// `/plugin marketplace remove`: drops the checkout and the entry. Refuses while an
    /// installed plugin names the marketplace.
    async fn marketplace_remove(
        &mut self,
        runtime: &mut AgentRuntime,
        name: &str,
    ) -> ControlFlow<()> {
        let config_dir = runtime.config_dir().to_path_buf();
        let owned = name.to_string();
        let removed = tokio::task::spawn_blocking(move || {
            plugin_ops::remove_marketplace(&config_dir, &owned)
        })
        .await
        .map_err(|e| OpError::caused("the plugin operation did not finish", e))
        .and_then(|result| result);
        match removed {
            Ok(()) => self
                .app
                .transcript
                .notice(format!("removed marketplace `{name}`")),
            Err(e) => self
                .app
                .transcript
                .error(format!("/plugin marketplace remove: {}", chain(&e))),
        }
        ControlFlow::Continue(())
    }
}
