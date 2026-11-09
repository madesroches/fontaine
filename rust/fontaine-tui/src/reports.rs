//! The text of the read-only commands. Each builder takes plain data, so the report can be
//! tested without a runtime; `lib.rs` gathers the inputs.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use fontaine_agents::registry::TypeOrigin;
use fontaine_core::afk_report::{AfkOutcome, AfkReport, Attempt, LinkKind};
use fontaine_core::config::{
    HandlerKind, Hook, HookEvent, Hooks, MatcherKind, McpServer, Origin, PermissionMode,
    ProjectScope, Slot,
};
use fontaine_core::context::ContextUsage;
use fontaine_core::extensions::{Extensions, Scope};
use fontaine_core::gate::Rules;
use fontaine_core::instructions::{MemoryEntry, MemoryKind};
use fontaine_core::ledger::LedgerRecap;
use fontaine_core::permission_rules::SourcedRule;
use fontaine_core::plugins::Registry;
use fontaine_core::skill_trust::TrustState;
use fontaine_core::usage_ledger::{Bucket, Served, UsageSource};
use fontaine_guardian::evasion::AttemptDecision;
use fontaine_tools::display::compact_count;

use crate::commands::{BUILTINS, MenuEntry, is_builtin};
use crate::keymap::Keymap;
use crate::statusbar::mode_label;
use crate::transcript::truncate;
use fontaine_core::keybindings::TuiAction;

/// What became of an MCP server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpState {
    Connected,
    /// Lost, and being reconnected: `attempt` is the one under way.
    Pending {
        attempt: u32,
    },
    /// The error text, which names the server.
    Failed(String),
    /// The human declined to start it.
    LeftOff,
    /// A repository's server in a workspace that is not trusted.
    NotTrusted,
}

/// The tail of `/help`: the key hints, rendered from the effective bindings.
fn keys_line(keymap: &Keymap) -> String {
    fn strokes(keymap: &Keymap, action: TuiAction) -> String {
        keymap
            .keys_for(action)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("/")
    }
    let mut parts: Vec<String> = Vec::new();
    if !strokes(keymap, TuiAction::CycleMode).is_empty() {
        parts.push(format!("{} mode", strokes(keymap, TuiAction::CycleMode)));
    }
    if !strokes(keymap, TuiAction::AgentsPanel).is_empty() {
        parts.push(format!(
            "{} agents",
            strokes(keymap, TuiAction::AgentsPanel)
        ));
    }
    if !strokes(keymap, TuiAction::Transcript).is_empty() {
        parts.push(format!(
            "{} transcript",
            strokes(keymap, TuiAction::Transcript)
        ));
    }
    if !strokes(keymap, TuiAction::Todos).is_empty() {
        parts.push(format!("{} todos", strokes(keymap, TuiAction::Todos)));
    }
    if !strokes(keymap, TuiAction::HistorySearch).is_empty() {
        parts.push(format!(
            "{} history",
            strokes(keymap, TuiAction::HistorySearch)
        ));
    }
    parts.extend([
        "! shell".to_string(),
        "@ file".to_string(),
        "esc interrupt (or end a waiting /loop)".to_string(),
    ]);
    parts.join(" · ")
}

fn command_line(names: &str, hint: Option<&str>, description: &str) -> String {
    match hint {
        Some(hint) => format!("{names} {hint} — {description}"),
        None => format!("{names} — {description}"),
    }
}

/// `skills` are the menu rows of the user-invocable skills no built-in hides.
pub fn help(skills: &[MenuEntry], keymap: &Keymap) -> String {
    let mut out = String::from("commands:");
    for builtin in BUILTINS {
        let names = std::iter::once(builtin.name)
            .chain(builtin.aliases.iter().copied())
            .map(|n| format!("/{n}"))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = write!(
            out,
            "\n  {}",
            command_line(&names, builtin.hint, builtin.description)
        );
    }
    if !skills.is_empty() {
        out.push_str("\nskills:");
        for skill in skills {
            let _ = write!(
                out,
                "\n  {}",
                command_line(
                    &format!("/{}", skill.name),
                    skill.hint.as_deref(),
                    &skill.description
                )
            );
        }
    }
    let _ = write!(out, "\n{}", keys_line(keymap));
    out
}

pub struct StatusInfo<'a> {
    pub version: &'a str,
    pub session_id: &'a str,
    pub roots: &'a [PathBuf],
    /// The session's scratch root, one of `roots`.
    pub scratch: Option<&'a Path>,
    /// The auto-memory store, one of `roots`.
    pub memory: Option<&'a Path>,
    pub mode: PermissionMode,
    pub model: &'a str,
    pub guardian_label: &'a str,
    pub guardian_unavailable: Option<&'a str>,
    pub guardian_off: bool,
    pub guardian_profile: &'a str,
    pub output_style: Option<&'a str>,
    pub mcp_connected: usize,
    pub mcp_configured: usize,
}

pub fn status(info: &StatusInfo) -> String {
    let mut out = format!("Fontaine {} · session {}", info.version, info.session_id);
    if let Some((primary, added)) = info.roots.split_first() {
        let _ = write!(out, "\nworkspace: {}", primary.display());
        let added: Vec<_> = added
            .iter()
            .filter(|p| info.scratch != Some(p.as_path()) && info.memory != Some(p.as_path()))
            .map(|p| p.display().to_string())
            .collect();
        if !added.is_empty() {
            let _ = write!(out, "\nadded roots: {}", added.join(", "));
        }
        if let Some(memory) = info.memory {
            let _ = write!(out, "\nauto memory: {}", memory.display());
        }
        if let Some(scratch) = info.scratch {
            let _ = write!(out, "\nscratch: {}", scratch.display());
        }
    }
    let _ = write!(out, "\nmode: {}", mode_label(info.mode));
    let _ = write!(out, "\nmodel: {}", info.model);
    if info.guardian_off {
        let _ = write!(
            out,
            "\nguardian: OFF for this session (every call runs unchecked)"
        );
    } else {
        match info.guardian_unavailable {
            Some(reason) => {
                let _ = write!(out, "\nguardian: unavailable ({reason})");
            }
            None => {
                let _ = write!(
                    out,
                    "\nguardian: {} (profile {})",
                    info.guardian_label, info.guardian_profile
                );
            }
        }
    }
    let _ = write!(
        out,
        "\noutput style: {}",
        info.output_style.unwrap_or("none")
    );
    let _ = write!(
        out,
        "\nMCP servers: {} of {} connected",
        info.mcp_connected, info.mcp_configured
    );
    out
}

fn rule_list(out: &mut String, kind: &str, rules: &[SourcedRule], set_aside: &[String]) {
    if rules.is_empty() {
        let _ = write!(out, "\n{kind}: none");
        return;
    }
    let _ = write!(out, "\n{kind}:");
    for rule in rules {
        let label = rule.label();
        let marker = if set_aside.contains(&label) {
            " (set aside in auto mode)"
        } else {
            ""
        };
        let _ = write!(out, "\n  {label}{marker}");
    }
}

/// `suspended` are the allow rules auto mode sets aside; they are marked only in auto mode.
pub fn permissions(
    mode: PermissionMode,
    rules: &Rules,
    suspended: &[String],
    roots: &[PathBuf],
    user_config: Option<&Path>,
) -> String {
    let mut out = format!("mode: {}", mode_label(mode));
    rule_list(&mut out, "deny", &rules.deny, &[]);
    rule_list(&mut out, "ask", &rules.ask, &[]);
    let set_aside = if mode.rules() == PermissionMode::Auto {
        suspended
    } else {
        &[]
    };
    rule_list(&mut out, "allow", &rules.allow, set_aside);
    let roots: Vec<_> = roots.iter().map(|p| p.display().to_string()).collect();
    let _ = write!(out, "\nworkspace roots: {}", roots.join(", "));
    if let Some(path) = user_config {
        let _ = write!(out, "\nrules are edited in {}", path.display());
    }
    out.push_str("\n/denied lists what the guardian denied this session");
    out
}

/// The `/mcp` listing: each server's state with the label of the scope that contributed it.
pub fn mcp_with_origins(
    servers: &[(String, McpState)],
    origins: &BTreeMap<String, Origin>,
) -> String {
    if servers.is_empty() {
        return "no MCP servers configured".to_string();
    }
    servers
        .iter()
        .map(|(name, state)| {
            let state = match state {
                McpState::Connected => "connected".to_string(),
                McpState::Pending { attempt } => format!(
                    "pending (reconnecting, attempt {attempt} of {})",
                    fontaine_core::agent::MCP_RECONNECT_ATTEMPTS
                ),
                McpState::Failed(error) => format!("failed: {error}"),
                McpState::LeftOff => "left off".to_string(),
                McpState::NotTrusted => "not loaded (workspace not trusted)".to_string(),
            };
            let origin = origins
                .get(name)
                .map(|origin| origin.slot.label())
                .unwrap_or("configured");
            format!("{name}: {state} [{origin}]")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every event that has hooks, with each hook's matcher, type, target and scope, and why one that
/// is configured does not run. A last line names the events with none.
pub fn hooks(hooks: &Hooks, project_executables: bool) -> String {
    let mut sections = Vec::new();
    let mut empty = Vec::new();
    for (event, list) in hooks.iter() {
        if list.is_empty() {
            empty.push(event.config_key());
            continue;
        }
        let mut section = format!("{}:", event.config_key());
        for hook in list {
            let matcher = hook
                .matcher
                .as_deref()
                .unwrap_or(match event.matcher_kind() {
                    MatcherKind::Tool => "every call",
                    _ => "every time",
                });
            let _ = write!(
                section,
                "\n  {matcher} → ({}) {} [{}]",
                hook.kind.as_str(),
                hook.target(),
                hook.origin.slot.label()
            );
            if let Some(secs) = hook.timeout_secs {
                let _ = write!(section, " timeout {secs}s");
            }
            if let Some(why) = not_run(event, hook, project_executables) {
                let _ = write!(section, " not run ({why})");
            }
        }
        sections.push(section);
    }
    if sections.is_empty() {
        return "no hooks configured".to_string();
    }
    sections.push(format!("no hooks for: {}", empty.join(", ")));
    sections.join("\n")
}

/// Why a configured hook does not run, when it does not.
fn not_run(event: HookEvent, hook: &Hook, project_executables: bool) -> Option<&'static str> {
    if !hook.origin.is_project() {
        return None;
    }
    if !project_executables {
        return Some("workspace not trusted");
    }
    if matches!(event.project_scope(), ProjectScope::IgnoredWithWarning(_)) {
        return Some("a repository may not define it: it forces model calls");
    }
    if hook.kind != HandlerKind::Command && !hook.origin.user_trust {
        return Some("a repository may not define http or prompt hooks");
    }
    None
}

pub fn skills(extensions: &Extensions) -> String {
    if extensions.skills.is_empty() {
        return "no skills loaded".to_string();
    }
    extensions
        .skills
        .iter()
        .map(|skill| {
            let scope = match skill.scope {
                Scope::User => "user".to_string(),
                Scope::Project => "project".to_string(),
                Scope::Plugin => format!(
                    "plugin {}",
                    skill.name.split(':').next().unwrap_or(&skill.name)
                ),
            };
            let mut line = format!("{} — {} [{scope}]", skill.name, skill.description);
            if !skill.user_invocable {
                line.push_str(" (model only)");
            }
            if skill.disable_model_invocation {
                line.push_str(" (you only)");
            }
            if skill.user_invocable && is_builtin(&skill.name) {
                let _ = write!(line, " (hidden by /{})", skill.name);
            }
            match extensions.trust_state(skill) {
                TrustState::Trusted => line.push_str(" (trusted)"),
                TrustState::Stale => line.push_str(" (changed (re-trust))"),
                TrustState::Untrusted => {}
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `/plugins`: installed plugins from the registry, with their loaded component counts.
pub fn plugins(
    registry: &Registry,
    extensions: &Extensions,
    hooks: &Hooks,
    servers: &BTreeMap<String, McpServer>,
    origins: &BTreeMap<String, Origin>,
) -> String {
    if registry.plugin.is_empty() {
        return "no plugins installed".to_string();
    }
    registry
        .plugin
        .iter()
        .map(|plugin| {
            let prefix = format!("{}:", plugin.name);
            let skills = extensions
                .skills
                .iter()
                .filter(|skill| skill.scope == Scope::Plugin && skill.name.starts_with(&prefix))
                .count();
            let agents = extensions
                .agent_types
                .iter()
                .filter(|agent| {
                    matches!(agent.origin, TypeOrigin::PluginFile(_))
                        && agent.name.starts_with(&prefix)
                })
                .count();
            let styles = extensions
                .output_styles
                .iter()
                .filter(|style| style.scope == Scope::Plugin && style.name.starts_with(&prefix))
                .count();
            let hooks = hooks
                .all()
                .filter(|hook| hook.plugin.as_deref() == Some(plugin.name.as_str()))
                .count();
            let server_prefix = format!("plugin_{}_", plugin.name);
            let servers = servers
                .keys()
                .filter(|name| {
                    name.starts_with(&server_prefix)
                        && origins
                            .get(*name)
                            .is_some_and(|origin| origin.slot == Slot::Plugin)
                })
                .count();
            let pin = fontaine_core::plugins::short_pin(&plugin.pin);
            format!(
                "{} ({}): {} — pin {} — {} skill(s), {} agent(s), {} style(s), {} hook(s), {} server(s)",
                plugin.name,
                plugin.marketplace,
                if plugin.enabled { "enabled" } else { "disabled" },
                pin,
                skills,
                agents,
                styles,
                hooks,
                servers
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Where the auto-memory store is and whether this session uses it.
pub struct AutoMemory<'a> {
    pub on: bool,
    pub dir: &'a Path,
}

fn age(modified: SystemTime) -> String {
    let secs = SystemTime::now()
        .duration_since(modified)
        .map_or(0, |d| d.as_secs());
    match secs {
        0..60 => "just now".to_string(),
        60..3600 => format!("{}m ago", secs / 60),
        3600..86_400 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86_400),
    }
}

/// `/memory`: the auto-memory header, then the instruction files and the saved notes,
/// numbered for `/memory <n>`.
pub fn memory(entries: &[MemoryEntry], auto: &AutoMemory<'_>) -> String {
    let mut out = format!(
        "auto memory: {} ({})\ninstruction files and saved notes (`/memory <n>` opens one in your editor):",
        if auto.on { "on" } else { "off" },
        auto.dir.display()
    );
    for (at, entry) in entries.iter().enumerate() {
        let what = match &entry.kind {
            MemoryKind::Launch(origin) => origin.label(),
            MemoryKind::OnDemand => "loaded on demand".to_string(),
            MemoryKind::NotCreated => "not created".to_string(),
            MemoryKind::Present => "empty or a repeat".to_string(),
            MemoryKind::Auto { modified } => match modified {
                Some(at) => format!("saved note, modified {}", age(*at)),
                None => "saved note".to_string(),
            },
        };
        let _ = write!(out, "\n  {}. {} — {what}", at + 1, entry.path.display());
    }
    out
}

fn afk_outcome_words(report: &AfkReport) -> String {
    let source = match report.stretch.source {
        fontaine_core::afk_report::AfkSource::Command => "/afk",
        fontaine_core::afk_report::AfkSource::Cli => "the session start",
        fontaine_core::afk_report::AfkSource::Cycle => "Shift+Tab",
        fontaine_core::afk_report::AfkSource::Plan => "/plan",
    };
    let span = match (report.stretch.ended_ts_ms, report.stretch.started_ts_ms) {
        (Some(end), start) => format!(", lasted {}s", end.saturating_sub(start) / 1000),
        (None, _) => String::new(),
    };
    format!("{} (started by {source}{span})", report.outcome.as_str())
}

fn afk_attempt_line(attempt: &Attempt, start_ms: u64) -> String {
    let decision = match attempt.decision {
        AttemptDecision::Allowed => "allowed".to_string(),
        AttemptDecision::Denied => "denied".to_string(),
        AttemptDecision::Converted => "denied (needed approval)".to_string(),
    };
    let mut line = format!(
        "  #{} +{}s {} {} {decision}",
        attempt.seq,
        attempt.ts_ms.saturating_sub(start_ms) / 1000,
        attempt.agent_name,
        attempt.tool,
    );
    if let Some(rule) = &attempt.rule {
        let _ = write!(line, " [{rule}]");
    }
    if !attempt.summary.is_empty() {
        let _ = write!(line, ": {}", one_line(&attempt.summary));
    }
    if attempt.decision == AttemptDecision::Allowed {
        let _ = write!(
            line,
            " — {}",
            match attempt.outcome {
                Some(fontaine_core::afk_report::OutcomeStatus::Ok) => "succeeded",
                Some(fontaine_core::afk_report::OutcomeStatus::Error) => "failed",
                Some(fontaine_core::afk_report::OutcomeStatus::Cancelled) => "cancelled",
                None => "outcome not recorded",
            }
        );
    } else if let Some(why) = &attempt.explanation {
        let _ = write!(line, " — {}", one_line(why));
    }
    line
}

/// The `/afk report` text: the outcome, the temperature, the security review, then every call
/// of the stretch in order. Credential-shaped text was redacted when the ledger recorded it.
pub fn afk_report(report: &AfkReport) -> String {
    let counts = &report.counts;
    let mut lines = vec![
        "AFK report".to_string(),
        format!("outcome: {}", afk_outcome_words(report)),
        format!(
            "calls: {} allowed ({} succeeded, {} failed, {} cancelled, {} outcome not recorded), {} denied ({} needed approval nobody could give)",
            counts.allowed,
            counts.succeeded,
            counts.failed,
            counts.cancelled,
            counts.unknown,
            counts.denied,
            counts.converted,
        ),
    ];
    let temperature = &report.temperature;
    lines.push(format!(
        "temperature: {:.1} now, peak {:.1}{}, ceiling {:.1}",
        temperature.current,
        temperature.peak,
        temperature
            .peak_seq
            .map_or(String::new(), |seq| format!(" at #{seq}")),
        temperature.ceiling,
    ));
    let mut top: Vec<_> = temperature.contributing.iter().collect();
    top.sort_by(|a, b| b.weight.total_cmp(&a.weight));
    for c in top.into_iter().take(3) {
        lines.push(format!(
            "  denial #{} added {:.1} (now {:.1})",
            c.seq, c.weight, c.remaining
        ));
    }
    lines.push("security review:".to_string());
    for link in &report.review.links {
        let kind = match link.kind {
            LinkKind::ExactRetry => "retry",
            LinkKind::SameTargetOtherTool => "same target by another tool",
            LinkKind::Suspected => "suspected evasion",
        };
        lines.push(format!(
            "  {kind}: call #{} after the denial of #{} — {}",
            link.attempt_seq,
            link.denial_seq,
            one_line(&link.reason)
        ));
    }
    lines.push(format!("  {}", report.review.assessment.describe()));
    lines.push("timeline:".to_string());
    if report.timeline.is_empty() {
        lines.push("  no calls were made".to_string());
    }
    lines.extend(
        report
            .timeline
            .iter()
            .map(|attempt| afk_attempt_line(attempt, report.stretch.started_ts_ms)),
    );
    lines.join("\n")
}

/// The one-paragraph notice after a stretch ends, blocks or stops.
pub fn afk_outcome_notice(report: &AfkReport) -> String {
    let counts = &report.counts;
    let head = match report.outcome {
        AfkOutcome::Blocked => "AFK: blocked — repeated denials paused auto mode.",
        AfkOutcome::Stopped => "AFK: stopped — the temperature ceiling was reached.",
        AfkOutcome::Ended => "AFK: ended.",
        AfkOutcome::Active => "AFK:",
    };
    format!(
        "{head} {} allowed, {} denied, temperature {:.1} (peak {:.1}). Full report: /afk report",
        counts.allowed, counts.denied, report.temperature.current, report.temperature.peak
    )
}

fn window_label(window: u32) -> String {
    compact_count(u64::from(window))
}

/// One row of the `/context` report, with the items an `all` report lists under it.
struct Row<'a> {
    label: &'static str,
    tokens: u64,
    count: Option<usize>,
    items: &'a [(String, u64)],
}

/// What `/recap` reports, gathered by the handler.
pub struct RecapInput<'a> {
    pub recap: &'a LedgerRecap,
    /// The active goal's condition.
    pub goal: Option<&'a str>,
    /// Subagents running now, from the live tree: the ledger cannot tell a running agent from
    /// one cut off with its process.
    pub subagents_running: usize,
    pub mode: PermissionMode,
}

const RECAP_RECENT: usize = 3;
const RECAP_FILES: usize = 5;
const RECAP_TOOLS: usize = 5;
const RECAP_LINE: usize = 100;

fn one_line(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    truncate(line, RECAP_LINE)
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The `/recap` report: the ledger's figures, each line left out when it has nothing to say.
pub fn recap(input: &RecapInput) -> String {
    let recap = input.recap;
    let mut lines: Vec<String> = Vec::new();
    if let Some((first, rest)) = recap.user_messages.split_first() {
        lines.push(format!(
            "you asked: {} since the last /clear or /rewind",
            plural(recap.user_messages.len(), "message", "messages")
        ));
        lines.push(format!("  first: {}", one_line(first)));
        let from = rest.len().saturating_sub(RECAP_RECENT);
        for message in &rest[from..] {
            lines.push(format!("  then: {}", one_line(message)));
        }
    }
    if let Some(goal) = input.goal {
        lines.push(format!("goal: {}", one_line(goal)));
    }
    if !recap.written.is_empty() {
        let named: Vec<String> = recap.written.iter().take(RECAP_FILES).cloned().collect();
        let more = recap.written.len().saturating_sub(RECAP_FILES);
        let mut line = format!(
            "files allowed to be written or edited: {}: {}",
            recap.written.len(),
            named.join(", ")
        );
        if more > 0 {
            let _ = write!(line, " and {more} more");
        }
        lines.push(line);
    }
    if !recap.tools.is_empty() {
        let top: Vec<String> = recap
            .tools
            .iter()
            .take(RECAP_TOOLS)
            .map(|(tool, n)| format!("{tool} {n}"))
            .collect();
        lines.push(format!("tool calls: {}", top.join(", ")));
    }
    if let Some(last) = &recap.last_denial {
        let mut line = format!(
            "denials: {}, latest {} ({})",
            recap.denials, last.tool, last.rule
        );
        if !last.summary.is_empty() {
            let _ = write!(line, ": {}", one_line(&last.summary));
        }
        lines.push(line);
    }
    if recap.agents_spawned > 0 {
        lines.push(format!(
            "subagents: {} spawned, {} running now",
            recap.agents_spawned, input.subagents_running
        ));
    }
    if let Some(outcome) = recap.afk {
        lines.push(format!("afk: {} — /afk report", outcome.as_str()));
    }
    if lines.is_empty() {
        lines.push("nothing has happened in this session yet".to_string());
    }
    lines.push(format!("mode: {}", mode_label(input.mode)));
    format!("Session recap\n{}", lines.join("\n"))
}

/// What `/cost` reports: the ledger's buckets and the session cost budget.
pub struct UsageReport {
    /// In `UsageSource::ALL` order.
    pub by_source: [Bucket; 6],
    /// One bucket per role and model that served a call.
    pub by_model: Vec<(Served, Bucket)>,
    /// The `[agents]` session cost cap and what the session has spent against it.
    pub budget: Option<(f64, f64)>,
}

/// `/cost`: tokens, cache tokens and dollars per source, then the total.
pub fn cost(report: &UsageReport) -> String {
    let bucket = |source: UsageSource| report.by_source[source.index()];
    let shown = [
        ("main", UsageSource::Main),
        ("guardian", UsageSource::Guardian),
        ("subagents", UsageSource::Subagent),
        ("goal", UsageSource::Goal),
        ("compaction", UsageSource::Compaction),
        ("side questions", UsageSource::SideQuestion),
    ];
    let total = fontaine_core::usage_ledger::total(&report.by_source);
    let cache = total.cache_read_input_tokens > 0 || total.cache_creation_input_tokens > 0;
    let cost_cell = |b: &Bucket| {
        let mut cell = match b.cost_usd {
            Some(cost) => format!("${cost:.4}"),
            None => return "n/a".to_string(),
        };
        if b.unpriced_tokens > 0 {
            let _ = write!(cell, " · {} unpriced tok", compact_count(b.unpriced_tokens));
        }
        cell
    };
    let cells = |label: &str, b: &Bucket| {
        let mut row = vec![
            label.to_string(),
            b.calls.to_string(),
            compact_count(b.input_tokens),
            compact_count(b.output_tokens),
        ];
        if cache {
            row.push(compact_count(b.cache_read_input_tokens));
            row.push(compact_count(b.cache_creation_input_tokens));
        }
        row.push(cost_cell(b));
        row
    };
    let header = |first: &str| {
        let mut head = vec![first, "calls", "input", "output"];
        if cache {
            head.extend(["cache read", "cache write"]);
        }
        head.push("cost");
        head.into_iter().map(String::from).collect::<Vec<_>>()
    };
    let mut table: Vec<Vec<String>> = vec![header("source")];
    for (label, source) in shown {
        let b = bucket(source);
        if b.calls > 0 {
            table.push(cells(label, &b));
        }
    }
    let mut out = String::from("Session usage");
    if table.len() == 1 {
        out.push_str("\n  no model calls yet");
    } else {
        table.push(cells("total", &total));
        write_table(&mut out, &table);
        if !report.by_model.is_empty() {
            let mut by_model = vec![header("model")];
            by_model.extend(
                report
                    .by_model
                    .iter()
                    .map(|(served, bucket)| cells(&served.to_string(), bucket)),
            );
            out.push_str("\nBy model");
            write_table(&mut out, &by_model);
        }
    }
    out.push_str(
        "\nExcludes the guardian estimate for requests a composition dropped, the tool-result scanner and local screen calls, and the auto-mode setup scan.",
    );
    if let Some((spent, cap)) = report.budget {
        let _ = write!(out, "\nSession cost budget: ${spent:.4} / ${cap:.2}");
    }
    out
}

/// Appends `table` as aligned rows: the first column left, the last unpadded, the rest right.
fn write_table(out: &mut String, table: &[Vec<String>]) {
    let columns = table[0].len();
    let widths: Vec<usize> = (0..columns)
        .map(|c| {
            table
                .iter()
                .map(|row| row[c].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    for row in table {
        out.push_str("\n ");
        for (c, cell) in row.iter().enumerate() {
            match c {
                0 => {
                    let _ = write!(out, " {cell:<w$}", w = widths[c]);
                }
                c if c + 1 == columns => {
                    let _ = write!(out, "  {cell}");
                }
                _ => {
                    let _ = write!(out, "  {cell:>w$}", w = widths[c]);
                }
            }
        }
    }
}

/// `/context`: what fills the window, estimated. With `all`, each instruction file and tool
/// gets a line of its own under its row.
pub fn context(usage: &ContextUsage, all: bool) -> String {
    let total = usage.total();
    let mut out = match usage.window {
        Some(window) => format!(
            "context ~{} of {} tokens ({}%), estimated",
            compact_count(total),
            window_label(window),
            total.saturating_mul(100) / u64::from(window).max(1),
        ),
        None => format!(
            "context ~{} tokens, estimated (window unknown)",
            compact_count(total)
        ),
    };
    let _ = write!(out, " · {}", usage.model);

    let instructions: Vec<(String, u64)> = usage
        .instructions
        .iter()
        .map(|(path, n)| (path.display().to_string(), *n))
        .collect();
    let sum = |items: &[(String, u64)]| items.iter().map(|(_, n)| n).sum::<u64>();
    let row = |label, tokens, count, items| Row {
        label,
        tokens,
        count,
        items,
    };
    let mut rows = vec![row("system prompt", usage.system_prompt, None, &[][..])];
    if usage.output_style > 0 {
        rows.push(row("output style", usage.output_style, None, &[]));
    }
    rows.push(row("instructions", sum(&instructions), None, &instructions));
    if usage.memory > 0 {
        rows.push(row("memory", usage.memory, None, &[]));
    }
    rows.push(row("skills", usage.skills, None, &[]));
    rows.push(row(
        "tools",
        sum(&usage.tools),
        Some(usage.tools.len()),
        &usage.tools,
    ));
    rows.push(row(
        "MCP tools",
        sum(&usage.mcp_tools),
        Some(usage.mcp_tools.len()),
        &usage.mcp_tools,
    ));
    rows.push(row("messages", usage.messages, None, &[]));
    let free = usage
        .window
        .map(|window| u64::from(window).saturating_sub(total));
    let width = rows
        .iter()
        .map(|row| compact_count(row.tokens).len())
        .chain(free.map(|n| compact_count(n).len()))
        .max()
        .unwrap_or(1);
    for row in &rows {
        let _ = write!(
            out,
            "\n  {:<14}{:>width$}",
            row.label,
            compact_count(row.tokens)
        );
        if let Some(count) = row.count {
            let _ = write!(out, "  ({count})");
        }
        if all {
            let mut items: Vec<&(String, u64)> = row.items.iter().collect();
            items.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            for (name, n) in items {
                let _ = write!(out, "\n    {name}  {}", compact_count(*n));
            }
        }
    }
    if let Some(free) = free {
        let _ = write!(out, "\n  {:<14}{:>width$}", "free", compact_count(free));
    }

    let reserved = usage
        .window
        .map(|window| u64::from(window).saturating_sub(usage.reserve));
    out.push('\n');
    match (usage.listed, usage.autocompact_at) {
        (false, _) => out.push_str("no automatic compaction: the model is not in the catalog"),
        (true, None) => out.push_str("no automatic compaction: the model's window is unknown"),
        (true, Some(at)) => {
            let configured = usage.configured == Some(at) && reserved.is_none_or(|r| at <= r);
            let _ = write!(
                out,
                "auto-compacts when messages pass ~{} (",
                compact_count(at)
            );
            if configured {
                out.push_str("[context] autocompact_window)");
            } else {
                let _ = write!(
                    out,
                    "window minus a {} reserve)",
                    compact_count(usage.reserve)
                );
            }
        }
    }
    if let Some(window) = usage.window
        && total > u64::from(window)
    {
        let _ = write!(
            out,
            "\nover the window by ~{} tokens: /compact or /clear frees space",
            compact_count(total - u64::from(window))
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use fontaine_core::config::{Origin, Slot};
    use fontaine_core::extensions::Skill;
    use fontaine_core::permission_rules::{RuleSource, parse_rule};

    fn recap_input(recap: &LedgerRecap) -> RecapInput<'_> {
        RecapInput {
            recap,
            goal: None,
            subagents_running: 0,
            mode: PermissionMode::Manual,
        }
    }

    #[test]
    fn an_empty_recap_says_so_and_still_names_the_mode() {
        let recap = LedgerRecap::default();
        let text = super::recap(&recap_input(&recap));
        assert_eq!(
            text,
            "Session recap\nnothing has happened in this session yet\nmode: manual"
        );
    }

    #[test]
    fn a_recap_lists_what_the_ledger_holds_and_leaves_out_what_is_empty() {
        use fontaine_core::ledger::DenialEntry;
        let recap = LedgerRecap {
            user_messages: ["one", "two", "three", "four", "five", "six\nsecond line"]
                .map(String::from)
                .to_vec(),
            written: (0..7).map(|i| format!("/w/f{i}.rs")).collect(),
            tools: vec![("shell".into(), 4), ("write".into(), 1)],
            denials: 2,
            last_denial: Some(DenialEntry {
                tool: "shell".into(),
                agent: "root".into(),
                rule: "deny(shell)".into(),
                summary: "rm -rf x".into(),
            }),
            afk: Some(fontaine_core::afk_report::AfkOutcome::Blocked),
            agents_spawned: 3,
        };
        let text = super::recap(&RecapInput {
            goal: Some("tests pass"),
            subagents_running: 1,
            ..recap_input(&recap)
        });
        assert!(text.contains("you asked: 6 messages since the last /clear or /rewind"));
        assert!(text.contains("  first: one\n  then: four\n  then: five\n  then: six\n"));
        assert!(!text.contains("two") && !text.contains("three") && !text.contains("second line"));
        assert!(text.contains("goal: tests pass"));
        assert!(text.contains(
            "files allowed to be written or edited: 7: /w/f0.rs, /w/f1.rs, /w/f2.rs, /w/f3.rs, /w/f4.rs and 2 more"
        ));
        assert!(text.contains("tool calls: shell 4, write 1"));
        assert!(text.contains("denials: 2, latest shell (deny(shell)): rm -rf x"));
        assert!(text.contains("subagents: 3 spawned, 1 running now"));
        assert!(text.contains("afk: blocked — /afk report"));
        assert!(text.ends_with("mode: manual"));
    }

    #[test]
    fn a_recap_cuts_a_long_message_to_one_line() {
        let recap = LedgerRecap {
            user_messages: vec!["x".repeat(500)],
            ..LedgerRecap::default()
        };
        let text = super::recap(&recap_input(&recap));
        assert!(
            text.contains(&format!("first: {}…", "x".repeat(99))),
            "{text}"
        );
    }

    fn rule(text: &str, source: RuleSource) -> SourcedRule {
        SourcedRule::new(parse_rule(text).unwrap(), source)
    }

    fn hook(matcher: Option<&str>, command: &str, slot: Slot, timeout: Option<u64>) -> Hook {
        Hook {
            timeout_secs: timeout,
            origin: Origin {
                slot,
                user_trust: true,
            },
            ..Hook::command(matcher, command)
        }
    }

    fn skill(name: &str, scope: Scope) -> Skill {
        Skill {
            name: name.to_string(),
            description: "does it".to_string(),
            body: String::new(),
            argument_hint: None,
            user_invocable: true,
            disable_model_invocation: false,
            allowed_tools: Vec::new(),
            declared_tools: Vec::new(),
            digest: format!("sha256:{name}"),
            scope,
            path: PathBuf::new(),
        }
    }

    fn info<'a>(roots: &'a [PathBuf]) -> StatusInfo<'a> {
        StatusInfo {
            version: "1.2.3",
            session_id: "s1",
            roots,
            scratch: None,
            memory: None,
            mode: PermissionMode::Plan,
            model: "work/claude",
            guardian_label: "work/haiku",
            guardian_unavailable: None,
            guardian_off: false,
            guardian_profile: "full",
            output_style: None,
            mcp_connected: 1,
            mcp_configured: 2,
        }
    }

    #[test]
    fn help_lists_builtins_then_skills_then_keys() {
        let skill = MenuEntry {
            name: "review".into(),
            hint: Some("[pr]".into()),
            description: "Review a pull request".into(),
            aliases: vec![],
        };
        let text = help(&[skill], &Keymap::default());
        for builtin in BUILTINS {
            assert!(text.contains(&format!("/{}", builtin.name)), "{text}");
        }
        assert!(text.contains("/exit, /quit — leave Fontaine"), "{text}");
        assert!(text.contains("/agents, /tasks"), "{text}");
        assert!(
            text.contains("/add-dir <path> — add a workspace root"),
            "{text}"
        );
        let (builtins, rest) = text.split_once("skills:").unwrap();
        assert!(builtins.starts_with("commands:"));
        let (skills, keys) = rest.split_once("\nshift+tab").unwrap();
        assert!(skills.contains("/review [pr] — Review a pull request"));
        assert!(keys.contains("esc interrupt"));
        assert!(!help(&[], &Keymap::default()).contains("skills:"));
    }

    #[test]
    fn the_keys_line_renders_the_effective_bindings() {
        let text = help(&[], &Keymap::default());
        assert!(
            text.contains(
                "shift+tab mode · ctrl+o transcript · ctrl+t todos · ctrl+r history · ! shell · @ file"
            ),
            "{text}"
        );
        let keymap = Keymap::resolve(
            &[
                ("f2".to_string(), "agents_panel".to_string()),
                ("ctrl+t".to_string(), String::new()),
            ]
            .into_iter()
            .collect(),
        )
        .unwrap();
        let text = help(&[], &keymap);
        assert!(text.contains("f2 agents"), "{text}");
        assert!(!text.contains("ctrl+t"), "{text}");
        let text = help(&[], &Keymap::default());
        assert!(!text.contains(" agents ·"), "{text}");
    }

    #[test]
    fn status_labels_the_memory_root_instead_of_listing_it_as_added() {
        let roots = [PathBuf::from("/w"), PathBuf::from("/data/memory/k")];
        let mut i = info(&roots);
        i.memory = Some(Path::new("/data/memory/k"));
        let text = status(&i);
        assert!(text.contains("auto memory: /data/memory/k"), "{text}");
        assert!(!text.contains("added roots"), "{text}");
    }

    #[test]
    fn status_labels_the_scratch_root_instead_of_listing_it_as_added() {
        let roots = [
            PathBuf::from("/work"),
            PathBuf::from("/extra"),
            PathBuf::from("/run/scratch"),
        ];
        let mut i = info(&roots);
        i.scratch = Some(Path::new("/run/scratch"));
        let text = status(&i);
        assert!(text.contains("added roots: /extra\n"), "{text}");
        assert!(text.contains("scratch: /run/scratch"), "{text}");
        assert!(!text.contains("extra, /run/scratch"), "{text}");
    }

    #[test]
    fn status_shows_the_session_and_what_it_runs_on() {
        let roots = [PathBuf::from("/work"), PathBuf::from("/extra")];
        let text = status(&info(&roots));
        assert!(text.contains("Fontaine 1.2.3 · session s1"), "{text}");
        assert!(text.contains("workspace: /work"), "{text}");
        assert!(text.contains("added roots: /extra"), "{text}");
        assert!(text.contains("mode: ⏸ plan mode on"), "{text}");
        assert!(text.contains("model: work/claude"), "{text}");
        assert!(
            text.contains("guardian: work/haiku (profile full)"),
            "{text}"
        );
        assert!(text.contains("output style: none"), "{text}");
        assert!(text.contains("MCP servers: 1 of 2 connected"), "{text}");
    }

    #[test]
    fn status_shows_why_the_guardian_is_unavailable() {
        let roots = [PathBuf::from("/work")];
        let mut i = info(&roots);
        i.guardian_unavailable = Some("no key");
        i.output_style = Some("terse");
        let text = status(&i);
        assert!(text.contains("guardian: unavailable (no key)"), "{text}");
        assert!(text.contains("output style: terse"), "{text}");
        assert!(!text.contains("added roots"), "{text}");
    }

    #[test]
    fn permissions_group_rules_with_their_source() {
        let rules = Rules {
            deny: vec![rule("shell(rm)", RuleSource::Config(Slot::User))],
            ask: Vec::new(),
            allow: vec![rule("shell(cargo)", RuleSource::Session)],
        };
        let roots = [PathBuf::from("/work")];
        let text = permissions(
            PermissionMode::Manual,
            &rules,
            &[],
            &roots,
            Some(Path::new("/home/u/config.toml")),
        );
        assert!(text.contains("deny:\n  shell(rm) [user]"), "{text}");
        assert!(text.contains("ask: none"), "{text}");
        assert!(text.contains("allow:\n  shell(cargo) [session]"), "{text}");
        assert!(text.contains("workspace roots: /work"), "{text}");
        assert!(
            text.contains("rules are edited in /home/u/config.toml"),
            "{text}"
        );
        assert!(
            text.contains("/denied lists what the guardian denied"),
            "{text}"
        );
        let without = permissions(PermissionMode::Manual, &rules, &[], &roots, None);
        assert!(!without.contains("rules are edited"), "{without}");
    }

    #[test]
    fn the_set_aside_marker_needs_auto_mode_and_a_suspended_rule() {
        let rules = Rules {
            allow: vec![
                rule("shell(bash)", RuleSource::Config(Slot::User)),
                rule("shell(ls)", RuleSource::Config(Slot::User)),
            ],
            ..Rules::default()
        };
        let suspended = vec!["shell(bash) [user]".to_string()];
        let roots = [PathBuf::from("/work")];
        let auto = permissions(PermissionMode::Auto, &rules, &suspended, &roots, None);
        assert!(
            auto.contains("shell(bash) [user] (set aside in auto mode)"),
            "{auto}"
        );
        assert!(!auto.contains("shell(ls) [user] (set aside"), "{auto}");
        let manual = permissions(PermissionMode::Manual, &rules, &suspended, &roots, None);
        assert!(!manual.contains("set aside"), "{manual}");
    }

    #[test]
    fn mcp_shows_each_state_with_its_origin() {
        let origins = BTreeMap::from([
            (
                "a".to_string(),
                Origin {
                    slot: Slot::User,
                    user_trust: true,
                },
            ),
            (
                "plugin_p_srv".to_string(),
                Origin {
                    slot: Slot::Plugin,
                    user_trust: false,
                },
            ),
        ]);
        assert_eq!(mcp_with_origins(&[], &origins), "no MCP servers configured");
        let text = mcp_with_origins(
            &[
                ("a".into(), McpState::Connected),
                (
                    "b".into(),
                    McpState::Failed("MCP server `b` failed: boom".into()),
                ),
                ("c".into(), McpState::LeftOff),
                ("d".into(), McpState::Pending { attempt: 2 }),
                ("plugin_p_srv".into(), McpState::NotTrusted),
            ],
            &origins,
        );
        assert_eq!(
            text,
            "a: connected [user]\nb: failed: MCP server `b` failed: boom [configured]\nc: left off [configured]\nd: pending (reconnecting, attempt 2 of 5) [configured]\nplugin_p_srv: not loaded (workspace not trusted) [plugin]"
        );
    }

    #[test]
    fn plugins_lists_components_with_their_counts() {
        let registry = Registry {
            marketplace: Vec::new(),
            plugin: vec![fontaine_core::plugins::InstalledPlugin {
                name: "p".to_string(),
                marketplace: "team".to_string(),
                source: "./plugins/p".to_string(),
                pin: "a1b2c3d4e5".to_string(),
                enabled: true,
                ..Default::default()
            }],
        };
        let mut extensions = Extensions::default();
        extensions.skills.push(skill("p:review", Scope::Plugin));
        extensions.skills.push(skill("mine", Scope::User));
        let mut hooks = Hooks::default();
        let mut plugin_hook = hook(None, "check.sh", Slot::Plugin, None);
        plugin_hook.plugin = Some("p".to_string());
        hooks.list_mut(HookEvent::PreToolUse).push(plugin_hook);
        let servers = BTreeMap::from([(
            "plugin_p_srv".to_string(),
            McpServer {
                command: Some("srv".to_string()),
                args: None,
                env_vars: None,
                env: None,
                url: None,
            },
        )]);
        let origins = BTreeMap::from([(
            "plugin_p_srv".to_string(),
            Origin {
                slot: Slot::Plugin,
                user_trust: false,
            },
        )]);
        let text = plugins(&registry, &extensions, &hooks, &servers, &origins);
        assert_eq!(
            text,
            "p (team): enabled — pin a1b2c3d — 1 skill(s), 0 agent(s), 0 style(s), 1 hook(s), 1 server(s)"
        );
        assert_eq!(
            plugins(
                &Registry::default(),
                &extensions,
                &hooks,
                &servers,
                &origins
            ),
            "no plugins installed"
        );
    }

    #[test]
    fn hooks_group_by_event() {
        assert_eq!(hooks(&Hooks::default(), true), "no hooks configured");
        let mut configured = Hooks::default();
        configured.list_mut(HookEvent::PreToolUse).extend([
            hook(Some("shell(git push)"), "check.sh", Slot::User, Some(5)),
            hook(None, "log.sh", Slot::Project, None),
        ]);
        configured.list_mut(HookEvent::PermissionDenied).push(hook(
            None,
            "notify.sh",
            Slot::User,
            None,
        ));
        let text = hooks(&configured, true);
        assert!(
            text.starts_with(
                "pre_tool_use:\n  shell(git push) → (command) check.sh [user] timeout 5s\n  every call → (command) log.sh [project]\npermission_denied:\n  every call → (command) notify.sh [user]\nno hooks for: "
            ),
            "{text}"
        );
        assert!(text.contains("stop, "), "{text}");
        let untrusted = hooks(&configured, false);
        assert!(
            untrusted.contains("log.sh [project] not run (workspace not trusted)"),
            "{untrusted}"
        );
        assert!(!untrusted.contains("timeout 5s not run"), "{untrusted}");
    }

    #[test]
    fn hooks_show_types_and_why_a_repository_hook_does_not_run() {
        let mut configured = Hooks::default();
        let mut stop = hook(None, "stop.sh", Slot::Project, None);
        stop.origin.user_trust = false;
        configured.list_mut(HookEvent::Stop).push(stop);
        let mut post = Hook {
            kind: HandlerKind::Http,
            command: None,
            url: Some("https://example.test/hook".to_string()),
            origin: Origin {
                slot: Slot::Project,
                user_trust: false,
            },
            ..Hook::default()
        };
        configured
            .list_mut(HookEvent::PostToolUse)
            .push(post.clone());
        post.origin = Origin::default();
        configured.list_mut(HookEvent::SessionEnd).push(Hook {
            matcher: Some("clear".into()),
            ..post
        });
        let text = hooks(&configured, true);
        assert!(
            text.contains("stop:\n  every time → (command) stop.sh [project] not run (a repository may not define it"),
            "{text}"
        );
        assert!(
            text.contains("(http) https://example.test/hook [project] not run (a repository may not define http or prompt hooks)"),
            "{text}"
        );
        assert!(
            text.contains("session_end:\n  clear → (http) https://example.test/hook [user]\n"),
            "{text}"
        );
    }

    #[test]
    fn memory_lists_numbered_entries_with_their_origin_and_what_is_not_created() {
        use fontaine_core::instructions::Origin;
        let entries = vec![
            MemoryEntry {
                path: "/repo/CLAUDE.md".into(),
                kind: MemoryKind::Launch(Origin::Walk),
            },
            MemoryEntry {
                path: "/repo/docs/a.md".into(),
                kind: MemoryKind::Launch(Origin::Import {
                    from: "/repo/CLAUDE.md".into(),
                }),
            },
            MemoryEntry {
                path: "/repo/sub/CLAUDE.md".into(),
                kind: MemoryKind::OnDemand,
            },
            MemoryEntry {
                path: "/repo/FONTAINE.md".into(),
                kind: MemoryKind::NotCreated,
            },
            MemoryEntry {
                path: "/data/memory/k/MEMORY.md".into(),
                kind: MemoryKind::Auto {
                    modified: Some(SystemTime::now() - std::time::Duration::from_secs(7_300)),
                },
            },
        ];
        let text = memory(
            &entries,
            &AutoMemory {
                on: true,
                dir: Path::new("/data/memory/k"),
            },
        );
        assert!(
            text.starts_with("auto memory: on (/data/memory/k)\n"),
            "{text}"
        );
        assert!(
            text.contains("\n  5. /data/memory/k/MEMORY.md — saved note, modified 2h ago"),
            "{text}"
        );
        let off = memory(
            &entries[..1],
            &AutoMemory {
                on: false,
                dir: Path::new("/data/memory/k"),
            },
        );
        assert!(
            off.starts_with("auto memory: off (/data/memory/k)"),
            "{off}"
        );
        assert!(text.contains("\n  1. /repo/CLAUDE.md — project"), "{text}");
        assert!(
            text.contains("\n  2. /repo/docs/a.md — imported by /repo/CLAUDE.md"),
            "{text}"
        );
        assert!(
            text.contains("\n  3. /repo/sub/CLAUDE.md — loaded on demand"),
            "{text}"
        );
        assert!(
            text.contains("\n  4. /repo/FONTAINE.md — not created"),
            "{text}"
        );
    }

    #[test]
    fn skills_carry_their_scope_and_markers() {
        assert_eq!(skills(&Extensions::default()), "no skills loaded");
        let mut model_only = skill("internal", Scope::Project);
        model_only.user_invocable = false;
        let mut you_only = skill("deploy", Scope::User);
        you_only.disable_model_invocation = true;
        let extensions = Extensions {
            skills: vec![model_only, you_only, skill("status", Scope::User)],
            ..Extensions::default()
        };
        assert_eq!(
            skills(&extensions),
            "internal — does it [project] (model only)\ndeploy — does it [user] (you only)\nstatus — does it [user] (hidden by /status)"
        );
    }

    #[test]
    fn skills_show_their_trust_state() {
        let mut extensions = Extensions {
            skills: vec![
                skill("p:fresh", Scope::Plugin),
                skill("p:old", Scope::Plugin),
                skill("p:new", Scope::Plugin),
            ],
            ..Extensions::default()
        };
        let record = |name: &str, digest: &str| fontaine_core::skill_trust::TrustRecord {
            name: name.into(),
            digest: digest.into(),
            tools: Vec::new(),
            commands: Vec::new(),
        };
        extensions.trusted = fontaine_core::skill_trust::TrustedSkills::from_file(
            fontaine_core::skill_trust::TrustFile {
                skill: vec![
                    record("p:fresh", "sha256:p:fresh"),
                    record("p:old", "sha256:earlier"),
                ],
            },
        );
        let text = skills(&extensions);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].ends_with("[plugin p] (trusted)"), "{text}");
        assert!(
            lines[1].ends_with("[plugin p] (changed (re-trust))"),
            "{text}"
        );
        assert!(lines[2].ends_with("[plugin p]"), "{text}");
    }

    fn usage() -> ContextUsage {
        ContextUsage {
            model: "anthropic/claude-sonnet-5-5".into(),
            window: Some(200_000),
            autocompact_at: Some(183_616),
            configured: None,
            reserve: 16_384,
            listed: true,
            system_prompt: 2_100,
            output_style: 0,
            instructions: vec![("/w/FONTAINE.md".into(), 1_300)],
            memory: 0,
            skills: 400,
            tools: vec![("read".into(), 300), ("shell".into(), 900)],
            mcp_tools: vec![("mcp__gh__issue".into(), 2_900)],
            messages: 35_700,
        }
    }

    fn spent(calls: u64, input: u64, output: u64, cost: Option<f64>, unpriced: u64) -> Bucket {
        Bucket {
            calls,
            input_tokens: input,
            output_tokens: output,
            cost_usd: cost,
            unpriced_tokens: unpriced,
            ..Bucket::default()
        }
    }

    fn usage_report(rows: &[(UsageSource, Bucket)]) -> UsageReport {
        let mut by_source = [Bucket::default(); 6];
        for (source, bucket) in rows {
            by_source[source.index()] = *bucket;
        }
        UsageReport {
            by_source,
            by_model: Vec::new(),
            budget: None,
        }
    }

    #[test]
    fn cost_lists_priced_sources_in_order_with_a_total() {
        let text = cost(&usage_report(&[
            (
                UsageSource::Subagent,
                spent(8, 40_100, 5_000, Some(0.19), 0),
            ),
            (UsageSource::Main, spent(12, 84_200, 9_100, Some(0.412), 0)),
            (
                UsageSource::Guardian,
                spent(31, 22_000, 1_400, Some(0.015), 0),
            ),
        ]));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "Session usage");
        assert!(lines[1].starts_with("  source"), "{text}");
        assert!(lines[2].starts_with("  main"), "{text}");
        assert!(lines[3].starts_with("  guardian"), "{text}");
        assert!(lines[4].starts_with("  subagents"), "{text}");
        assert!(lines[5].starts_with("  total"), "{text}");
        assert!(lines[5].ends_with("$0.6170"), "{text}");
        assert!(lines[2].contains("84.2k"), "{text}");
        assert!(!text.contains("unpriced"), "{text}");
        assert!(!text.contains("budget"), "{text}");
    }

    #[test]
    fn cost_breaks_spend_down_by_role_and_model() {
        let mut report = usage_report(&[
            (UsageSource::Main, spent(12, 84_200, 9_100, Some(0.412), 0)),
            (
                UsageSource::Subagent,
                spent(8, 40_100, 5_000, Some(0.19), 0),
            ),
        ]);
        report.by_model = vec![
            (
                Served::new(Some("coding"), "anthropic/claude-sonnet-5-5"),
                spent(5, 30_000, 4_000, Some(0.1), 0),
            ),
            (
                Served::new(Some("coding"), "kimi/kimi-for-coding"),
                spent(3, 10_100, 1_000, Some(0.09), 0),
            ),
            (
                Served::direct("anthropic/claude-opus-5-5"),
                spent(12, 84_200, 9_100, Some(0.412), 0),
            ),
        ];
        let text = cost(&report);
        let at = text.find("By model").expect("a by-model block");
        let block: Vec<&str> = text[at..].lines().collect();
        assert!(block[1].starts_with("  model"), "{text}");
        assert!(
            block[2].starts_with("  coding · anthropic/claude-sonnet-5-5"),
            "{text}"
        );
        assert!(
            block[3].starts_with("  coding · kimi/kimi-for-coding"),
            "{text}"
        );
        assert!(
            block[4].starts_with("  anthropic/claude-opus-5-5"),
            "{text}"
        );
        assert!(block[2].ends_with("$0.1000"), "{text}");
        assert!(
            !cost(&usage_report(&[(
                UsageSource::Main,
                spent(1, 1, 1, Some(0.1), 0)
            )]))
            .contains("By model")
        );
    }

    #[test]
    fn cost_marks_a_partial_figure_with_the_unpriced_tokens() {
        let text = cost(&usage_report(&[
            (UsageSource::Main, spent(12, 84_200, 9_100, Some(0.412), 0)),
            (
                UsageSource::Subagent,
                spent(8, 40_100, 5_000, Some(0.19), 5_000),
            ),
        ]));
        let rows: Vec<&str> = text.lines().collect();
        assert!(rows[3].ends_with("$0.1900 · 5.0k unpriced tok"), "{text}");
        assert!(rows[4].ends_with("$0.6020 · 5.0k unpriced tok"), "{text}");
        assert!(!rows[2].contains("unpriced"), "{text}");
    }

    #[test]
    fn cost_with_no_priced_source_says_n_a_and_has_no_dollar_total() {
        let text = cost(&usage_report(&[(
            UsageSource::Main,
            spent(3, 1_000, 200, None, 1_200),
        )]));
        assert!(!text.contains('$'), "{text}");
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[2].ends_with("n/a"), "{text}");
        assert!(lines[3].ends_with("n/a"), "{text}");
    }

    #[test]
    fn cost_hides_the_cache_columns_when_every_cache_counter_is_zero() {
        let plain = cost(&usage_report(&[(
            UsageSource::Main,
            spent(1, 10, 1, Some(0.1), 0),
        )]));
        assert!(!plain.contains("cache"), "{plain}");

        let mut cached = spent(1, 10, 1, Some(0.1), 0);
        cached.cache_read_input_tokens = 61_000;
        cached.cache_creation_input_tokens = 3_200;
        let text = cost(&usage_report(&[(UsageSource::Main, cached)]));
        assert!(text.contains("cache read"), "{text}");
        assert!(text.contains("cache write"), "{text}");
        assert!(text.contains("61.0k"), "{text}");
    }

    #[test]
    fn cost_omits_sources_that_made_no_call() {
        let mut idle = spent(0, 500, 5, Some(0.2), 0);
        idle.calls = 0;
        let text = cost(&usage_report(&[
            (UsageSource::Main, spent(1, 10, 1, Some(0.1), 0)),
            (UsageSource::Goal, idle),
        ]));
        assert!(!text.contains("goal"), "{text}");
        let empty = cost(&usage_report(&[]));
        assert!(empty.contains("no model calls yet"), "{empty}");
        assert!(!empty.contains("total"), "{empty}");
    }

    #[test]
    fn cost_shows_the_budget_footer_only_with_a_cap_and_always_the_exclusions() {
        let mut report = usage_report(&[(UsageSource::Main, spent(1, 10, 1, Some(0.1), 0))]);
        let without = cost(&report);
        assert!(!without.contains("Session cost budget"), "{without}");
        assert!(without.contains("setup scan"), "{without}");
        report.budget = Some((0.1, 5.0));
        let with = cost(&report);
        assert!(
            with.ends_with("Session cost budget: $0.1000 / $5.00"),
            "{with}"
        );
    }

    #[test]
    fn context_shows_the_window_the_rows_and_the_reserve() {
        let text = context(&usage(), false);
        assert_eq!(
            text,
            "context ~43.6k of 200.0k tokens (21%), estimated · anthropic/claude-sonnet-5-5\n  system prompt   2.1k\n  instructions    1.3k\n  skills           400\n  tools           1.2k  (2)\n  MCP tools       2.9k  (1)\n  messages       35.7k\n  free          156.4k\nauto-compacts when messages pass ~183.6k (window minus a 16.4k reserve)"
        );
    }

    #[test]
    fn context_has_a_memory_row_only_when_auto_memory_loaded_something() {
        assert!(!context(&usage(), false).contains("memory"));
        let mut u = usage();
        u.memory = 600;
        let text = context(&u, false);
        assert!(text.contains("\n  memory           600"), "{text}");
        assert_eq!(u.total(), usage().total() + 600);
        assert!(text.starts_with("context ~44.2k of 200.0k"), "{text}");
    }

    #[test]
    fn context_all_lists_items_largest_first_and_the_style_row_only_when_set() {
        let plain = context(&usage(), false);
        assert!(!plain.contains("output style"), "{plain}");
        let mut with_style = usage();
        with_style.output_style = 150;
        assert!(context(&with_style, false).contains("output style"));
        let text = context(&usage(), true);
        let shell = text.find("    shell").unwrap();
        let read = text.find("    read").unwrap();
        assert!(shell < read, "{text}");
        assert!(text.contains("    /w/FONTAINE.md  1.3k"), "{text}");
        assert!(text.contains("    mcp__gh__issue  2.9k"), "{text}");
    }

    #[test]
    fn context_without_a_known_window_has_no_free_row() {
        let mut u = usage();
        u.window = None;
        u.autocompact_at = None;
        let text = context(&u, false);
        assert!(
            text.starts_with("context ~43.6k tokens, estimated (window unknown) · "),
            "{text}"
        );
        assert!(!text.contains("free"), "{text}");
        assert!(
            text.ends_with("no automatic compaction: the model's window is unknown"),
            "{text}"
        );
        u.configured = Some(150_000);
        u.autocompact_at = Some(150_000);
        assert!(
            context(&u, false).ends_with(
                "auto-compacts when messages pass ~150.0k ([context] autocompact_window)"
            )
        );
    }

    #[test]
    fn context_names_the_configured_window_when_it_is_the_lower_limit() {
        let mut u = usage();
        u.configured = Some(50_000);
        u.autocompact_at = Some(50_000);
        assert!(
            context(&u, false).ends_with(
                "auto-compacts when messages pass ~50.0k ([context] autocompact_window)"
            )
        );
        u.configured = Some(900_000);
        u.autocompact_at = Some(183_616);
        assert!(context(&u, false).ends_with("(window minus a 16.4k reserve)"));
    }

    #[test]
    fn context_says_when_the_model_is_not_in_the_catalog() {
        let mut u = usage();
        u.window = None;
        u.autocompact_at = None;
        u.listed = false;
        assert!(
            context(&u, false)
                .ends_with("no automatic compaction: the model is not in the catalog")
        );
    }

    #[test]
    fn context_warns_when_the_conversation_is_over_the_window() {
        let mut u = usage();
        u.window = Some(30_000);
        u.autocompact_at = Some(13_616);
        let text = context(&u, false);
        assert!(
            text.ends_with("over the window by ~13.6k tokens: /compact or /clear frees space"),
            "{text}"
        );
    }
}
