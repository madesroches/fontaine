//! The screen stage in `auto`: it can stop a call as an escalation but never allows one, and
//! every call it does not stop reaches the guardian.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::pipeline::{
    Options, Rig, allow_verdict, allowed, denied_message, fake_guardian, verdict_json,
};
use fontaine_agents::mode::Ladder;
use fontaine_core::audit::Decision;
use fontaine_core::config::PermissionMode::Auto;
use fontaine_core::gate::ScreenStage;
use fontaine_core::permissions::Answer;
use fontaine_core::tier::Tier;
use fontaine_guardian::{Profile, Question, Screen, ScreenSettings};
use fontaine_tools::gate::ROOT_AGENT;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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

fn stage(server: &MockServer, shadow: bool) -> ScreenStage {
    ScreenStage {
        screen: Arc::new(Screen::new(
            reqwest::Client::new(),
            ScreenSettings {
                endpoint: url::Url::parse(&server.uri()).unwrap(),
                model: "qwen3:4b".into(),
                question: Question::EmployeeAllowed,
                t_stop: Some(0.9),
                timeout: Duration::from_secs(2),
                profile: Profile::Full,
            },
        )),
        label: "local/qwen3:4b".into(),
        shadow,
    }
}

fn rig(fake: &common::pipeline::Fake, headless: bool, screen: ScreenStage) -> Rig {
    let mut options = Options::new(Auto).guardian(fake.guardian.clone());
    if headless {
        options = options.headless();
    }
    let rig = Rig::with(options);
    rig.state.set_screen(Some(screen));
    rig
}

#[tokio::test]
async fn a_stop_escalates_to_the_human_and_never_asks_the_guardian() {
    let server = risky(0.97).await;
    let fake = fake_guardian(allow_verdict());
    let rig = rig(&fake, false, stage(&server, false));
    rig.approver.answer(Answer::Deny { feedback: None });
    let decision = rig.check("shell", shell("python a.py")).await;
    assert!(!allowed(&decision));
    assert_eq!(fake.calls(), 0);
    let request = rig.approver.last_request();
    assert_eq!(request.tier, Tier::Screen);
    assert!(
        request
            .reason
            .contains("a local screen judged this call outside the user's request (risk 0.97)")
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
    assert!(!rig.state.auto_paused());
}

#[tokio::test]
async fn the_human_can_still_approve_a_stopped_call() {
    let server = risky(0.97).await;
    let fake = fake_guardian(allow_verdict());
    let rig = rig(&fake, false, stage(&server, false));
    rig.approver.answer(Answer::AllowOnce);
    assert!(allowed(&rig.check("shell", shell("python a.py")).await));
    assert_eq!(fake.calls(), 0);
    let tiers: Vec<_> = rig.audit().iter().map(|r| r.tier).collect();
    assert_eq!(tiers, [Tier::Screen, Tier::Human]);
}

#[tokio::test]
async fn a_headless_stop_is_a_denial_that_names_the_screen() {
    let server = risky(0.97).await;
    let fake = fake_guardian(allow_verdict());
    let rig = rig(&fake, true, stage(&server, false));
    let decision = rig.check("shell", shell("python a.py")).await;
    let message = denied_message(&decision);
    assert!(message.contains("a local screen judged this call outside the user's request"));
    assert!(message.contains("headless"));
    assert_eq!(fake.calls(), 0);
    let record = &rig.audit()[0];
    assert_eq!(
        (record.tier, record.decision),
        (Tier::Screen, Decision::Deny)
    );
}

#[tokio::test]
async fn a_pass_reaches_the_guardian_and_the_guardian_decides() {
    let server = risky(0.1).await;
    let fake = fake_guardian(verdict_json("deny", "Download+Execute", 0.97));
    let rig = rig(&fake, false, stage(&server, false));
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
    let rig = rig(&fake, false, stage(&server, false));
    assert!(!allowed(&rig.check("shell", shell("python a.py")).await));
    assert_eq!(fake.calls(), 1);
}

#[tokio::test]
async fn an_unavailable_screen_warns_once_and_the_guardian_decides() {
    let server = ollama(ResponseTemplate::new(500)).await;
    let fake = fake_guardian(allow_verdict());
    let rig = rig(&fake, false, stage(&server, false));
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
    let rig = rig(&fake, false, stage(&server, true));
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
}

#[tokio::test]
async fn the_screen_never_sees_a_call_the_floor_or_the_fast_path_settled() {
    let server = risky(0.99).await;
    let fake = fake_guardian(allow_verdict());
    let rig = rig(&fake, false, stage(&server, false));
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
    let fake = fake_guardian(allow_verdict());
    let rig = rig(&fake, true, stage(&server, false));
    let child = rig
        .agents
        .reserve(ROOT_AGENT, "worker", "m", Vec::new(), Ladder::Auto, false)
        .unwrap();
    let decision = rig.check_as(&child, "shell", shell("python a.py")).await;
    assert!(!allowed(&decision));
    assert_eq!(fake.calls(), 0);
    let record = &rig.audit()[0];
    assert_eq!(
        (record.agent_id.as_str(), record.tier),
        (child.as_str(), Tier::Screen)
    );
}

#[tokio::test]
async fn the_screen_tier_never_produces_an_allow() {
    for risk in [0.0001, 0.3, 0.9, 0.97, 0.9999] {
        for shadow in [false, true] {
            for headless in [false, true] {
                let server = risky(risk).await;
                let fake = fake_guardian(verdict_json("deny", "Download+Execute", 0.97));
                let rig = rig(&fake, headless, stage(&server, shadow));
                rig.approver.answer(Answer::Deny { feedback: None });
                assert!(!allowed(&rig.check("shell", shell("python a.py")).await));
                for record in rig.audit() {
                    assert!(
                        !(record.tier == Tier::Screen && record.decision == Decision::Allow),
                        "{record:?}"
                    );
                }
            }
        }
    }
}
