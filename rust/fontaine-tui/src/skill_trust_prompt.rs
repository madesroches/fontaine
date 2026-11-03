//! The confirmation before `/skills trust`: the user decides to honour a skill's `allowed-tools`,
//! so the dialog shows what the file declares now, or what changed since they last trusted it.

use std::time::Instant;

use fontaine_core::extensions::Skill;
use fontaine_core::skill_trust::{self, TrustRecord};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;

use crate::input::wrap;
use crate::server_prompt::ANSWER_GRACE;
use crate::theme::Theme;

pub struct SkillTrustPrompt {
    /// Set by the app; tells two consecutive prompts apart.
    pub(crate) id: u64,
    opened: Instant,
    pub name: String,
    lines: Vec<String>,
}

impl SkillTrustPrompt {
    /// `previous` is the record the user made for an earlier version of the file, if any.
    pub fn new(skill: &Skill, previous: Option<&TrustRecord>) -> SkillTrustPrompt {
        SkillTrustPrompt {
            id: 0,
            opened: Instant::now(),
            name: skill.name.clone(),
            lines: describe(skill, previous),
        }
    }

    /// `Some(true)` records the trust, `Some(false)` leaves things as they are.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<bool> {
        self.handle_key_at(key, Instant::now())
    }

    pub fn handle_key_at(&mut self, key: KeyEvent, now: Instant) -> Option<bool> {
        if key.kind != KeyEventKind::Press
            || now.saturating_duration_since(self.opened) < ANSWER_GRACE
            || key.modifiers.contains(KeyModifiers::CONTROL)
        {
            return None;
        }
        match key.code {
            KeyCode::Char('y') => Some(true),
            KeyCode::Char('n') | KeyCode::Esc => Some(false),
            _ => None,
        }
    }

    pub fn body(&self, width: usize, theme: &Theme) -> Vec<Line<'static>> {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let mut out = vec![Line::styled(
            format!("Honour the allowed-tools of {}?", self.name),
            bold.fg(theme.heading),
        )];
        for line in &self.lines {
            if line.is_empty() {
                out.push(Line::raw(""));
                continue;
            }
            for piece in wrap(line, width) {
                out.push(Line::raw(piece));
            }
        }
        out
    }

    pub fn footer(&self) -> Vec<Line<'static>> {
        let dim = Style::default().add_modifier(Modifier::DIM);
        vec![Line::styled("y trusts it · n leaves it as it is", dim)]
    }
}

/// What the dialog says, one entry per line (long ones wrap when drawn).
pub fn describe(skill: &Skill, previous: Option<&TrustRecord>) -> Vec<String> {
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
            section(
                &mut out,
                "allowed-tools that become allows for the turn:",
                specific.iter().map(|rule| format!("  {rule}")).collect(),
                "(none)",
            );
            section(
                &mut out,
                "wildcard shell rules, shown to the guardian as context and never an allow:",
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
    out.push(
        "The trust applies from the next session start; this session keeps its current grants."
            .to_string(),
    );
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
    use fontaine_core::extensions::Scope;
    use fontaine_core::permission_rules::parse_rule;
    use std::time::Duration;

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
        )
        .join("\n");
        assert!(
            lines.contains("+ shell(rm -rf x)") && !lines.contains("- shell(git status)"),
            "{lines}"
        );
        assert!(lines.contains("+ !make"), "{lines}");
        assert!(lines.contains("digest: sha256:new"), "{lines}");
    }

    #[test]
    fn only_y_trusts_and_not_before_the_grace_period() {
        let mut prompt = SkillTrustPrompt::new(&skill(&[], ""), None);
        let y = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE);
        let opened = prompt.opened;
        assert_eq!(prompt.handle_key_at(y, opened), None);
        let later = opened + Duration::from_millis(450);
        assert_eq!(prompt.handle_key_at(y, later), Some(true));
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(prompt.handle_key_at(esc, later), Some(false));
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(prompt.handle_key_at(enter, later), None);
    }
}
