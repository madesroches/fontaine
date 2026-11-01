//! The Rune surface of a composition. `Call`, `Score` and `Verdict` have no Rune constructors and
//! no settable fields, so a script cannot forge them: an allow can only come from `llm()` or from
//! `allow_below` applied to a score a declared read produced.

use std::future::poll_fn;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;

use rune::runtime::{Future as RuneFuture, Ref, Value, VmResult};
use rune::{Any, ContextError, Module};

use super::eval::EvalCtx;
use crate::guardian::Adjudication;
use crate::verdict::Decision;

#[derive(Any, Clone)]
#[rune(item = ::Call)]
pub(crate) struct Call {
    pub(crate) ctx: Arc<EvalCtx>,
}

impl Call {
    #[rune::function]
    fn tool(&self) -> String {
        self.ctx.request().tool_call.tool.clone()
    }

    #[rune::function]
    fn destructive(&self) -> bool {
        self.ctx.request().tool_call.destructive
    }
}

#[derive(Any, Clone)]
#[rune(item = ::Score)]
pub(crate) struct Score {
    value: f64,
}

impl Score {
    #[rune::function]
    fn value(&self) -> f64 {
        self.value
    }
}

/// What a composition returned, tagged with where the verdict was minted.
#[derive(Clone, Debug)]
pub(crate) enum VerdictKind {
    Abstain,
    /// The LLM guardian's own verdict, whatever it decided.
    Llm(Arc<Adjudication>),
    Escalate {
        node: String,
        reason: String,
    },
    Deny {
        node: String,
        reason: String,
    },
    Allow {
        node: String,
        score: f64,
        threshold: f64,
    },
}

#[derive(Any, Clone)]
#[rune(item = ::Verdict)]
pub(crate) struct Verdict {
    pub(crate) kind: VerdictKind,
}

impl Verdict {
    fn of(kind: VerdictKind) -> Verdict {
        Verdict { kind }
    }

    #[rune::function]
    fn is_abstain(&self) -> bool {
        matches!(self.kind, VerdictKind::Abstain)
    }

    #[rune::function]
    fn is_allow(&self) -> bool {
        match &self.kind {
            VerdictKind::Allow { .. } => true,
            VerdictKind::Llm(adjudication) => adjudication.decision == Decision::Allow,
            _ => false,
        }
    }
}

/// The verdict a value is, if it is one.
pub(crate) fn verdict_of(value: &Value) -> Result<VerdictKind, String> {
    match value.borrow_ref::<Verdict>() {
        Ok(verdict) => Ok(verdict.kind.clone()),
        Err(_) => Err(format!(
            "the composition returned {} instead of a verdict",
            value.type_info()
        )),
    }
}

async fn read(call: Ref<Call>, name: Ref<str>) -> Result<Score, String> {
    let ctx = call.ctx.clone();
    let name = name.to_string();
    drop(call);
    let record = ctx.start_read(&name)?.await;
    record.result.clone().map(|value| Score { value })
}

async fn llm(call: Ref<Call>) -> Verdict {
    let ctx = call.ctx.clone();
    drop(call);
    Verdict::of(VerdictKind::Llm(ctx.llm().await))
}

/// Polls every future at once and returns the first verdict that is not an abstain, listed order
/// winning when several are ready together. The others are dropped.
async fn first_decisive(futures: Value) -> VmResult<Verdict> {
    let mut pending: Vec<Option<RuneFuture>> = Vec::new();
    {
        let list = match futures.borrow_ref::<rune::runtime::Vec>() {
            Ok(list) => list,
            Err(error) => return VmResult::err(error),
        };
        for value in list.iter() {
            match value.clone().into_future() {
                Ok(future) => pending.push(Some(future)),
                Err(error) => return VmResult::err(error),
            }
        }
    }
    poll_fn(|cx| {
        let mut waiting = false;
        for slot in &mut pending {
            let Some(future) = slot.as_mut() else {
                continue;
            };
            match Pin::new(future).poll(cx) {
                Poll::Pending => waiting = true,
                Poll::Ready(result) => {
                    *slot = None;
                    let value = match result {
                        VmResult::Ok(value) => value,
                        VmResult::Err(error) => return Poll::Ready(VmResult::Err(error)),
                    };
                    match verdict_of(&value) {
                        Ok(VerdictKind::Abstain) => {}
                        Ok(kind) => return Poll::Ready(VmResult::Ok(Verdict::of(kind))),
                        Err(message) => return Poll::Ready(VmResult::panic(message)),
                    }
                }
            }
        }
        if waiting {
            Poll::Pending
        } else {
            Poll::Ready(VmResult::Ok(Verdict::of(VerdictKind::Abstain)))
        }
    })
    .await
}

fn escalate(node: &str, reason: &str) -> Verdict {
    Verdict::of(VerdictKind::Escalate {
        node: node.to_owned(),
        reason: reason.to_owned(),
    })
}

fn deny(node: &str, reason: &str) -> Verdict {
    Verdict::of(VerdictKind::Deny {
        node: node.to_owned(),
        reason: reason.to_owned(),
    })
}

fn abstain() -> Verdict {
    Verdict::of(VerdictKind::Abstain)
}

fn allow_below(node: &str, score: &Score, threshold: f64) -> Verdict {
    if score.value < threshold {
        Verdict::of(VerdictKind::Allow {
            node: node.to_owned(),
            score: score.value,
            threshold,
        })
    } else {
        abstain()
    }
}

/// The functions are in scope of every script without an import.
pub(crate) fn module() -> Result<Module, ContextError> {
    let mut module = Module::new();
    module.ty::<Call>()?;
    module.ty::<Score>()?;
    module.ty::<Verdict>()?;
    module.function_meta(Call::tool)?;
    module.function_meta(Call::destructive)?;
    module.function_meta(Score::value)?;
    module.function_meta(Verdict::is_abstain)?;
    module.function_meta(Verdict::is_allow)?;
    module.function("read", read).build()?;
    module.function("llm", llm).build()?;
    module.function("first_decisive", first_decisive).build()?;
    module.function("escalate", escalate).build()?;
    module.function("deny", deny).build()?;
    module.function("abstain", abstain).build()?;
    module.function("allow_below", allow_below).build()?;
    #[cfg(test)]
    module
        .function("host_panic", || -> bool {
            panic!("a host function panicked")
        })
        .build()?;
    Ok(module)
}
