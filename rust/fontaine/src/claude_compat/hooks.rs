//! Callback hooks: the host program registers hooks in `initialize`, and Fontaine calls back over
//! the stream when one fires. They are tighten-only: a callback can deny, ask and rewrite the
//! input, and an `allow` is ignored, so no host callback skips the mode, `canUseTool` or the
//! guardian.

use std::sync::Arc;
use std::time::Duration;

use fontaine_core::config::{HookEvent, MatcherKind};
use fontaine_core::extensions::map_tool_entry;
use fontaine_core::permissions::HookError;
use fontaine_core::user_hooks::{CallbackHook, HookTransport};
use futures::future::BoxFuture;
use serde_json::{Value, json};

use super::router::Router;
use super::translate::wire_name;

/// Sends a hook's event to the host and waits for its output.
pub struct RouterHooks {
    pub router: Arc<Router>,
}

impl HookTransport for RouterHooks {
    fn call<'a>(
        &'a self,
        callback_id: &'a str,
        input: &'a Value,
    ) -> BoxFuture<'a, Result<Value, HookError>> {
        Box::pin(async move {
            // The host's callbacks match on Claude's tool names, which is what its own matcher
            // used; the arguments stay in Fontaine's schema.
            let mut input = input.clone();
            if let Some(name) = input.get("tool_name").and_then(Value::as_str) {
                let wire = wire_name(name);
                input["tool_name"] = json!(wire);
            }
            let tool_use_id = input.get("tool_use_id").cloned().unwrap_or(Value::Null);
            let reply = self
                .router
                .request(json!({
                    "subtype": "hook_callback",
                    "callback_id": callback_id,
                    "input": input,
                    "tool_use_id": tool_use_id,
                }))
                .await;
            match reply {
                Ok(Value::Null) => Ok(json!({})),
                Ok(output) => Ok(output),
                Err(e) => Err(HookError::Failed {
                    context: "the host's hook callback gave no answer".to_string(),
                    source: Some(Box::new(e)),
                }),
            }
        })
    }
}

/// The callback hooks `initialize.hooks` registers. An event Fontaine has no equivalent for, and a
/// tool matcher that does not map, fail the `initialize` naming it.
pub fn build(
    hooks: &Value,
    transport: &Arc<dyn HookTransport>,
) -> Result<Vec<CallbackHook>, String> {
    let events = hooks
        .as_object()
        .ok_or("`hooks` must be an object keyed by event name")?;
    let mut out = Vec::new();
    for (name, entries) in events {
        let event = HookEvent::from_wire_name(name)
            .ok_or_else(|| format!("the hook event `{name}` has no equivalent in Fontaine"))?;
        let entries = entries
            .as_array()
            .ok_or_else(|| format!("`hooks.{name}` must be a list"))?;
        for entry in entries {
            let ids = entry
                .get("hookCallbackIds")
                .and_then(Value::as_array)
                .ok_or_else(|| format!("a `{name}` hook has no `hookCallbackIds`"))?;
            let timeout = match entry.get("timeout") {
                None | Some(Value::Null) => None,
                Some(value) => Some(
                    value
                        .as_f64()
                        .filter(|secs| secs.is_finite() && *secs > 0.0)
                        .map(Duration::from_secs_f64)
                        .ok_or_else(|| format!("a `{name}` hook has an unusable `timeout`"))?,
                ),
            };
            let fails_closed = match entry.get("onFailure").and_then(Value::as_str) {
                None | Some("continue") => false,
                Some("block") => true,
                Some(other) => {
                    return Err(format!(
                        "a `{name}` hook has the unknown `onFailure` `{other}`"
                    ));
                }
            };
            let matchers = alternatives(event, entry.get("matcher").and_then(Value::as_str))
                .map_err(|why| format!("the `{name}` hook matcher: {why}"))?;
            for matcher in &matchers {
                for id in ids {
                    let callback_id = id.as_str().ok_or_else(|| {
                        format!("a `{name}` hook has a callback id that is not text")
                    })?;
                    out.push(CallbackHook {
                        event,
                        matcher: matcher.clone(),
                        callback_id: callback_id.to_string(),
                        timeout,
                        fails_closed,
                        transport: transport.clone(),
                    });
                }
            }
        }
    }
    Ok(out)
}

/// One matcher per `|` alternative, in Fontaine's grammar. An empty or `*` matcher stays broad: it
/// runs for every call.
fn alternatives(event: HookEvent, matcher: Option<&str>) -> Result<Vec<Option<String>>, String> {
    let matcher = matcher.map(str::trim).unwrap_or("");
    if matcher.is_empty() || matcher == "*" {
        return Ok(vec![None]);
    }
    if event.matcher_kind() == MatcherKind::Tool {
        return matcher
            .split('|')
            .map(|alternative| map_tool_entry(alternative).map(Some))
            .collect();
    }
    event.check_matcher(matcher)?;
    Ok(vec![Some(matcher.to_string())])
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Never;

    impl HookTransport for Never {
        fn call<'a>(&'a self, _: &'a str, _: &'a Value) -> BoxFuture<'a, Result<Value, HookError>> {
            Box::pin(async { Ok(json!({})) })
        }
    }

    fn transport() -> Arc<dyn HookTransport> {
        Arc::new(Never)
    }

    fn built(hooks: Value) -> Result<Vec<CallbackHook>, String> {
        build(&hooks, &transport())
    }

    #[test]
    fn an_alternation_becomes_one_callback_hook_per_mapped_tool() {
        let hooks = built(json!({"PreToolUse": [{"matcher": "Write|Edit", "hookCallbackIds": ["a"], "timeout": 5}]})).unwrap();
        let matchers: Vec<_> = hooks.iter().map(|h| h.matcher.clone()).collect();
        assert_eq!(
            matchers,
            [Some("write".to_string()), Some("edit".to_string())]
        );
        assert!(
            hooks
                .iter()
                .all(|h| h.timeout == Some(Duration::from_secs(5)) && !h.fails_closed)
        );
        assert!(hooks.iter().all(|h| h.event == HookEvent::PreToolUse));
    }

    #[test]
    fn an_unmappable_alternative_names_itself() {
        let error =
            built(json!({"PreToolUse": [{"matcher": "Bash|Nonsense", "hookCallbackIds": ["a"]}]}))
                .unwrap_err();
        assert!(error.contains("Nonsense"), "{error}");
    }

    #[test]
    fn empty_and_star_matchers_stay_broad() {
        for matcher in [json!(""), json!("*"), Value::Null] {
            let hooks =
                built(json!({"PreToolUse": [{"matcher": matcher, "hookCallbackIds": ["a", "b"]}]}))
                    .unwrap();
            assert_eq!(hooks.len(), 2);
            assert!(hooks.iter().all(|h| h.matcher.is_none()));
        }
        let hooks = built(json!({"UserPromptSubmit": [{"hookCallbackIds": ["a"]}]})).unwrap();
        assert!(hooks[0].matcher.is_none());
    }

    #[test]
    fn on_failure_block_fails_closed_and_continue_does_not() {
        let hooks = built(json!({"PreToolUse": [
            {"hookCallbackIds": ["a"], "onFailure": "block"},
            {"hookCallbackIds": ["b"], "onFailure": "continue"},
            {"hookCallbackIds": ["c"]},
        ]}))
        .unwrap();
        assert_eq!(
            hooks.iter().map(|h| h.fails_closed).collect::<Vec<_>>(),
            [true, false, false]
        );
        assert!(
            built(json!({"PreToolUse": [{"hookCallbackIds": ["a"], "onFailure": "explode"}]}))
                .is_err()
        );
    }

    #[test]
    fn an_event_with_no_equivalent_is_an_error_naming_it() {
        let error = built(json!({"PermissionRequest": [{"hookCallbackIds": ["a"]}]})).unwrap_err();
        assert!(error.contains("PermissionRequest"), "{error}");
    }

    #[tokio::test]
    async fn the_hook_input_reaches_the_host_under_claudes_tool_name() {
        use super::super::router::{ControlHandler, Router};
        use crate::headless::Cancel;
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        struct Quiet;
        impl ControlHandler for Quiet {
            fn handle(&self, _: Arc<Router>, _: String, _: Value) -> BoxFuture<'static, ()> {
                Box::pin(async {})
            }
        }
        let (mut host_in, stdin) = tokio::io::duplex(1 << 16);
        let (stdout, host_out) = tokio::io::duplex(1 << 16);
        let router = Router::start(stdin, stdout, Cancel::default(), Arc::new(Quiet));
        let hooks = RouterHooks { router };
        let input =
            json!({"tool_name": "shell", "tool_input": {"command": "ls"}, "tool_use_id": "t1"});
        let call = tokio::spawn(async move { hooks.call("cb1", &input).await });
        let mut lines = BufReader::new(host_out).lines();
        let sent: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(sent["request"]["subtype"], "hook_callback");
        assert_eq!(sent["request"]["callback_id"], "cb1");
        assert_eq!(sent["request"]["input"]["tool_name"], "Bash");
        assert_eq!(sent["request"]["tool_use_id"], "t1");
        let id = sent["request_id"].as_str().unwrap();
        let reply = json!({"type":"control_response","response":{"subtype":"success","request_id":id,"response":{"decision":"block"}}});
        host_in
            .write_all(format!("{reply}\n").as_bytes())
            .await
            .unwrap();
        assert_eq!(call.await.unwrap().unwrap(), json!({"decision": "block"}));
    }
}
