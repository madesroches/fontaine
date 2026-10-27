mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use common::{Dirs, as_root, call, env_with, registry, scope};
use fontaine_tools::gate::{AgentId, CallView, GateDecision, ToolGate};
use fontaine_tools::guarded::{Guard, GuardedTool, guard_callback};
use fontaine_tools::result_pipeline::{ResultPipeline, ResultStage, StageKind, ToolOutcome};
use fontaine_tools::risk::RiskClass;
use futures::future::BoxFuture;
use rig_core::completion::ToolDefinition;
use rig_core::message::ToolName;
use rig_core::serve::adapters::ToolCallback;
use rig_core::tool::{Tool, ToolContext, ToolExecutionError, ToolOutput};
use serde::Deserialize;
use serde_json::{Value, json};

/// A tool that counts its runs.
struct Counter {
    runs: Arc<AtomicUsize>,
}

#[derive(Deserialize)]
struct CounterArgs {
    n: u32,
}

impl Tool for Counter {
    const NAME: &'static str = "counter";
    type Args = CounterArgs;
    type Output = String;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "counts".to_string()
    }

    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {"n": {"type": "integer"}}, "required": ["n"]})
    }

    async fn call(
        &self,
        _: &mut ToolContext,
        args: CounterArgs,
    ) -> Result<String, ToolExecutionError> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        Ok(format!("ran {}", args.n))
    }
}

/// Decides by a closure and records each call's tool, agent and arguments.
struct Scripted {
    decide: Box<dyn Fn(&CallView<'_>) -> GateDecision + Send + Sync>,
    seen: Mutex<Vec<(String, String, Value)>>,
}

impl Scripted {
    fn new(decide: impl Fn(&CallView<'_>) -> GateDecision + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Scripted {
            decide: Box::new(decide),
            seen: Mutex::new(Vec::new()),
        })
    }
}

impl ToolGate for Scripted {
    fn check<'a>(&'a self, call: CallView<'a>) -> BoxFuture<'a, GateDecision> {
        Box::pin(async move {
            self.seen.lock().unwrap().push((
                call.tool.to_string(),
                call.agent.to_string(),
                call.args.clone(),
            ));
            (self.decide)(&call)
        })
    }
}

fn allow(call: &CallView<'_>) -> GateDecision {
    GateDecision::Allow {
        args: call.args.clone(),
    }
}

fn guard(gate: Arc<dyn ToolGate>) -> Guard {
    Guard {
        gate,
        results: Arc::new(ResultPipeline::new()),
        hide_mcp_names: false,
        pre_images: None,
    }
}

fn counter_tool(gate: Arc<dyn ToolGate>) -> (Arc<AtomicUsize>, rig_core::tool::DynamicTool) {
    let runs = Arc::new(AtomicUsize::new(0));
    let tool = GuardedTool::single(
        Counter { runs: runs.clone() },
        RiskClass::ReadOnly,
        guard(gate),
    )
    .into_dynamic();
    (runs, tool)
}

#[tokio::test]
async fn the_inner_tool_runs_exactly_once_and_only_after_the_gate_allows() {
    let gate = Scripted::new(allow);
    let (runs, tool) = counter_tool(gate.clone());
    let output = tool
        .run_with(&mut as_root(), json!({"n": 4}))
        .await
        .unwrap();
    assert_eq!(output.render(), "ran 4");
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert_eq!(gate.seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_denied_call_never_runs_the_tool_and_the_model_gets_the_reason() {
    let gate = Scripted::new(|_| GateDecision::Deny {
        message: "not today".to_string(),
    });
    let (runs, tool) = counter_tool(gate);
    let error = tool
        .run_with(&mut as_root(), json!({"n": 1}))
        .await
        .unwrap_err();
    assert_eq!(error.message(), "not today");
    assert_eq!(error.model_output().render(), "not today");
    assert_eq!(runs.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn the_tool_runs_with_the_arguments_the_gate_allowed() {
    let gate = Scripted::new(|_| GateDecision::Allow {
        args: json!({"n": 99}),
    });
    let (_, tool) = counter_tool(gate);
    assert_eq!(
        tool.run_with(&mut as_root(), json!({"n": 1}))
            .await
            .unwrap()
            .render(),
        "ran 99"
    );
}

#[tokio::test]
async fn allowed_arguments_that_do_not_fit_the_tool_are_an_error_and_the_tool_does_not_run() {
    let gate = Scripted::new(allow);
    let (runs, tool) = counter_tool(gate);
    let error = tool
        .run_with(&mut as_root(), json!({"n": "seven"}))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), rig_core::tool::ToolErrorKind::InvalidArgs);
    assert_eq!(runs.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn null_arguments_are_read_as_an_empty_object() {
    let gate = Scripted::new(allow);
    let (_, tool) = counter_tool(gate.clone());
    let error = tool
        .run_with(&mut as_root(), Value::Null)
        .await
        .unwrap_err();
    // `n` is required, so the empty object fails to deserialize; the gate saw `{}`.
    assert_eq!(error.kind(), rig_core::tool::ToolErrorKind::InvalidArgs);
    assert_eq!(gate.seen.lock().unwrap()[0].2, json!({}));
}

#[tokio::test]
async fn the_gate_sees_the_agent_of_the_call() {
    let gate = Scripted::new(allow);
    let (_, tool) = counter_tool(gate.clone());
    tool.run_with(&mut as_root(), json!({"n": 1}))
        .await
        .unwrap();
    let mut context = ToolContext::new();
    context.insert(AgentId("agent-7".to_string())).unwrap();
    tool.run_with(&mut context, json!({"n": 1})).await.unwrap();
    let agents: Vec<String> = gate
        .seen
        .lock()
        .unwrap()
        .iter()
        .map(|c| c.1.clone())
        .collect();
    assert_eq!(agents, ["root", "agent-7"]);
}

#[tokio::test]
async fn a_call_that_does_not_say_whose_it_is_fails_before_the_gate() {
    let gate = Scripted::new(allow);
    let (runs, tool) = counter_tool(gate.clone());
    let error = tool.run(json!({"n": 1})).await.unwrap_err();
    assert!(
        error.model_output().render().contains("which agent"),
        "{error:?}"
    );
    assert!(gate.seen.lock().unwrap().is_empty());
    assert_eq!(runs.load(Ordering::SeqCst), 0);
}

#[test]
fn the_wrapper_shows_the_model_the_inner_tools_schema() {
    let (_, tool) = counter_tool(Scripted::new(allow));
    let definition = tool.definition();
    assert_eq!(definition.name.to_string(), "counter");
    assert_eq!(definition.description, "counts");
    assert_eq!(definition.parameters["required"], json!(["n"]));
}

fn mcp_tool(
    gate: Arc<dyn ToolGate>,
    results: ResultPipeline,
) -> (Arc<AtomicUsize>, rig_core::tool::DynamicTool) {
    let runs = Arc::new(AtomicUsize::new(0));
    let counted = runs.clone();
    let callback: Arc<dyn ToolCallback> =
        Arc::new(move |_context: &mut ToolContext, args: Value| {
            let counted = counted.clone();
            Box::pin(async move {
                counted.fetch_add(1, Ordering::SeqCst);
                Ok(ToolOutput::text(format!("mcp saw {args}")))
            }) as rig_core::wasm_compat::WasmBoxedFuture<'_, _>
        });
    let definition = ToolDefinition::new(
        ToolName::new("mcp__demo__ping").unwrap(),
        "pings",
        json!({"type": "object"}),
    );
    let tool = guard_callback(
        definition,
        callback,
        RiskClass::Arbitrary,
        Guard {
            gate,
            results: Arc::new(results),
            hide_mcp_names: false,
            pre_images: None,
        },
        false,
    );
    (runs, tool)
}

struct Suffix;

impl ResultStage for Suffix {
    fn process<'a>(&'a self, _: CallView<'a>, outcome: ToolOutcome) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(
            async move { outcome.map(|o| ToolOutput::text(format!("{} [scanned]", o.render()))) },
        )
    }
}

#[tokio::test]
async fn a_wrapped_mcp_callback_runs_once_after_allow_and_its_result_passes_the_pipeline() {
    let gate = Scripted::new(allow);
    let pipeline = ResultPipeline::new().with_stage(StageKind::Scanner, Arc::new(Suffix));
    let (runs, tool) = mcp_tool(gate.clone(), pipeline);
    assert_eq!(tool.definition().name.to_string(), "mcp__demo__ping");
    let output = tool
        .run_with(&mut as_root(), json!({"x": 1}))
        .await
        .unwrap();
    assert_eq!(output.render(), "mcp saw {\"x\":1} [scanned]");
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    let seen = gate.seen.lock().unwrap();
    assert_eq!(seen[0].0, "mcp__demo__ping");
}

#[tokio::test]
async fn a_wrapped_mcp_callback_never_runs_on_a_deny() {
    let gate = Scripted::new(|_| GateDecision::Deny {
        message: "blocked".to_string(),
    });
    let (runs, tool) = mcp_tool(gate, ResultPipeline::new());
    let error = tool.run_with(&mut as_root(), json!({})).await.unwrap_err();
    assert_eq!(error.message(), "blocked");
    assert_eq!(runs.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_built_in_tool_does_not_touch_the_disk_when_the_gate_denies() {
    let d = Dirs::new();
    let root = d.dir("work");
    let gate = Scripted::new(|_| GateDecision::Deny {
        message: "no".to_string(),
    });
    let tools = registry(&env_with(scope(&root, &[]), gate));
    let denied = call(&tools, "write", json!({"path": "new.txt", "content": "x"})).await;
    assert!(denied.is_err());
    assert!(!root.join("new.txt").exists());
}

#[cfg(unix)]
mod shell_env {
    use std::ffi::OsString;
    use std::sync::Arc;

    use fontaine_tools::builtins;

    use super::common::{AllowAll, Dirs, call, env_with, registry, scope, text};
    use serde_json::json;

    const CHILD_MARKER: &str = "FONTAINE_TEST_SHELL_ENV_CHILD";

    async fn shell_output(env: &builtins::BuiltinEnv, command: &str) -> String {
        let tools = registry(env);
        let out = call(&tools, "shell", json!({ "command": command }))
            .await
            .unwrap();
        text(&out)
    }

    #[tokio::test]
    async fn configured_env_reaches_the_shell_literally_and_wins_over_the_scrub() {
        let d = Dirs::new();
        let mut env = env_with(scope(&d.dir("work"), &[]), Arc::new(AllowAll::default()));
        env.shell.scrub_env = vec![OsString::from("FONTAINE_TEST_BOTH")];
        env.shell.set_env = vec![
            (
                OsString::from("FONTAINE_TEST_SET"),
                OsString::from("$HOME/x"),
            ),
            (
                OsString::from("FONTAINE_TEST_BOTH"),
                OsString::from("set-value"),
            ),
        ];
        let shown = shell_output(
            &env,
            "printf '%s|%s' \"$FONTAINE_TEST_SET\" \"$FONTAINE_TEST_BOTH\"",
        )
        .await;
        assert!(shown.contains("$HOME/x|set-value"), "{shown}");
    }

    /// The `shell` child's environment holds none of the scrubbed names. The variables must exist
    /// in this process to be inherited, and setting them here would race other threads, so the
    /// test re-runs itself with them set.
    #[test]
    fn the_shell_children_do_not_inherit_credential_variables() {
        if std::env::var_os(CHILD_MARKER).is_some() {
            return;
        }
        let scrubbed = [
            "MICROMEGAS_INGESTION_API_KEY",
            "OTEL_EXPORTER_OTLP_HEADERS",
            "CONFIGURED_TELEMETRY_KEY",
            "CONFIGURED_ACCOUNT_KEY",
            "CONFIGURED_SCANNER_TOKEN",
        ];
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "shell_env::shell_env_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD_MARKER, "1")
            .env("UNRELATED_VARIABLE", "kept");
        for name in scrubbed {
            command.env(name, "leaked-value");
        }
        let out = command.output().unwrap();
        assert!(
            out.status.success(),
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(String::from_utf8_lossy(&out.stdout).contains("1 passed"));
    }

    #[tokio::test]
    async fn shell_env_child() {
        if std::env::var_os(CHILD_MARKER).is_none() {
            return;
        }
        let d = Dirs::new();
        let mut env = env_with(scope(&d.dir("work"), &[]), Arc::new(AllowAll::default()));
        env.shell.scrub_env = [
            "MICROMEGAS_INGESTION_API_KEY",
            "OTEL_EXPORTER_OTLP_HEADERS",
            "CONFIGURED_TELEMETRY_KEY",
            "CONFIGURED_ACCOUNT_KEY",
            "CONFIGURED_SCANNER_TOKEN",
        ]
        .map(OsString::from)
        .to_vec();
        let tools = registry(&env);
        let out = call(&tools, "shell", json!({"command": "env"}))
            .await
            .unwrap();
        let printed = text(&out);
        assert!(printed.contains("UNRELATED_VARIABLE=kept"), "{printed}");
        assert!(!printed.contains("leaked-value"), "{printed}");
    }
}

/// Remembers each capture request and the order of captures and finishes.
#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<String>>,
}

struct Finisher {
    recorder: Arc<Recorder>,
}

impl fontaine_tools::checkpoint::Capture for Finisher {
    fn finish(self: Box<Self>, succeeded: bool) {
        self.recorder
            .events
            .lock()
            .unwrap()
            .push(format!("finish {succeeded}"));
    }
}

struct Saver {
    recorder: Arc<Recorder>,
    /// The file's content when `before` ran, which is what a pre-image would hold.
    content: Mutex<Vec<Option<String>>>,
}

impl fontaine_tools::checkpoint::PreImages for Saver {
    fn before(
        &self,
        agent: &str,
        tool: &str,
        path: &std::path::Path,
    ) -> Option<Box<dyn fontaine_tools::checkpoint::Capture>> {
        self.recorder
            .events
            .lock()
            .unwrap()
            .push(format!("before {agent} {tool} {}", path.display()));
        self.content
            .lock()
            .unwrap()
            .push(std::fs::read_to_string(path).ok());
        Some(Box::new(Finisher {
            recorder: self.recorder.clone(),
        }))
    }
}

fn saving_registry(
    gate: Arc<dyn ToolGate>,
    scope: Arc<fontaine_tools::scope::Scope>,
) -> (Arc<Saver>, Vec<fontaine_tools::builtins::Registered>) {
    let saver = Arc::new(Saver {
        recorder: Arc::new(Recorder::default()),
        content: Mutex::new(Vec::new()),
    });
    let mut env = env_with(scope, gate);
    env.guard.pre_images = Some(saver.clone());
    (saver, registry(&env))
}

#[tokio::test]
async fn an_allowed_write_and_edit_are_saved_before_the_tool_runs() {
    let d = Dirs::new();
    let root = d.dir("work");
    std::fs::write(root.join("a.txt"), "old").unwrap();
    let (saver, tools) = saving_registry(Scripted::new(allow), scope(&root, &[]));
    call(&tools, "read", json!({"path": "a.txt"}))
        .await
        .unwrap();
    call(&tools, "write", json!({"path": "a.txt", "content": "new"}))
        .await
        .unwrap();
    call(
        &tools,
        "edit",
        json!({"path": "a.txt", "edits": [{"old_text": "new", "new_text": "newer"}]}),
    )
    .await
    .unwrap();
    let events = saver.recorder.events.lock().unwrap().clone();
    let path = root.join("a.txt").display().to_string();
    assert_eq!(
        events,
        [
            format!("before root write {path}"),
            "finish true".to_string(),
            format!("before root edit {path}"),
            "finish true".to_string(),
        ]
    );
    assert_eq!(
        *saver.content.lock().unwrap(),
        [Some("old".to_string()), Some("new".to_string())]
    );
}

#[tokio::test]
async fn a_denied_edit_saves_nothing() {
    let d = Dirs::new();
    let root = d.dir("work");
    let gate = Scripted::new(|_| GateDecision::Deny {
        message: "no".to_string(),
    });
    let (saver, tools) = saving_registry(gate, scope(&root, &[]));
    let denied = call(&tools, "write", json!({"path": "n.txt", "content": "x"})).await;
    assert!(denied.is_err());
    assert!(saver.recorder.events.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_failed_tool_call_still_finishes_the_capture() {
    let d = Dirs::new();
    let root = d.dir("work");
    std::fs::write(root.join("a.txt"), "old").unwrap();
    let (saver, tools) = saving_registry(Scripted::new(allow), scope(&root, &[]));
    let failed = call(
        &tools,
        "edit",
        json!({"path": "a.txt", "edits": [{"old_text": "missing", "new_text": "x"}]}),
    )
    .await;
    assert!(failed.is_err());
    let events = saver.recorder.events.lock().unwrap().clone();
    assert_eq!(events.len(), 2);
    assert_eq!(events[1], "finish false");
}

#[tokio::test]
async fn read_only_tools_and_the_shell_are_never_saved() {
    let d = Dirs::new();
    let root = d.dir("work");
    std::fs::write(root.join("a.txt"), "old").unwrap();
    let (saver, tools) = saving_registry(Scripted::new(allow), scope(&root, &[]));
    call(&tools, "read", json!({"path": "a.txt"}))
        .await
        .unwrap();
    call(&tools, "ls", json!({"path": "."})).await.unwrap();
    call(&tools, "grep", json!({"pattern": "old"}))
        .await
        .unwrap();
    let _ = call(&tools, "shell", json!({"command": "true"})).await;
    assert!(saver.recorder.events.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_mcp_callback_is_never_saved() {
    let saver = Arc::new(Saver {
        recorder: Arc::new(Recorder::default()),
        content: Mutex::new(Vec::new()),
    });
    let callback: Arc<dyn ToolCallback> = Arc::new(|_: &mut ToolContext, _: Value| {
        Box::pin(async { Ok(ToolOutput::text("done")) })
            as rig_core::wasm_compat::WasmBoxedFuture<'_, _>
    });
    let tool = guard_callback(
        ToolDefinition::new(
            ToolName::new("write").unwrap(),
            "pretends to write",
            json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        ),
        callback,
        RiskClass::Arbitrary,
        Guard {
            gate: Scripted::new(allow),
            results: Arc::new(ResultPipeline::new()),
            hide_mcp_names: false,
            pre_images: Some(saver.clone()),
        },
        false,
    );
    tool.run_with(&mut as_root(), json!({"path": "/tmp/x"}))
        .await
        .unwrap();
    assert!(saver.recorder.events.lock().unwrap().is_empty());
}

#[tokio::test]
async fn each_root_captures_its_own_path() {
    let d = Dirs::new();
    let (first, second) = (d.dir("one"), d.dir("two"));
    std::fs::write(second.join("b.txt"), "old").unwrap();
    let (saver, tools) = saving_registry(Scripted::new(allow), scope(&first, &[&second]));
    let path = second.join("b.txt");
    call(&tools, "read", json!({"path": path.to_str().unwrap()}))
        .await
        .unwrap();
    call(
        &tools,
        "write",
        json!({"path": path.to_str().unwrap(), "content": "new"}),
    )
    .await
    .unwrap();
    let events = saver.recorder.events.lock().unwrap().clone();
    assert_eq!(events[0], format!("before root write {}", path.display()));
}
