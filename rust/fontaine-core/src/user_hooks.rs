//! User hooks: commands the user configures to look at a tool call before it runs, after it ran,
//! and when it was denied. Each gets the event as one JSON object on stdin and answers with one
//! on stdout, in Claude Code's shape. They are the user's own processes, not tool calls, so the
//! pipeline does not gate them; they run with the same scrubbed environment as `shell`.
//!
//! A hook can only add: `deny` and `ask` tighten, `allow` acts exactly as an allow rule does,
//! and a hook that fails, times out or says something unreadable decides nothing.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use fontaine_tools::gate::CallView;
use fontaine_tools::git_bash::{GitBash, sh_command};
use fontaine_tools::result_pipeline::{ResultStage, ToolOutcome, append_note};
use futures::future::BoxFuture;
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tracing::Instrument;

use crate::config::{Config, Hook};
use crate::gate::SessionState;
use crate::permission_rules::{CallContext, Posture, matches, parse_rule};
use crate::permissions::{
    HookDecision, HookError, HookPermission, PermissionDeniedEvent, PermissionDeniedHook,
    PreToolUseEvent, PreToolUseHook,
};

const TARGET: &str = "fontaine::telemetry";

/// A hook that takes longer than this is cut off unless it says otherwise.
pub const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Set in a hook's environment so a script can find the project it runs for.
pub const PROJECT_DIR_VAR: &str = "FONTAINE_PROJECT_DIR";

/// How a hook's command is run: the same environment as the `shell` tool (scrubbed, plus `[env]`), through the
/// same shell (Git Bash on Windows).
#[derive(Clone)]
pub struct HookProcess {
    pub scrub_env: Vec<OsString>,
    /// The user's `[env]`, set after the scrub.
    pub set_env: Vec<(OsString, OsString)>,
    pub git_bash: Option<GitBash>,
    pub project_dir: PathBuf,
}

/// What a hook process produced.
#[derive(Debug)]
pub struct HookOutput {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl HookProcess {
    async fn run(&self, command: &str, stdin: &Value) -> Result<HookOutput, HookError> {
        let sh = sh_command(command, self.git_bash.is_some());
        let mut process = tokio::process::Command::new("sh");
        process
            .arg("-c")
            .arg(sh.script)
            .current_dir(&self.project_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for name in &self.scrub_env {
            process.env_remove(name);
        }
        for (name, value) in &self.set_env {
            process.env(name, value);
        }
        process.env(PROJECT_DIR_VAR, &self.project_dir);
        if let Some((name, value)) = sh.env {
            process.env(name, value);
        }
        if let Some(git_bash) = &self.git_bash {
            process.env(
                "PATH",
                git_bash.path_value(fontaine_tools::git_bash::base_path(&self.set_env)),
            );
        }
        let mut child = process
            .spawn()
            .map_err(|e| HookError::failed_by("cannot start the hook", e))?;
        if let Some(mut input) = child.stdin.take() {
            let bytes = serde_json::to_vec(stdin)
                .map_err(|e| HookError::failed_by("cannot encode the event", e))?;
            // A hook that never reads its input is fine; the write simply fails.
            tokio::spawn(async move {
                let _ = input.write_all(&bytes).await;
                let _ = input.shutdown().await;
            });
        }
        let output = child
            .wait_with_output()
            .await
            .map_err(|e| HookError::failed_by("the hook did not finish", e))?;
        Ok(HookOutput {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// The names the three events carry on the wire, as Claude Code writes them.
mod wire {
    pub const PRE: &str = "PreToolUse";
    pub const POST: &str = "PostToolUse";
    pub const DENIED: &str = "PermissionDenied";
}

/// One configured hook.
pub struct CommandHook {
    id: String,
    matcher: Option<String>,
    command: String,
    timeout: Duration,
    /// The hook came from a scope that grants user trust; a repository's hook never loosens.
    user_trust: bool,
    process: Arc<HookProcess>,
}

impl CommandHook {
    pub fn new(event: &str, hook: &Hook, process: Arc<HookProcess>) -> CommandHook {
        CommandHook {
            id: format!("{event}:{}", hook.command),
            matcher: hook.matcher.clone(),
            command: hook.command.clone(),
            timeout: Duration::from_secs(hook.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS)),
            user_trust: hook.origin.user_trust,
            process,
        }
    }

    async fn call(&self, event: &str, input: Value) -> Result<HookOutput, HookError> {
        let span = tracing::info_span!(
            target: TARGET,
            "fontaine.hook",
            hook_event = event,
            success = tracing::field::Empty,
        );
        let result = self
            .process
            .run(&self.command, &input)
            .instrument(span.clone())
            .await;
        span.record("success", result.is_ok());
        result
    }
}

/// The part of a hook's stdout that matters: `hookSpecificOutput`, or nothing for an empty
/// answer.
fn parse_specific(output: &HookOutput, event: &str) -> Result<Option<Value>, HookError> {
    let text = output.stdout.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let value: Value =
        serde_json::from_str(text).map_err(|e| HookError::malformed_by("stdout is not JSON", e))?;
    let Some(object) = value.as_object() else {
        return Err(HookError::malformed("stdout is not a JSON object"));
    };
    let Some(specific) = object.get("hookSpecificOutput") else {
        return Ok(None);
    };
    let Some(map) = specific.as_object() else {
        return Err(HookError::malformed(
            "`hookSpecificOutput` is not an object",
        ));
    };
    if let Some(name) = map.get("hookEventName").and_then(Value::as_str)
        && name != event
    {
        return Err(HookError::malformed(format!(
            "`hookEventName` is `{name}`, not `{event}`"
        )));
    }
    Ok(Some(specific.clone()))
}

/// What every event tells a hook about the call and the session.
struct Wire<'a> {
    session_id: &'a str,
    cwd: &'a Path,
    tool: &'a str,
    args: &'a Value,
    agent: &'a str,
    mode: &'a str,
    roots: &'a [String],
    tool_use_id: Option<&'a str>,
}

fn base_event(event: &str, wire: Wire<'_>) -> Value {
    json!({
        "session_id": wire.session_id,
        "cwd": wire.cwd.display().to_string(),
        "hook_event_name": event,
        "tool_name": wire.tool,
        "tool_input": wire.args,
        "tool_use_id": wire.tool_use_id,
        "agent_id": wire.agent,
        "permission_mode": wire.mode,
        "workspace_roots": wire.roots,
    })
}

impl PreToolUseHook for CommandHook {
    fn id(&self) -> String {
        self.id.clone()
    }

    fn matcher(&self) -> Option<String> {
        self.matcher.clone()
    }

    fn timeout(&self) -> Duration {
        self.timeout
    }

    fn can_allow(&self) -> bool {
        self.user_trust
    }

    fn run<'a>(
        &'a self,
        event: PreToolUseEvent<'a>,
    ) -> BoxFuture<'a, Result<HookDecision, HookError>> {
        Box::pin(async move {
            let input = base_event(
                wire::PRE,
                Wire {
                    session_id: event.session_id,
                    cwd: event.cwd,
                    tool: event.tool,
                    args: event.args,
                    agent: event.agent,
                    mode: event.permission_mode,
                    roots: event.workspace_roots,
                    tool_use_id: event.tool_use_id,
                },
            );
            let output = self.call(wire::PRE, input).await?;
            match output.code {
                Some(0) => {}
                // Exit 2 blocks the call and stderr is the reason.
                Some(2) => {
                    return Ok(HookDecision {
                        permission: Some(HookPermission::Deny),
                        reason: Some(output.stderr.trim().to_string()).filter(|r| !r.is_empty()),
                        ..HookDecision::default()
                    });
                }
                other => {
                    return Err(HookError::failed(format!(
                        "the hook exited with {}",
                        other.map_or_else(|| "a signal".to_string(), |c| c.to_string())
                    )));
                }
            }
            let Some(specific) = parse_specific(&output, wire::PRE)? else {
                return Ok(HookDecision::default());
            };
            let permission = match specific.get("permissionDecision").and_then(Value::as_str) {
                None => None,
                Some("allow") => Some(HookPermission::Allow),
                Some("deny") => Some(HookPermission::Deny),
                Some("ask") => Some(HookPermission::Ask),
                Some(other) => {
                    return Err(HookError::malformed(format!(
                        "`permissionDecision` is `{other}`, not allow, deny or ask"
                    )));
                }
            };
            let text = |key: &str| {
                specific
                    .get(key)
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .filter(|t| !t.is_empty())
            };
            Ok(HookDecision {
                permission,
                updated_input: specific
                    .get("updatedInput")
                    .cloned()
                    .filter(Value::is_object),
                reason: text("permissionDecisionReason"),
                guardian_context: text("guardian_context"),
            })
        })
    }
}

impl PermissionDeniedHook for CommandHook {
    fn id(&self) -> String {
        self.id.clone()
    }

    fn matcher(&self) -> Option<String> {
        self.matcher.clone()
    }

    fn timeout(&self) -> Duration {
        self.timeout
    }

    fn can_retry(&self) -> bool {
        self.user_trust
    }

    fn run<'a>(
        &'a self,
        event: PermissionDeniedEvent<'a>,
    ) -> BoxFuture<'a, Result<bool, HookError>> {
        Box::pin(async move {
            let mut input = base_event(
                wire::DENIED,
                Wire {
                    session_id: event.session_id,
                    cwd: event.cwd,
                    tool: event.tool,
                    args: event.args,
                    agent: event.agent,
                    mode: event.permission_mode,
                    roots: event.workspace_roots,
                    tool_use_id: event.tool_use_id,
                },
            );
            input["reason"] = json!(event.reason);
            input["tier"] = json!(event.tier.as_str());
            let output = self.call(wire::DENIED, input).await?;
            if output.code != Some(0) {
                return Err(HookError::failed(format!(
                    "the hook exited with {}",
                    output
                        .code
                        .map_or_else(|| "a signal".to_string(), |c| c.to_string())
                )));
            }
            let Some(specific) = parse_specific(&output, wire::DENIED)? else {
                return Ok(false);
            };
            match specific.get("retry") {
                None => Ok(false),
                Some(Value::Bool(retry)) => Ok(*retry),
                Some(_) => Err(HookError::malformed("`retry` is not a boolean")),
            }
        })
    }
}

/// The `post_tool_use` hooks as the last stage of the result pipeline. A hook's
/// `additionalContext` is added to what the tool returned, so the agent reads it with the
/// result.
pub struct PostHookStage {
    hooks: Vec<CommandHook>,
    state: Arc<SessionState>,
}

impl PostHookStage {
    pub fn new(hooks: Vec<CommandHook>, state: Arc<SessionState>) -> PostHookStage {
        PostHookStage { hooks, state }
    }

    fn response(outcome: &ToolOutcome) -> Value {
        match outcome {
            Ok(output) => json!({ "is_error": false, "output": output.render() }),
            Err(error) => {
                json!({ "is_error": true, "output": error.model_output().render() })
            }
        }
    }

    async fn run_one(
        &self,
        hook: &CommandHook,
        call: &CallView<'_>,
        response: &Value,
    ) -> Option<String> {
        let scope = self.state.scope();
        let covered = match hook
            .matcher
            .as_deref()
            .and_then(|text| parse_rule(text).ok())
        {
            Some(rule) => matches(
                &rule,
                &CallContext {
                    tool: call.tool,
                    args: call.args,
                    scope: &scope,
                    home: self.state.home.as_deref(),
                },
                Posture::Deny,
            ),
            None => true,
        };
        if !covered {
            return None;
        }
        let roots: Vec<String> = scope
            .roots()
            .iter()
            .map(|root| root.display().to_string())
            .collect();
        let tree = self.state.agents();
        let mode = if tree.is_child(call.agent) {
            crate::gate::permission_mode(tree.posture(call.agent).mode)
        } else {
            self.state.mode()
        };
        let tool_use_id = self.state.current_call(call.agent);
        let mut input = base_event(
            wire::POST,
            Wire {
                session_id: &self.state.session_id,
                cwd: scope.primary(),
                tool: call.tool,
                args: call.args,
                agent: call.agent,
                mode: mode.as_str(),
                roots: &roots,
                tool_use_id: tool_use_id.as_deref(),
            },
        );
        input["tool_response"] = response.clone();
        let output = match tokio::time::timeout(hook.timeout, hook.call(wire::POST, input)).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => {
                tracing::warn!(hook = %hook.id, error = ?e, "a post_tool_use hook failed; it adds nothing");
                return None;
            }
            Err(_) => {
                tracing::warn!(hook = %hook.id, "a post_tool_use hook timed out; it adds nothing");
                return None;
            }
        };
        match output.code {
            // The call already ran, so a block can only tell the agent why.
            Some(2) => Some(output.stderr.trim().to_string()).filter(|r| !r.is_empty()),
            Some(0) => match parse_specific(&output, wire::POST) {
                Ok(Some(specific)) => specific
                    .get("additionalContext")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .filter(|t| !t.is_empty()),
                Ok(None) => None,
                Err(e) => {
                    tracing::warn!(hook = %hook.id, error = ?e, "a post_tool_use hook answered unreadably; it adds nothing");
                    None
                }
            },
            _ => None,
        }
    }
}

impl ResultStage for PostHookStage {
    fn process<'a>(
        &'a self,
        call: CallView<'a>,
        outcome: ToolOutcome,
    ) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let response = Self::response(&outcome);
            let mut notes = Vec::new();
            for hook in &self.hooks {
                if let Some(note) = self.run_one(hook, &call, &response).await {
                    notes.push(note);
                }
            }
            append_note(outcome, &notes.join("\n\n"))
        })
    }
}

/// The three hook lists a session runs.
#[derive(Default)]
pub struct BuiltHooks {
    pub pre: Vec<Arc<dyn PreToolUseHook>>,
    pub denied: Vec<Arc<dyn PermissionDeniedHook>>,
    pub post: Vec<CommandHook>,
}

/// Builds the configured hooks. A hook the repository defines is left out when
/// `project_executables` is false: the workspace was not trusted.
pub fn build(config: &Config, project_executables: bool, process: Arc<HookProcess>) -> BuiltHooks {
    let keep = |hook: &&Hook| project_executables || !hook.origin.is_project();
    let mut built = BuiltHooks::default();
    for hook in config.hooks.pre_tool_use.iter().filter(keep) {
        built.pre.push(Arc::new(CommandHook::new(
            "pre_tool_use",
            hook,
            process.clone(),
        )));
    }
    for hook in config.hooks.permission_denied.iter().filter(keep) {
        built.denied.push(Arc::new(CommandHook::new(
            "permission_denied",
            hook,
            process.clone(),
        )));
    }
    for hook in config.hooks.post_tool_use.iter().filter(keep) {
        built
            .post
            .push(CommandHook::new("post_tool_use", hook, process.clone()));
    }
    built
}
