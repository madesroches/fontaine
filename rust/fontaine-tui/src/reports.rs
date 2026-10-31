//! The text of the read-only commands. Each builder takes plain data, so the report can be
//! tested without a runtime; `lib.rs` gathers the inputs.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use fontaine_agents::registry::TypeOrigin;
use fontaine_core::config::{Hook, Hooks, McpServer, Origin, PermissionMode, Slot};
use fontaine_core::context::ContextUsage;
use fontaine_core::extensions::{Extensions, Scope, Skill};
use fontaine_core::gate::Rules;
use fontaine_core::instructions::{MemoryEntry, MemoryKind};
use fontaine_core::permission_rules::SourcedRule;
use fontaine_core::plugins::Registry;

use crate::commands::{BUILTINS, MenuEntry, is_builtin};
use crate::statusbar::mode_label;

/// What became of an MCP server at start-up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpState {
    Connected,
    /// The error text, which names the server.
    Failed(String),
    /// The human declined to start it.
    LeftOff,
    /// A repository's server in a workspace that is not trusted.
    NotTrusted,
}

const KEYS: &str = "shift+tab mode · ctrl+t agents · ctrl+o todos · ctrl+r history · ! shell · @ file · esc interrupt (or end a waiting /loop)";

fn command_line(names: &str, hint: Option<&str>, description: &str) -> String {
    match hint {
        Some(hint) => format!("{names} {hint} — {description}"),
        None => format!("{names} — {description}"),
    }
}

/// `skills` are the menu rows of the user-invocable skills no built-in hides.
pub fn help(skills: &[MenuEntry]) -> String {
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
    let _ = write!(out, "\n{KEYS}");
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
    let set_aside = if mode == PermissionMode::Auto {
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

pub fn hooks(
    pre_tool_use: &[Hook],
    post_tool_use: &[Hook],
    permission_denied: &[Hook],
    project_executables: bool,
) -> String {
    let mut sections = Vec::new();
    for (event, hooks) in [
        ("pre_tool_use", pre_tool_use),
        ("post_tool_use", post_tool_use),
        ("permission_denied", permission_denied),
    ] {
        if hooks.is_empty() {
            continue;
        }
        let mut section = format!("{event}:");
        for hook in hooks {
            let matcher = hook.matcher.as_deref().unwrap_or("every call");
            let _ = write!(
                section,
                "\n  {matcher} → {} [{}]",
                hook.command,
                hook.origin.slot.label()
            );
            if let Some(secs) = hook.timeout_secs {
                let _ = write!(section, " timeout {secs}s");
            }
            if hook.origin.is_project() && !project_executables {
                section.push_str(" not loaded (workspace not trusted)");
            }
        }
        sections.push(section);
    }
    if sections.is_empty() {
        return "no hooks configured".to_string();
    }
    sections.join("\n")
}

pub fn skills(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return "no skills loaded".to_string();
    }
    skills
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
                .pre_tool_use
                .iter()
                .chain(&hooks.post_tool_use)
                .chain(&hooks.permission_denied)
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

/// A token count as the reports show it: `842`, `35.7k`.
pub fn tokens(n: u64) -> String {
    if n < 1000 {
        n.to_string()
    } else {
        format!("{:.1}k", n as f64 / 1000.0)
    }
}

fn window_label(window: u32) -> String {
    if window >= 1000 && window.is_multiple_of(1000) {
        format!("{}k", window / 1000)
    } else {
        tokens(u64::from(window))
    }
}

/// One row of the `/context` report, with the items an `all` report lists under it.
struct Row<'a> {
    label: &'static str,
    tokens: u64,
    count: Option<usize>,
    items: &'a [(String, u64)],
}

/// `/context`: what fills the window, estimated. With `all`, each instruction file and tool
/// gets a line of its own under its row.
pub fn context(usage: &ContextUsage, all: bool) -> String {
    let total = usage.total();
    let mut out = match usage.window {
        Some(window) => format!(
            "context ~{} of {} tokens ({}%), estimated",
            tokens(total),
            window_label(window),
            total.saturating_mul(100) / u64::from(window).max(1),
        ),
        None => format!(
            "context ~{} tokens, estimated (window unknown)",
            tokens(total)
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
        .map(|row| tokens(row.tokens).len())
        .chain(free.map(|n| tokens(n).len()))
        .max()
        .unwrap_or(1);
    for row in &rows {
        let _ = write!(out, "\n  {:<14}{:>width$}", row.label, tokens(row.tokens));
        if let Some(count) = row.count {
            let _ = write!(out, "  ({count})");
        }
        if all {
            let mut items: Vec<&(String, u64)> = row.items.iter().collect();
            items.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            for (name, n) in items {
                let _ = write!(out, "\n    {name}  {}", tokens(*n));
            }
        }
    }
    if let Some(free) = free {
        let _ = write!(out, "\n  {:<14}{:>width$}", "free", tokens(free));
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
            let _ = write!(out, "auto-compacts when messages pass ~{} (", tokens(at));
            if configured {
                out.push_str("[context] autocompact_window)");
            } else {
                let _ = write!(out, "window minus a {} reserve)", tokens(usage.reserve));
            }
        }
    }
    if let Some(window) = usage.window
        && total > u64::from(window)
    {
        let _ = write!(
            out,
            "\nover the window by ~{} tokens: /compact or /clear frees space",
            tokens(total - u64::from(window))
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use fontaine_core::config::{Origin, Slot};
    use fontaine_core::permission_rules::{RuleSource, parse_rule};

    fn rule(text: &str, source: RuleSource) -> SourcedRule {
        SourcedRule::new(parse_rule(text).unwrap(), source)
    }

    fn hook(matcher: Option<&str>, command: &str, slot: Slot, timeout: Option<u64>) -> Hook {
        Hook {
            matcher: matcher.map(str::to_string),
            command: command.to_string(),
            timeout_secs: timeout,
            origin: Origin {
                slot,
                user_trust: true,
            },
            plugin: None,
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
        let text = help(&[skill]);
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
        assert!(!help(&[]).contains("skills:"));
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
                ("plugin_p_srv".into(), McpState::NotTrusted),
            ],
            &origins,
        );
        assert_eq!(
            text,
            "a: connected [user]\nb: failed: MCP server `b` failed: boom [configured]\nc: left off [configured]\nplugin_p_srv: not loaded (workspace not trusted) [plugin]"
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
        hooks.pre_tool_use.push(plugin_hook);
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
        assert_eq!(hooks(&[], &[], &[], true), "no hooks configured");
        let pre = [
            hook(Some("shell(git push)"), "check.sh", Slot::User, Some(5)),
            hook(None, "log.sh", Slot::Project, None),
        ];
        let denied = [hook(None, "notify.sh", Slot::User, None)];
        let text = hooks(&pre, &[], &denied, true);
        assert_eq!(
            text,
            "pre_tool_use:\n  shell(git push) → check.sh [user] timeout 5s\n  every call → log.sh [project]\npermission_denied:\n  every call → notify.sh [user]"
        );
        let untrusted = hooks(&pre, &[], &[], false);
        assert!(
            untrusted.contains("every call → log.sh [project] not loaded (workspace not trusted)"),
            "{untrusted}"
        );
        assert!(
            !untrusted.contains("check.sh [user] timeout 5s not loaded"),
            "{untrusted}"
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
        assert_eq!(skills(&[]), "no skills loaded");
        let mut model_only = skill("internal", Scope::Project);
        model_only.user_invocable = false;
        let mut you_only = skill("deploy", Scope::User);
        you_only.disable_model_invocation = true;
        let text = skills(&[model_only, you_only, skill("status", Scope::User)]);
        assert_eq!(
            text,
            "internal — does it [project] (model only)\ndeploy — does it [user] (you only)\nstatus — does it [user] (hidden by /status)"
        );
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

    #[test]
    fn context_shows_the_window_the_rows_and_the_reserve() {
        let text = context(&usage(), false);
        assert_eq!(
            text,
            "context ~43.6k of 200k tokens (21%), estimated · anthropic/claude-sonnet-5-5\n  system prompt   2.1k\n  instructions    1.3k\n  skills           400\n  tools           1.2k  (2)\n  MCP tools       2.9k  (1)\n  messages       35.7k\n  free          156.4k\nauto-compacts when messages pass ~183.6k (window minus a 16.4k reserve)"
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
        assert!(text.starts_with("context ~44.2k of 200k"), "{text}");
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
