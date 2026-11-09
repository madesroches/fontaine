//! Applying a changed set of plugins and skills to a running session without losing the
//! conversation: what the registry and the extension directories hold now replaces what the
//! session loaded, plugin by plugin, and only what differs is touched.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use fontaine_agents::registry::AgentTypes;
use fontaine_tools::error_chain::chain;
use sha2::{Digest, Sha256};

use super::{AgentRuntime, AgentSetup, mcp::McpOutcome, setup};
use crate::config::{Config, LspServer, McpServer};
use crate::extensions::{self, Extensions, LoadParams};
use crate::permission_rules::SourcedRule;
use crate::plugin_ops;
use crate::plugins::{
    self, PluginBuild, PluginContext, PluginContributions, PluginHooks, registry_path,
};
use crate::skill_trust;
use crate::user_hooks::{self, HookLayer};

/// The note a reload leaves for the next turn, since the conversation is never edited.
const STALE_NOTE: &str =
    "skill content or tool results from these earlier in this conversation may be stale";

/// What one plugin contributes, as far as a reload can tell whether it changed: the content of
/// its component files and the definitions of everything it runs or enforces.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct PluginSig {
    files: BTreeMap<PathBuf, String>,
    manifests: Vec<PathBuf>,
    hooks: PluginHooks,
    servers: BTreeMap<String, McpServer>,
    lsp: Vec<(String, LspServer)>,
    deny: Vec<SourcedRule>,
    ask: Vec<SourcedRule>,
    hard_deny: Vec<String>,
    soft_deny: Vec<String>,
}

fn digest_of(path: &Path) -> String {
    match std::fs::read(path) {
        Ok(bytes) => Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
        Err(_) => "unreadable".to_string(),
    }
}

impl PluginSig {
    fn of(build: &PluginBuild) -> PluginSig {
        let dir = &build.dir;
        let files = dir
            .skills
            .iter()
            .chain(&dir.commands)
            .chain(&dir.agents)
            .chain(&dir.styles)
            .map(|file| (file.path.clone(), digest_of(&file.path)))
            .collect();
        let manifests = [
            plugins::MANIFEST_FILE,
            plugins::HOOKS_FILE,
            plugins::MCP_FILE,
            plugins::LSP_FILE,
        ]
        .iter()
        .map(|file| dir.root.join(file))
        .collect();
        PluginSig {
            files,
            manifests,
            hooks: build.hooks.clone(),
            servers: build.servers.clone(),
            lsp: build.lsp_servers.clone(),
            deny: build.deny.clone(),
            ask: build.ask.clone(),
            hard_deny: build.hard_deny.clone(),
            soft_deny: build.soft_deny.clone(),
        }
    }

    /// The files whose change a disk stamp watches.
    fn watched(&self) -> impl Iterator<Item = &PathBuf> {
        self.files.keys().chain(&self.manifests)
    }
}

/// The sigs of the plugins a load produced, by plugin name.
fn sigs_of(contributions: &PluginContributions) -> BTreeMap<String, PluginSig> {
    contributions
        .builds
        .iter()
        .map(|build| (build.dir.name.clone(), PluginSig::of(build)))
        .collect()
}

fn stamp_file(hasher: &mut Sha256, path: &Path) {
    hasher.update(path.as_os_str().as_encoded_bytes());
    match std::fs::metadata(path) {
        Ok(meta) => {
            let modified = meta
                .modified()
                .ok()
                .and_then(|at| at.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            hasher.update(modified.to_le_bytes());
            hasher.update(meta.len().to_le_bytes());
        }
        Err(_) => hasher.update([0u8]),
    }
}

/// Every file under `dir`, `.git` excluded, in a stable order.
fn stamp_tree(hasher: &mut Sha256, dir: &Path) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = read.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        if entry.file_name() == ".git" {
            continue;
        }
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            stamp_tree(hasher, &path);
        } else {
            stamp_file(hasher, &path);
        }
    }
}

/// A cheap fingerprint of every file `extensions::load` reads: skills, agents and output styles
/// of the user and project scopes, every plugin's component files and manifests, and the plugin
/// registry and the skill trust record. Path, modification time and length only, per file: an
/// in-place edit changes the file's time but not its directory's.
fn disk_stamp(
    config_dir: &Path,
    project_root: &Path,
    sigs: &BTreeMap<String, PluginSig>,
) -> String {
    let mut hasher = Sha256::new();
    let project = project_root.join(crate::config::PROJECT_DIR);
    for base in [config_dir, project.as_path()] {
        for dir in [
            extensions::SKILLS_DIR,
            extensions::AGENTS_DIR,
            extensions::STYLES_DIR,
        ] {
            stamp_tree(&mut hasher, &base.join(dir));
        }
    }
    for sig in sigs.values() {
        for path in sig.watched() {
            stamp_file(&mut hasher, path);
        }
    }
    stamp_file(&mut hasher, &registry_path(config_dir));
    stamp_file(&mut hasher, &config_dir.join(skill_trust::TRUST_FILE));
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// What a reload did, for the surface to show and for tests.
#[derive(Debug, Default)]
pub struct ReloadReport {
    /// One line per change, in the order they were applied.
    pub lines: Vec<String>,
    /// The reload was refused or stopped early; the session keeps what it had.
    pub failed: bool,
    /// The system prompt or the tools changed, so the agent was rebuilt.
    pub rebuilt: bool,
}

/// What the session knows about the plugins it loaded.
pub(super) struct PluginState {
    pub(super) sigs: BTreeMap<String, PluginSig>,
    /// The fingerprint of the extension files at the last load.
    pub(super) stamp: String,
}

impl PluginState {
    pub(super) fn at_start(setup: &AgentSetup) -> PluginState {
        let sigs = sigs_of(&setup.plugins);
        let stamp = disk_stamp(&setup.config_dir, &setup.config.workspace, &sigs);
        PluginState { sigs, stamp }
    }
}

/// Everything a reload would apply, loaded and validated before the session is touched.
struct Loaded {
    contributions: PluginContributions,
    config: Config,
    extensions: Extensions,
    types: AgentTypes,
    layer: user_hooks::PluginLayer,
    sigs: BTreeMap<String, PluginSig>,
    warnings: Vec<String>,
}

/// What differs between two sets of plugins.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct PluginDiff {
    pub(super) loads: Vec<String>,
    pub(super) unloads: Vec<String>,
    pub(super) reloads: Vec<String>,
    /// A plugin that comes, goes or changes has language servers on either side.
    pub(super) lsp_changed: bool,
}

impl PluginDiff {
    /// `forced` plugins reload whatever their definitions say, since a replaced checkout whose
    /// scripts alone changed leaves every definition equal.
    pub(super) fn between(
        old: &BTreeMap<String, PluginSig>,
        new: &BTreeMap<String, PluginSig>,
        forced: &BTreeSet<String>,
    ) -> PluginDiff {
        let loads: Vec<String> = new
            .keys()
            .filter(|n| !old.contains_key(*n))
            .cloned()
            .collect();
        let unloads: Vec<String> = old
            .keys()
            .filter(|n| !new.contains_key(*n))
            .cloned()
            .collect();
        let reloads: Vec<String> = old
            .iter()
            .filter(|(name, sig)| {
                new.get(*name)
                    .is_some_and(|other| other != *sig || forced.contains(*name))
            })
            .map(|(name, _)| name.clone())
            .collect();
        let has_lsp = |sigs: &BTreeMap<String, PluginSig>, name: &String| {
            sigs.get(name).is_some_and(|sig| !sig.lsp.is_empty())
        };
        let lsp_changed = loads.iter().any(|n| has_lsp(new, n))
            || unloads.iter().any(|n| has_lsp(old, n))
            || reloads
                .iter()
                .any(|n| old.get(n).map(|s| &s.lsp) != new.get(n).map(|s| &s.lsp));
        PluginDiff {
            loads,
            unloads,
            reloads,
            lsp_changed,
        }
    }

    fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (verb, names) in [
            ("loaded", &self.loads),
            ("reloaded", &self.reloads),
            ("unloaded", &self.unloads),
        ] {
            out.extend(names.iter().map(|name| format!("{verb} plugin `{name}`")));
        }
        out
    }

    /// The plugins whose earlier skill content and tool results the next turn is warned about.
    fn stale(&self) -> Vec<String> {
        self.unloads
            .iter()
            .chain(&self.reloads)
            .map(|name| format!("plugin `{name}`"))
            .collect()
    }
}

impl AgentRuntime {
    /// The fingerprint `refresh_before_turn` compares with the one taken at the last load.
    fn current_disk_stamp(&self) -> String {
        disk_stamp(
            &self.setup.config_dir,
            &self.config.workspace,
            &self.plugin_state.sigs,
        )
    }

    /// The tool definitions the model is sent, in a stable order.
    fn tool_fingerprint(&self) -> Vec<String> {
        let mut defs: Vec<String> = self
            .tool_server
            .snapshot()
            .definitions()
            .iter()
            .map(|def| format!("{}\0{}\0{}", def.name, def.description, def.parameters))
            .collect();
        defs.sort();
        defs
    }

    /// Stops what `plugin` runs, ahead of a change to its checkout: its MCP servers (in-flight
    /// calls get the drain bound) and, when it declares any, every language server, which start
    /// again on next need. Returns the MCP servers that were stopped.
    pub async fn stop_plugin_servers(&mut self, plugin: &str) -> Vec<String> {
        let Some(sig) = self.plugin_state.sigs.get(plugin) else {
            return Vec::new();
        };
        let names: Vec<String> = sig.servers.keys().cloned().collect();
        let has_lsp = !sig.lsp.is_empty();
        let mut stopped = Vec::new();
        for name in names {
            match self.stop_mcp_server(&name).await {
                Some(true) => stopped.push(name),
                Some(false) => {
                    self.approver.warn(&format!(
                        "MCP server `{name}` still had calls running after {} seconds; they end with a transport error",
                        super::mcp_supervisor::MCP_DRAIN_BOUND.as_secs()
                    ));
                    stopped.push(name);
                }
                None => {}
            }
        }
        if has_lsp {
            self.setup.lsp.shutdown_all().await;
        }
        stopped
    }

    /// Restarts the MCP servers of `plugins` from the current config, after a reload that kept
    /// the old set but had stopped them.
    async fn restart_servers(&mut self, plugins: &BTreeSet<String>) {
        let names: Vec<String> = plugins
            .iter()
            .filter_map(|plugin| self.plugin_state.sigs.get(plugin))
            .flat_map(|sig| sig.servers.keys().cloned())
            .collect();
        for name in names {
            let outcome = self.start_mcp_server(&name).await;
            self.warn_failed(&outcome);
        }
    }

    fn warn_failed(&self, outcome: &McpOutcome) {
        if let Some(error) = &outcome.error {
            self.approver.warn(error);
        }
    }

    fn refuse_reload(&self, report: &mut ReloadReport, why: String) {
        report.failed = true;
        self.approver.warn(&format!(
            "plugins were not reloaded; the current set stays active: {why}"
        ));
        report.lines.push(why);
    }

    /// Re-reads the plugin registry, the plugin checkouts and the user, project and plugin
    /// skills, agents and styles, and applies the difference to the running session between
    /// turns: staged plugin updates are swapped in, then each plugin is loaded, reloaded or
    /// unloaded as it differs, and an unchanged plugin is left running. `changed` names plugins
    /// whose checkout the caller just replaced; they reload whatever their definitions say,
    /// since a script-only change leaves every definition equal.
    ///
    /// A load that fails (a bad manifest, a server name that collides) leaves the current
    /// set running and says why. The conversation is never edited: a plugin or skill that went
    /// away or changed is named in a note the next turn carries.
    pub async fn reload_extensions(&mut self, changed: &[String]) -> ReloadReport {
        let mut report = ReloadReport::default();
        let before_prompt = self.prompt_pieces.join();
        let before_tools = self.tool_fingerprint();
        let mut forced: BTreeSet<String> = changed.iter().cloned().collect();

        // Staged updates first, so a plugin an auto-update fetched stops waiting for a restart.
        // Every plugin stopped for one restarts, swapped or not.
        let stopped = self.apply_staged_updates(&mut report).await;
        forced.extend(stopped.iter().cloned());

        let loaded = match self.load_changed() {
            Ok(loaded) => loaded,
            Err(why) => {
                self.refuse_reload(&mut report, why);
                self.restart_servers(&stopped).await;
                return report;
            }
        };
        for warning in &loaded.warnings {
            self.approver.warn(warning);
        }
        let diff = PluginDiff::between(&self.plugin_state.sigs, &loaded.sigs, &forced);
        let old_extensions = self.extensions.get();
        self.apply_loaded(loaded, &diff).await;
        let skills = skill_changes(&old_extensions, &self.extensions.get());

        report.lines.extend(diff.lines());
        report.lines.extend(skills.lines());
        if diff.lsp_changed {
            report.lines.push(
                "language servers of changed plugins apply from the next session; running ones were stopped and restart on next need"
                    .to_string(),
            );
        }
        // The index and the agent tool's description changed only if the prompt or the tools
        // did, so a reload that changed nothing the model sees costs no cache read.
        match self.rebuild_if_changed(&before_prompt, &before_tools) {
            Ok(rebuilt) => report.rebuilt = rebuilt,
            Err(e) => self.approver.warn(&format!(
                "the system prompt could not be updated after the reload: {}",
                chain(&e)
            )),
        }
        let mut stale = diff.stale();
        stale.extend(skills.stale());
        if !stale.is_empty() {
            self.state
                .queue_hook_context(format!("{}: {STALE_NOTE}", stale.join(", ")));
        }
        self.finish_report(&mut report);
        report
    }

    /// Swaps in the plugin revisions auto-update staged, after stopping those plugins' servers.
    /// Returns the plugins that were stopped.
    async fn apply_staged_updates(&mut self, report: &mut ReloadReport) -> BTreeSet<String> {
        let mut stopped = BTreeSet::new();
        if !self.config.plugins.auto_update {
            return stopped;
        }
        let config_dir = self.setup.config_dir.clone();
        for name in plugin_ops::staged_plugins(&config_dir) {
            self.stop_plugin_servers(&name).await;
            stopped.insert(name);
        }
        let gate = stopped.clone();
        let applied = tokio::task::spawn_blocking(move || {
            plugin_ops::apply_staged(&config_dir, SystemTime::now(), &mut |name| {
                gate.contains(name)
            })
        })
        .await;
        match applied {
            Ok(applied) => {
                for warning in &applied.warnings {
                    self.approver.warn(warning);
                }
                for name in &applied.swapped {
                    report
                        .lines
                        .push(format!("applied the staged update of `{name}`"));
                }
                for name in &applied.held {
                    self.approver.notice(&format!(
                        "plugin `{name}` has a newer revision held for review: run /plugin update {name}"
                    ));
                }
            }
            Err(e) => self.approver.warn(&format!(
                "staged plugin updates were not applied: {}",
                chain(&e)
            )),
        }
        stopped
    }

    /// Loads everything a reload would apply, without touching the session. `Err` is why the
    /// current set must stay.
    fn load_changed(&self) -> Result<Loaded, String> {
        // A registry that cannot be read loads no plugin, which would unload them all.
        plugins::read_registry(&self.setup.config_dir).map_err(|e| e.to_string())?;
        let contributions = plugins::load_installed(
            &self.setup.config_dir,
            &PluginContext {
                project_root: &self.config.workspace,
                env: &*self.setup.env,
            },
        )
        .map_err(|e| e.to_string())?;
        // `apply_plugins` strips every plugin entry before it can fail on a name collision, so
        // it runs on a copy that is only kept on success.
        let mut config = self.config.clone();
        config
            .apply_plugins(&contributions)
            .map_err(|e| e.to_string())?;
        let servers: Vec<String> = config.mcp_servers.keys().cloned().collect();
        let mut warnings = contributions.warnings.clone();
        let extensions = extensions::load(&mut LoadParams {
            config_dir: &self.setup.config_dir,
            project_root: &config.workspace,
            servers: &servers,
            plugins: &contributions.dirs,
            warnings: &mut warnings,
        })
        .map_err(|e| e.to_string())?;
        let (types, type_warnings) =
            setup::build_agent_types(&config, &extensions, &self.setup.roles)
                .map_err(|e| chain(&e))?;
        warnings.extend(type_warnings.into_iter().map(|notice| notice.headline));
        let layer = user_hooks::build_layer(
            &config,
            self.setup.project_executables,
            &self.setup.hook_deps,
            HookLayer::Plugin,
        )
        .map_err(|e| chain(&e))?;
        Ok(Loaded {
            sigs: sigs_of(&contributions),
            contributions,
            config,
            extensions,
            types,
            layer,
            warnings,
        })
    }

    /// Stops what goes away or changes, swaps every live cell to the new load, and starts what
    /// is new or changed.
    async fn apply_loaded(&mut self, loaded: Loaded, diff: &PluginDiff) {
        for name in diff.unloads.iter().chain(&diff.reloads) {
            self.stop_plugin_servers(name).await;
        }
        let Loaded {
            contributions,
            config,
            extensions,
            types,
            layer,
            sigs,
            ..
        } = loaded;
        let mut auto = self.state.auto_context();
        let (environment, rules) = config.effective_auto_mode();
        auto.environment = environment;
        auto.rules = rules;
        self.state.set_auto_context(auto);
        self.state
            .set_plugin_rules(contributions.deny.clone(), contributions.ask.clone());
        self.setup.plugin_hooks.set(Arc::new(layer));
        self.config = config;
        self.extensions.set(Arc::new(extensions));
        self.agent_types.set(Arc::new(types));
        for name in diff.loads.iter().chain(&diff.reloads) {
            let servers: Vec<String> = sigs
                .get(name)
                .map(|sig| sig.servers.keys().cloned().collect())
                .unwrap_or_default();
            for server in servers {
                let outcome = self.start_mcp_server(&server).await;
                self.warn_failed(&outcome);
            }
        }
        self.plugin_state.sigs = sigs;
        self.plugin_state.stamp = self.current_disk_stamp();
    }

    fn finish_report(&self, report: &mut ReloadReport) {
        if report.lines.is_empty() {
            report
                .lines
                .push("plugins and skills are already up to date".to_string());
        } else {
            self.state.record_plugin_change("reload", "", "");
        }
        for line in &report.lines {
            self.approver.notice(line);
        }
    }

    /// Builds the agent again and keeps it only when the prompt or the tools differ from
    /// `before_*`, so an unchanged session keeps its prompt-cache prefix. Returns whether it
    /// kept the new agent.
    fn rebuild_if_changed(
        &mut self,
        before_prompt: &str,
        before_tools: &[String],
    ) -> Result<bool, crate::runtime_error::RuntimeError> {
        let prompt_afk = self.state.afk_on();
        let (agent, prompt_pieces, launch_instructions, native_search) = self.build_candidate()?;
        if prompt_pieces.join() == before_prompt && self.tool_fingerprint() == before_tools {
            return Ok(false);
        }
        self.install_agent(
            agent,
            prompt_pieces,
            launch_instructions,
            native_search,
            prompt_afk,
        );
        Ok(true)
    }

    /// Tells once, at a turn boundary, that skill, agent, style or plugin files changed on disk
    /// since the last load. It never applies them: a stray edit must not change the toolset or
    /// cost a cache read unexpectedly.
    pub(super) fn notice_disk_changes(&mut self) {
        let now = self.current_disk_stamp();
        if now != self.plugin_state.stamp {
            self.plugin_state.stamp = now;
            self.approver
                .notice("skills/plugins changed on disk — run /reload-plugins");
        }
    }
}

/// The skills that differ between two loads.
struct SkillChanges {
    added: Vec<String>,
    removed: Vec<String>,
    changed: Vec<String>,
}

fn skill_changes(old: &Extensions, new: &Extensions) -> SkillChanges {
    let digests = |extensions: &Extensions| -> BTreeMap<String, String> {
        extensions
            .skills
            .iter()
            .map(|skill| (skill.name.clone(), skill.digest.clone()))
            .collect()
    };
    let (old, new) = (digests(old), digests(new));
    SkillChanges {
        added: new
            .keys()
            .filter(|n| !old.contains_key(*n))
            .cloned()
            .collect(),
        removed: old
            .keys()
            .filter(|n| !new.contains_key(*n))
            .cloned()
            .collect(),
        changed: new
            .iter()
            .filter(|(name, digest)| old.get(*name).is_some_and(|d| d != *digest))
            .map(|(name, _)| name.clone())
            .collect(),
    }
}

impl SkillChanges {
    fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (label, names) in [
            ("added", &self.added),
            ("removed", &self.removed),
            ("changed", &self.changed),
        ] {
            if !names.is_empty() {
                out.push(format!("skills {label}: {}", names.join(", ")));
            }
        }
        out
    }

    /// The names the next turn's note lists.
    fn stale(&self) -> Vec<String> {
        self.removed
            .iter()
            .chain(&self.changed)
            .map(|name| format!("skill `{name}`"))
            .collect()
    }
}
