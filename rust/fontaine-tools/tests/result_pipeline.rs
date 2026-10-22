mod common;

use std::sync::{Arc, Mutex};

use common::{AllowAll, Dirs, env_with, registry, scope, tool};
use fontaine_tools::builtins::{self, BuiltinEnv};
use fontaine_tools::gate::CallView;
use fontaine_tools::guarded::Guard;
use fontaine_tools::result_pipeline::{ResultPipeline, ResultStage, StageKind, ToolOutcome};
use futures::future::BoxFuture;
use rig_core::tool::ToolOutput;
use serde_json::json;

/// Appends its label to the text and records that it ran.
struct Marker {
    label: &'static str,
    ran: Arc<Mutex<Vec<&'static str>>>,
}

impl ResultStage for Marker {
    fn process<'a>(
        &'a self,
        _call: CallView<'a>,
        outcome: ToolOutcome,
    ) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            self.ran.lock().unwrap().push(self.label);
            outcome.map(|output| ToolOutput::text(format!("{}|{}", output.render(), self.label)))
        })
    }
}

fn marker(label: &'static str, ran: &Arc<Mutex<Vec<&'static str>>>) -> Arc<dyn ResultStage> {
    Arc::new(Marker {
        label,
        ran: ran.clone(),
    })
}

fn env_with_pipeline(d: &Dirs, results: ResultPipeline) -> BuiltinEnv {
    let mut env = env_with(scope(&d.dir("work"), &[]), Arc::new(AllowAll::default()));
    env.guard = Guard {
        gate: env.guard.gate.clone(),
        results: Arc::new(results),
    };
    env
}

#[tokio::test]
async fn with_no_stage_the_result_is_untouched() {
    let d = Dirs::new();
    std::fs::write(d.dir("work").join("a.txt"), "hello\n").unwrap();
    let tools = registry(&env_with_pipeline(&d, ResultPipeline::new()));
    let output = tool(&tools, "read")
        .tool
        .run(json!({"path": "a.txt"}))
        .await
        .unwrap();
    assert_eq!(output.render(), "hello\n");
}

#[tokio::test]
async fn every_tool_result_passes_through_the_pipeline_after_the_tool_returns() {
    let d = Dirs::new();
    std::fs::write(d.dir("work").join("a.txt"), "hello\n").unwrap();
    let ran = Arc::new(Mutex::new(Vec::new()));
    let pipeline = ResultPipeline::new().with_stage(StageKind::Scanner, marker("scanned", &ran));
    let tools = registry(&env_with_pipeline(&d, pipeline));
    let output = tool(&tools, "read")
        .tool
        .run(json!({"path": "a.txt"}))
        .await
        .unwrap();
    assert_eq!(output.render(), "hello\n|scanned");
    assert_eq!(*ran.lock().unwrap(), ["scanned"]);

    // A tool that failed is a result too.
    let failed = tool(&tools, "read")
        .tool
        .run(json!({"path": "missing.txt"}))
        .await;
    assert!(failed.is_err());
    assert_eq!(*ran.lock().unwrap(), ["scanned", "scanned"]);
}

#[tokio::test]
async fn stages_run_scanner_first_then_post_tool_use_whatever_the_attachment_order() {
    let d = Dirs::new();
    std::fs::write(d.dir("work").join("a.txt"), "x").unwrap();
    let ran = Arc::new(Mutex::new(Vec::new()));
    let pipeline = ResultPipeline::new()
        .with_stage(StageKind::PostToolUse, marker("hook", &ran))
        .with_stage(StageKind::Scanner, marker("scanner", &ran));
    let tools = registry(&env_with_pipeline(&d, pipeline));
    let output = tool(&tools, "read")
        .tool
        .run(json!({"path": "a.txt"}))
        .await
        .unwrap();
    assert_eq!(*ran.lock().unwrap(), ["scanner", "hook"]);
    assert_eq!(output.render(), "x\n|scanner|hook");
}

#[tokio::test]
async fn a_call_the_gate_denies_never_reaches_the_pipeline() {
    use fontaine_tools::gate::{GateDecision, ToolGate};

    struct DenyAll;
    impl ToolGate for DenyAll {
        fn check<'a>(&'a self, _call: CallView<'a>) -> BoxFuture<'a, GateDecision> {
            Box::pin(async {
                GateDecision::Deny {
                    message: "no".to_string(),
                }
            })
        }
    }

    let d = Dirs::new();
    let ran = Arc::new(Mutex::new(Vec::new()));
    let env = BuiltinEnv {
        guard: Guard {
            gate: Arc::new(DenyAll),
            results: Arc::new(
                ResultPipeline::new().with_stage(StageKind::Scanner, marker("s", &ran)),
            ),
        },
        ..env_with(scope(&d.dir("work"), &[]), Arc::new(DenyAll))
    };
    let tools = builtins::build(&env).unwrap();
    let denied = tool(&tools, "read")
        .tool
        .run(json!({"path": "a.txt"}))
        .await;
    assert_eq!(denied.unwrap_err().message(), "no");
    assert!(ran.lock().unwrap().is_empty());
}
