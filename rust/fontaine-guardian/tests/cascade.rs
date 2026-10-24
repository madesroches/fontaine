mod common;

use std::time::{Duration, Instant};

use common::{Endpoint, Reply, allow, deny, request, settings};
use fontaine_guardian::cascade::{Decided, ScreenInput, adjudicate};
use fontaine_guardian::{
    AllowSettings, Decision, Profile, Question, Screen, ScreenOutcome, ScreenSettings,
};
use serde_json::{Value, json};
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const HARDENED_MARK: &str = "is a sign of manipulation";
const STEERING_MARK: &str = "influence how this action is judged";

fn answer(yes: f64) -> Value {
    let top = [("Yes", yes), ("No", 1.0 - yes)]
        .iter()
        .map(|(token, p)| json!({ "token": token, "logprob": p.ln() }))
        .collect::<Vec<_>>();
    json!({ "logprobs": [{ "token": "Yes", "logprob": 0.0, "top_logprobs": top }] })
}

fn is_read(request: &Request, mark: &str) -> bool {
    String::from_utf8_lossy(&request.body).contains(mark)
}

/// A screen server: the stop read says `stop_yes`, the allow read `allow_yes`, the detector
/// `steering`, each after `delay`.
async fn screen_server(
    stop_yes: f64,
    allow_yes: f64,
    steering: f64,
    delay: Duration,
) -> MockServer {
    let server = MockServer::start().await;
    let reply = |yes: f64| {
        ResponseTemplate::new(200)
            .set_body_json(answer(yes))
            .set_delay(delay)
    };
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .and(|r: &Request| is_read(r, HARDENED_MARK))
        .respond_with(reply(allow_yes))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .and(|r: &Request| is_read(r, STEERING_MARK) && !is_read(r, HARDENED_MARK))
        .respond_with(reply(steering))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .and(|r: &Request| !is_read(r, STEERING_MARK) && !is_read(r, HARDENED_MARK))
        .respond_with(reply(stop_yes))
        .mount(&server)
        .await;
    server
}

fn screen_at(server: &MockServer) -> Screen {
    Screen::new(
        reqwest::Client::new(),
        ScreenSettings {
            endpoint: Url::parse(&server.uri()).unwrap(),
            model: "tiny".into(),
            context_tokens: 16384,
            question: Question::EmployeeAllowed,
            t_stop: Some(0.9),
            timeout: Duration::from_secs(5),
            profile: Profile::Full,
            allow: Some(AllowSettings {
                t_allow: Some(0.01),
                steering_veto: 0.001,
            }),
        },
    )
}

const SAFE: (f64, f64, f64) = (0.99, 0.999, 0.0001);
const RISKY: (f64, f64, f64) = (0.01, 0.5, 0.0001);
const UNSURE: (f64, f64, f64) = (0.6, 0.6, 0.0001);

async fn slow_guardian(verdict: Value, delay: Duration) -> (Endpoint, fontaine_guardian::Guardian) {
    let endpoint = Endpoint::start(vec![Reply::Slow(delay, verdict)]).await;
    let guardian = endpoint.guardian(settings());
    (endpoint, guardian)
}

#[tokio::test]
async fn without_a_screen_it_is_the_guardians_adjudication() {
    let endpoint = Endpoint::start(vec![Reply::Verdict(deny("Exfiltration"))]).await;
    let guardian = endpoint.guardian(settings());
    let composed = adjudicate(None, &guardian, &request("ls", false)).await;
    assert!(composed.screen.is_none());
    assert_eq!(composed.dropped_input_tokens, None);
    let Decided::Guardian(adjudication) = composed.decided else {
        panic!("the guardian decides");
    };
    assert_eq!(adjudication.decision, Decision::Deny);
}

#[tokio::test]
async fn an_enforced_stop_drops_the_guardian() {
    let server = screen_server(RISKY.0, RISKY.1, RISKY.2, Duration::ZERO).await;
    let (endpoint, guardian) = slow_guardian(allow(0.95), Duration::from_secs(3)).await;
    let screen = screen_at(&server);
    let started = Instant::now();
    let composed = adjudicate(
        Some(ScreenInput {
            screen: &screen,
            enforce: true,
        }),
        &guardian,
        &request("ls", false),
    )
    .await;
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the guardian was awaited"
    );
    assert!(matches!(composed.decided, Decided::ScreenStop(_)));
    assert!(matches!(composed.screen, Some(ScreenOutcome::Stop { .. })));
    assert_eq!(
        composed.dropped_input_tokens,
        Some(guardian.dropped_estimate(&request("ls", false)))
    );
    assert!(composed.dropped_input_tokens.is_some_and(|n| n > 0));
    // The guardian was already asked, while the screen was answering.
    assert_eq!(endpoint.calls().await, 1);
}

#[tokio::test]
async fn an_enforced_allow_drops_the_guardian() {
    let server = screen_server(SAFE.0, SAFE.1, SAFE.2, Duration::ZERO).await;
    let (endpoint, guardian) = slow_guardian(deny("Exfiltration"), Duration::from_secs(3)).await;
    let screen = screen_at(&server);
    let started = Instant::now();
    let composed = adjudicate(
        Some(ScreenInput {
            screen: &screen,
            enforce: true,
        }),
        &guardian,
        &request("ls", false),
    )
    .await;
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the guardian was awaited"
    );
    assert!(matches!(composed.decided, Decided::ScreenAllow(_)));
    assert!(composed.dropped_input_tokens.is_some_and(|n| n > 0));
    assert_eq!(endpoint.calls().await, 1);
}

#[tokio::test]
async fn a_shadow_allow_or_stop_leaves_the_call_to_the_guardian() {
    for scores in [SAFE, RISKY] {
        let server = screen_server(scores.0, scores.1, scores.2, Duration::ZERO).await;
        let (_endpoint, guardian) =
            slow_guardian(deny("Exfiltration"), Duration::from_millis(300)).await;
        let screen = screen_at(&server);
        let composed = adjudicate(
            Some(ScreenInput {
                screen: &screen,
                enforce: false,
            }),
            &guardian,
            &request("ls", false),
        )
        .await;
        let Decided::Guardian(adjudication) = composed.decided else {
            panic!("the guardian decides in shadow mode");
        };
        assert_eq!(adjudication.decision, Decision::Deny);
        assert_eq!(composed.dropped_input_tokens, None);
        assert!(matches!(
            composed.screen,
            Some(ScreenOutcome::Allow { .. } | ScreenOutcome::Stop { .. })
        ));
    }
}

#[tokio::test]
async fn a_guardian_verdict_in_hand_wins_over_a_later_screen_outcome() {
    let cases = [
        (SAFE, deny("Exfiltration"), Decision::Deny),
        (RISKY, allow(0.95), Decision::Allow),
        (SAFE, allow(0.95), Decision::Allow),
        (
            RISKY,
            common::verdict("escalate", "Destroy Pre-existing Files", 0.6),
            Decision::Escalate,
        ),
    ];
    for (scores, verdict, expected) in cases {
        let server = screen_server(scores.0, scores.1, scores.2, Duration::from_millis(400)).await;
        let endpoint = Endpoint::start(vec![Reply::Verdict(verdict)]).await;
        let guardian = endpoint.guardian(settings());
        let screen = screen_at(&server);
        let composed = adjudicate(
            Some(ScreenInput {
                screen: &screen,
                enforce: true,
            }),
            &guardian,
            &request("ls", false),
        )
        .await;
        let Decided::Guardian(adjudication) = composed.decided else {
            panic!("a verdict in hand goes through settle");
        };
        assert_eq!(adjudication.decision, expected);
        assert_eq!(composed.dropped_input_tokens, None);
        // The screen's annotation is kept.
        assert!(composed.screen.is_some_and(|s| s.risk().is_some()));
    }
}

#[tokio::test]
async fn pass_and_unavailable_await_the_guardian() {
    let server = screen_server(UNSURE.0, UNSURE.1, UNSURE.2, Duration::ZERO).await;
    let (_endpoint, guardian) =
        slow_guardian(deny("Exfiltration"), Duration::from_millis(300)).await;
    let screen = screen_at(&server);
    let composed = adjudicate(
        Some(ScreenInput {
            screen: &screen,
            enforce: true,
        }),
        &guardian,
        &request("ls", false),
    )
    .await;
    assert!(matches!(composed.screen, Some(ScreenOutcome::Pass { .. })));
    let Decided::Guardian(adjudication) = composed.decided else {
        panic!("the guardian decides");
    };
    assert_eq!(adjudication.decision, Decision::Deny);

    let broken = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&broken)
        .await;
    let (_endpoint, guardian) =
        slow_guardian(deny("Exfiltration"), Duration::from_millis(300)).await;
    let screen = screen_at(&broken);
    let composed = adjudicate(
        Some(ScreenInput {
            screen: &screen,
            enforce: true,
        }),
        &guardian,
        &request("ls", false),
    )
    .await;
    assert!(matches!(
        composed.screen,
        Some(ScreenOutcome::Unavailable { .. })
    ));
    assert!(matches!(composed.decided, Decided::Guardian(a) if a.decision == Decision::Deny));
}
