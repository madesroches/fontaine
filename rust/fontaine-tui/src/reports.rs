//! The text of the read-only commands. Each builder takes plain data, so the report can be
//! tested without a runtime; `lib.rs` gathers the inputs.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use fontaine_core::config::{Hook, PermissionMode};
use fontaine_core::extensions::{Scope, Skill};
use fontaine_core::gate::Rules;
use fontaine_core::permission_rules::SourcedRule;

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

const KEYS: &str =
    "shift+tab mode · ctrl+t agents · ctrl+r history · ! shell · @ file · esc interrupt";

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
    pub mode: PermissionMode,
    pub model: &'a str,
    pub guardian_label: &'a str,
    pub guardian_unavailable: Option<&'a str>,
    pub guardian_profile: &'a str,
    pub output_style: Option<&'a str>,
    pub mcp_connected: usize,
    pub mcp_configured: usize,
}

pub fn status(info: &StatusInfo) -> String {
    let mut out = format!("Fontaine {} · session {}", info.version, info.session_id);
    if let Some((primary, added)) = info.roots.split_first() {
        let _ = write!(out, "\nworkspace: {}", primary.display());
        if !added.is_empty() {
            let added: Vec<_> = added.iter().map(|p| p.display().to_string()).collect();
            let _ = write!(out, "\nadded roots: {}", added.join(", "));
        }
    }
    let _ = write!(out, "\nmode: {}", mode_label(info.mode));
    let _ = write!(out, "\nmodel: {}", info.model);
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

pub fn mcp(servers: &[(String, McpState)]) -> String {
    if servers.is_empty() {
        return "no MCP servers configured".to_string();
    }
    servers
        .iter()
        .map(|(name, state)| match state {
            McpState::Connected => format!("{name}: connected"),
            McpState::Failed(error) => format!("failed: {error}"),
            McpState::LeftOff => format!("{name}: left off"),
            McpState::NotTrusted => format!("{name}: not loaded (workspace not trusted)"),
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
        ("PreToolUse", pre_tool_use),
        ("PostToolUse", post_tool_use),
        ("PermissionDenied", permission_denied),
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
                Scope::User => "user",
                Scope::Project => "project",
            };
            let mut line = format!("{} — {} [{scope}]", skill.name, skill.description);
            if !skill.user_invocable {
                line.push_str(" (model only)");
            }
            if skill.disable_model_invocation {
                line.push_str(" (you only)");
            }
            if is_builtin(&skill.name) {
                let _ = write!(line, " (hidden by /{})", skill.name);
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
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
            mode: PermissionMode::Plan,
            model: "work/claude",
            guardian_label: "work/haiku",
            guardian_unavailable: None,
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
    fn mcp_shows_each_state() {
        assert_eq!(mcp(&[]), "no MCP servers configured");
        let text = mcp(&[
            ("a".into(), McpState::Connected),
            (
                "b".into(),
                McpState::Failed("MCP server `b` failed: boom".into()),
            ),
            ("c".into(), McpState::LeftOff),
            ("d".into(), McpState::NotTrusted),
        ]);
        assert_eq!(
            text,
            "a: connected\nfailed: MCP server `b` failed: boom\nc: left off\nd: not loaded (workspace not trusted)"
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
            "PreToolUse:\n  shell(git push) → check.sh [user] timeout 5s\n  every call → log.sh [project]\nPermissionDenied:\n  every call → notify.sh [user]"
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
}
