//! The lifecycle hooks as real handlers: what each event sends, what an answer can and cannot do,
//! the caps that bound a hook that keeps blocking, and the guarantee that a hook's words are never
//! the user's. The command hook is the `hook_double` example, a program rather than a shell
//! script, so the same test runs on every platform.

mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use common::pipeline::{Options, Rig};
use fontaine_agents::spawn::LifecycleGate;
use fontaine_agents::tasks::{QualityGate, Task, TaskStatus};
use fontaine_core::config::PermissionMode::Manual;
use fontaine_core::config::{HandlerKind, Hook, HookEvent, Origin, Slot};
use fontaine_core::models::{GuardianModel, ModelHandle, ModelRef};
use fontaine_core::permissions::{Answer, ApprovalRequest, Approver};
use fontaine_core::user_hooks::{
    self, CompactGate, HookDeps, HookProcess, LifecycleHooks, PostHookStage, PromptGate,
    SubagentGate, TeamGate, UserHook,
};
use fontaine_testkit::example_bin;
use fontaine_tools::gate::{CallView, ROOT_AGENT};
use fontaine_tools::result_pipeline::{ResultPipeline, StageKind};
use fontaine_tools::risk::RiskClass;
use futures::future::BoxFuture;
use rig_core::test_utils::{MockCompletionModel, MockTurn};
use rig_core::tool::ToolOutput;
use serde_json::{Value, json};

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

fn path_arg(dir: &tempfile::TempDir, name: &str) -> String {
    dir.path().join(name).to_string_lossy().into_owned()
}

fn process() -> Arc<HookProcess> {
    Arc::new(HookProcess {
        scrub_env: Vec::new(),
        set_env: Vec::new(),
        shell: common::command_shell(),
        project_dir: std::env::temp_dir(),
    })
}

/// Collects what hooks tell the human.
#[derive(Default)]
struct Warnings(Mutex<Vec<String>>);

impl Warnings {
    fn all(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

impl Approver for Warnings {
    fn approve<'a>(&'a self, _request: ApprovalRequest) -> BoxFuture<'a, Answer> {
        Box::pin(async { Answer::Deny { feedback: None } })
    }

    fn warn(&self, message: &str) {
        self.0.lock().unwrap().push(message.to_string());
    }
}

fn deps(warnings: &Arc<Warnings>) -> HookDeps {
    HookDeps {
        approver: warnings.clone(),
        ..HookDeps::new(process())
    }
}

fn hook(args: &[&str]) -> Hook {
    Hook {
        timeout_secs: Some(10),
        ..Hook::command(None, &command(args))
    }
}

fn user_hook(event: HookEvent, hook: &Hook, deps: &HookDeps) -> UserHook {
    UserHook::new(event, hook, deps)
}

struct Fixture {
    rig: Rig,
    warnings: Arc<Warnings>,
}

impl Fixture {
    fn new() -> Fixture {
        Fixture {
            rig: Rig::new(Manual),
            warnings: Arc::default(),
        }
    }

    fn lifecycle(&self, hooks: Vec<(HookEvent, Hook)>) -> LifecycleHooks {
        let deps = deps(&self.warnings);
        let mut map: BTreeMap<HookEvent, Vec<UserHook>> = BTreeMap::new();
        for (event, hook) in hooks {
            map.entry(event)
                .or_default()
                .push(user_hook(event, &hook, &deps));
        }
        LifecycleHooks::new(map, self.rig.state.clone(), self.warnings.clone())
    }

    fn user_messages(&self) -> usize {
        self.rig.state.ledger.view().user_messages.len()
    }
}

fn recorded(path: &str) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn reply(event: &str, field: &str, value: &str) -> String {
    json!({"hookSpecificOutput": {"hookEventName": event, field: value}}).to_string()
}

// ---- context is never the user's words ----

#[tokio::test]
async fn plain_stdout_of_a_session_start_hook_waits_for_the_next_prompt_as_a_note() {
    let f = Fixture::new();
    let lifecycle = f.lifecycle(vec![(
        HookEvent::SessionStart,
        hook(&["--reply", "use tabs"]),
    )]);
    lifecycle.session_start("startup", "test/main").await;
    assert_eq!(
        f.rig.state.take_hook_context(),
        ["[Hook context from session_start, not the user's words: use tabs]"]
    );
    assert!(f.rig.state.take_hook_context().is_empty(), "taken once");
    assert_eq!(f.user_messages(), 0);
}

#[tokio::test]
async fn a_session_start_matcher_selects_on_the_source() {
    let f = Fixture::new();
    let mut only_compact = hook(&["--reply", "after compaction"]);
    only_compact.matcher = Some("compact|clear".into());
    let lifecycle = f.lifecycle(vec![(HookEvent::SessionStart, only_compact)]);
    lifecycle.session_start("startup", "m").await;
    assert!(f.rig.state.take_hook_context().is_empty());
    lifecycle.session_start("compact", "m").await;
    assert_eq!(f.rig.state.take_hook_context().len(), 1);
}

#[tokio::test]
async fn a_prompt_hook_blocks_or_adds_context_and_records_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let seen = path_arg(&dir, "seen.jsonl");
    let f = Fixture::new();
    let blocking = f.lifecycle(vec![(
        HookEvent::UserPromptSubmit,
        hook(&["--append", &seen, "--exit", "2", "--stderr", "not now"]),
    )]);
    let PromptGate::Blocked(message) = blocking.prompt_submit("do it").await else {
        panic!("the hook blocks");
    };
    assert!(
        message.starts_with("UserPromptSubmit operation blocked by hook:\n["),
        "{message}"
    );
    assert!(message.ends_with("]: not now"), "{message}");
    assert_eq!(recorded(&seen)[0]["prompt"], "do it");
    assert_eq!(f.user_messages(), 0);

    let adding = f.lifecycle(vec![(
        HookEvent::UserPromptSubmit,
        hook(&[
            "--reply",
            &reply("UserPromptSubmit", "additionalContext", "be brief"),
        ]),
    )]);
    let gate = adding.prompt_submit("do it").await;
    assert_eq!(
        gate,
        PromptGate::Proceed {
            context: Some(
                "[Hook context from user_prompt_submit, not the user's words: be brief]".into()
            )
        }
    );
    assert_eq!(f.user_messages(), 0);
}

#[tokio::test]
async fn no_event_can_turn_a_hook_answer_into_an_allow() {
    let f = Fixture::new();
    let allow = json!({"hookSpecificOutput": {"permissionDecision": "allow"}}).to_string();
    let lifecycle = f.lifecycle(vec![
        (HookEvent::Stop, hook(&["--reply", &allow])),
        (HookEvent::UserPromptSubmit, hook(&["--reply", &allow])),
    ]);
    assert_eq!(lifecycle.stop("done").await, None);
    assert_eq!(
        lifecycle.prompt_submit("go").await,
        PromptGate::Proceed { context: None }
    );
    assert_eq!(f.user_messages(), 0);
}

#[tokio::test]
async fn hook_text_for_the_model_is_capped() {
    let f = Fixture::new();
    let long = "x".repeat(20_000);
    let lifecycle = f.lifecycle(vec![(
        HookEvent::SessionStart,
        hook(&[
            "--reply",
            &reply("SessionStart", "additionalContext", &long),
        ]),
    )]);
    lifecycle.session_start("startup", "m").await;
    let notes = f.rig.state.take_hook_context();
    assert!(notes[0].contains("[hook output truncated at 10000 characters]"));
    assert!(notes[0].len() < 10_300, "{}", notes[0].len());
}

// ---- stop ----

#[tokio::test]
async fn a_stop_hook_continues_the_turn_and_reports_the_continuation() {
    let dir = tempfile::tempdir().unwrap();
    let seen = path_arg(&dir, "seen.jsonl");
    let f = Fixture::new();
    let lifecycle = f.lifecycle(vec![(
        HookEvent::Stop,
        hook(&[
            "--append",
            &seen,
            "--block-first",
            "1",
            "--stderr",
            "run the tests",
        ]),
    )]);
    let note = lifecycle.stop("first").await.expect("blocked");
    assert_eq!(
        note,
        "[Hook context from stop, not the user's words: run the tests]"
    );
    assert_eq!(lifecycle.stop("second").await, None, "the hook let go");
    let events = recorded(&seen);
    assert_eq!(events[0]["stop_hook_active"], false);
    assert_eq!(events[0]["last_assistant_message"], "first");
    assert_eq!(events[1]["stop_hook_active"], true);
    assert_eq!(events[0]["hook_event_name"], "Stop");
    assert_eq!(f.user_messages(), 0);
}

#[tokio::test]
async fn a_stop_hook_that_always_blocks_is_let_go_after_eight_and_tool_calls_reset_the_count() {
    let f = Fixture::new();
    let lifecycle = f.lifecycle(vec![(
        HookEvent::Stop,
        hook(&["--exit", "2", "--stderr", "again"]),
    )]);
    for _ in 0..8 {
        assert!(lifecycle.stop("a").await.is_some());
    }
    assert_eq!(lifecycle.stop("a").await, None);
    assert_eq!(lifecycle.stop("a").await, None);
    let warned: Vec<_> = f
        .warnings
        .all()
        .into_iter()
        .filter(|w| w.contains("8 times in a row"))
        .collect();
    assert_eq!(warned.len(), 1, "warned once");
    // The agent worked since the previous stop: it is not looping.
    f.rig.state.set_current_call(ROOT_AGENT, "call-1".into());
    assert!(lifecycle.stop("a").await.is_some());
    // A human prompt starts the count again.
    for _ in 0..7 {
        assert!(lifecycle.stop("a").await.is_some());
    }
    assert_eq!(lifecycle.stop("a").await, None);
    lifecycle.prompt_submit("again").await;
    assert!(lifecycle.stop("a").await.is_some());
}

#[tokio::test]
async fn stop_failure_matches_the_error_kind() {
    let dir = tempfile::tempdir().unwrap();
    let seen = path_arg(&dir, "seen.jsonl");
    let f = Fixture::new();
    let mut limited = hook(&["--append", &seen]);
    limited.matcher = Some("rate_limit|overloaded".into());
    let lifecycle = f.lifecycle(vec![(HookEvent::StopFailure, limited)]);
    lifecycle.stop_failure("server_error", "boom", "", "").await;
    assert!(recorded(&seen).is_empty());
    lifecycle
        .stop_failure("rate_limit", "slow down", "", "so far")
        .await;
    let events = recorded(&seen);
    assert_eq!(events[0]["error"], "rate_limit");
    assert_eq!(events[0]["error_details"], "slow down");
    assert_eq!(events[0]["last_assistant_message"], "so far");
}

// ---- failure and system messages ----

#[tokio::test]
async fn a_failing_lifecycle_hook_warns_and_decides_nothing() {
    let f = Fixture::new();
    let lifecycle = f.lifecycle(vec![(HookEvent::Stop, hook(&["--exit", "3"]))]);
    assert_eq!(lifecycle.stop("done").await, None);
    let warnings = f.warnings.all();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("stop hook"), "{warnings:?}");
    assert!(warnings[0].contains("decides nothing"), "{warnings:?}");
}

#[tokio::test]
async fn system_message_is_shown_except_on_the_events_that_discard_it() {
    let f = Fixture::new();
    let message = json!({"systemMessage": "heads up"}).to_string();
    let lifecycle = f.lifecycle(vec![
        (HookEvent::Stop, hook(&["--reply", &message])),
        (HookEvent::SessionEnd, hook(&["--reply", &message])),
        (HookEvent::PreCompact, hook(&["--reply", &message])),
        (HookEvent::PostCompact, hook(&["--reply", &message])),
        (HookEvent::StopFailure, hook(&["--reply", &message])),
    ]);
    lifecycle.stop("done").await;
    assert_eq!(f.warnings.all(), ["heads up"]);
    lifecycle.session_end("other").await;
    lifecycle.pre_compact("auto", None).await;
    lifecycle.post_compact("auto", "s").await;
    lifecycle.stop_failure("unknown", "e", "", "").await;
    assert_eq!(f.warnings.all(), ["heads up"], "the rest discard it");
}

// ---- compaction ----

#[tokio::test]
async fn pre_compact_blocks_or_adds_focus_and_post_compact_gets_the_summary() {
    let dir = tempfile::tempdir().unwrap();
    let seen = path_arg(&dir, "seen.jsonl");
    let f = Fixture::new();
    let focus = f.lifecycle(vec![(
        HookEvent::PreCompact,
        hook(&["--append", &seen, "--reply", "keep the API names"]),
    )]);
    assert_eq!(
        focus.pre_compact("manual", Some("the parser")).await,
        CompactGate::Proceed {
            focus: Some("keep the API names".into())
        }
    );
    let event = &recorded(&seen)[0];
    assert_eq!(event["trigger"], "manual");
    assert_eq!(event["custom_instructions"], "the parser");
    let blocking = f.lifecycle(vec![(
        HookEvent::PreCompact,
        hook(&["--exit", "2", "--stderr", "not while I work"]),
    )]);
    assert_eq!(
        blocking.pre_compact("auto", None).await,
        CompactGate::Blocked("not while I work".into())
    );
    let post_seen = path_arg(&dir, "post.jsonl");
    let post = f.lifecycle(vec![(
        HookEvent::PostCompact,
        hook(&["--append", &post_seen]),
    )]);
    post.post_compact("auto", "the summary").await;
    assert_eq!(recorded(&post_seen)[0]["compact_summary"], "the summary");
}

#[tokio::test]
async fn a_compaction_with_no_hooks_proceeds_untouched() {
    let f = Fixture::new();
    let none = f.lifecycle(Vec::new());
    assert_eq!(
        none.pre_compact("auto", None).await,
        CompactGate::Proceed { focus: None }
    );
}

#[tokio::test]
async fn notification_runs_in_the_background() {
    let dir = tempfile::tempdir().unwrap();
    let seen = path_arg(&dir, "seen.jsonl");
    let f = Fixture::new();
    let mut approvals = hook(&["--append", &seen]);
    approvals.matcher = Some("permission_prompt".into());
    let lifecycle = f.lifecycle(vec![(HookEvent::Notification, approvals)]);
    lifecycle.notify("elicitation_dialog", "a question", "Fontaine");
    lifecycle.notify("permission_prompt", "an approval", "Fontaine");
    for _ in 0..100 {
        if !recorded(&seen).is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let events = recorded(&seen);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["notification_type"], "permission_prompt");
    assert_eq!(events[0]["message"], "an approval");
    assert_eq!(events[0]["title"], "Fontaine");
}

// ---- subagents and task boards ----

#[tokio::test]
async fn subagent_start_adds_notes_for_its_agent_type_only() {
    let f = Fixture::new();
    let deps = deps(&f.warnings);
    let mut explore_only = hook(&[
        "--reply",
        &reply("SubagentStart", "additionalContext", "be quick"),
    ]);
    explore_only.matcher = Some("explore".into());
    let gate = SubagentGate::new(
        vec![user_hook(HookEvent::SubagentStart, &explore_only, &deps)],
        Vec::new(),
        f.rig.state.clone(),
        f.warnings.clone(),
    );
    assert_eq!(
        gate.subagent_start("agent-1", "explore").await,
        ["[Hook context from subagent_start, not the user's words: be quick]"]
    );
    assert!(gate.subagent_start("agent-2", "worker").await.is_empty());
}

#[tokio::test]
async fn subagent_stop_resumes_the_agent_up_to_the_cap() {
    let dir = tempfile::tempdir().unwrap();
    let seen = path_arg(&dir, "seen.jsonl");
    let f = Fixture::new();
    let deps = deps(&f.warnings);
    let blocking = hook(&["--append", &seen, "--exit", "2", "--stderr", "write a test"]);
    let gate = SubagentGate::new(
        Vec::new(),
        vec![user_hook(HookEvent::SubagentStop, &blocking, &deps)],
        f.rig.state.clone(),
        f.warnings.clone(),
    );
    for round in 0..8 {
        let reason = gate.subagent_stop("agent-1", "worker", "report").await;
        assert_eq!(reason.as_deref(), Some("write a test"), "round {round}");
        gate.subagent_resumed("agent-1");
    }
    assert_eq!(
        gate.subagent_stop("agent-1", "worker", "report").await,
        None
    );
    let events = recorded(&seen);
    assert_eq!(events.len(), 8);
    assert_eq!(events[0]["stop_hook_active"], false);
    assert_eq!(events[1]["stop_hook_active"], true);
    assert_eq!(events[0]["agent_type"], "worker");
    assert_eq!(events[0]["last_assistant_message"], "report");
    assert_eq!(events[0]["agent_id"], "agent-1");
    // Another agent has a streak of its own.
    assert!(gate.subagent_stop("agent-2", "worker", "r").await.is_some());
}

fn draft(title: &str) -> Task {
    Task {
        id: String::new(),
        title: title.into(),
        description: "details".into(),
        status: TaskStatus::Pending,
        owner: None,
        blocked_by: Vec::new(),
        created_by: "agent-1".into(),
    }
}

#[tokio::test]
async fn task_created_blocks_a_create_and_sends_the_task_without_an_id() {
    let dir = tempfile::tempdir().unwrap();
    let seen = path_arg(&dir, "seen.jsonl");
    let f = Fixture::new();
    let deps = deps(&f.warnings);
    let blocking = hook(&[
        "--append",
        &seen,
        "--exit",
        "2",
        "--stderr",
        "no vague tasks",
    ]);
    let gate = TeamGate::new(
        Vec::new(),
        Vec::new(),
        f.rig.state.clone(),
        f.warnings.clone(),
    )
    .with_task_created(vec![user_hook(HookEvent::TaskCreated, &blocking, &deps)]);
    let reason = gate.task_created("agent-1", &draft("fix it")).await;
    assert_eq!(reason.as_deref(), Some("no vague tasks"));
    let event = &recorded(&seen)[0];
    assert_eq!(event["hook_event_name"], "TaskCreated");
    assert!(event["task"].get("id").is_none(), "{event}");
    assert_eq!(event["task"]["title"], "fix it");
    assert_eq!(event["task_subject"], "fix it");
    assert_eq!(event["task_description"], "details");
}

// ---- tool failures ----

fn stage_for(f: &Fixture, post: &[&str], failure: &[&str]) -> ResultPipeline {
    let deps = deps(&f.warnings);
    ResultPipeline::new().with_stage(
        StageKind::PostToolUse,
        Arc::new(
            PostHookStage::new(
                vec![user_hook(HookEvent::PostToolUse, &hook(post), &deps)],
                f.rig.state.clone(),
            )
            .with_failure_hooks(vec![user_hook(
                HookEvent::PostToolUseFailure,
                &hook(failure),
                &deps,
            )]),
        ),
    )
}

async fn through(
    pipeline: &ResultPipeline,
    outcome: Result<ToolOutput, rig_core::tool::ToolExecutionError>,
) -> String {
    let args = json!({"path": "a.txt"});
    let view = CallView {
        tool: "read",
        risk: RiskClass::ReadOnly,
        args: &args,
        agent: ROOT_AGENT,
        annotations: None,
        requires_user_interaction: false,
        synthetic: false,
    };
    match pipeline.run(view, outcome).await.0 {
        Ok(output) => output.render(),
        Err(e) => e.model_output().render(),
    }
}

#[tokio::test]
async fn post_tool_use_fires_on_success_and_the_failure_event_on_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let ok = path_arg(&dir, "ok.jsonl");
    let bad = path_arg(&dir, "bad.jsonl");
    let f = Fixture::new();
    let pipeline = stage_for(
        &f,
        &[
            "--append",
            &ok,
            "--reply",
            &reply("PostToolUse", "additionalContext", "fine"),
        ],
        &[
            "--append",
            &bad,
            "--reply",
            &reply("PostToolUseFailure", "additionalContext", "check the path"),
        ],
    );
    let text = through(&pipeline, Ok(ToolOutput::text("contents"))).await;
    assert!(text.contains("fine"), "{text}");
    assert_eq!(recorded(&ok).len(), 1);
    assert!(recorded(&bad).is_empty());
    let failure = Err(rig_core::tool::ToolExecutionError::other("no such file"));
    let text = through(&pipeline, failure).await;
    assert!(
        text.contains("no such file") && text.contains("check the path"),
        "{text}"
    );
    assert_eq!(recorded(&ok).len(), 1, "post_tool_use did not fire again");
    let event = &recorded(&bad)[0];
    assert_eq!(event["hook_event_name"], "PostToolUseFailure");
    assert!(event["error"].as_str().unwrap().contains("no such file"));
}

// ---- handler types ----

fn http_hook(url: &str) -> Hook {
    Hook {
        kind: HandlerKind::Http,
        command: None,
        url: Some(url.to_string()),
        timeout_secs: Some(5),
        ..Hook::default()
    }
}

fn http_deps(warnings: &Arc<Warnings>) -> HookDeps {
    HookDeps {
        http: Some(reqwest::Client::new()),
        env: Arc::new(|name| match name {
            "ALLOWED" => Some("sekret".into()),
            "OTHER" => Some("leak".into()),
            _ => None,
        }),
        ..deps(warnings)
    }
}

#[tokio::test]
async fn an_http_hook_posts_the_event_with_interpolated_headers_and_can_block() {
    use wiremock::matchers::{header, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header("x-token", "Bearer sekret"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"decision": "block", "reason": "the server said no"})),
        )
        .mount(&server)
        .await;
    let f = Fixture::new();
    let deps = http_deps(&f.warnings);
    let mut h = http_hook(&server.uri());
    h.headers = Some(BTreeMap::from([
        ("x-token".to_string(), "Bearer $ALLOWED".to_string()),
        ("x-leak".to_string(), "${OTHER}".to_string()),
    ]));
    h.allowed_env_vars = Some(vec!["ALLOWED".into()]);
    let lifecycle = LifecycleHooks::new(
        BTreeMap::from([(HookEvent::Stop, vec![user_hook(HookEvent::Stop, &h, &deps)])]),
        f.rig.state.clone(),
        f.warnings.clone(),
    );
    let note = lifecycle.stop("done").await.expect("the server blocks");
    assert!(note.contains("the server said no"), "{note}");
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(
        request
            .headers
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap(),
        "application/json"
    );
    assert_eq!(
        request.headers.get("x-leak").unwrap().to_str().unwrap(),
        "",
        "a variable the hook does not list expands to nothing"
    );
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["hook_event_name"], "Stop");
    assert_eq!(body["last_assistant_message"], "done");
}

#[tokio::test]
async fn an_http_hook_that_fails_or_answers_oddly_decides_nothing() {
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};
    for (response, warns) in [
        (ResponseTemplate::new(500), true),
        (ResponseTemplate::new(200).set_body_string("not json"), true),
        (ResponseTemplate::new(200).set_body_string("[1]"), true),
        (ResponseTemplate::new(200), false),
        (ResponseTemplate::new(200).set_body_json(json!({})), false),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(response)
            .mount(&server)
            .await;
        let f = Fixture::new();
        let deps = http_deps(&f.warnings);
        let lifecycle = LifecycleHooks::new(
            BTreeMap::from([(
                HookEvent::Stop,
                vec![user_hook(HookEvent::Stop, &http_hook(&server.uri()), &deps)],
            )]),
            f.rig.state.clone(),
            f.warnings.clone(),
        );
        assert_eq!(lifecycle.stop("done").await, None);
        assert_eq!(
            !f.warnings.all().is_empty(),
            warns,
            "{:?}",
            f.warnings.all()
        );
    }
}

#[tokio::test]
async fn an_http_pre_tool_use_deny_denies_the_call_and_a_500_decides_nothing() {
    use common::pipeline::{allowed, denied_message};
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let denying = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": "http says no"
            }
        })))
        .mount(&denying)
        .await;
    let failing = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&failing)
        .await;
    let warnings = Arc::new(Warnings::default());
    let deps = http_deps(&warnings);

    let mut options = Options::new(Manual);
    options.hooks = vec![Arc::new(user_hook(
        HookEvent::PreToolUse,
        &http_hook(&denying.uri()),
        &deps,
    ))];
    let rig = Rig::with(options);
    std::fs::write(rig.work().join("a.txt"), "a").unwrap();
    let decision = rig.check("read", json!({"path": "a.txt"})).await;
    assert!(!allowed(&decision));
    assert!(denied_message(&decision).contains("http says no"));

    let mut options = Options::new(Manual);
    options.hooks = vec![Arc::new(user_hook(
        HookEvent::PreToolUse,
        &http_hook(&failing.uri()),
        &deps,
    ))];
    let rig = Rig::with(options);
    std::fs::write(rig.work().join("a.txt"), "a").unwrap();
    let decision = rig.check("read", json!({"path": "a.txt"})).await;
    assert!(allowed(&decision), "a failing hook decides nothing");
}

fn prompt_hook(prompt: &str) -> Hook {
    Hook {
        kind: HandlerKind::Prompt,
        command: None,
        prompt: Some(prompt.to_string()),
        timeout_secs: Some(5),
        ..Hook::default()
    }
}

fn verdict(ok: bool, reason: &str, impossible: bool) -> MockTurn {
    MockTurn::tool_call(
        "s1",
        "submit",
        json!({"ok": ok, "reason": reason, "impossible": impossible}),
    )
}

fn prompt_deps(
    warnings: &Arc<Warnings>,
    mock: &MockCompletionModel,
    spent: Arc<Mutex<u64>>,
) -> HookDeps {
    let handle = ModelHandle::new(
        ModelRef::parse("test/cheap").unwrap(),
        fontaine_core::config::AccountKind::named("openai-compatible"),
        mock.clone().into(),
    );
    HookDeps {
        guardian: Arc::new(RwLock::new(Some(GuardianModel::Ready(handle)))),
        usage: Some(Arc::new(move |_agent, usage, _answered| {
            *spent.lock().unwrap() += usage.total_tokens.unwrap_or(0);
        })),
        ..deps(warnings)
    }
}

#[tokio::test]
async fn a_prompt_hook_blocks_a_stop_with_the_models_reason_and_counts_its_usage() {
    let mut usage = rig_core::completion::Usage::new();
    usage.total_tokens = Some(42);
    let mock = MockCompletionModel::from_turns(vec![
        verdict(false, "tests were not run", false).with_usage(usage),
    ]);
    let f = Fixture::new();
    let spent = Arc::new(Mutex::new(0));
    let deps = prompt_deps(&f.warnings, &mock, spent.clone());
    let lifecycle = LifecycleHooks::new(
        BTreeMap::from([(
            HookEvent::Stop,
            vec![user_hook(
                HookEvent::Stop,
                &prompt_hook("Were the tests run? $ARGUMENTS"),
                &deps,
            )],
        )]),
        f.rig.state.clone(),
        f.warnings.clone(),
    );
    let note = lifecycle.stop("all done").await.expect("the model blocks");
    assert!(note.contains("tests were not run"), "{note}");
    assert_eq!(*spent.lock().unwrap(), 42);
    let request = serde_json::to_string(&mock.requests()[0]).unwrap();
    assert!(request.contains("Were the tests run?"), "{request}");
    assert!(request.contains("hook_input"), "{request}");
    assert!(request.contains("all done"), "{request}");
    assert!(!request.contains("$ARGUMENTS"), "{request}");
}

#[tokio::test]
async fn a_prompt_hook_can_say_it_is_impossible_and_a_model_error_decides_nothing() {
    let f = Fixture::new();
    let spent = Arc::new(Mutex::new(0));
    let impossible = MockCompletionModel::from_turns(vec![verdict(false, "cannot be met", true)]);
    let deps = prompt_deps(&f.warnings, &impossible, spent.clone());
    let lifecycle = LifecycleHooks::new(
        BTreeMap::from([(
            HookEvent::Stop,
            vec![user_hook(HookEvent::Stop, &prompt_hook("ok?"), &deps)],
        )]),
        f.rig.state.clone(),
        f.warnings.clone(),
    );
    assert_eq!(
        lifecycle.stop("done").await,
        None,
        "impossible lets it stop"
    );

    let failing = MockCompletionModel::from_turns(vec![MockTurn::error("provider down"); 4]);
    let deps = prompt_deps(&f.warnings, &failing, spent);
    let lifecycle = LifecycleHooks::new(
        BTreeMap::from([(
            HookEvent::Stop,
            vec![user_hook(HookEvent::Stop, &prompt_hook("ok?"), &deps)],
        )]),
        f.rig.state.clone(),
        f.warnings.clone(),
    );
    assert_eq!(lifecycle.stop("done").await, None);
    assert!(
        f.warnings
            .all()
            .iter()
            .any(|w| w.contains("could not be reached"))
    );
}

#[tokio::test]
async fn a_prompt_hook_denies_a_pre_tool_use_call_and_without_a_model_decides_nothing() {
    use common::pipeline::{allowed, denied_message};
    let f = Fixture::new();
    let mock = MockCompletionModel::from_turns(vec![verdict(false, "looks risky", false)]);
    let deps = prompt_deps(&f.warnings, &mock, Arc::default());
    let mut options = Options::new(Manual);
    options.hooks = vec![Arc::new(user_hook(
        HookEvent::PreToolUse,
        &prompt_hook("is this fine? $ARGUMENTS"),
        &deps,
    ))];
    let rig = Rig::with(options);
    std::fs::write(rig.work().join("a.txt"), "a").unwrap();
    let decision = rig.check("read", json!({"path": "a.txt"})).await;
    assert!(!allowed(&decision));
    assert!(denied_message(&decision).contains("looks risky"));

    let no_model = HookDeps {
        guardian: Arc::new(RwLock::new(None)),
        ..deps
    };
    let mut options = Options::new(Manual);
    options.hooks = vec![Arc::new(user_hook(
        HookEvent::PreToolUse,
        &prompt_hook("is this fine?"),
        &no_model,
    ))];
    let rig = Rig::with(options);
    std::fs::write(rig.work().join("a.txt"), "a").unwrap();
    assert!(allowed(&rig.check("read", json!({"path": "a.txt"})).await));
}

// ---- trust: what `build` keeps ----

fn config_with(w: &common::World, user: &str, project: &str) -> fontaine_core::config::Config {
    w.user_file(user);
    if !project.is_empty() {
        w.project_file(project);
    }
    w.load().unwrap()
}

const HTTP: &str = "[[hooks.session_end]]\ntype = \"http\"\nurl = \"https://example.test/h\"\n";
const PROMPT: &str = "[[hooks.user_prompt_submit]]\ntype = \"prompt\"\nprompt = \"fine?\"\n";
const STOP: &str =
    "[[hooks.stop]]\ncommand = \"true\"\n[[hooks.subagent_stop]]\ncommand = \"true\"\n";
const PLAIN: &str = "[[hooks.session_start]]\ncommand = \"true\"\n";

#[test]
fn build_ignores_a_repositorys_http_prompt_and_forcing_hooks_with_a_count() {
    let w = common::World::new();
    let config = config_with(&w, "", &format!("{HTTP}{PROMPT}{STOP}{PLAIN}"));
    let built = user_hooks::build(&config, true, &HookDeps::new(process())).unwrap();
    assert_eq!(built.ignored_project_handlers, 2);
    assert_eq!(
        built.ignored_project,
        [(HookEvent::Stop, 1), (HookEvent::SubagentStop, 1)]
    );
    assert_eq!(built.lifecycle.len(), 1, "the plain command hook loads");
    assert!(built.lifecycle.contains_key(&HookEvent::SessionStart));
    let warnings = built.warnings(true);
    assert_eq!(warnings.len(), 3, "{warnings:?}");
    assert!(warnings.iter().any(|w| w.contains("stop hook(s)")));
    assert!(warnings.iter().any(|w| w.contains("http or prompt")));
}

#[test]
fn build_drops_every_repository_hook_when_the_workspace_is_not_trusted() {
    let w = common::World::new();
    let config = config_with(&w, "", &format!("{HTTP}{STOP}{PLAIN}"));
    let built = user_hooks::build(&config, false, &HookDeps::new(process())).unwrap();
    assert!(built.lifecycle.is_empty());
    assert!(built.warnings(true).is_empty());
}

#[test]
fn build_keeps_user_and_plugin_http_and_prompt_hooks() {
    let w = common::World::new();
    let mut config = config_with(&w, &format!("{HTTP}{PROMPT}{STOP}"), "");
    let mut plugin = fontaine_core::plugins::PluginContributions::default();
    plugin.hooks.list_mut(HookEvent::Stop).push(Hook {
        kind: HandlerKind::Prompt,
        command: None,
        prompt: Some("plugin check".into()),
        origin: Origin {
            slot: Slot::Plugin,
            user_trust: false,
        },
        plugin: Some("p".into()),
        ..Hook::default()
    });
    config.apply_plugins(&plugin).unwrap();
    let built = user_hooks::build(&config, true, &HookDeps::new(process())).unwrap();
    assert_eq!(built.ignored_project_handlers, 0);
    assert!(built.ignored_project.is_empty());
    assert_eq!(built.lifecycle[&HookEvent::Stop].len(), 2);
    assert_eq!(built.lifecycle[&HookEvent::SessionEnd].len(), 1);
    assert_eq!(built.lifecycle[&HookEvent::UserPromptSubmit].len(), 1);
    assert_eq!(built.subagent_stop.len(), 1);
}

#[test]
fn the_frozen_and_plugin_layers_split_the_hooks_by_origin() {
    let w = common::World::new();
    let mut config = config_with(&w, &format!("{HTTP}{STOP}"), "");
    let mut plugin = fontaine_core::plugins::PluginContributions::default();
    plugin.hooks.list_mut(HookEvent::Stop).push(Hook {
        kind: HandlerKind::Prompt,
        command: None,
        prompt: Some("plugin check".into()),
        origin: Origin {
            slot: Slot::Plugin,
            user_trust: false,
        },
        plugin: Some("p".into()),
        ..Hook::default()
    });
    config.apply_plugins(&plugin).unwrap();
    let deps = HookDeps::new(process());
    let frozen =
        user_hooks::build_layer(&config, true, &deps, user_hooks::HookLayer::Frozen).unwrap();
    let layer =
        user_hooks::build_layer(&config, true, &deps, user_hooks::HookLayer::Plugin).unwrap();
    assert_eq!(frozen.lifecycle[&HookEvent::Stop].len(), 1);
    assert_eq!(layer.lifecycle[&HookEvent::Stop].len(), 1);
    assert!(
        !layer.lifecycle.contains_key(&HookEvent::SessionEnd),
        "the user's hooks are not in the plugin layer"
    );
    assert!(frozen.lifecycle.contains_key(&HookEvent::SessionEnd));
}

#[test]
fn an_allowed_env_var_the_shell_scrub_removes_is_a_startup_error() {
    let w = common::World::new();
    let config = config_with(
        &w,
        "[[hooks.session_end]]\ntype = \"http\"\nurl = \"https://example.test/h\"\nallowed_env_vars = [\"SECRET_KEY\"]\n",
        "",
    );
    let process = Arc::new(HookProcess {
        scrub_env: vec!["SECRET_KEY".into()],
        ..(*process()).clone()
    });
    let error = user_hooks::build(&config, true, &HookDeps::new(process))
        .err()
        .expect("the variable is scrubbed");
    let text = error.to_string();
    assert!(
        text.contains("SECRET_KEY") && text.contains("hooks.session_end"),
        "{text}"
    );
    // A variable the scrub leaves alone is fine.
    let ok = user_hooks::build(&config, true, &HookDeps::new(self::process()));
    assert!(ok.is_ok());
}

// ---- the runtime: compaction and /clear ----

use fontaine_core::models::{GuardianChoice, Models};
use fontaine_core::session::CompactResult;
use rig_core::catalog::Catalog;
use rig_core::completion::Message;
use rig_memory::CompactReason;

fn compacting_runtime(
    world: &common::World,
    approver: Arc<dyn Approver>,
    fill: bool,
) -> (fontaine_core::agent::AgentRuntime, MockCompletionModel) {
    let mut spec = (*Catalog::builtin()
        .resolve("openai/gpt-4o-mini")
        .unwrap()
        .spec)
        .clone();
    spec.context_window = Some(1_000);
    let main = MockCompletionModel::from_turns(vec![MockTurn::text("SUMMARY"); 4]).priced_as(spec);
    let main_handle = ModelHandle::new(
        ModelRef::parse("test/main").unwrap(),
        fontaine_core::config::AccountKind::named("openai-compatible"),
        main.clone().into(),
    );
    let models = Models {
        main_chain: fontaine_core::model_roles::ModelChain::single(main_handle.reference.clone()),
        main: main_handle,
        main_listing: fontaine_core::models::Listing::default(),
        guardian: None,
        guardian_choice: GuardianChoice::Pinned,
        notices: Vec::new(),
    };
    let runtime = common::runtime::build_with_approver(
        world,
        Manual,
        models,
        None,
        false,
        fontaine_core::scanner_stage::ScannerStart::default(),
        None,
        approver,
    );
    let mut conversation = rig_memory::load(
        &runtime.session.store(),
        fontaine_core::session::CONVERSATION_LOG,
    )
    .unwrap();
    for i in (0..8).filter(|_| fill) {
        conversation.extend([
            Message::user(format!("question {i}: {}", "lorem ipsum ".repeat(100))),
            Message::assistant(format!("answer {i}: {}", "dolor sit ".repeat(100))),
        ]);
    }
    rig_memory::save(
        &runtime.session.store(),
        fontaine_core::session::CONVERSATION_LOG,
        &mut conversation,
    )
    .unwrap();
    (runtime, main)
}

fn hook_toml(event: &str, extra: &str, args: &[&str]) -> String {
    format!(
        "[[hooks.{event}]]\n{extra}command = '''\n{}\n'''\n",
        command(args)
    )
}

#[tokio::test]
async fn a_compaction_runs_its_hooks_and_a_session_start_for_compact() {
    let dir = tempfile::tempdir().unwrap();
    let (pre, post, start) = (
        path_arg(&dir, "pre.jsonl"),
        path_arg(&dir, "post.jsonl"),
        path_arg(&dir, "start.jsonl"),
    );
    let world = common::World::new();
    world.user_file(&format!(
        "{}{}{}",
        hook_toml(
            "pre_compact",
            "",
            &["--append", &pre, "--reply", "keep the API names"]
        ),
        hook_toml("post_compact", "", &["--append", &post]),
        hook_toml(
            "session_start",
            "matcher = \"compact\"\n",
            &["--append", &start, "--reply", "reread the plan"]
        ),
    ));
    let warnings = Arc::new(Warnings::default());
    let (runtime, main) = compacting_runtime(&world, warnings.clone(), true);
    let result = runtime
        .compact_with(CompactReason::Asked {
            focus: "the parser".into(),
        })
        .await;
    assert!(
        matches!(result, CompactResult::Compacted { .. }),
        "{result:?}"
    );
    let request = serde_json::to_string(&main.requests()[0]).unwrap();
    assert!(
        request.contains("Additional focus: the parser"),
        "{request}"
    );
    assert!(
        request.contains("Additional focus: keep the API names"),
        "{request}"
    );
    let pre = &recorded(&pre)[0];
    assert_eq!(pre["trigger"], "manual");
    assert_eq!(pre["custom_instructions"], "the parser");
    let post = &recorded(&post)[0];
    assert_eq!(post["compact_summary"], "SUMMARY");
    assert_eq!(recorded(&start)[0]["source"], "compact");
    assert_eq!(
        runtime.take_hook_context(),
        ["[Hook context from session_start, not the user's words: reread the plan]"]
    );
}

#[tokio::test]
async fn a_blocking_pre_compact_hook_skips_the_compaction_with_a_warning() {
    let world = common::World::new();
    world.user_file(&hook_toml(
        "pre_compact",
        "",
        &["--exit", "2", "--stderr", "not while I work"],
    ));
    let warnings = Arc::new(Warnings::default());
    let (runtime, main) = compacting_runtime(&world, warnings.clone(), true);
    let result = runtime
        .compact_with(CompactReason::Asked {
            focus: String::new(),
        })
        .await;
    assert_eq!(result, CompactResult::Skipped);
    assert_eq!(main.request_count(), 0, "the model was never asked");
    assert!(
        warnings
            .all()
            .iter()
            .any(|w| w.contains("not while I work")),
        "{:?}",
        warnings.all()
    );
}

#[tokio::test]
async fn a_threshold_check_that_compacts_nothing_fires_no_pre_compact_hook() {
    let dir = tempfile::tempdir().unwrap();
    let pre = path_arg(&dir, "pre.jsonl");
    let world = common::World::new();
    world.user_file(&hook_toml("pre_compact", "", &["--append", &pre]));
    // Nothing to summarize: no model call, so no hook.
    let (runtime, main) = compacting_runtime(&world, Arc::new(Warnings::default()), false);
    assert_eq!(runtime.compact_if_needed().await, CompactResult::Skipped);
    assert_eq!(main.request_count(), 0);
    assert!(recorded(&pre).is_empty());
}

#[tokio::test]
async fn clear_ends_the_session_for_the_hooks_and_starts_it_again() {
    let dir = tempfile::tempdir().unwrap();
    let (end, start) = (path_arg(&dir, "end.jsonl"), path_arg(&dir, "start.jsonl"));
    let world = common::World::new();
    world.user_file(&format!(
        "{}{}",
        hook_toml("session_end", "", &["--append", &end]),
        hook_toml(
            "session_start",
            "",
            &["--append", &start, "--reply", "welcome back"]
        ),
    ));
    let (runtime, _) = compacting_runtime(&world, Arc::new(Warnings::default()), true);
    runtime.state.queue_hook_context("stale".into());
    runtime.clear_conversation().await.unwrap();
    assert_eq!(recorded(&end)[0]["reason"], "clear");
    assert_eq!(recorded(&start)[0]["source"], "clear");
    assert_eq!(
        runtime.take_hook_context(),
        ["[Hook context from session_start, not the user's words: welcome back]"],
        "context queued for the cleared conversation is gone"
    );
}

#[tokio::test]
async fn an_abandoned_prompt_gate_drops_only_its_own_note() {
    let f = Fixture::new();
    f.rig.state.queue_hook_context("session".into());
    f.rig.state.set_prompt_context(Some("gate".into()));
    f.rig.state.clear_prompt_context();
    assert_eq!(f.rig.state.take_hook_context(), ["session"]);
    f.rig.state.set_prompt_context(Some("old".into()));
    f.rig.state.set_prompt_context(None);
    assert!(
        f.rig.state.take_hook_context().is_empty(),
        "a new gate replaces the old note"
    );
}

#[tokio::test]
async fn impossible_does_not_cancel_a_block_outside_stop_events() {
    let f = Fixture::new();
    let mock = MockCompletionModel::from_turns(vec![verdict(false, "not allowed", true)]);
    let deps = prompt_deps(&f.warnings, &mock, Arc::new(Mutex::new(0)));
    let lifecycle = LifecycleHooks::new(
        BTreeMap::from([(
            HookEvent::UserPromptSubmit,
            vec![user_hook(
                HookEvent::UserPromptSubmit,
                &prompt_hook("ok?"),
                &deps,
            )],
        )]),
        f.rig.state.clone(),
        f.warnings.clone(),
    );
    assert!(matches!(
        lifecycle.prompt_submit("go").await,
        PromptGate::Blocked(_)
    ));
}

// ---- the plugin layer: what a plugin reload swaps ----

fn live_layer(
    event: HookEvent,
    hook: &Hook,
    deps: &HookDeps,
) -> Arc<fontaine_tools::live::Live<fontaine_core::user_hooks::PluginLayer>> {
    let mut layer = fontaine_core::user_hooks::PluginLayer::default();
    let user = user_hook(event, hook, deps);
    match event {
        HookEvent::SubagentStart => layer.subagent_start.push(user),
        other => layer.lifecycle.entry(other).or_default().push(user),
    }
    Arc::new(fontaine_tools::live::Live::new(layer))
}

#[tokio::test]
async fn plugin_lifecycle_hooks_run_until_the_layer_is_emptied() {
    let dir = tempfile::tempdir().unwrap();
    let seen = path_arg(&dir, "seen.jsonl");
    let f = Fixture::new();
    let deps = deps(&f.warnings);
    let live = live_layer(HookEvent::SessionEnd, &hook(&["--append", &seen]), &deps);
    let lifecycle = f.lifecycle(Vec::new()).with_plugins(live.clone());
    assert!(lifecycle.has(HookEvent::SessionEnd));
    lifecycle.session_end("exit").await;
    assert_eq!(recorded(&seen).len(), 1);

    live.set(Arc::default());
    assert!(!lifecycle.has(HookEvent::SessionEnd));
    lifecycle.session_end("exit").await;
    assert_eq!(
        recorded(&seen).len(),
        1,
        "an unloaded plugin's hook is silent"
    );
}

#[tokio::test]
async fn plugin_subagent_hooks_follow_the_layer_and_the_gate_exists_without_hooks() {
    let f = Fixture::new();
    let deps = deps(&f.warnings);
    let hook = hook(&[
        "--reply",
        &reply("SubagentStart", "additionalContext", "from the plugin"),
    ]);
    let live = live_layer(HookEvent::SubagentStart, &hook, &deps);
    let gate = SubagentGate::new(
        Vec::new(),
        Vec::new(),
        f.rig.state.clone(),
        f.warnings.clone(),
    )
    .with_plugins(live.clone());
    assert_eq!(
        gate.subagent_start("agent-1", "explore").await,
        ["[Hook context from subagent_start, not the user's words: from the plugin]"]
    );
    live.set(Arc::default());
    assert!(gate.subagent_start("agent-1", "explore").await.is_empty());
}

// ---- the final exit ----

#[tokio::test]
async fn the_final_shutdown_fires_session_end_once_and_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let ended = path_arg(&dir, "ended.jsonl");
    let world = common::World::new();
    world.user_file(&hook_toml("session_end", "", &["--append", &ended]));
    let (runtime, _) = compacting_runtime(&world, Arc::new(Warnings::default()), false);

    runtime.begin_final_shutdown();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    runtime.finish_final_shutdown(deadline).await;
    runtime.finish_final_shutdown(deadline).await;

    let events = recorded(&ended);
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["reason"], "prompt_input_exit");
    assert!(runtime.state.agents().shutting_down());
}

#[tokio::test]
async fn a_slow_session_end_hook_is_cut_off_at_the_shared_deadline() {
    let world = common::World::new();
    world.user_file(&hook_toml(
        "session_end",
        "timeout_secs = 30\n",
        &["--sleep", "20"],
    ));
    let (runtime, _) = compacting_runtime(&world, Arc::new(Warnings::default()), false);
    let started = std::time::Instant::now();
    runtime
        .finish_final_shutdown(started + Duration::from_millis(600))
        .await;
    let took = started.elapsed();
    assert!(took >= Duration::from_millis(500), "{took:?}");
    assert!(took < Duration::from_secs(4), "{took:?}");
}
