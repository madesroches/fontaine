//! `fontaine_sdk::SessionBuilder` against scripted endpoints: the embedded run, its approvals, its
//! turns and its end, and that its event stream is the one `fontaine -p` prints.

mod common;

use std::ffi::OsString;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{Endpoint, Route, Sandbox, Turn, Wire};
use fontaine_sdk::core::permissions::{Answer, ApprovalRequest};
use fontaine_sdk::{
    Approver, BoxFuture, EmbedError, Event, HostTool, PermissionMode, Session, SessionBuilder,
    ToolExecutionError,
};
use fontaine_testkit::example_bin;
use futures::StreamExt;
use serde_json::{Value, json};

/// Looks variables up in the sandbox's own environment first, then in the process's.
fn lookup(s: &Sandbox) -> fontaine_sdk::EnvLookup {
    let runtime = s.runtime.clone().into_os_string();
    let (config, data) = (
        s.config.clone().into_os_string(),
        s.data.clone().into_os_string(),
    );
    Arc::new(move |name: &str| -> Option<OsString> {
        match name {
            "FONTAINE_CONFIG_DIR" => Some(config.clone()),
            "FONTAINE_DATA_DIR" => Some(data.clone()),
            "XDG_RUNTIME_DIR" | "TMP" | "TEMP" => Some(runtime.clone()),
            "PAID_KEY" => Some(OsString::from("sk-test-not-a-real-key")),
            "FONTAINE_LOG" | "MICROMEGAS_TELEMETRY_URL" | "OTEL_EXPORTER_OTLP_ENDPOINT" => None,
            other => std::env::var_os(other),
        }
    })
}

fn builder(s: &Sandbox) -> SessionBuilder {
    SessionBuilder::new(&s.work)
        .dirs(s.config.clone(), s.data.clone())
        .env(lookup(s))
}

#[derive(Default)]
struct Recorder {
    asked: Mutex<Vec<ApprovalRequest>>,
    warnings: Mutex<Vec<String>>,
    answer: Mutex<Option<Answer>>,
}

impl Recorder {
    fn allowing() -> Arc<Recorder> {
        let recorder = Recorder::default();
        *recorder.answer.lock().unwrap() = Some(Answer::AllowOnce);
        Arc::new(recorder)
    }

    fn asked(&self) -> Vec<ApprovalRequest> {
        self.asked.lock().unwrap().clone()
    }

    fn warnings(&self) -> Vec<String> {
        self.warnings.lock().unwrap().clone()
    }
}

impl Approver for Recorder {
    fn approve<'a>(&'a self, request: ApprovalRequest) -> BoxFuture<'a, Answer> {
        self.asked.lock().unwrap().push(request);
        let answer = self
            .answer
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(Answer::Deny { feedback: None });
        Box::pin(async move { answer })
    }

    fn warn(&self, message: &str) {
        self.warnings.lock().unwrap().push(message.to_string());
    }
}

struct Echo {
    calls: Arc<Mutex<Vec<Value>>>,
}

impl HostTool for Echo {
    fn name(&self) -> &str {
        "host_echo"
    }

    fn description(&self) -> &str {
        "Echoes its arguments back."
    }

    fn parameters(&self) -> Value {
        json!({"type": "object"})
    }

    fn call<'a>(&'a self, args: Value) -> BoxFuture<'a, Result<String, ToolExecutionError>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(args.clone());
            Ok(format!("echoed {args}"))
        })
    }
}

fn json_of(event: &Event) -> Value {
    serde_json::to_value(event).unwrap()
}

/// Every event of one prompt, up to and including its `result`.
async fn run_turn(session: &mut Session, prompt: &str) -> Vec<Value> {
    let mut turn = session.send(prompt);
    let mut events = Vec::new();
    while let Some(event) = turn.next().await {
        events.push(json_of(&event.expect("the run is alive")));
    }
    events
}

fn kinds(events: &[Value]) -> Vec<&str> {
    events.iter().map(|e| e["type"].as_str().unwrap()).collect()
}

fn result_of(events: &[Value]) -> &Value {
    events.iter().rev().find(|e| e["type"] == "result").unwrap()
}

fn host_call_script() -> Vec<Turn> {
    vec![
        Turn::tool("c1", "host_echo", json!({"say": "hi"})),
        Turn::text("done"),
    ]
}

#[tokio::test(flavor = "multi_thread")]
async fn an_approver_makes_the_channel_delegate_and_is_asked_where_a_person_would_be() {
    let endpoint = Endpoint::start(Wire::Chat, host_call_script()).await;
    let s = Sandbox::compatible(&endpoint.url());
    let approver = Recorder::allowing();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut session = builder(&s)
        .mode(PermissionMode::Manual)
        .approver(approver.clone())
        .tool(Echo {
            calls: calls.clone(),
        })
        .build()
        .await
        .expect("builds");
    let id = session.id().to_string();
    let events = run_turn(&mut session, "call it").await;
    session.close().await.expect("closes");

    let asked = approver.asked();
    assert_eq!(asked.len(), 1, "{events:?}");
    assert_eq!(asked[0].tool, "host_echo");
    assert!(!asked[0].offers.always, "a host is never offered always");
    assert_eq!(calls.lock().unwrap().as_slice(), [json!({"say": "hi"})]);
    let result = events
        .iter()
        .find(|e| e["type"] == "tool_result")
        .expect("the call ran");
    assert_eq!(result["success"], json!(true));

    let audit = std::fs::read_to_string(s.data.join("audit").join(format!("{id}.jsonl"))).unwrap();
    assert!(audit.contains("\"human_channel\":\"delegate\""), "{audit}");
}

#[tokio::test(flavor = "multi_thread")]
async fn without_an_approver_a_prompt_is_a_denial_and_the_tool_never_runs() {
    let endpoint = Endpoint::start(Wire::Chat, host_call_script()).await;
    let s = Sandbox::compatible(&endpoint.url());
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut session = builder(&s)
        .tool(Echo {
            calls: calls.clone(),
        })
        .build()
        .await
        .expect("builds");
    let id = session.id().to_string();
    let events = run_turn(&mut session, "call it").await;
    session.close().await.expect("closes");
    assert!(calls.lock().unwrap().is_empty());
    let result = events.iter().find(|e| e["type"] == "tool_result").unwrap();
    assert_eq!(result["success"], json!(false));
    assert!(result["text"].as_str().unwrap().contains("denied"));
    let audit = std::fs::read_to_string(s.data.join("audit").join(format!("{id}.jsonl"))).unwrap();
    assert!(audit.contains("\"human_channel\":\"absent\""), "{audit}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_tool_call_is_judged_by_the_guardian_in_auto() {
    let agent = Endpoint::start(Wire::Chat, host_call_script()).await;
    let guardian = Endpoint::start(
        Wire::Chat,
        vec![Turn::tool(
            "g1",
            "submit",
            json!({
                "decision": "allow",
                "rule": "allow",
                "explanation": "harmless",
                "confidence": 0.95,
            }),
        )],
    )
    .await;
    let s = Sandbox::new();
    s.user_config(&format!(
        "[accounts.agent]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\nmodels = [\"scripted\"]\n\n[accounts.judge]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\nmodels = [\"scripted\"]\n\n[model]\nmodel = \"agent/scripted\"\n\n[guardian]\nmodel = \"judge/scripted\"\n",
        agent.url(),
        guardian.url()
    ));
    let approver = Recorder::allowing();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut session = builder(&s)
        .mode(PermissionMode::Auto)
        .approver(approver.clone())
        .tool(Echo {
            calls: calls.clone(),
        })
        .build()
        .await
        .expect("builds");
    let events = run_turn(&mut session, "call it").await;
    session.close().await.expect("closes");
    let decision = events
        .iter()
        .find(|e| e["type"] == "gate_decision" && e["tool"] == "host_echo")
        .expect("a decision");
    assert_eq!(decision["tier"], "guardian");
    assert_eq!(decision["decision"], "allow");
    assert!(approver.asked().is_empty(), "the guardian decided");
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert_eq!(guardian.requests().await.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn assembly_warnings_reach_the_approvers_warn() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("hi")]).await;
    let s = Sandbox::compatible(&endpoint.url());
    let approver = Recorder::allowing();
    let session = builder(&s)
        .approver(approver.clone())
        .add_dir(s.work.join("no-such-directory"))
        .build()
        .await
        .expect("builds");
    session.close().await.expect("closes");
    let warnings = approver.warnings();
    assert!(
        warnings.iter().any(|w| w.contains("no-such-directory")),
        "{warnings:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn init_is_the_first_item_of_the_first_turn_only_and_a_dropped_turn_leaves_the_next_clean() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("one"), Turn::text("two")]).await;
    let s = Sandbox::compatible(&endpoint.url());
    let mut session = builder(&s).build().await.expect("builds");

    {
        let mut turn = session.send("first");
        let first = turn.next().await.unwrap().unwrap();
        assert_eq!(first.kind(), "init");
        // Dropped with the rest of its events, and its `result`, still to come.
    }
    let second = run_turn(&mut session, "second").await;
    assert_ne!(second[0]["type"], "init", "{:?}", kinds(&second));
    assert_ne!(second[0]["type"], "result", "{:?}", kinds(&second));
    assert_eq!(kinds(&second).last(), Some(&"result"));
    assert_eq!(result_of(&second)["result"], "two");
    assert_eq!(kinds(&second).iter().filter(|k| **k == "result").count(), 1);
    session.close().await.expect("closes");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_afk_runs_report_comes_from_close() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("hello")]).await;
    let s = Sandbox::compatible(&endpoint.url());
    let mut session = builder(&s)
        .mode(PermissionMode::Afk)
        .build()
        .await
        .expect("builds");
    let events = run_turn(&mut session, "hi").await;
    assert!(
        !kinds(&events).contains(&"afk_report"),
        "the trailing report is not on a turn: {:?}",
        kinds(&events)
    );
    assert_eq!(result_of(&events)["afk"]["outcome"], "ended");
    let report = session.close().await.expect("closes");
    assert!(report.is_some(), "an AFK run ends with its report");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_send_after_the_run_ended_is_a_runtime_error_and_close_reports_the_exit() {
    let window = r#"{"error":{"message":"You've reached your 5-hour usage limit.","type":"exceeded_current_quota_error"}}"#;
    let endpoint = Endpoint::start(
        Wire::Chat,
        (0..5)
            .map(|_| Turn::Reply(429, window.to_string()))
            .collect(),
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    let config = std::fs::read_to_string(s.config.join("config.toml")).unwrap();
    s.user_config(&format!(
        "{config}\n[headless]\non_usage_limit = \"exit\"\n"
    ));
    let mut session = builder(&s).build().await.expect("builds");
    let first = run_turn(&mut session, "hi").await;
    assert_eq!(result_of(&first)["subtype"], "error_usage_limit");

    let mut turn = session.send("again");
    match turn.next().await {
        Some(Err(EmbedError::Runtime(_))) => {}
        other => panic!("expected a runtime error, got {other:?}"),
    }
    assert!(turn.next().await.is_none());
    drop(turn);
    match session.close().await {
        Err(EmbedError::Runtime(_)) => {}
        other => panic!("expected a runtime error from close, got {other:?}"),
    }
}

fn hook_config(s: &Sandbox, ends: &std::path::Path) {
    let config = std::fs::read_to_string(s.config.join("config.toml")).unwrap();
    s.user_config(&format!(
        "{config}\n[[hooks.session_end]]\ncommand = '''\n'{}' --append '{}'\n'''\n",
        example_bin("fontaine", "hook_marker").to_string_lossy(),
        ends.display()
    ));
}

async fn gone(path: &std::path::Path) -> bool {
    for _ in 0..100 {
        if !path.exists() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

#[tokio::test(flavor = "multi_thread")]
async fn close_fires_session_end_and_removes_the_scratch_directory() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("hi")]).await;
    let s = Sandbox::compatible(&endpoint.url());
    let ends = s.work.join("ends.jsonl");
    hook_config(&s, &ends);
    let mut session = builder(&s).build().await.expect("builds");
    let scratch = s.scratch_parent().join(session.id());
    assert!(scratch.is_dir());
    run_turn(&mut session, "hi").await;
    session.close().await.expect("closes");
    let recorded = std::fs::read_to_string(&ends).expect("the hook ran");
    assert!(recorded.contains("\"reason\":\"other\""), "{recorded}");
    assert!(!scratch.exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn dropping_a_session_cancels_the_turn_removes_the_scratch_and_fires_no_hook() {
    let endpoint = Endpoint::start_routed(
        Wire::Chat,
        vec![Route::new("slow", vec![Turn::text("late")]).slow(Duration::from_secs(60))],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    let ends = s.work.join("ends.jsonl");
    hook_config(&s, &ends);
    let mut session = builder(&s).build().await.expect("builds");
    let scratch = s.scratch_parent().join(session.id());
    assert!(scratch.is_dir());
    {
        let mut turn = session.send("slow");
        assert_eq!(turn.next().await.unwrap().unwrap().kind(), "init");
        // The request is in flight and will not answer for a minute.
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    let started = std::time::Instant::now();
    drop(session);
    assert!(gone(&scratch).await, "the scratch directory stayed");
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "the turn was cancelled"
    );
    assert!(!ends.exists(), "no SessionEnd hook fires without close()");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_interrupt_ends_the_turn_with_the_sdk_modes_result_and_the_next_turn_runs() {
    let endpoint = Endpoint::start_routed(
        Wire::Chat,
        vec![
            // The interrupted prompt stays in the next request, so the later marker goes first.
            Route::on_last_user("quick", vec![Turn::text("fast answer")]),
            Route::on_last_user("slow", vec![Turn::text("late")]).slow(Duration::from_secs(60)),
        ],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    let mut session = builder(&s).build().await.expect("builds");
    let handle = session.interrupt_handle();
    let interrupted = {
        let mut turn = session.send("slow");
        assert_eq!(turn.next().await.unwrap().unwrap().kind(), "init");
        let stopper = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(400)).await;
            handle.interrupt();
        });
        let mut events = Vec::new();
        while let Some(event) = turn.next().await {
            events.push(json_of(&event.unwrap()));
        }
        stopper.await.unwrap();
        events
    };
    let result = result_of(&interrupted);
    assert_eq!(result["subtype"], "error_during_execution");
    assert_eq!(result["is_error"], json!(true));
    assert_eq!(result["error"], "the turn was interrupted");

    let next = run_turn(&mut session, "quick").await;
    assert_eq!(result_of(&next)["subtype"], "success");
    assert_eq!(result_of(&next)["result"], "fast answer");
    session.close().await.expect("closes");
}

/// What differs between two runs of the same script and nothing else.
fn normalized(mut lines: Vec<Value>) -> Vec<Value> {
    for line in &mut lines {
        line["session_id"] = json!("S");
        if line.get("duration_ms").is_some() {
            line["duration_ms"] = json!(0);
        }
        if let Some(roots) = line
            .get_mut("workspace_roots")
            .and_then(Value::as_array_mut)
        {
            // The scratch directory is named after the session.
            if let Some(last) = roots.last_mut() {
                *last = json!("SCRATCH");
            }
        }
    }
    lines
}

struct Case {
    name: &'static str,
    turns: Vec<Turn>,
    cli: Vec<&'static str>,
    configure: fn(SessionBuilder) -> SessionBuilder,
}

async fn parity(case: Case, priced: bool) {
    let routes = vec![
        Route::new("CLI-RUN", case.turns.clone()),
        Route::new("EMB-RUN", case.turns.clone()),
    ];
    let endpoint =
        Endpoint::start_routed(if priced { Wire::Responses } else { Wire::Chat }, routes).await;
    let s = if priced {
        Sandbox::priced(&endpoint.url())
    } else {
        Sandbox::compatible(&endpoint.url())
    };
    let mut args = vec![
        "-p",
        "--permission-mode",
        "manual",
        "--output-format",
        "stream-json",
        "--include-partial-messages",
    ];
    args.extend(case.cli.iter().copied());
    args.push("CLI-RUN");
    let out = s.run(&args).await;
    let expected = normalized(out.lines());

    let mut session = (case.configure)(builder(&s).include_partial_messages(true))
        .mode(PermissionMode::Manual)
        .build()
        .await
        .expect("builds");
    let actual = normalized(run_turn(&mut session, "EMB-RUN").await);
    session.close().await.ok();

    assert_eq!(actual, expected, "{}: {}", case.name, out.stderr);
    assert!(!expected.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_embedded_run_and_fontaine_p_produce_the_same_events() {
    parity(
        Case {
            name: "plain",
            turns: vec![
                Turn::tool("c1", "write", json!({"path": "x.txt", "content": "x"})),
                Turn::text("could not write"),
            ],
            cli: vec![],
            configure: |b| b,
        },
        false,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_same_events_hold_with_max_turns() {
    parity(
        Case {
            name: "max_turns",
            turns: vec![
                Turn::tool("c1", "write", json!({"path": "x.txt", "content": "x"})),
                Turn::text("unreached"),
            ],
            cli: vec!["--max-turns", "1"],
            configure: |b| b.max_turns(1),
        },
        false,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_same_events_hold_with_json_schema() {
    parity(
        Case {
            name: "json_schema",
            turns: vec![Turn::text("{\"answer\": 42}")],
            cli: vec!["--json-schema", "{\"type\":\"object\"}"],
            configure: |b| b.json_schema(json!({"type": "object"})),
        },
        false,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_same_events_hold_with_max_budget_usd() {
    parity(
        Case {
            name: "max_budget_usd",
            turns: vec![
                Turn::tool("c1", "write", json!({"path": "x.txt", "content": "x"})),
                Turn::text("unreached"),
            ],
            cli: vec!["--max-budget-usd", "0.0000001"],
            configure: |b| b.max_budget_usd(0.0000001),
        },
        true,
    )
    .await;
}
