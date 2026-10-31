//! Lives in a binary of its own: a thread-local subscriber races with callsite registration in
//! tests that run beside it.

mod common;

use common::World;
use common::pipeline::{Options, Rig};
use common::runtime::build;
use fontaine_core::audit::DecisionRecord;
use fontaine_core::config::PermissionMode::{self, AcceptEdits, DontAsk, Manual, Plan};
use fontaine_core::permissions::Answer;
use fontaine_core::turn::{Drive, Flow, TurnOutcome, TurnParams, drive, run_turn};
use rig_core::ProviderResponseError;
use rig_core::test_utils::{MockError, MockStreamEvent, TraceCapture, mock_final};
use serde_json::{Value, json};

const MODES: [PermissionMode; 4] = [Manual, AcceptEdits, Plan, DontAsk];

fn calls() -> Vec<(&'static str, Value)> {
    vec![
        ("read", json!({"path": "a.txt"})),
        ("read", json!({"path": "../outside.txt"})),
        ("write", json!({"path": "n.txt", "content": "x"})),
        ("write", json!({"path": ".git/config", "content": "x"})),
        ("shell", json!({"command": "ls"})),
        ("shell", json!({"command": "touch x"})),
        ("shell", json!({"command": "rm -rf /"})),
        ("webfetch", json!({"url": "https://example.com/"})),
    ]
}

async fn run_matrix(headless: bool) -> (Rig, Vec<DecisionRecord>, Vec<DecisionRecord>) {
    // One rig per mode, one shared feed of everything: collect per rig and merge.
    let mut audit = Vec::new();
    let mut feed = Vec::new();
    let mut last_rig = None;
    for mode in MODES {
        let mut options =
            Options::new(mode).settings("[permissions]\nallow = [\"shell(touch x)\"]\n");
        options.headless = headless;
        let rig = Rig::with(options);
        std::fs::write(rig.work().join("a.txt"), "a").unwrap();
        for (tool, args) in calls() {
            rig.approver.answer(Answer::AllowOnce);
            rig.check(tool, args).await;
        }
        audit.extend(rig.audit());
        {
            let mut guard = rig.state.feed().await;
            while let Some(record) = guard.try_next() {
                feed.push(record);
            }
        }
        last_rig = Some(rig);
    }
    (last_rig.unwrap(), audit, feed)
}

#[tokio::test]
async fn each_audit_entry_has_one_tool_decision_event_and_one_feed_record() {
    for headless in [false, true] {
        let capture = TraceCapture::default();
        let _guard = tracing::subscriber::set_default(capture.subscriber());
        let (_rig, audit, feed) = run_matrix(headless).await;
        assert!(audit.len() >= calls().len() * MODES.len());
        assert_eq!(
            feed, audit,
            "the feed carries exactly what the audit log holds"
        );

        let events: Vec<_> = capture
            .events()
            .into_iter()
            .filter(|e| e.message() == "tool_decision")
            .collect();
        assert_eq!(events.len(), audit.len());
        for (record, event) in audit.iter().zip(&events) {
            assert_eq!(event.target, "fontaine::telemetry");
            assert_eq!(
                event.fields["tool_use_id"],
                record.tool_use_id.clone().unwrap().as_str()
            );
            assert_eq!(event.fields["tier"], record.tier.as_str());
            assert_eq!(event.fields["decision"], record.decision.as_str());
            assert_eq!(event.fields["tool_name"], record.tool.as_str());
            assert_eq!(
                event.fields["permission_mode"],
                record.permission_mode.as_str()
            );
            assert_eq!(event.fields["headless"], record.headless);
        }
        // Content-free: neither the command nor the path reaches telemetry.
        let rendered =
            serde_json::to_string(&events.iter().map(|e| &e.fields).collect::<Vec<_>>()).unwrap();
        assert!(!rendered.contains("rm -rf") && !rendered.contains(".git/config"));
    }
}

#[tokio::test]
async fn the_permission_span_records_the_first_tier_and_decision() {
    let capture = TraceCapture::default();
    let _guard = tracing::subscriber::set_default(capture.subscriber());
    let rig = Rig::with(Options::new(Manual));
    rig.approver.answer(Answer::AllowOnce);
    rig.check("shell", json!({"command": "touch x"})).await;
    let span = capture
        .spans()
        .into_iter()
        .find(|s| s.name == "fontaine.permission")
        .expect("a permission span");
    assert_eq!(span.target, "fontaine::telemetry");
    assert!(span.recorded.contains(&("tier".to_string(), json!("mode"))));
    assert!(
        span.recorded
            .contains(&("decision".to_string(), json!("prompt")))
    );
    assert!(
        capture
            .spans()
            .iter()
            .any(|s| s.name == "fontaine.tool.blocked_on_user"
                && s.parent_name == Some("fontaine.permission"))
    );
}

#[tokio::test]
async fn an_approved_external_read_records_the_resolved_target() {
    let rig = Rig::new(Manual);
    let outside = rig.world.root.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("x.txt"), "x").unwrap();
    rig.approver.answer(Answer::AllowOnce);
    assert!(matches!(
        rig.check("read", json!({"path": "../outside/x.txt"})).await,
        fontaine_tools::gate::GateDecision::Allow { .. }
    ));

    // The action record names the resolved path, and the audit digest is the resolved
    // arguments', not the written alias'.
    let records: Vec<_> = rig
        .session
        .ledger
        .read()
        .unwrap()
        .into_iter()
        .map(|e| e.record)
        .collect();
    let action = records.iter().find_map(|r| match r {
        fontaine_core::ledger::Record::Action { tool, paths, .. } if tool == "read" => {
            Some(paths.clone())
        }
        _ => None,
    });
    assert_eq!(
        action,
        Some(vec![outside.join("x.txt").display().to_string()]),
        "{records:?}"
    );
    let resolved_digest = fontaine_core::audit::args_digest(&json!({
        "path": outside.join("x.txt").to_str().unwrap()
    }));
    let written_digest = fontaine_core::audit::args_digest(&json!({"path": "../outside/x.txt"}));
    let records = rig.audit();
    let record = records
        .iter()
        .find(|r| r.tier == fontaine_core::tier::Tier::Human)
        .expect("the human's allow record");
    assert_eq!(record.args_digest, resolved_digest);
    assert_ne!(record.args_digest, written_digest);
}

#[tokio::test]
async fn a_mode_change_emits_an_event() {
    let capture = TraceCapture::default();
    let _guard = tracing::subscriber::set_default(capture.subscriber());
    let rig = Rig::new(Manual);
    rig.state.set_mode(Plan);
    rig.state.set_mode(Plan);
    let events: Vec<_> = capture
        .events()
        .into_iter()
        .filter(|e| e.message() == "permission_mode_changed")
        .collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].fields["from"], "manual");
    assert_eq!(events[0].fields["to"], "plan");
}

#[tokio::test]
async fn the_audit_log_is_written_with_no_subscriber_installed() {
    // No `[telemetry]` at all: nothing listens, and the record is still on disk.
    let rig = Rig::new(Manual);
    rig.check("shell", json!({"command": "ls"})).await;
    let text = std::fs::read_to_string(rig.session.audit.path()).unwrap();
    assert_eq!(text.lines().count(), 1);
    assert!(text.contains("\"tier\":\"tier05\""), "{text}");
}

/// A guardian whose model sits behind a scripted chat endpoint that answers after `delay`.
async fn slow_guardian(
    delay: std::time::Duration,
) -> (
    wiremock::MockServer,
    std::sync::Arc<fontaine_guardian::Guardian>,
) {
    use rig_core::providers::openai::OpenAIConfig;
    use rig_core::providers::openai::wire::{Auth, Dialect};
    use wiremock::matchers::any;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const GATEWAY: Dialect = Dialect::gateway("openai-compatible", "", "");
    let server = MockServer::builder().start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(500).set_delay(delay))
        .mount(&server)
        .await;
    let client = OpenAIConfig::with_key(&GATEWAY, "k")
        .with_auth(Auth::OptionalBearer)
        .with_base_url(server.uri())
        .client();
    let guardian = fontaine_guardian::Guardian::new(
        client.completion("scripted").erase(),
        "test/slow",
        fontaine_guardian::Settings {
            timeout: std::time::Duration::from_millis(200),
            ..Default::default()
        },
    );
    (server, std::sync::Arc::new(guardian))
}

#[tokio::test]
async fn each_fail_closed_reason_is_counted_once_under_its_name_with_its_audit_label() {
    use common::pipeline::{allow_verdict, fake_guardian, verdict_json};
    use fontaine_core::config::PermissionMode::Auto;
    use rig_core::test_utils::{MockCompletionModel, MockTurn};

    let capture = TraceCapture::default();
    let _guard = tracing::subscriber::set_default(capture.subscriber());

    let model_guardian = |turns: Vec<MockTurn>| {
        let mock = MockCompletionModel::from_turns(turns);
        std::sync::Arc::new(fontaine_guardian::Guardian::new(
            mock.into(),
            "test/mock",
            fontaine_guardian::Settings::default(),
        ))
    };
    let (_server, slow) = slow_guardian(std::time::Duration::from_secs(30)).await;
    let cases: Vec<(
        &str,
        &str,
        std::sync::Arc<fontaine_guardian::Guardian>,
        &str,
    )> = vec![
        ("timeout", "Timeout", slow, "make all"),
        (
            "unparseable",
            "Unparseable",
            model_guardian(vec![MockTurn::text("looks fine"); 3]),
            "make all",
        ),
        (
            "error",
            "GuardianUnavailable",
            model_guardian(vec![MockTurn::error("boom"); 5]),
            "make all",
        ),
        (
            "unavailable",
            "GuardianUnavailable",
            fontaine_core::gate::no_guardian(),
            "make all",
        ),
        (
            "low_confidence",
            "LowConfidence",
            fake_guardian(verdict_json("allow", "allow", 0.4)).guardian,
            "rm notes.txt",
        ),
    ];
    for (reason, label, guardian, command) in &cases {
        let rig = Rig::with(Options::new(Auto).headless().guardian(guardian.clone()));
        rig.check("shell", json!({"command": command})).await;
        let record = rig.audit().pop().unwrap();
        assert_eq!(record.fail_closed_reason.as_deref(), Some(*reason));
        assert_eq!(record.rule.as_deref(), Some(*label), "{reason}");
    }
    let _ = allow_verdict;

    let events: Vec<_> = capture
        .events()
        .into_iter()
        .filter(|e| e.message() == "tool_decision")
        .collect();
    assert_eq!(events.len(), cases.len());
    for (reason, ..) in &cases {
        let counted = events
            .iter()
            .filter(|e| e.fields.get("fail_closed_reason") == Some(&json!(reason)))
            .count();
        assert_eq!(counted, 1, "{reason}");
    }
    // The guardian fields ride on the event; the rule text and explanation do not.
    let rendered =
        serde_json::to_string(&events.iter().map(|e| &e.fields).collect::<Vec<_>>()).unwrap();
    assert!(rendered.contains("guardian.model"));
    assert!(!rendered.contains("Denied by guardian") && !rendered.contains("GuardianUnavailable"));
}

fn failing_turn(status: u16) -> Vec<Vec<MockStreamEvent>> {
    let status = http::StatusCode::from_u16(status).unwrap();
    vec![vec![MockStreamEvent::Error(MockError::ProviderResponse(
        ProviderResponseError::new(status, "CANARY-BODY echoed the request"),
    ))]]
}

fn api_errors(capture: &TraceCapture) -> Vec<rig_core::test_utils::CapturedEvent> {
    capture
        .events()
        .into_iter()
        .filter(|e| e.message() == "api_error")
        .collect()
}

#[tokio::test]
async fn a_failed_main_model_call_emits_an_api_error_without_the_message() {
    let capture = TraceCapture::default();
    let _guard = tracing::subscriber::set_default(capture.subscriber());
    // A 400 fails without retries, so the one event below is the single attempt's.
    let w = World::new();
    let built = build(&w, Manual, failing_turn(400), true);
    let outcome = run_turn(
        TurnParams {
            agent: &built.runtime.agent,
            state: &built.runtime.state,
            prompt: "hi".to_string(),
            max_turns: Some(3),
            priced: true,
            model: "scripted".to_string(),
            provider: "mock".to_string(),
            native_search: false,
        },
        &mut |_| Flow::Continue,
    )
    .await;
    assert!(matches!(outcome, TurnOutcome::Failed(_)), "{outcome:?}");
    let events = api_errors(&capture);
    assert_eq!(events.len(), 1, "{events:?}");
    let event = &events[0];
    assert_eq!(event.target, "fontaine::telemetry");
    assert_eq!(event.fields["error_class"], "provider_response");
    assert_eq!(event.fields["status_code"], 400);
    assert_eq!(event.fields["query_source"], "main");
    assert_eq!(event.fields["model"], "scripted");
    assert_eq!(event.fields["provider"], "mock");
    assert!(
        !format!("{:?}", event.fields).contains("CANARY"),
        "{:?}",
        event.fields
    );
}

#[tokio::test]
async fn a_failed_subagent_call_reports_the_subagent_as_its_source() {
    let capture = TraceCapture::default();
    let _guard = tracing::subscriber::set_default(capture.subscriber());
    // A 400 fails without retries, so the one event below is the single attempt's.
    let w = World::new();
    let built = build(&w, Manual, failing_turn(400), true);
    let outcome = drive(
        Drive {
            agent: &built.runtime.agent,
            state: &built.runtime.state,
            agent_id: "child-1",
            prompt: "hi".to_string(),
            max_turns: Some(3),
            priced: true,
            model: "scripted".to_string(),
            provider: "mock".to_string(),
            native_search: false,
            span: tracing::Span::none(),
        },
        None,
        &mut |_| Flow::Continue,
    )
    .await;
    assert!(matches!(outcome, TurnOutcome::Failed(_)), "{outcome:?}");
    let events = api_errors(&capture);
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0].fields["query_source"], "subagent");
    assert_eq!(events[0].fields["agent.id"], "child-1");
    assert_eq!(events[0].fields["status_code"], 400);
}

#[tokio::test]
async fn reaching_the_turn_limit_is_not_an_api_error() {
    let capture = TraceCapture::default();
    let _guard = tracing::subscriber::set_default(capture.subscriber());
    let w = World::new();
    let call = vec![
        MockStreamEvent::ToolCall {
            id: "t1".to_string(),
            name: "read".to_string(),
            arguments: json!({"path": "a.txt"}),
            call_id: None,
        },
        MockStreamEvent::FinalResponse(mock_final(rig_core::completion::Usage::new())),
    ];
    std::fs::write(w.work.join("a.txt"), "a").unwrap();
    let built = build(&w, Manual, vec![call], true);
    let outcome = run_turn(
        TurnParams {
            agent: &built.runtime.agent,
            state: &built.runtime.state,
            prompt: "hi".to_string(),
            max_turns: Some(1),
            priced: true,
            model: "scripted".to_string(),
            provider: "mock".to_string(),
            native_search: false,
        },
        &mut |_| Flow::Continue,
    )
    .await;
    assert!(matches!(outcome, TurnOutcome::MaxTurns(_)), "{outcome:?}");
    assert!(api_errors(&capture).is_empty());
}

#[tokio::test]
async fn every_sink_failure_emits_an_internal_error_without_the_error_text() {
    use fontaine_core::gate::Sink;
    let capture = TraceCapture::default();
    let _guard = tracing::subscriber::set_default(capture.subscriber());
    let rig = Rig::new(Manual);
    let error = std::io::Error::other("CANARY-PATH /home/x/audit.jsonl");
    rig.state.sink_failed(Sink::Audit, &error);
    rig.state.sink_failed(Sink::Audit, &error);
    rig.state.sink_failed(Sink::Ledger, &error);
    let events: Vec<_> = capture
        .events()
        .into_iter()
        .filter(|e| e.message() == "internal_error")
        .collect();
    let classes: Vec<_> = events.iter().map(|e| e.fields["class"].clone()).collect();
    assert_eq!(
        classes,
        [
            json!("audit_write"),
            json!("audit_write"),
            json!("ledger_write")
        ]
    );
    for event in &events {
        assert_eq!(event.target, "fontaine::telemetry");
        assert!(
            !format!("{:?}", event.fields).contains("CANARY"),
            "{:?}",
            event.fields
        );
    }
}
