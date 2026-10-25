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
        },
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
