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

use crate::checkpoint::{Capture, PreImages};
use crate::gate::{AgentId, CallView, GateDecision, ToolGate};
use crate::kills::KillSwitches;
use crate::result_pipeline::{ResultPipeline, ToolOutcome};
use crate::risk::{RiskClass, ToolSource};
use crate::scope::Scope;

/// The tracing target the telemetry allowlist exports from.
const TARGET: &str = "fontaine::telemetry";

/// What exporters see for an MCP tool whose name is not opted in.
pub const HIDDEN: &str = "third-party";

/// What a call refused because its agent was killed reports back.
const KILLED_REFUSAL: &str = "the agent was killed";

/// How a call picks the tool instance that serves it.
enum Routing {
    One,
    /// One instance per workspace root, in the scope's order.
    PerRoot(Arc<Scope>),
    /// One instance per workspace root, plus an unconfined read-only fallback at the end of
    /// the instance list for a call whose target lies outside every root. Only the four
    /// read-only file tools register this route, and only after the gate has allowed the
    /// call with the resolved arguments.
    PerRootWithFallback(Arc<Scope>),
}

/// What the gate and the result pipeline are shared with.
#[derive(Clone)]
pub struct Guard {
    pub gate: Arc<dyn ToolGate>,
    pub results: Arc<ResultPipeline>,
    /// Telemetry names an MCP tool `third-party` instead of by its server and tool.
    pub hide_mcp_names: bool,
    /// Saves the file a `write` or `edit` is about to change.
    pub pre_images: Option<Arc<dyn PreImages>>,
    /// The session's kill switches; `None` only where no agent can be killed. A call whose
    /// agent is killed while it waits on the gate or runs is refused instead of continuing.
    pub kills: Option<Arc<KillSwitches>>,
}

/// Rewrites a call's arguments for the agent making it, before the gate sees them, and may
/// return a note to append to the result.
pub type PreGate = Arc<dyn Fn(&str, Value) -> (Value, Option<String>) + Send + Sync>;

pub struct GuardedTool<T: Tool> {
    instances: Vec<T>,
    routing: Routing,
    risk: RiskClass,
    guard: Guard,
    pre_gate: Option<PreGate>,
}

impl<T: Tool> GuardedTool<T> {
    /// A tool with a single instance, which serves every call.
    pub fn single(tool: T, risk: RiskClass, guard: Guard) -> Self {
        GuardedTool {
            instances: vec![tool],
            routing: Routing::One,
            risk,
            guard,
            pre_gate: None,
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
            pre_gate: None,
        }
    }

    /// A tool with one confined instance per root of `scope` plus one unconfined fallback
    /// based at the primary root, built by `make_fallback`. A call whose path lies outside
    /// every root goes to the fallback, with the resolved arguments the pipeline normalized:
    /// the fallback validates their absolute external shape but does not touch the file
    /// system again.
    pub fn per_root_with_fallback(
        scope: Arc<Scope>,
        risk: RiskClass,
        guard: Guard,
        make: impl Fn(&std::path::Path) -> T,
        make_fallback: impl FnOnce(&std::path::Path) -> T,
    ) -> Self {
        let mut instances: Vec<T> = scope.roots().iter().map(|root| make(root)).collect();
        instances.push(make_fallback(scope.primary()));
        GuardedTool {
            instances,
            routing: Routing::PerRootWithFallback(scope),
            risk,
            guard,
            pre_gate: None,
        }
    }

    /// Runs `prepare` on every call's arguments before the gate, so the gate, the hooks and
    /// the tool all see the same ones. It must derive its result from harness state, never from
    /// what the model wrote.
    pub fn with_pre_gate(mut self, prepare: PreGate) -> Self {
        self.pre_gate = Some(prepare);
        self
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
            Routing::PerRootWithFallback(scope) => match scope.locate(T::NAME, &args) {
                Ok(located) => located,
                Err(crate::scope::ScopeError::OutOfScope(_)) => {
                    crate::scope::external_shape(T::NAME, &args)
                        .map_err(|e| ToolExecutionError::refused(e.to_string()))?;
                    crate::scope::Located {
                        root: self.instances.len() - 1,
                        args,
                    }
                }
                Err(e) => return Err(ToolExecutionError::refused(e.to_string())),
            },
        };
        let typed: T::Args = serde_json::from_value(located.args.clone()).map_err(|e| {
            ToolExecutionError::invalid_args(format!("failed to parse tool arguments: {e}"))
        })?;
        let capture = self.capture(context, &located.args).await;
        let tool = &self.instances[located.root];
        let (succeeded, output) = {
            let result = tool.call(context, typed).await;
            (
                result.is_ok(),
                result
                    .map_err(|e| tool.map_error(e))
                    .and_then(|o| o.into_tool_output()),
            )
        };
        if let Some(capture) = capture {
            // Hashing the file must not block the executor. A lost record leaves the file
            // looking unchanged to a later rewind, so it is logged.
            if let Err(e) = rig_tools::blocking(move || {
                capture.finish(succeeded);
                Ok(())
            })
            .await
            {
                tracing::warn!(error = %e, "the checkpoint record of an edit was lost");
            }
        }
        let output = output?;
        Ok(output)
    }

    /// Saves the file the call names, when this tool is one whose edits are kept. `located`
    /// already holds the path the tool will open; capture failures never stop the call.
    async fn capture(&self, context: &ToolContext, located: &Value) -> Option<Box<dyn Capture>> {
        let scope = match &self.routing {
            Routing::PerRoot(scope) | Routing::PerRootWithFallback(scope) => scope,
            Routing::One => return None,
        };
        let pre_images = self.guard.pre_images.clone()?;
        if !crate::descriptor::find(T::NAME).is_some_and(|d| d.checkpointed) {
            return None;
        }
        let agent = agent_of(context).ok()?;
        let path = located.get("path")?.as_str()?;
        let (_, resolved) = scope.find(path).ok()?;
        match rig_tools::blocking(move || Ok(pre_images.before(&agent, T::NAME, &resolved))).await {
            Ok(capture) => capture,
            Err(e) => {
                tracing::warn!(error = %e, "the pre-image of an edit could not be taken");
                None
            }
        }
    }
}

/// Whose call this is. A call that does not say would be gated as the root agent, with none of
/// a child's own mode or restrictions, so it fails instead.
fn agent_of(context: &ToolContext) -> Result<String, ToolExecutionError> {
    context.require::<AgentId>().map(|id| id.0).map_err(|e| {
        ToolExecutionError::other(format!("the call does not say which agent made it: {e}"))
    })
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
    requires_user_interaction: bool,
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
        requires_user_interaction,
    } = meta;
    let args = normalize(args);
    let agent = agent_of(context)?;
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
        requires_user_interaction,
        synthetic: false,
    };
    let mut scan = None;
    let kills = guard.kills.as_ref();
    let outcome = if kills.is_some_and(|kills| kills.killed(&agent)) {
        // Nothing new starts for a dead agent; this also covers a call that begins after the
        // kill but before the run's next hook boundary stops it.
        Err(ToolExecutionError::refused(KILLED_REFUSAL))
    } else {
        // A kill while the call waits on the gate drops the wait, exactly as a dropped drive
        // future does; the wait's own guards (the permission pipeline's WaitGuard) unwind with it.
        let decision = match kills {
            Some(kills) => tokio::select! {
                biased;
                _ = kills.wait(&agent) => None,
                decision = guard.gate.check(view).instrument(span.clone()) => Some(decision),
            },
            None => Some(guard.gate.check(view).instrument(span.clone()).await),
        };
        match decision {
            None => Err(ToolExecutionError::refused(KILLED_REFUSAL)),
            Some(GateDecision::Deny { message }) => Err(ToolExecutionError::refused(message)),
            Some(GateDecision::Allow { args: allowed }) => {
                let allowed = normalize(allowed);
                let mut cut_short = false;
                let outcome = match kills {
                    Some(kills) => tokio::select! {
                        biased;
                        _ = kills.wait(&agent) => {
                            // The in-flight tool is dropped from inside the harness, the same
                            // drop an instantaneous kill performs from the top.
                            cut_short = true;
                            Err(ToolExecutionError::refused(KILLED_REFUSAL))
                        }
                        outcome = run(context, allowed.clone()).instrument(span.clone()) => outcome,
                    },
                    None => run(context, allowed.clone()).instrument(span.clone()).await,
                };
                if cut_short {
                    outcome
                } else {
                    let view = CallView {
                        tool: name,
                        risk,
                        args: &allowed,
                        agent: &agent,
                        annotations,
                        requires_user_interaction,
                        synthetic: false,
                    };
                    let (outcome, record) = guard
                        .results
                        .run(view, outcome)
                        .instrument(span.clone())
                        .await;
                    scan = record;
                    outcome
                }
            }
        }
    };

    let success = outcome.is_ok();
    span.record("success", success);
    span.in_scope(|| {
        tracing::info!(
            target: TARGET,
            tool_name = shown,
            risk_class = risk.as_str(),
            tool_source = source.as_str(),
            success = success,
            duration_ms = started.elapsed().as_millis() as u64,
            args_bytes = args_bytes as u64,
            result_bytes = output_bytes(&outcome) as u64,
            scanner_verdict = scan.as_ref().map(|s| s.verdict.as_str()),
            scanner_latency_ms = scan.as_ref().map(|s| s.latency_ms),
            "tool_result"
        )
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
        let mut note = None;
        let mut args = args;
        if let Some(prepare) = &self.pre_gate
            && let Ok(agent) = agent_of(context)
        {
            (args, note) = prepare(&agent, normalize(args));
        }
        let outcome = gated(
            &self.guard,
            Meta {
                name: T::NAME,
                risk: self.risk,
                source: ToolSource::Builtin,
                annotations: None,
                requires_user_interaction: false,
            },
            context,
            args,
            |context, allowed| self.run(context, allowed),
        )
        .await;
        match note {
            Some(note) => crate::result_pipeline::append_note(outcome, &note),
            None => outcome,
        }
    }
}

/// An MCP tool behind the same gate: the callback runs only after the gate allows the call.
/// The tool keeps its original definition.
pub fn guard_callback(
    definition: ToolDefinition,
    callback: Arc<dyn ToolCallback>,
    risk: RiskClass,
    guard: Guard,
    requires_user_interaction: bool,
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
                    requires_user_interaction,
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
