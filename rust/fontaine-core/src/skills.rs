//! Running a skill: substituting its arguments, running its `!` commands through the permission
//! pipeline as `shell` calls, and lending its `allowed-tools` to the agent that invoked it for
//! the rest of the turn. Both the `skill` tool and a `/name` command come through here, so the
//! two behave the same.

use std::sync::{Arc, Mutex};

use fontaine_tools::gate::AgentId;
use fontaine_tools::lock::lock;
use fontaine_tools::skill::SkillSource;
use futures::future::BoxFuture;
use rig_core::tool::{DynamicTool, ToolContext};
use serde_json::json;

use crate::extensions::{Extensions, Scope, Skill};
use crate::gate::SessionState;

/// Splits an argument string into words the way a shell would: whitespace separates, and single
/// or double quotes keep a word together.
pub fn split_arguments(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    for c in text.chars() {
        match (quote, c) {
            (Some(open), c) if c == open => quote = None,
            (Some(_), c) => current.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            (None, c) => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        words.push(current);
    }
    words
}

/// `body` with `$ARGUMENTS`, `$ARGUMENTS[N]` and `$N` replaced from `arguments`; positions
/// count from zero and one past the end is empty. A body with no placeholder at all gets
/// `ARGUMENTS: <arguments>` appended when there are arguments.
pub fn substitute(body: &str, arguments: &str) -> String {
    let words = split_arguments(arguments);
    let mut out = String::with_capacity(body.len());
    let mut seen = false;
    let mut rest = body;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        if let Some(after) = tail.strip_prefix("$ARGUMENTS") {
            seen = true;
            if let Some(index) = after.strip_prefix('[')
                && let Some(end) = index.find(']')
                && let Ok(n) = index[..end].trim().parse::<usize>()
            {
                out.push_str(words.get(n).map_or("", String::as_str));
                rest = &index[end + 1..];
            } else {
                out.push_str(arguments.trim());
                rest = after;
            }
        } else if tail[1..].starts_with(|c: char| c.is_ascii_digit()) {
            let digits: String = tail[1..].chars().take_while(char::is_ascii_digit).collect();
            seen = true;
            let n: usize = digits.parse().unwrap_or(usize::MAX);
            out.push_str(words.get(n).map_or("", String::as_str));
            rest = &tail[1 + digits.len()..];
        } else {
            out.push('$');
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    if !seen && !arguments.trim().is_empty() {
        out.push_str(&format!("\n\nARGUMENTS: {}", arguments.trim()));
    }
    out
}

#[derive(Debug, PartialEq, Eq)]
pub enum Part {
    Text(String),
    /// A command the skill asks to have run before it is read.
    Command(String),
}

/// Cuts `body` at its `` !`command` `` spans and ```` ```! ```` blocks.
pub fn parts(body: &str) -> Vec<Part> {
    let mut out = Vec::new();
    let mut text = String::new();
    let mut rest = body;
    while !rest.is_empty() {
        let at_line_start = text.is_empty() && out.is_empty() || text.ends_with('\n');
        if at_line_start
            && let Some(block) = rest.strip_prefix("```!")
            && let Some(end) = block.find("\n```")
        {
            if !text.is_empty() {
                out.push(Part::Text(std::mem::take(&mut text)));
            }
            out.push(Part::Command(block[..end].trim().to_string()));
            let after = &block[end + 4..];
            rest = after.strip_prefix('\n').unwrap_or(after);
            continue;
        }
        if let Some(inline) = rest.strip_prefix("!`")
            && let Some(end) = inline.find('`')
            && !inline[..end].contains('\n')
        {
            if !text.is_empty() {
                out.push(Part::Text(std::mem::take(&mut text)));
            }
            out.push(Part::Command(inline[..end].trim().to_string()));
            rest = &inline[end + 1..];
            continue;
        }
        let c = rest.chars().next().unwrap_or(' ');
        text.push(c);
        rest = &rest[c.len_utf8()..];
    }
    if !text.is_empty() {
        out.push(Part::Text(text));
    }
    out
}

/// The skills a session loaded, and what running one touches.
pub struct SkillRuntime {
    extensions: Arc<Extensions>,
    state: Arc<SessionState>,
    /// The registered, wrapped `shell` tool, so a skill's commands go through the gate.
    shell: Mutex<Option<DynamicTool>>,
    /// Whether commands in a repository's skills may run: the workspace was trusted, or the
    /// surface does not ask.
    project_commands: bool,
}

impl SkillRuntime {
    pub fn new(
        extensions: Arc<Extensions>,
        state: Arc<SessionState>,
        project_commands: bool,
    ) -> SkillRuntime {
        SkillRuntime {
            extensions,
            state,
            shell: Mutex::new(None),
            project_commands,
        }
    }

    pub fn set_shell(&self, shell: DynamicTool) {
        *lock(&self.shell, "skill shell") = Some(shell);
    }

    pub fn extensions(&self) -> &Arc<Extensions> {
        &self.extensions
    }

    async fn run_command(&self, skill: &Skill, agent: &str, command: &str) -> String {
        if skill.scope == Scope::Project && !self.project_commands {
            return "[command not run: this project's executables are not trusted]".to_string();
        }
        if agent != fontaine_tools::gate::ROOT_AGENT
            && !self
                .state
                .agents()
                .tools_of(agent)
                .iter()
                .any(|tool| tool == "shell")
        {
            return "[command not run: this agent has no shell tool]".to_string();
        }
        let shell = lock(&self.shell, "skill shell").clone();
        let Some(shell) = shell else {
            return "[command not run: the shell tool is not available]".to_string();
        };
        let mut context = ToolContext::new();
        context
            .insert(AgentId(agent.to_string()))
            .expect("the agent id serializes");
        match shell
            .run_with(&mut context, json!({ "command": command }))
            .await
        {
            Ok(output) => {
                let text = output.render();
                // The shell tool ends its output with the exit code; a clean exit is not news.
                let text = text.trim_end();
                text.strip_suffix("[exit code 0]")
                    .unwrap_or(text)
                    .trim_end()
                    .to_string()
            }
            Err(error) => format!("[command failed: {}]", error.model_output().render().trim()),
        }
    }

    /// The text of skill `name` for `agent`: arguments substituted first, then its commands
    /// run, and its `allowed-tools` lent to `agent` until the turn ends. `by_model` is a call
    /// of the `skill` tool, which cannot reach a skill that is only for the user.
    pub async fn expand(
        &self,
        name: &str,
        arguments: &str,
        agent: &str,
        by_model: bool,
    ) -> Result<String, String> {
        let Some(skill) = self.extensions.skill(name) else {
            let known: Vec<&str> = self
                .extensions
                .skills
                .iter()
                .filter(|s| !by_model || !s.disable_model_invocation)
                .map(|s| s.name.as_str())
                .collect();
            return Err(format!(
                "no skill named `{name}`; available: {}",
                if known.is_empty() {
                    "none".to_string()
                } else {
                    known.join(", ")
                }
            ));
        };
        if by_model && skill.disable_model_invocation {
            return Err(format!(
                "the skill `{name}` is for the user to run with /{name}; it cannot be loaded by the model"
            ));
        }
        if !skill.allowed_tools.is_empty() {
            self.state
                .grant_skill_rules(agent, skill.allowed_tools.clone());
        }
        let body = substitute(&skill.body, arguments);
        let mut out = String::new();
        for part in parts(&body) {
            match part {
                Part::Text(text) => out.push_str(&text),
                Part::Command(command) => {
                    out.push_str(&self.run_command(skill, agent, &command).await)
                }
            }
        }
        Ok(out.trim().to_string())
    }
}

impl SkillSource for SkillRuntime {
    fn load<'a>(&'a self, name: &'a str, agent: &'a str) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move { self.expand(name, "", agent, true).await })
    }
}

/// The skill list the system prompt carries: names and descriptions only, so the bodies load
/// only when asked for.
pub fn index(extensions: &Extensions) -> Option<String> {
    let lines: Vec<String> = extensions
        .skills
        .iter()
        .filter(|skill| !skill.disable_model_invocation)
        .map(|skill| {
            let description: String = skill
                .description
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(300)
                .collect();
            format!("- {}: {description}", skill.name)
        })
        .collect();
    if lines.is_empty() {
        return None;
    }
    Some(format!(
        "Skills you can load with the `skill` tool, each a set of instructions for a kind of task:\n{}",
        lines.join("\n")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_split_like_a_shell_would() {
        assert_eq!(
            split_arguments(r#"a "b c" 'd e' f"#),
            ["a", "b c", "d e", "f"]
        );
        assert!(split_arguments("   ").is_empty());
        assert_eq!(split_arguments("''"), [""]);
    }

    #[test]
    fn placeholders_are_replaced_and_positions_count_from_zero() {
        assert_eq!(
            substitute("Fix $ARGUMENTS now", "issue 12"),
            "Fix issue 12 now"
        );
        assert_eq!(substitute("$ARGUMENTS[1] then $0", "a b"), "b then a");
        assert_eq!(substitute("[$2]", "a b"), "[]");
        assert_eq!(substitute("$ARGUMENTS[5]x", "a"), "x");
    }

    #[test]
    fn a_body_with_no_placeholder_gets_the_arguments_appended() {
        assert_eq!(substitute("Do it.", "now"), "Do it.\n\nARGUMENTS: now");
        assert_eq!(substitute("Do it.", ""), "Do it.");
        assert_eq!(substitute("Cost is $5 or $", ""), "Cost is  or $");
    }

    #[test]
    fn substitution_comes_before_commands_so_arguments_land_in_them() {
        let body = substitute("Branch: !`git rev-parse $0`", "main");
        assert_eq!(
            parts(&body),
            [
                Part::Text("Branch: ".into()),
                Part::Command("git rev-parse main".into())
            ]
        );
    }

    #[test]
    fn inline_and_fenced_commands_are_found() {
        let found = parts("a !`ls` b\n```!\nls -l\npwd\n```\nend");
        assert_eq!(
            found,
            [
                Part::Text("a ".into()),
                Part::Command("ls".into()),
                Part::Text(" b\n".into()),
                Part::Command("ls -l\npwd".into()),
                Part::Text("end".into()),
            ]
        );
        assert_eq!(parts("no !`unclosed"), [Part::Text("no !`unclosed".into())]);
    }
}
