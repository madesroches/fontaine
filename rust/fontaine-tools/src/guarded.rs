//! The single choke point: every tool is registered wrapped, so nothing runs without the gate's
//! consent and every result passes the result pipeline before the agent sees it.

use std::sync::Arc;
use std::time::Instant;

use rig_core::completion::{ToolAnnotations, ToolDefinition};
use rig_core::serve::adapters::ToolCallback;
use rig_core::tool::{
    DynamicTool, IntoToolOutput, Tool, ToolContext, ToolExecutionError, ToolOutput, tool_definition,
};
use serde_json::Value;
use tracing::Instrument;

use crate::gate::{AgentId, CallView, GateDecision, ROOT_AGENT, ToolGate};
use crate::result_pipeline::{ResultPipeline, ToolOutcome};
use crate::risk::{RiskClass, ToolSource};
use crate::scope::Scope;

/// The tracing target the telemetry allowlist exports from.
const TARGET: &str = "fontaine::telemetry";

/// What exporters see for an MCP tool whose name is not opted in.
pub const HIDDEN: &str = "third-party";

/// How a call picks the tool instance that serves it.
enum Routing {
    One,
    /// One instance per workspace root, in the scope's order.
    PerRoot(Arc<Scope>),
}

/// What the gate and the result pipeline are shared with.
#[derive(Clone)]
pub struct Guard {
    pub gate: Arc<dyn ToolGate>,
    pub results: Arc<ResultPipeline>,
    /// Telemetry names an MCP tool `third-party` instead of by its server and tool.
    pub hide_mcp_names: bool,
}

pub struct GuardedTool<T: Tool> {
    instances: Vec<T>,
    routing: Routing,
    risk: RiskClass,
    guard: Guard,
}

impl<T: Tool> GuardedTool<T> {
    /// A tool with a single instance, which serves every call.
    pub fn single(tool: T, risk: RiskClass, guard: Guard) -> Self {
        GuardedTool {
            instances: vec![tool],
            routing: Routing::One,
            risk,
            guard,
        }
    }

    /// A tool with one instance per root of `scope`, built by `make`. A call goes to the
    /// instance of the root that holds the path it names.
    pub fn per_root(
        scope: Arc<Scope>,
        risk: RiskClass,
        guard: Guard,
        make: impl Fn(&std::path::Path) -> T,
    ) -> Self {
        GuardedTool {
            instances: scope.roots().iter().map(|root| make(root)).collect(),
            routing: Routing::PerRoot(scope),
            risk,
            guard,
        }
    }

    pub fn risk(&self) -> RiskClass {
        self.risk
    }

    fn first(&self) -> &T {
        &self.instances[0]
    }

    /// The registered form: a dynamic tool that calls this wrapper.
    pub fn into_dynamic(self) -> DynamicTool
    where
        T: 'static,
    {
        let definition = tool_definition(&self);
        let ToolDefinition {
            name,
            description,
            parameters,
            annotations,
            ..
        } = definition;
        let this = Arc::new(self);
        DynamicTool::new_with_context(name, description, parameters, move |context, args| {
            let this = this.clone();
            Box::pin(async move { this.call(context, args).await })
        })
        .with_annotations(annotations)
    }

    async fn run(&self, context: &mut ToolContext, args: Value) -> ToolOutcome {
        let located = match &self.routing {
            Routing::One => crate::scope::Located { root: 0, args },
            Routing::PerRoot(scope) => scope
                .locate(T::NAME, &args)
                .map_err(|e| ToolExecutionError::refused(e.to_string()))?,
        };
        let typed: T::Args = serde_json::from_value(located.args.clone()).map_err(|e| {
            ToolExecutionError::invalid_args(format!("failed to parse tool arguments: {e}"))
        })?;
        let tool = &self.instances[located.root];
        let output = tool
            .call(context, typed)
            .await
            .map_err(|e| tool.map_error(e))?
            .into_tool_output()?;
        Ok(output)
    }
}

fn agent_of(context: &ToolContext) -> String {
    context
        .get::<AgentId>()
        .ok()
        .flatten()
        .map_or_else(|| ROOT_AGENT.to_string(), |id| id.0)
}

fn normalize(args: Value) -> Value {
    match args {
        Value::Null => Value::Object(Default::default()),
        other => other,
    }
}

fn output_bytes(outcome: &ToolOutcome) -> usize {
    match outcome {
        Ok(output) => output.render().len(),
        Err(error) => error.model_output().render().len(),
    }
}

/// One gated call. The gate decides first; `run` executes only after an allow, and what the
/// tool returns passes the result pipeline before anything else sees it. A denial or a tool
/// failure still ends in a `tool_result` event. Built-in and MCP tools share this path.
struct Meta<'a> {
    name: &'a str,
    risk: RiskClass,
    source: ToolSource,
    annotations: Option<&'a str>,
}

async fn gated<'a, F, Fut>(
    guard: &Guard,
    meta: Meta<'_>,
    context: &'a mut ToolContext,
    args: Value,
    run: F,
) -> ToolOutcome
where
    F: FnOnce(&'a mut ToolContext, Value) -> Fut + Send,
    Fut: std::future::Future<Output = ToolOutcome> + Send + 'a,
{
    let Meta {
        name,
        risk,
        source,
        annotations,
    } = meta;
    let args = normalize(args);
    let agent = agent_of(context);
    let shown = if source == ToolSource::Mcp && guard.hide_mcp_names {
        HIDDEN
    } else {
        name
    };
    let span = tracing::info_span!(
        target: TARGET,
        "fontaine.tool",
        tool_name = shown,
        risk_class = risk.as_str(),
        tool_source = source.as_str(),
        success = tracing::field::Empty,
    );
    let started = Instant::now();
    let args_bytes = args.to_string().len();

    let view = CallView {
        tool: name,
        risk,
        args: &args,
        agent: &agent,
        annotations,
    };
    let mut scan = None;
    let outcome = match guard.gate.check(view).instrument(span.clone()).await {
        GateDecision::Deny { message } => Err(ToolExecutionError::refused(message)),
        GateDecision::Allow { args: allowed } => {
            let allowed = normalize(allowed);
            let outcome = run(context, allowed.clone()).instrument(span.clone()).await;
            let view = CallView {
                tool: name,
                risk,
                args: &allowed,
                agent: &agent,
                annotations,
            };
            let (outcome, record) = guard
                .results
                .run(view, outcome)
                .instrument(span.clone())
                .await;
            scan = record;
            outcome
        }
    };

    let success = outcome.is_ok();
    span.record("success", success);
    span.in_scope(|| match scan {
        Some(scan) => tracing::info!(
            target: TARGET,
            tool_name = shown,
            risk_class = risk.as_str(),
            tool_source = source.as_str(),
            success = success,
            duration_ms = started.elapsed().as_millis() as u64,
            args_bytes = args_bytes as u64,
            result_bytes = output_bytes(&outcome) as u64,
            scanner_verdict = scan.verdict.as_str(),
            scanner_latency_ms = scan.latency_ms,
            "tool_result"
        ),
        None => tracing::info!(
            target: TARGET,
            tool_name = shown,
            risk_class = risk.as_str(),
            tool_source = source.as_str(),
            success = success,
            duration_ms = started.elapsed().as_millis() as u64,
            args_bytes = args_bytes as u64,
            result_bytes = output_bytes(&outcome) as u64,
            "tool_result"
        ),
    });
    outcome
}

impl<T: Tool> Tool for GuardedTool<T> {
    const NAME: &'static str = T::NAME;
    type Args = Value;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        self.first().description()
    }

    fn parameters(&self) -> Value {
        self.first().parameters()
    }

    fn annotations(&self) -> ToolAnnotations {
        self.first().annotations()
    }

    async fn call(&self, context: &mut ToolContext, args: Value) -> ToolOutcome {
        gated(
            &self.guard,
            Meta {
                name: T::NAME,
                risk: self.risk,
                source: ToolSource::Builtin,
                annotations: None,
            },
            context,
            args,
            |context, allowed| self.run(context, allowed),
        )
        .await
    }
}

/// An MCP tool behind the same gate: the callback runs only after the gate allows the call.
/// The tool keeps its original definition.
pub fn guard_callback(
    definition: ToolDefinition,
    callback: Arc<dyn ToolCallback>,
    risk: RiskClass,
    guard: Guard,
) -> DynamicTool {
    let ToolDefinition {
        name,
        description,
        parameters,
        annotations,
        ..
    } = definition;
    let label = name.to_string();
    // The server's own words about the tool, kept as text for the guardian to weigh.
    let declared = (!annotations.is_empty())
        .then(|| serde_json::to_string(&annotations).ok())
        .flatten();
    let kept = annotations.clone();
    DynamicTool::new_with_context(name, description, parameters, move |context, args| {
        let guard = guard.clone();
        let callback = callback.clone();
        let label = label.clone();
        let declared = declared.clone();
        Box::pin(async move {
            gated(
                &guard,
                Meta {
                    name: &label,
                    risk,
                    source: ToolSource::Mcp,
                    annotations: declared.as_deref(),
                },
                context,
                args,
                |context, allowed| (*callback)(context, allowed),
            )
            .await
        })
    })
    .with_annotations(kept)
}
