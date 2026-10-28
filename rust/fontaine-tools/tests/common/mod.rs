#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use fontaine_tools::builtins::{self, BuiltinEnv, Registered, ShellSettings};
use fontaine_tools::gate::{AgentId, CallView, GateDecision, ROOT_AGENT, ToolGate};
use fontaine_tools::guarded::Guard;
use fontaine_tools::result_pipeline::ResultPipeline;
use fontaine_tools::scope::Scope;
use fontaine_tools::webfetch::WebfetchConfig;
use futures::future::BoxFuture;
use rig_core::tool::{ToolContext, ToolExecutionError, ToolOutput};
use serde_json::Value;
use tempfile::TempDir;

/// A gate that allows everything and remembers what it was asked.
#[derive(Default)]
pub struct AllowAll {
    pub seen: Mutex<Vec<(String, Value)>>,
}

impl ToolGate for AllowAll {
    fn check<'a>(&'a self, call: CallView<'a>) -> BoxFuture<'a, GateDecision> {
        Box::pin(async move {
            self.seen
                .lock()
                .unwrap()
                .push((call.tool.to_string(), call.args.clone()));
            GateDecision::Allow {
                args: call.args.clone(),
            }
        })
    }
}

/// A scratch directory with canonical paths.
pub struct Dirs {
    _tmp: TempDir,
    pub base: PathBuf,
}

impl Dirs {
    pub fn new() -> Dirs {
        let tmp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(tmp.path()).unwrap();
        Dirs { _tmp: tmp, base }
    }

    /// A new directory called `name` under the scratch directory.
    pub fn dir(&self, name: &str) -> PathBuf {
        let path = self.base.join(name);
        std::fs::create_dir_all(&path).unwrap();
        path
    }
}

pub fn scope(primary: &Path, added: &[&Path]) -> Arc<Scope> {
    Arc::new(Scope::new(
        primary.to_path_buf(),
        added.iter().map(|p| p.to_path_buf()),
    ))
}

pub fn env_with(scope: Arc<Scope>, gate: Arc<dyn ToolGate>) -> BuiltinEnv {
    BuiltinEnv {
        scope,
        seen: Default::default(),
        guard: Guard {
            gate,
            results: Arc::new(ResultPipeline::new()),
            hide_mcp_names: false,
            pre_images: None,
        },
        shell: ShellSettings::default(),
        webfetch: WebfetchConfig::default(),
        websearch: None,
        searxng: Default::default(),
        network: Default::default(),
        jobs: Default::default(),
    }
}

pub fn registry(env: &BuiltinEnv) -> Vec<Registered> {
    builtins::build(env).unwrap()
}

pub fn tool<'a>(registered: &'a [Registered], name: &str) -> &'a Registered {
    registered
        .iter()
        .find(|r| r.name == name)
        .unwrap_or_else(|| panic!("no tool `{name}`"))
}

pub async fn call(
    registered: &[Registered],
    name: &str,
    args: Value,
) -> Result<ToolOutput, ToolExecutionError> {
    tool(registered, name)
        .tool
        .run_with(&mut as_root(), args)
        .await
}

/// A context that says whose call it is, as a turn's does. Guarded tools refuse one that
/// does not.
pub fn as_root() -> ToolContext {
    let mut context = ToolContext::new();
    context
        .insert(AgentId(ROOT_AGENT.to_string()))
        .expect("the agent id serializes");
    context
}

pub fn text(output: &ToolOutput) -> String {
    output.render()
}
