//! The `elicitation` hook as a real handler: the wire event, how verdicts combine, what a
//! repository's hook can never do, and how `HookElicitor` puts the hooks ahead of the dialog.
//! The command is the `hook_double` example, so the same test runs on every platform.

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use common::pipeline::Rig;
use fontaine_core::agent::HookElicitor;
use fontaine_core::config::PermissionMode::Manual;
use fontaine_core::config::{Hook, HookEvent, Origin, Slot};
use fontaine_core::permissions::ElicitationHook;
use fontaine_core::user_hooks::{BuiltHooks, HookDeps, HookProcess, LivePlugins, UserHook};
use fontaine_testkit::example_bin;
use fontaine_tools::live::Live;
use fontaine_tools::mcp::elicitation::ElicitationSchema;
use fontaine_tools::mcp::{ElicitForm, ElicitReply, ElicitUrl, Elicitor};
use serde_json::{Map, Value, json};

fn double() -> PathBuf {
    example_bin("fontaine-core", "hook_double")
}

fn word(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

fn command(args: &[&str]) -> String {
    let mut line = word(&double().to_string_lossy());
    for arg in args {
        line.push(' ');
        line.push_str(&word(arg));
    }
    line
}

fn user() -> Origin {
    Origin::default()
}

fn project() -> Origin {
    Origin {
        slot: Slot::Project,
        user_trust: false,
    }
}

fn hook(matcher: Option<&str>, args: &[&str], origin: Origin) -> Arc<dyn ElicitationHook> {
    let hook = Hook {
        timeout_secs: Some(10),
        origin,
        ..Hook::command(matcher, &command(args))
    };
    let process = Arc::new(HookProcess {
        scrub_env: Vec::new(),
        set_env: Vec::new(),
        shell: common::command_shell(),
        project_dir: std::env::temp_dir(),
    });
    Arc::new(UserHook::new(
        HookEvent::Elicitation,
        &hook,
        &HookDeps::new(process),
    ))
}

fn answer(action: &str, content: Value) -> String {
    json!({"hookSpecificOutput": {
        "hookEventName": "Elicitation",
        "action": action,
        "content": content,
    }})
    .to_string()
}

fn replying(action: &str, origin: Origin) -> Arc<dyn ElicitationHook> {
    let reply = if action == "accept" {
        answer(action, json!({"name": "Ada"}))
    } else {
        answer(action, Value::Null)
    };
    hook(None, &["--reply", &reply], origin)
}

fn no_plugins() -> LivePlugins {
    Arc::new(Live::new(BuiltHooks::default()))
}

fn form() -> ElicitForm {
    ElicitForm {
        server: "srv".into(),
        id: "7".into(),
        message: "Who?".into(),
        schema: ElicitationSchema::builder()
            .required_string("name")
            .build()
            .unwrap(),
    }
}

fn link() -> ElicitUrl {
    ElicitUrl {
        server: "srv".into(),
        id: "8".into(),
        message: "Sign in".into(),
        url: "https://example.com/".into(),
    }
}

fn elicitor(rig: &Rig, hooks: Vec<Arc<dyn ElicitationHook>>) -> HookElicitor {
    HookElicitor::new(hooks, no_plugins(), rig.approver.clone(), rig.state.clone())
}

fn name(text: &str) -> ElicitReply {
    let mut map = Map::new();
    map.insert("name".into(), json!(text));
    ElicitReply::Accept(map)
}

#[tokio::test]
async fn a_hook_accept_answers_a_form_without_reaching_the_approver() {
    let rig = Rig::new(Manual);
    let e = elicitor(&rig, vec![replying("accept", user())]);
    assert_eq!(e.elicit(form()).await, name("Ada"));
    assert!(rig.approver.elicited.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_hook_decline_or_cancel_answers_a_form() {
    let rig = Rig::new(Manual);
    let e = elicitor(&rig, vec![replying("decline", user())]);
    assert_eq!(e.elicit(form()).await, ElicitReply::Decline);
    let e = elicitor(&rig, vec![replying("cancel", user())]);
    assert_eq!(e.elicit(form()).await, ElicitReply::Cancel);
    assert!(rig.approver.elicited.lock().unwrap().is_empty());
}

#[tokio::test]
async fn no_hook_or_no_verdict_forwards_to_the_approver() {
    let rig = Rig::new(Manual);
    rig.approver.elicit_reply(ElicitReply::Decline);
    let e = elicitor(&rig, Vec::new());
    assert_eq!(e.elicit(form()).await, ElicitReply::Decline);

    // A hook that says nothing, and one whose matcher names another server, decide nothing.
    rig.approver.elicit_reply(ElicitReply::Decline);
    let silent = hook(None, &["--reply", "{}"], user());
    let other = hook(
        Some("other"),
        &["--reply", &answer("cancel", Value::Null)],
        user(),
    );
    let e = elicitor(&rig, vec![silent, other]);
    assert_eq!(e.elicit(form()).await, ElicitReply::Decline);
    assert_eq!(rig.approver.elicited.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn the_matcher_selects_on_the_server_name() {
    let rig = Rig::new(Manual);
    let reply = answer("cancel", Value::Null);
    let e = elicitor(
        &rig,
        vec![hook(Some("a|srv"), &["--reply", &reply], user())],
    );
    assert_eq!(e.elicit(form()).await, ElicitReply::Cancel);
    let e = elicitor(&rig, vec![hook(Some("*"), &["--reply", &reply], user())]);
    assert_eq!(e.elicit(form()).await, ElicitReply::Cancel);
}

#[tokio::test]
async fn cancel_beats_decline_beats_accept() {
    let rig = Rig::new(Manual);
    let e = elicitor(
        &rig,
        vec![
            replying("accept", user()),
            replying("decline", user()),
            replying("cancel", user()),
        ],
    );
    assert_eq!(e.elicit(form()).await, ElicitReply::Cancel);
    let e = elicitor(
        &rig,
        vec![replying("accept", user()), replying("decline", user())],
    );
    assert_eq!(e.elicit(form()).await, ElicitReply::Decline);
}

#[tokio::test]
async fn a_repository_hooks_accept_is_ignored_with_a_warning_but_its_decline_counts() {
    let rig = Rig::new(Manual);
    rig.approver.elicit_reply(ElicitReply::Decline);
    let e = elicitor(&rig, vec![replying("accept", project())]);
    assert_eq!(e.elicit(form()).await, ElicitReply::Decline);
    assert_eq!(rig.approver.elicited.lock().unwrap().len(), 1);
    assert!(
        rig.approver
            .warnings
            .lock()
            .unwrap()
            .iter()
            .any(|w| w.contains("accept is ignored"))
    );

    let e = elicitor(&rig, vec![replying("decline", project())]);
    assert_eq!(e.elicit(form()).await, ElicitReply::Decline);
    assert_eq!(rig.approver.elicited.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_plugin_hook_answers_like_a_repository_hook_does() {
    let rig = Rig::new(Manual);
    let plugin = Origin {
        slot: Slot::Plugin,
        user_trust: false,
    };
    let layer = BuiltHooks {
        elicitation: vec![replying("cancel", plugin)],
        ..BuiltHooks::default()
    };
    let e = HookElicitor::new(
        Vec::new(),
        Arc::new(Live::new(layer)),
        rig.approver.clone(),
        rig.state.clone(),
    );
    assert_eq!(e.elicit(form()).await, ElicitReply::Cancel);
}

#[tokio::test]
async fn exit_two_and_a_block_decision_decline() {
    let rig = Rig::new(Manual);
    let e = elicitor(&rig, vec![hook(None, &["--exit", "2"], user())]);
    assert_eq!(e.elicit(form()).await, ElicitReply::Decline);
    let blocked = json!({"decision": "block", "reason": "no"}).to_string();
    let e = elicitor(&rig, vec![hook(None, &["--reply", &blocked], user())]);
    assert_eq!(e.elicit(form()).await, ElicitReply::Decline);
    // The specific output is ignored once the hook blocks.
    let both = json!({"decision": "block", "hookSpecificOutput": {
        "hookEventName": "Elicitation", "action": "accept", "content": {"name": "x"}}})
    .to_string();
    let e = elicitor(&rig, vec![hook(None, &["--reply", &both], user())]);
    assert_eq!(e.elicit(form()).await, ElicitReply::Decline);
}

#[tokio::test]
async fn a_failing_or_unreadable_hook_decides_nothing_and_is_told_to_the_human() {
    let rig = Rig::new(Manual);
    let cases = [
        hook(None, &["--exit", "1"], user()),
        hook(None, &["--reply", "not json"], user()),
        hook(None, &["--reply", &answer("maybe", Value::Null)], user()),
        // An accept with no content is unreadable.
        hook(
            None,
            &[
                "--reply",
                &json!({"hookSpecificOutput": {"action": "accept"}}).to_string(),
            ],
            user(),
        ),
    ];
    for case in cases {
        rig.approver.elicit_reply(ElicitReply::Decline);
        let e = elicitor(&rig, vec![case]);
        assert_eq!(e.elicit(form()).await, ElicitReply::Decline);
    }
    assert_eq!(rig.approver.elicited.lock().unwrap().len(), 4);
    assert!(rig.approver.warnings.lock().unwrap().len() >= 3);
}

#[tokio::test]
async fn a_hook_that_times_out_decides_nothing() {
    let rig = Rig::new(Manual);
    let slow = Hook {
        timeout_secs: Some(1),
        ..Hook::command(None, &command(&["--sleep", "5"]))
    };
    let process = Arc::new(HookProcess {
        scrub_env: Vec::new(),
        set_env: Vec::new(),
        shell: common::command_shell(),
        project_dir: std::env::temp_dir(),
    });
    let slow: Arc<dyn ElicitationHook> = Arc::new(UserHook::new(
        HookEvent::Elicitation,
        &slow,
        &HookDeps::new(process),
    ));
    rig.approver.elicit_reply(ElicitReply::Decline);
    let e = elicitor(&rig, vec![slow]);
    assert_eq!(e.elicit(form()).await, ElicitReply::Decline);
    assert!(
        rig.approver
            .warnings
            .lock()
            .unwrap()
            .iter()
            .any(|w| w.contains("timed out"))
    );
}

#[tokio::test]
async fn the_wire_event_carries_the_request() {
    let dir = tempfile::tempdir().unwrap();
    let record = dir.path().join("event.json");
    let rig = Rig::new(Manual);
    let e = elicitor(
        &rig,
        vec![hook(None, &["--record", &record.to_string_lossy()], user())],
    );
    rig.approver.elicit_reply(ElicitReply::Cancel);
    e.elicit(form()).await;
    let event: Value = serde_json::from_str(&std::fs::read_to_string(&record).unwrap()).unwrap();
    assert_eq!(event["hook_event_name"], "Elicitation");
    assert_eq!(event["mcp_server_name"], "srv");
    assert_eq!(event["message"], "Who?");
    assert_eq!(event["mode"], "form");
    assert_eq!(event["elicitation_id"], "7");
    assert_eq!(event["session_id"], rig.state.session_id);
    assert_eq!(event["permission_mode"], "manual");
    assert!(event["requested_schema"]["properties"]["name"].is_object());
    assert!(event.get("url").is_none());

    e.elicit_url(link()).await;
    let event: Value = serde_json::from_str(&std::fs::read_to_string(&record).unwrap()).unwrap();
    assert_eq!(event["mode"], "url");
    assert_eq!(event["url"], "https://example.com/");
    assert_eq!(event["elicitation_id"], "8");
    assert!(event.get("requested_schema").is_none());
}

#[tokio::test]
async fn a_hook_never_accepts_a_link_but_may_decline_or_cancel_it() {
    struct Urls(std::sync::Mutex<usize>);
    impl fontaine_core::permissions::Approver for Urls {
        fn approve<'a>(
            &'a self,
            _: fontaine_core::permissions::ApprovalRequest,
        ) -> futures::future::BoxFuture<'a, fontaine_core::permissions::Answer> {
            Box::pin(async { fontaine_core::permissions::Answer::Deny { feedback: None } })
        }
        fn warn(&self, _: &str) {}
        fn elicit_url<'a>(&'a self, _: ElicitUrl) -> futures::future::BoxFuture<'a, ElicitReply> {
            *self.0.lock().unwrap() += 1;
            Box::pin(async { ElicitReply::Decline })
        }
    }
    let rig = Rig::new(Manual);
    let approver = Arc::new(Urls(Default::default()));
    let make = |hooks| HookElicitor::new(hooks, no_plugins(), approver.clone(), rig.state.clone());
    let url_accept = hook(None, &["--reply", &answer("accept", json!({}))], user());
    assert_eq!(
        make(vec![url_accept]).elicit_url(link()).await,
        ElicitReply::Decline
    );
    assert_eq!(*approver.0.lock().unwrap(), 1, "the dialog was reached");
    assert_eq!(
        make(vec![replying("decline", user())])
            .elicit_url(link())
            .await,
        ElicitReply::Decline
    );
    assert_eq!(
        make(vec![replying("cancel", user())])
            .elicit_url(link())
            .await,
        ElicitReply::Cancel
    );
    assert_eq!(*approver.0.lock().unwrap(), 1);
}

#[tokio::test]
async fn an_unhooked_unattended_request_still_cancels() {
    let rig = Rig::new(Manual);
    let e = elicitor(&rig, Vec::new());
    assert_eq!(e.elicit(form()).await, ElicitReply::Cancel);
}
