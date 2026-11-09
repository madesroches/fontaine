//! The report behind `fontaine doctor`: what loaded, what did not, and why, as sections of
//! findings. Each section is built from plain facts the caller gathered, so none of them reads
//! the file system or the network, and the settings rows come from the same renderers `/config`
//! uses, so no credential or environment value can appear.

use std::error::Error;
use std::fmt::Write;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::agent::McpServerPlan;
use crate::config::{Config, LayerDiagnosis, LayerOutcome, Slot};
use crate::config_catalog::{KEYS, Running};
use crate::memory::MemoryFinding;
use fontaine_tools::error_chain::chain;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Fail,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Ok => "ok",
            Level::Warn => "warn",
            Level::Fail => "fail",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub level: Level,
    pub subject: String,
    /// The error chain text for a failure, otherwise what was found.
    pub detail: String,
}

impl Finding {
    fn new(level: Level, subject: impl Into<String>, detail: impl Into<String>) -> Finding {
        Finding {
            level,
            subject: subject.into(),
            detail: detail.into(),
        }
    }

    fn ok(subject: impl Into<String>, detail: impl Into<String>) -> Finding {
        Finding::new(Level::Ok, subject, detail)
    }

    pub fn warn(subject: impl Into<String>, detail: impl Into<String>) -> Finding {
        Finding::new(Level::Warn, subject, detail)
    }

    fn fail(subject: impl Into<String>, detail: impl Into<String>) -> Finding {
        Finding::new(Level::Fail, subject, detail)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    pub title: &'static str,
    pub findings: Vec<Finding>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DoctorReport {
    pub sections: Vec<Section>,
}

impl DoctorReport {
    /// Whether any check failed. A warning does not count.
    pub fn failed(&self) -> bool {
        self.sections
            .iter()
            .flat_map(|s| &s.findings)
            .any(|f| f.level == Level::Fail)
    }

    pub fn render_text(&self) -> String {
        let mut out = String::new();
        for (index, section) in self.sections.iter().enumerate() {
            if index > 0 {
                out.push('\n');
            }
            let _ = writeln!(out, "{}", section.title);
            for finding in &section.findings {
                let mut lines = finding.detail.lines();
                let _ = writeln!(
                    out,
                    "  [{}] {}: {}",
                    finding.level.as_str(),
                    finding.subject,
                    lines.next().unwrap_or_default()
                );
                for line in lines {
                    let _ = writeln!(out, "        {line}");
                }
            }
        }
        out
    }

    pub fn to_json(&self) -> Value {
        let sections: Vec<Value> = self
            .sections
            .iter()
            .map(|section| {
                let findings: Vec<Value> = section
                    .findings
                    .iter()
                    .map(|f| {
                        json!({
                            "level": f.level.as_str(),
                            "subject": f.subject,
                            "detail": f.detail,
                        })
                    })
                    .collect();
                json!({ "title": section.title, "findings": findings })
            })
            .collect();
        json!({ "failed": self.failed(), "sections": sections })
    }
}

pub const INSTALL: &str = "Install";
pub const LAYERS: &str = "Config layers";
pub const SETTINGS: &str = "Effective settings";
pub const TRUST: &str = "Trust";
pub const MODELS: &str = "Guardian and models";
pub const MCP: &str = "MCP servers";
pub const EXTENSIONS: &str = "Instructions, hooks, skills";
pub const MEMORY: &str = "Memory";

/// The reason a section that needs the configuration was not built.
pub const CONFIG_NOT_LOADED: &str = "config did not load";

pub fn skipped_section(title: &'static str) -> Section {
    Section {
        title,
        findings: vec![Finding::warn("skipped", CONFIG_NOT_LOADED)],
    }
}

pub fn install_section(
    version: &str,
    config_dir: &Path,
    data_dir: &Path,
    workspace: &Path,
) -> Section {
    Section {
        title: INSTALL,
        findings: vec![
            Finding::ok("version", version),
            Finding::ok("config directory", config_dir.display().to_string()),
            Finding::ok("data directory", data_dir.display().to_string()),
            Finding::ok("workspace root", workspace.display().to_string()),
        ],
    }
}

/// The config and data directories could not be determined, so nothing else can be checked.
pub fn install_failure_section(version: &str, error: &(dyn Error + 'static)) -> Section {
    Section {
        title: INSTALL,
        findings: vec![
            Finding::ok("version", version),
            Finding::fail("directories", chain(error)),
        ],
    }
}

/// One row per layer. `load_error` is the loader's own failure when it stopped, which can come
/// from a check across layers that no single row shows.
pub fn layers_section(
    layers: &[LayerDiagnosis],
    load_error: Option<&(dyn Error + 'static)>,
) -> Section {
    let mut findings = Vec::new();
    for layer in layers {
        let subject = layer.slot.label();
        let path = layer
            .path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let demoted = layer
            .demotion
            .as_ref()
            .map(|why| format!("loads as project settings: {why}"));
        findings.push(match &layer.outcome {
            LayerOutcome::Absent => Finding::ok(subject, format!("{path} (absent)")),
            LayerOutcome::Loaded => Finding::ok(
                subject,
                match demoted {
                    Some(why) => format!("{path} (loaded; {why})"),
                    None => format!("{path} (loaded)"),
                },
            ),
            LayerOutcome::Failed(e) => Finding::fail(
                subject,
                match demoted {
                    Some(why) => format!("{}\n{why}", chain(e)),
                    None => chain(e),
                },
            ),
            LayerOutcome::NotChecked(reason) if layer.slot == Slot::Managed => {
                Finding::ok(subject, reason.clone())
            }
            LayerOutcome::NotChecked(reason) => {
                Finding::warn(subject, format!("not checked: {reason}"))
            }
        });
    }
    if let Some(error) = load_error {
        findings.push(Finding::fail("load", chain(error)));
    }
    Section {
        title: LAYERS,
        findings,
    }
}

/// Every key a layer set, with its effective value and the layer that won; `all` adds the keys
/// left at their defaults.
pub fn settings_section(config: &Config, running: &Running, all: bool) -> Section {
    let findings = KEYS
        .iter()
        .filter(|row| all || !row.slots(config).is_empty())
        .map(|row| {
            Finding::ok(
                row.path,
                format!(
                    "{} ({})",
                    (row.effective)(config, running),
                    row.source_label(config)
                ),
            )
        })
        .collect();
    Section {
        title: SETTINGS,
        findings,
    }
}

/// What doctor knows about trust, whether or not the configuration loaded.
pub struct TrustFacts<'a> {
    pub workspace: &'a Path,
    pub trusted: bool,
    /// Why `trust.toml` cannot be used, if it cannot.
    pub store_problem: Option<String>,
    /// Hooks and servers the repository defines; `None` when the configuration did not load.
    pub project: Option<ProjectExecutables>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectExecutables {
    pub hooks: usize,
    pub servers: Vec<String>,
    pub skills: bool,
}

pub fn trust_section(facts: &TrustFacts) -> Section {
    let mut findings = Vec::new();
    findings.push(if facts.trusted {
        Finding::ok(
            "workspace",
            format!("{} is trusted", facts.workspace.display()),
        )
    } else {
        Finding::warn(
            "workspace",
            format!("{} is not trusted", facts.workspace.display()),
        )
    });
    findings.push(match &facts.store_problem {
        Some(problem) => Finding::fail("trust.toml", problem.clone()),
        None => Finding::ok("trust.toml", "readable"),
    });
    findings.push(match &facts.project {
        None => Finding::warn("project executables", "unknown: config did not load"),
        Some(exec) if exec.hooks == 0 && exec.servers.is_empty() && !exec.skills => {
            Finding::ok("project executables", "the repository defines none")
        }
        Some(exec) => {
            let mut parts = Vec::new();
            if exec.hooks > 0 {
                parts.push(format!("{} hook(s)", exec.hooks));
            }
            if !exec.servers.is_empty() {
                parts.push(format!("MCP server(s) {}", exec.servers.join(", ")));
            }
            if exec.skills {
                parts.push("project skills".to_string());
            }
            let interactive = if facts.trusted {
                "load; unapproved MCP servers are asked about first"
            } else {
                "do not load until the workspace is trusted"
            };
            Finding::ok(
                "project executables",
                format!(
                    "{}\nheadless: all load and every server starts\ninteractive: {interactive}",
                    parts.join(", ")
                ),
            )
        }
    });
    Section {
        title: TRUST,
        findings,
    }
}

/// The guardian's model as a session would find it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GuardianFact {
    Ready(String),
    Unavailable { reference: String, reason: String },
    NotConfigured,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelFacts {
    pub main: String,
    pub guardian: GuardianFact,
    /// `pinned`, `default` or `main model`.
    pub choice: &'static str,
    pub notices: Vec<String>,
}

/// `resolved` is the facts, or the resolver's error chain when it failed.
pub fn models_section(resolved: Result<ModelFacts, String>, profile: &str) -> Section {
    let mut findings = Vec::new();
    match resolved {
        Err(error) => {
            findings.push(Finding::fail("models", error));
            findings.push(Finding::warn("guardian", "not resolved"));
        }
        Ok(facts) => {
            findings.push(Finding::ok("main model", facts.main));
            findings.push(match facts.guardian {
                GuardianFact::Ready(reference) => Finding::ok(
                    "guardian model",
                    format!("{reference} (ready; {})", facts.choice),
                ),
                GuardianFact::Unavailable { reference, reason } => Finding::fail(
                    "guardian model",
                    format!(
                        "{reference} is unavailable ({}): every call the guardian must judge is denied\n{reason}",
                        facts.choice
                    ),
                ),
                GuardianFact::NotConfigured => {
                    Finding::warn("guardian model", "none is configured")
                }
            });
            for notice in facts.notices {
                findings.push(Finding::warn("notice", notice));
            }
        }
    }
    findings.push(Finding::ok("guardian profile", profile));
    findings.push(Finding::ok("guardian composition", "validated at load"));
    Section {
        title: MODELS,
        findings,
    }
}

/// Whether a configured server would start in a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpGate {
    Start,
    /// A repository-defined server the user has not approved.
    Unapproved,
    /// A repository server in a workspace that is not trusted.
    Untrusted,
}

/// The gate for one server from the plan the session would make; `None` when the plan left it
/// out, which only an untrusted workspace does.
pub fn mcp_gate(plan: Option<&McpServerPlan>) -> McpGate {
    match plan {
        None => McpGate::Untrusted,
        Some(plan) if plan.needs_approval => McpGate::Unapproved,
        Some(_) => McpGate::Start,
    }
}

impl McpGate {
    /// What the approval column says.
    pub fn approval(self, project: bool) -> &'static str {
        match self {
            McpGate::Untrusted => "workspace not trusted",
            McpGate::Unapproved => "not approved",
            McpGate::Start if project => "allowed",
            McpGate::Start => "user-defined",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpState {
    Connected(usize),
    Failed(String),
    NotStarted(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpRow {
    pub name: String,
    pub origin: &'static str,
    pub transport: &'static str,
    pub gate: McpGate,
    pub project: bool,
    pub state: McpState,
}

pub fn mcp_section(rows: &[McpRow]) -> Section {
    let mut findings: Vec<Finding> = rows
        .iter()
        .map(|row| {
            let head = format!(
                "{} server from {}, {}",
                row.transport,
                row.origin,
                row.gate.approval(row.project)
            );
            match &row.state {
                McpState::Connected(tools) => {
                    Finding::ok(&row.name, format!("{head}\nconnected ({tools} tools)"))
                }
                McpState::Failed(error) => {
                    Finding::warn(&row.name, format!("{head}\nfailed: {error}"))
                }
                McpState::NotStarted(why) => {
                    Finding::ok(&row.name, format!("{head}\nnot started: {why}"))
                }
            }
        })
        .collect();
    if findings.is_empty() {
        findings.push(Finding::ok("servers", "none configured"));
    }
    Section {
        title: MCP,
        findings,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookCount {
    pub event: &'static str,
    pub scope: &'static str,
    pub project: bool,
    pub count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillCounts {
    pub user: usize,
    pub project: usize,
    pub plugin: usize,
    pub agents: usize,
}

#[derive(Clone, Debug)]
pub struct ExtensionFacts {
    /// Path and origin label, in load order.
    pub instructions: Vec<(PathBuf, String)>,
    pub instruction_warnings: Vec<String>,
    pub hooks: Vec<HookCount>,
    pub workspace_trusted: bool,
    /// The counts, or the load failure's chain.
    pub skills: Result<SkillCounts, String>,
    /// Plugin load failed: its chain. Plugin servers, hooks and skills are then missing.
    pub plugin_error: Option<String>,
    /// Plugin, language-server and skill-loading warnings.
    pub warnings: Vec<String>,
}

pub fn extensions_section(facts: &ExtensionFacts) -> Section {
    let mut findings = Vec::new();
    if facts.instructions.is_empty() {
        findings.push(Finding::ok("instructions", "no instruction files"));
    }
    for (path, origin) in &facts.instructions {
        findings.push(Finding::ok(
            "instructions",
            format!("{} ({origin})", path.display()),
        ));
    }
    for warning in &facts.instruction_warnings {
        findings.push(Finding::warn("instructions", warning.clone()));
    }
    let project_state = if facts.workspace_trusted {
        "headless: loaded; interactive: loaded"
    } else {
        "headless: loaded; interactive: not loaded (workspace not trusted)"
    };
    if facts.hooks.is_empty() {
        findings.push(Finding::ok("hooks", "none"));
    }
    for hook in &facts.hooks {
        let ignored = crate::config::HookEvent::ALL
            .into_iter()
            .find(|e| e.config_key() == hook.event)
            .and_then(|e| match e.project_scope() {
                crate::config::ProjectScope::IgnoredWithWarning(_) => Some(e),
                crate::config::ProjectScope::Loads => None,
            });
        let detail = if let (true, Some(event)) = (hook.project, ignored) {
            format!(
                "{} ({}); never run (a repository may not define {} hooks)",
                hook.count,
                hook.scope,
                event.config_key()
            )
        } else if hook.project {
            format!("{} ({}); {project_state}", hook.count, hook.scope)
        } else {
            format!("{} ({})", hook.count, hook.scope)
        };
        findings.push(Finding::ok(format!("hooks {}", hook.event), detail));
    }
    if let Some(error) = &facts.plugin_error {
        findings.push(Finding::fail(
            "plugins",
            format!("{error}\nplugin servers, hooks and skills are not loaded"),
        ));
    }
    match &facts.skills {
        Ok(counts) => {
            findings.push(Finding::ok(
                "skills",
                format!(
                    "{} (user {}, project {}, plugin {})",
                    counts.user + counts.project + counts.plugin,
                    counts.user,
                    counts.project,
                    counts.plugin
                ),
            ));
            if counts.project > 0 {
                findings.push(Finding::ok("project skills", project_state));
            }
            findings.push(Finding::ok("agent types", counts.agents.to_string()));
        }
        Err(error) => {
            findings.push(Finding::fail("skills", error.clone()));
            findings.push(Finding::warn("agent types", "not loaded"));
        }
    }
    for warning in &facts.warnings {
        findings.push(Finding::warn("warning", warning.clone()));
    }
    Section {
        title: EXTENSIONS,
        findings,
    }
}

/// `auto` is the `[memory] auto` setting, `None` when the configuration did not load.
pub fn memory_section(dir: &Path, auto: Option<bool>, findings: &[MemoryFinding]) -> Section {
    let mut out = vec![Finding::ok("store", dir.display().to_string())];
    out.push(match auto {
        Some(true) => Finding::ok("auto memory", "on"),
        Some(false) => Finding::ok(
            "auto memory",
            "off ([memory] auto = false); an existing store is still checked",
        ),
        None => Finding::warn("auto memory", "setting unknown: config did not load"),
    });
    for found in findings {
        out.push(memory_finding(found));
    }
    if findings.is_empty() {
        out.push(Finding::ok("index", "the index and the topic files agree"));
    }
    Section {
        title: MEMORY,
        findings: out,
    }
}

fn memory_finding(found: &MemoryFinding) -> Finding {
    use MemoryFinding::*;
    match found {
        StoreMissing => Finding::ok("store", "nothing saved yet"),
        StoreUnusable { reason } => Finding::fail("store", reason.clone()),
        IndexUnusable { reason } => Finding::fail("MEMORY.md", reason.clone()),
        IndexTruncated { lines_beyond } => Finding::warn(
            "MEMORY.md",
            format!("{lines_beyond} line(s) lie beyond the cap and never reach the prompt"),
        ),
        DanglingEntry { line, target } => Finding::fail(
            "MEMORY.md",
            format!("line {line} links to {target}, which does not exist"),
        ),
        UnloadedTarget {
            line,
            target,
            is_link,
        } => Finding::warn(
            "MEMORY.md",
            if *is_link {
                format!("line {line} links to {target}, which is a link and is never loaded")
            } else {
                format!("line {line} links to {target}, which is not a topic file in the store")
            },
        ),
        OrphanFile { file } => Finding::warn(
            file,
            "no index line within the prompt cap links to this file",
        ),
        DuplicateEntry { target, lines } => {
            let lines: Vec<String> = lines.iter().map(ToString::to_string).collect();
            Finding::warn(
                "MEMORY.md",
                format!("lines {} all link to {target}", lines.join(", ")),
            )
        }
        EscapingLink { line, target } => Finding::warn(
            "MEMORY.md",
            format!("line {line} links to {target}, which leaves the store and is never followed"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Column, ConfigError};

    fn section(findings: Vec<Finding>) -> Section {
        Section {
            title: "T",
            findings,
        }
    }

    fn levels(section: &Section) -> Vec<Level> {
        section.findings.iter().map(|f| f.level).collect()
    }

    #[test]
    fn failed_is_true_only_on_a_fail_finding() {
        let warn = DoctorReport {
            sections: vec![section(vec![
                Finding::ok("a", "b"),
                Finding::warn("c", "d"),
            ])],
        };
        assert!(!warn.failed());
        let fail = DoctorReport {
            sections: vec![section(vec![Finding::fail("c", "d")])],
        };
        assert!(fail.failed());
        assert_eq!(fail.to_json()["failed"], true);
    }

    #[test]
    fn text_indents_the_lines_after_the_first() {
        let report = DoctorReport {
            sections: vec![section(vec![Finding::fail("x", "one\ntwo")])],
        };
        assert_eq!(report.render_text(), "T\n  [fail] x: one\n        two\n");
    }

    #[test]
    fn json_lists_sections_and_findings() {
        let report = DoctorReport {
            sections: vec![section(vec![Finding::warn("x", "y")])],
        };
        let json = report.to_json();
        assert_eq!(json["sections"][0]["title"], "T");
        assert_eq!(json["sections"][0]["findings"][0]["level"], "warn");
        assert_eq!(json["sections"][0]["findings"][0]["subject"], "x");
    }

    #[test]
    fn a_section_that_needs_config_says_why_it_was_skipped() {
        let skipped = skipped_section(MCP);
        assert_eq!(skipped.findings[0].detail, CONFIG_NOT_LOADED);
        assert_eq!(levels(&skipped), [Level::Warn]);
    }

    #[test]
    fn install_failure_is_one_fail() {
        let error = ConfigError::Dir {
            what: "config",
            var: "FONTAINE_CONFIG_DIR",
        };
        let s = install_failure_section("1.0", &error);
        assert_eq!(levels(&s), [Level::Ok, Level::Fail]);
    }

    fn layer(slot: Slot, path: Option<&str>, outcome: LayerOutcome) -> LayerDiagnosis {
        LayerDiagnosis {
            slot,
            column: Some(Column::User),
            path: path.map(PathBuf::from),
            demotion: None,
            outcome,
        }
    }

    #[test]
    fn layers_show_each_outcome_and_the_loader_error_last() {
        let failed = ConfigError::Parse {
            path: PathBuf::from("/u/config.toml"),
            message: "1:1: bad".into(),
        };
        let layers = [
            layer(
                Slot::Managed,
                None,
                LayerOutcome::NotChecked("not supported".into()),
            ),
            layer(
                Slot::User,
                Some("/u/config.toml"),
                LayerOutcome::Failed(failed),
            ),
            layer(
                Slot::Project,
                Some("/p/settings.toml"),
                LayerOutcome::Absent,
            ),
            layer(
                Slot::ProjectLocal,
                None,
                LayerOutcome::NotChecked("the workspace directory cannot be resolved".into()),
            ),
        ];
        let load = ConfigError::usage("stopped");
        let s = layers_section(&layers, Some(&load));
        assert_eq!(
            levels(&s),
            [Level::Ok, Level::Fail, Level::Ok, Level::Warn, Level::Fail]
        );
        assert!(s.findings[1].detail.contains("1:1: bad"));
        assert!(s.findings[3].detail.starts_with("not checked"));
        assert_eq!(s.findings[4].subject, "load");
    }

    #[test]
    fn settings_list_only_set_keys_unless_all() {
        let mut config = Config::defaults(PathBuf::from("."));
        config
            .sources
            .insert("tui.show_reasoning".into(), vec![Slot::User]);
        let running = Running {
            theme: "dark",
            main_model: "a/b",
            guardian_model: None,
            effort: None,
        };
        let some = settings_section(&config, &running, false);
        assert_eq!(some.findings.len(), 1);
        assert_eq!(some.findings[0].subject, "tui.show_reasoning");
        assert!(some.findings[0].detail.ends_with("(user)"));
        let all = settings_section(&config, &running, true);
        assert!(all.findings.len() > 1);
        assert!(all.findings.iter().any(|f| f.detail.ends_with("(default)")));
    }

    #[test]
    fn settings_never_show_account_secrets_or_env_values() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path().join("config");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("config.toml"),
            "[accounts.work]\nkind = \"openai-compatible\"\nbase_url = \"http://127.0.0.1:1\"\napi_key_env = \"sk-secret-value\"\nmodels = [\"m\"]\n\n[env]\nTOKEN = \"hunter2-env-value\"\n",
        )
        .unwrap();
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let config = crate::config::load(&crate::config::LoadParams {
            launch_dir: &work,
            config_dir: &config_dir,
            data_dir: dir.path(),
            settings_file: None,
            cli_rules: Default::default(),
            env: &|_| None,
        })
        .unwrap();
        let running = Running {
            theme: "dark",
            main_model: "work/m",
            guardian_model: None,
            effort: None,
        };
        let text = DoctorReport {
            sections: vec![settings_section(&config, &running, true)],
        }
        .render_text();
        assert!(text.contains("accounts"), "{text}");
        assert!(!text.contains("sk-secret-value"), "{text}");
        assert!(!text.contains("hunter2-env-value"), "{text}");
    }

    #[test]
    fn an_untrusted_workspace_is_a_warning_and_a_bad_store_a_failure() {
        let facts = TrustFacts {
            workspace: Path::new("/w"),
            trusted: false,
            store_problem: None,
            project: Some(ProjectExecutables {
                hooks: 2,
                servers: vec!["s".into()],
                skills: false,
            }),
        };
        let s = trust_section(&facts);
        assert_eq!(levels(&s), [Level::Warn, Level::Ok, Level::Ok]);
        assert!(s.findings[2].detail.contains("headless: all load"));
        assert!(s.findings[2].detail.contains("do not load until"));
        let facts = TrustFacts {
            trusted: true,
            store_problem: Some("trust.toml cannot be used".into()),
            project: None,
            ..facts
        };
        let s = trust_section(&facts);
        assert_eq!(levels(&s), [Level::Ok, Level::Fail, Level::Warn]);
    }

    fn facts(guardian: GuardianFact) -> ModelFacts {
        ModelFacts {
            main: "a/m".into(),
            guardian,
            choice: "default",
            notices: vec!["falling back".into()],
        }
    }

    #[test]
    fn an_unavailable_guardian_fails_and_a_notice_warns() {
        let s = models_section(
            Ok(facts(GuardianFact::Unavailable {
                reference: "a/g".into(),
                reason: "no credential".into(),
            })),
            "full",
        );
        assert_eq!(
            levels(&s),
            [Level::Ok, Level::Fail, Level::Warn, Level::Ok, Level::Ok]
        );
        assert!(s.findings[1].detail.contains("no credential"));
        let ready = models_section(Ok(facts(GuardianFact::Ready("a/g".into()))), "full");
        assert_eq!(ready.findings[1].level, Level::Ok);
    }

    #[test]
    fn a_resolve_error_fails_and_leaves_the_guardian_unresolved() {
        let s = models_section(Err("no main model".into()), "minimal");
        assert_eq!(s.findings[0].level, Level::Fail);
        assert_eq!(s.findings[1].detail, "not resolved");
    }

    fn plan(needs_approval: bool) -> McpServerPlan {
        McpServerPlan {
            name: "s".into(),
            description: String::new(),
            project: true,
            needs_approval,
            digest: String::new(),
        }
    }

    #[test]
    fn the_gate_follows_the_session_plan() {
        assert_eq!(mcp_gate(None), McpGate::Untrusted);
        assert_eq!(mcp_gate(Some(&plan(true))), McpGate::Unapproved);
        assert_eq!(mcp_gate(Some(&plan(false))), McpGate::Start);
    }

    #[test]
    fn mcp_rows_show_connected_failed_and_not_started() {
        let row = |name: &str, gate, state| McpRow {
            name: name.into(),
            origin: "user",
            transport: "stdio",
            gate,
            project: false,
            state,
        };
        let s = mcp_section(&[
            row("a", McpGate::Start, McpState::Connected(3)),
            row("b", McpGate::Start, McpState::Failed("boom".into())),
            row(
                "c",
                McpGate::Untrusted,
                McpState::NotStarted("workspace not trusted".into()),
            ),
        ]);
        assert_eq!(levels(&s), [Level::Ok, Level::Warn, Level::Ok]);
        assert!(s.findings[0].detail.contains("connected (3 tools)"));
        assert!(s.findings[1].detail.contains("failed: boom"));
        assert!(
            s.findings[2]
                .detail
                .contains("not started: workspace not trusted")
        );
        assert_eq!(mcp_section(&[]).findings[0].detail, "none configured");
    }

    #[test]
    fn an_extension_error_fails_and_project_rows_carry_the_load_state() {
        let facts = ExtensionFacts {
            instructions: vec![(PathBuf::from("/w/CLAUDE.md"), "project".into())],
            instruction_warnings: Vec::new(),
            hooks: vec![
                HookCount {
                    event: "pre_tool_use",
                    scope: "project",
                    project: true,
                    count: 1,
                },
                HookCount {
                    event: "teammate_idle",
                    scope: "project",
                    project: true,
                    count: 1,
                },
                HookCount {
                    event: "pre_tool_use",
                    scope: "user",
                    project: false,
                    count: 2,
                },
            ],
            workspace_trusted: false,
            skills: Err("/w/.fontaine/skills/x/SKILL.md: bad".into()),
            plugin_error: Some("collision".into()),
            warnings: vec!["w".into()],
        };
        let s = extensions_section(&facts);
        assert!(
            s.findings[1]
                .detail
                .contains("not loaded (workspace not trusted)")
        );
        assert!(s.findings[2].detail.contains("never run"));
        assert!(!s.findings[2].detail.contains("loaded"));
        assert!(!s.findings[3].detail.contains("interactive"));
        let skills = s.findings.iter().find(|f| f.subject == "skills").unwrap();
        assert_eq!(skills.level, Level::Fail);
        assert!(
            s.findings
                .iter()
                .any(|f| f.subject == "plugins" && f.level == Level::Fail)
        );
        assert_eq!(s.findings.last().unwrap().level, Level::Warn);
    }

    #[test]
    fn memory_findings_map_to_levels() {
        let found = [
            MemoryFinding::StoreMissing,
            MemoryFinding::IndexUnusable { reason: "r".into() },
            MemoryFinding::IndexTruncated { lines_beyond: 4 },
            MemoryFinding::DanglingEntry {
                line: 2,
                target: "x.md".into(),
            },
            MemoryFinding::UnloadedTarget {
                line: 3,
                target: "l.md".into(),
                is_link: true,
            },
            MemoryFinding::OrphanFile {
                file: "o.md".into(),
            },
            MemoryFinding::DuplicateEntry {
                target: "d.md".into(),
                lines: vec![1, 2],
            },
            MemoryFinding::EscapingLink {
                line: 5,
                target: "../x".into(),
            },
        ];
        let s = memory_section(Path::new("/m"), Some(false), &found);
        assert_eq!(
            levels(&s)[2..],
            [
                Level::Ok,
                Level::Fail,
                Level::Warn,
                Level::Fail,
                Level::Warn,
                Level::Warn,
                Level::Warn,
                Level::Warn
            ]
        );
        assert!(s.findings[3].subject == "MEMORY.md");
        assert!(s.findings[6].detail.contains("is a link"));
        let clean = memory_section(Path::new("/m"), None, &[]);
        assert_eq!(levels(&clean), [Level::Ok, Level::Warn, Level::Ok]);
    }
}
