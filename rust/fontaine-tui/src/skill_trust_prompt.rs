//! The confirmation before `/skills trust`: the user decides to honour a skill's `allowed-tools`,
//! so the dialog shows what the file declares now, or what changed since they last trusted it.

use crate::confirm_prompt::{Block, ConfirmPrompt};
use fontaine_core::extensions::{Scope, Skill};
use fontaine_core::skill_trust::{self, TrustRecord};

/// When the trust the dialog records takes effect, which depends on who asks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SkillTrustFooter {
    /// `/skills trust`: a running session keeps its grants until a reload.
    Command,
    /// A plugin install: the install's own reload applies it.
    Install,
}

impl SkillTrustFooter {
    fn text(self) -> &'static str {
        match self {
            SkillTrustFooter::Command => {
                "The trust applies after /reload-plugins or at the next session start; until then this session keeps its current grants."
            }
            SkillTrustFooter::Install => {
                "The trust applies when the install finishes loading, moments after this answer."
            }
        }
    }
}

/// `previous` is the record the user made for an earlier version of the file, if any.
/// `Some(true)` from the prompt records the trust, `Some(false)` leaves things as they are.
pub fn new(
    skill: &Skill,
    previous: Option<&TrustRecord>,
    footer: SkillTrustFooter,
) -> ConfirmPrompt {
    ConfirmPrompt::new(
        format!("Honour the allowed-tools of {}?", skill.name),
        describe(skill, previous, footer)
            .into_iter()
            .map(Block::Text)
            .collect(),
        "y trusts it · n leaves it as it is",
    )
}

/// What the dialog says, one entry per line (long ones wrap when drawn).
pub fn describe(
    skill: &Skill,
    previous: Option<&TrustRecord>,
    footer: SkillTrustFooter,
) -> Vec<String> {
    let current = skill_trust::record_of(skill);
    let (specific, wildcard) = skill_trust::split_tools(skill);
    let mut out = Vec::new();
    match previous {
        Some(before) => {
            out.push("The skill changed since you trusted it.".to_string());
            out.push(String::new());
            let tools = skill_trust::diff_lines(&before.tools, &current.tools);
            section(
                &mut out,
                "allowed-tools changes:",
                tools,
                "allowed-tools are unchanged",
            );
            let commands = skill_trust::diff_lines(&before.commands, &current.commands);
            section(
                &mut out,
                "`!` command changes:",
                commands,
                "its `!` commands are unchanged",
            );
        }
        None => {
            let user = skill.scope == Scope::User;
            section(
                &mut out,
                if user {
                    "allowed-tools already allowed for the turn, trusted or not:"
                } else {
                    "allowed-tools that become allows for the turn:"
                },
                specific.iter().map(|rule| format!("  {rule}")).collect(),
                "(none)",
            );
            section(
                &mut out,
                if user {
                    "wildcard shell rules, already allowed too; trust only shows them to the guardian as context:"
                } else {
                    "wildcard shell rules, shown to the guardian as context and never an allow:"
                },
                wildcard.iter().map(|rule| format!("  {rule}")).collect(),
                "(none)",
            );
            section(
                &mut out,
                "`!` commands it runs:",
                current.commands.iter().map(|c| format!("  {c}")).collect(),
                "(none)",
            );
        }
    }
    out.push(String::new());
    out.push(format!("file: {}", skill.path.display()));
    out.push(format!("digest: {}", current.digest));
    out.push(String::new());
    out.push(footer.text().to_string());
    out
}

fn section(out: &mut Vec<String>, heading: &str, lines: Vec<String>, empty: &str) {
    if lines.is_empty() {
        out.push(empty.to_string());
        return;
    }
    out.push(heading.to_string());
    out.extend(lines);
}

#[cfg(test)]
mod tests {
    use super::*;
    use fontaine_core::permission_rules::parse_rule;

    fn skill(tools: &[&str], body: &str) -> Skill {
        Skill {
            name: "p:pr".into(),
            description: "d".into(),
            body: body.into(),
            argument_hint: None,
            user_invocable: true,
            disable_model_invocation: false,
            allowed_tools: Vec::new(),
            declared_tools: tools.iter().map(|t| parse_rule(t).unwrap()).collect(),
            digest: "sha256:new".into(),
            scope: Scope::Plugin,
            path: "/c/pr/SKILL.md".into(),
        }
    }

    #[test]
    fn a_first_trust_lists_the_split_tools_and_the_commands() {
        let lines = describe(
            &skill(&["shell(git *)", "shell(git status)"], "!`git log`"),
            None,
            SkillTrustFooter::Command,
        )
        .join("\n");
        assert!(
            lines.contains("become allows for the turn:\n  shell(git status)"),
            "{lines}"
        );
        assert!(lines.contains("never an allow:\n  shell(git *)"), "{lines}");
        assert!(lines.contains("  !git log"), "{lines}");
        assert!(lines.contains("next session start"), "{lines}");
    }

    #[test]
    fn the_install_footer_says_the_trust_applies_when_the_install_loads() {
        let lines = describe(
            &skill(&["shell(git status)"], ""),
            None,
            SkillTrustFooter::Install,
        )
        .join("\n");
        assert!(
            lines.contains("when the install finishes loading"),
            "{lines}"
        );
        assert!(!lines.contains("/reload-plugins"), "{lines}");
    }

    #[test]
    fn a_user_skills_wildcards_are_not_called_never_an_allow() {
        let mut user = skill(&["shell(git *)", "shell(git status)"], "");
        user.scope = Scope::User;
        let lines = describe(&user, None, SkillTrustFooter::Command).join("\n");
        assert!(!lines.contains("never an allow"), "{lines}");
        assert!(
            lines.contains("already allowed too; trust only shows them to the guardian"),
            "{lines}"
        );
        assert!(
            lines.contains("trusted or not:\n  shell(git status)"),
            "{lines}"
        );
    }

    #[test]
    fn a_retrust_shows_the_tool_and_command_diffs() {
        let before = TrustRecord {
            name: "p:pr".into(),
            digest: "sha256:old".into(),
            tools: vec!["shell(git status)".into()],
            commands: vec!["!git log".into()],
        };
        let lines = describe(
            &skill(
                &["shell(git status)", "shell(rm -rf x)"],
                "!`git log` !`make`",
            ),
            Some(&before),
            SkillTrustFooter::Command,
        )
        .join("\n");
        assert!(
            lines.contains("+ shell(rm -rf x)") && !lines.contains("- shell(git status)"),
            "{lines}"
        );
        assert!(lines.contains("+ !make"), "{lines}");
        assert!(lines.contains("digest: sha256:new"), "{lines}");
    }
}
