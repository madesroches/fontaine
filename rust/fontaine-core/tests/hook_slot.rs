//! The `pre_tool_use` slot, driven by test doubles: user hooks load in a later phase, but the
//! composition and failure rules are fixed here.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::pipeline::{Options, Rig, allowed};
use fontaine_core::audit::{Decision, args_digest};
use fontaine_core::config::PermissionMode::{Manual, Plan};
use fontaine_core::permissions::{
    HookDecision, HookError, HookPermission, PreToolUseEvent, PreToolUseHook,
};
use fontaine_core::tier::Tier;
use futures::future::BoxFuture;
use serde_json::{Value, json};

enum Reply {
    Decide(HookDecision),
    Fail,
    Malformed,
    Hang,
}

struct Double {
    id: &'static str,
    reply: Reply,
}

impl PreToolUseHook for Double {
    fn id(&self) -> String {
        self.id.to_string()
    }

    fn timeout(&self) -> Duration {
        Duration::from_millis(100)
    }

    fn run<'a>(
        &'a self,
        _event: PreToolUseEvent<'a>,
    ) -> BoxFuture<'a, Result<HookDecision, HookError>> {
        Box::pin(async move {
            match &self.reply {
                Reply::Decide(decision) => Ok(decision.clone()),
                Reply::Fail => Err(HookError::Failed("boom".into())),
                Reply::Malformed => Err(HookError::Malformed("not json".into())),
                Reply::Hang => {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    Ok(HookDecision::default())
                }
            }
        })
    }
}

fn hook(id: &'static str, permission: Option<HookPermission>, input: Option<Value>) -> Arc<Double> {
    Arc::new(Double {
        id,
        reply: Reply::Decide(HookDecision {
            permission,
            updated_input: input,
            reason: None,
            guardian_context: None,
        }),
    })
}

fn rig(
    mode: fontaine_core::config::PermissionMode,
    settings: &str,
    hooks: Vec<Arc<Double>>,
) -> Rig {
    let mut options = Options::new(mode).settings(settings);
    options.hooks = hooks
        .into_iter()
        .map(|h| h as Arc<dyn PreToolUseHook>)
        .collect();
    Rig::with(options)
}

fn shell(command: &str) -> Value {
    json!({ "command": command })
}

#[tokio::test]
async fn the_rewritten_input_is_what_the_floor_the_rules_and_the_audit_see() {
    let settings = "[permissions]\ndeny = [\"read(.env)\"]\n";
    let rewrite = json!({"path": ".env"});
    let rig = rig(
        Manual,
        settings,
        vec![hook("rewrite", None, Some(rewrite.clone()))],
    );
    let original = json!({"path": "a.txt"});
    let d = rig.check("read", original.clone()).await;
    assert!(!allowed(&d));
    let record = rig.audit()[0].clone();
    assert_eq!(record.tier, Tier::DenyRule);
    assert_eq!(record.args_digest, args_digest(&rewrite));
    assert_eq!(record.original_args_digest, Some(args_digest(&original)));

    // The floor judges the rewrite too.
    let rig2 = self::rig(
        Manual,
        "",
        vec![hook("rewrite", None, Some(shell("rm -rf /")))],
    );
    rig2.check("shell", shell("ls")).await;
    assert_eq!(rig2.audit()[0].tier, Tier::CriticalPath);

    // And an allowed call runs with the rewritten arguments.
    let rig3 = self::rig(
        Manual,
        "",
        vec![hook("rewrite", None, Some(json!({"path": "b.txt"})))],
    );
    std::fs::write(rig3.work().join("b.txt"), "b").unwrap();
    match rig3.check("read", json!({"path": "a.txt"})).await {
        fontaine_tools::gate::GateDecision::Allow { args } => {
            assert_eq!(args, json!({"path": "b.txt"}));
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn the_last_rewrite_in_configured_order_wins() {
    let rig = rig(
        Manual,
        "",
        vec![
            hook("one", None, Some(json!({"path": "one.txt"}))),
            hook("two", None, Some(json!({"path": "two.txt"}))),
        ],
    );
    match rig.check("read", json!({"path": "a.txt"})).await {
        fontaine_tools::gate::GateDecision::Allow { args } => {
            assert_eq!(args, json!({"path": "two.txt"}));
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn deny_beats_ask_beats_allow_whatever_the_order() {
    use HookPermission::{Allow, Ask, Deny};
    let orders: [&[(&'static str, HookPermission)]; 4] = [
        &[("a", Allow), ("b", Ask), ("c", Deny)],
        &[("c", Deny), ("b", Ask), ("a", Allow)],
        &[("b", Ask), ("c", Deny), ("a", Allow)],
        &[("a", Allow), ("c", Deny)],
    ];
    for order in orders {
        let hooks = order
            .iter()
            .map(|(id, p)| hook(id, Some(*p), None))
            .collect();
        let rig = rig(Manual, "", hooks);
        assert!(!allowed(&rig.check("shell", shell("touch x")).await));
        let record = rig.audit()[0].clone();
        assert_eq!((record.decision, record.tier), (Decision::Deny, Tier::Hook));
        assert_eq!(record.rule.as_deref(), Some("c"));
        assert_eq!(rig.approver.asked(), 0);
    }
    let rig = rig(
        Manual,
        "",
        vec![hook("a", Some(Allow), None), hook("b", Some(Ask), None)],
    );
    rig.check("shell", shell("touch x")).await;
    assert_eq!(
        (rig.audit()[0].decision, rig.audit()[0].tier),
        (Decision::Prompt, Tier::Hook)
    );
    let rig = self::rig(Manual, "", vec![hook("a", Some(Allow), None)]);
    assert!(allowed(&rig.check("shell", shell("touch x")).await));
    let record = rig.audit()[0].clone();
    assert_eq!(
        (record.decision, record.tier),
        (Decision::Allow, Tier::Hook)
    );
    assert_eq!(record.rule.as_deref(), Some("a"));
}

#[tokio::test]
async fn a_hook_ask_asks_even_where_nothing_else_would() {
    let rig = rig(
        Manual,
        "",
        vec![hook("careful", Some(HookPermission::Ask), None)],
    );
    std::fs::write(rig.work().join("a.txt"), "a").unwrap();
    rig.check("read", json!({"path": "a.txt"})).await;
    assert_eq!(rig.approver.asked(), 1);
    let request = rig.approver.requests.lock().unwrap()[0].clone();
    assert!(!request.offers.session && !request.offers.always);
}

#[tokio::test]
async fn a_hook_allow_never_passes_the_floor() {
    let allow = || vec![hook("yes", Some(HookPermission::Allow), None)];
    let settings = "[permissions]\ndeny = [\"read(.env)\"]\nask = [\"shell(git push)\"]\n";
    let rig = rig(Manual, settings, allow());
    std::fs::write(rig.work().join(".env"), "x").unwrap();
    for (tool, args, tier, decision) in [
        (
            "read",
            json!({"path": ".env"}),
            Tier::DenyRule,
            Decision::Deny,
        ),
        ("shell", shell("git push"), Tier::AskRule, Decision::Prompt),
        (
            "shell",
            shell("rm -rf /"),
            Tier::CriticalPath,
            Decision::Prompt,
        ),
        (
            "write",
            json!({"path": ".git/config", "content": "x"}),
            Tier::ProtectedPath,
            Decision::Prompt,
        ),
    ] {
        let before = rig.audit().len();
        rig.check(tool, args).await;
        let record = rig.audit()[before].clone();
        assert_eq!((record.decision, record.tier), (decision, tier));
    }
    // Nor does it let an edit run in plan mode.
    let rig = self::rig(Plan, "", allow());
    let d = rig
        .check("write", json!({"path": "n.txt", "content": "x"}))
        .await;
    assert!(!allowed(&d));
    assert_eq!(rig.audit()[0].tier, Tier::PlanBlock);
}

#[tokio::test]
async fn an_erroring_timing_out_or_malformed_hook_decides_nothing() {
    for reply in [Reply::Fail, Reply::Malformed, Reply::Hang] {
        let double = Arc::new(Double { id: "bad", reply });
        let rig = rig(Manual, "", vec![double]);
        // It could have allowed; instead the normal pipeline runs and asks.
        rig.check("shell", shell("touch x")).await;
        let record = rig.audit()[0].clone();
        assert_eq!(
            (record.decision, record.tier),
            (Decision::Prompt, Tier::Mode)
        );
    }
    // A rewrite that is not an object is ignored.
    let rig = rig(Manual, "", vec![hook("odd", None, Some(json!("a string")))]);
    std::fs::write(rig.work().join("a.txt"), "a").unwrap();
    assert!(allowed(&rig.check("read", json!({"path": "a.txt"})).await));
    assert!(rig.audit()[0].original_args_digest.is_none());
}
