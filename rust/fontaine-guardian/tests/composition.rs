#![cfg(feature = "composition")]

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use common::{Endpoint, Reply, allow, deny, request, settings};
use fontaine_guardian::composition::{
    Composed, Composition, CompositionSpec, Decided, Outcome, Param, Script, adjudicate,
};
use fontaine_guardian::{
    Decision, FailClosed, Guardian, Profile, Prompt, Question, Read, Reader, ReaderSettings,
};
use serde_json::{Value, json};
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const HARDENED_MARK: &str = "is a sign of manipulation";
const STEERING_MARK: &str = "influence how this action is judged";
const MODEL: &str = "tiny";

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
    screen_server_with(stop_yes, allow_yes, steering, move |yes| {
        move |_: &Request| {
            ResponseTemplate::new(200)
                .set_body_json(answer(yes))
                .set_delay(delay)
        }
    })
    .await
}

/// A screen server that answers only once `guardian_arrivals` shows the guardian was asked, so an
/// enforced screen decision always finds the guardian request already sent and can drop it.
async fn screen_server_after_guardian(
    stop_yes: f64,
    allow_yes: f64,
    steering: f64,
    guardian_arrivals: Arc<AtomicUsize>,
) -> MockServer {
    screen_server_with(stop_yes, allow_yes, steering, move |yes| {
        let arrivals = guardian_arrivals.clone();
        move |_: &Request| {
            // Blocking is fine here: the mock server runs on its own thread. The bound keeps a
            // guardian that is never asked from hanging the test.
            let give_up = Instant::now() + Duration::from_secs(1);
            while arrivals.load(Ordering::SeqCst) == 0 && Instant::now() < give_up {
                std::thread::sleep(Duration::from_millis(1));
            }
            ResponseTemplate::new(200).set_body_json(answer(yes))
        }
    })
    .await
}

async fn screen_server_with<R: Respond + 'static>(
    stop_yes: f64,
    allow_yes: f64,
    steering: f64,
    reply: impl Fn(f64) -> R,
) -> MockServer {
    let server = MockServer::start().await;
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

fn reader_at(server: &MockServer, timeout: Duration) -> Reader {
    Reader::new(
        reqwest::Client::new(),
        ReaderSettings {
            endpoint: Url::parse(&server.uri()).unwrap(),
            model: "tiny".into(),
            context_tokens: 16384,
            profile: Profile::Full,
            timeout,
        },
    )
}

fn number(value: f64) -> Param {
    Param::Number(value)
}

/// Both thresholds the calibration fixtures use.
fn both() -> Vec<(&'static str, Param)> {
    vec![
        ("stop_threshold", number(0.9)),
        ("allow_threshold", number(0.01)),
        ("steering_veto", number(0.001)),
    ]
}

fn with(
    mut params: Vec<(&'static str, Param)>,
    extra: &[(&'static str, Param)],
) -> Vec<(&'static str, Param)> {
    for (key, value) in extra {
        params.retain(|(k, _)| k != key);
        params.push((*key, value.clone()));
    }
    params
}

fn params_of(params: &[(&str, Param)]) -> BTreeMap<String, Param> {
    params
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

fn preset(server: &MockServer, params: &[(&'static str, Param)], shadow: bool) -> Arc<Composition> {
    Arc::new(
        Composition::compile(
            CompositionSpec {
                script: Script::Preset("screen".into()),
                model: MODEL.into(),
                reads: BTreeMap::new(),
                params: params_of(params),
                shadow,
            },
            vec![(MODEL.into(), reader_at(server, Duration::from_secs(5)))],
        )
        .expect("the preset compiles"),
    )
}

fn read(prompt: Prompt, question: Question) -> Read {
    Read {
        model: MODEL.into(),
        prompt,
        question,
    }
}

/// A script that declares the reads `a`..`d` (the stop question) and `h` (hardened).
fn script_spec(source: &str, shadow: bool) -> CompositionSpec {
    let plain = || read(Prompt::Plain, Question::EmployeeAllowed);
    CompositionSpec {
        script: Script::Source(source.into()),
        model: MODEL.into(),
        reads: BTreeMap::from([
            ("a".into(), plain()),
            ("b".into(), plain()),
            ("c".into(), plain()),
            ("d".into(), plain()),
            (
                "h".into(),
                read(Prompt::Hardened, Question::EmployeeAllowed),
            ),
        ]),
        params: BTreeMap::new(),
        shadow,
    }
}

fn script(server: &MockServer, source: &str, shadow: bool) -> Arc<Composition> {
    script_timeout(server, source, shadow, Duration::from_secs(5))
}

fn script_timeout(
    server: &MockServer,
    source: &str,
    shadow: bool,
    timeout: Duration,
) -> Arc<Composition> {
    Arc::new(
        Composition::compile(
            script_spec(source, shadow),
            vec![(MODEL.into(), reader_at(server, timeout))],
        )
        .unwrap_or_else(|e| panic!("the script compiles: {e}")),
    )
}

async fn idle_server() -> MockServer {
    screen_server(0.5, 0.5, 0.5, Duration::ZERO).await
}

async fn run(composition: &Arc<Composition>, guardian: &Arc<Guardian>) -> Composed {
    adjudicate(
        composition.clone(),
        guardian.clone(),
        Arc::new(request("ls", false)),
    )
    .await
}

fn guardian_of(endpoint: &Endpoint) -> Arc<Guardian> {
    Arc::new(endpoint.guardian(settings()))
}

async fn slow_guardian(verdict: Value, delay: Duration) -> (Endpoint, Arc<Guardian>) {
    let endpoint = Endpoint::start(vec![Reply::Slow(delay, verdict)]).await;
    let guardian = guardian_of(&endpoint);
    (endpoint, guardian)
}

fn adjudication(composed: Composed) -> fontaine_guardian::Adjudication {
    let Decided::Guardian(adjudication) = composed.decided else {
        panic!("the guardian decides, not {:?}", composed.decided);
    };
    *adjudication
}

const SAFE: (f64, f64, f64) = (0.99, 0.999, 0.0001);
const RISKY: (f64, f64, f64) = (0.01, 0.5, 0.0001);
const UNSURE: (f64, f64, f64) = (0.6, 0.6, 0.0001);

const CONCURRENT: [(&str, Param); 1] = [("concurrent", Param::Bool(true))];

// ---------------------------------------------------------------- the screen preset, racing

#[tokio::test]
async fn dropping_the_caller_cancels_the_script_and_its_reads() {
    let (endpoint, guardian) = slow_guardian(allow(0.95), Duration::from_secs(5)).await;
    let server = screen_server(SAFE.0, SAFE.1, SAFE.2, Duration::from_secs(5)).await;
    let composition = preset(&server, &with(both(), &CONCURRENT), false);
    let request = Arc::new(request("ls", false));
    let weak = Arc::downgrade(&request);
    let dropped = tokio::time::timeout(
        Duration::from_millis(300),
        adjudicate(composition, guardian, request),
    )
    .await;
    assert!(dropped.is_err(), "the evaluation outlasted the timeout");
    // Every task that held the request is gone once the script and the reads are aborted.
    let give_up = Instant::now() + Duration::from_secs(2);
    while weak.strong_count() > 0 && Instant::now() < give_up {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(weak.strong_count(), 0, "a task outlived its dropped caller");
    drop(endpoint);
}

#[tokio::test]
async fn an_enforced_stop_drops_the_guardian() {
    let (endpoint, guardian) = slow_guardian(allow(0.95), Duration::from_secs(3)).await;
    let server = screen_server_after_guardian(RISKY.0, RISKY.1, RISKY.2, endpoint.arrivals()).await;
    let composition = preset(&server, &with(both(), &CONCURRENT), false);
    let started = Instant::now();
    let composed = run(&composition, &guardian).await;
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the guardian was awaited"
    );
    assert!(matches!(&composed.decided, Decided::Escalate { node, .. } if node == "screen.stop"));
    assert_eq!(composed.outcome, Outcome::Stop);
    assert_eq!(
        composed.dropped_input_tokens,
        Some(guardian.dropped_estimate(&request("ls", false)))
    );
    assert!(composed.dropped_input_tokens.is_some_and(|n| n > 0));
    // The screen answered only after the guardian's responder ran, and the server records a request
    // before running its responder, so the request is already counted.
    assert_eq!(endpoint.calls().await, 1);
}

#[tokio::test]
async fn an_enforced_allow_drops_the_guardian() {
    let (endpoint, guardian) =
        slow_guardian(deny("Data Exfiltration"), Duration::from_secs(3)).await;
    let server = screen_server_after_guardian(SAFE.0, SAFE.1, SAFE.2, endpoint.arrivals()).await;
    let composition = preset(&server, &with(both(), &CONCURRENT), false);
    let started = Instant::now();
    let composed = run(&composition, &guardian).await;
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the guardian was awaited"
    );
    assert!(
        matches!(&composed.decided, Decided::Allow { node, threshold, .. } if node == "screen.allow" && *threshold == 0.01)
    );
    assert_eq!(composed.outcome, Outcome::Allow);
    assert!(composed.dropped_input_tokens.is_some_and(|n| n > 0));
    assert_eq!(endpoint.calls().await, 1);
}

#[tokio::test]
async fn a_guardian_verdict_in_hand_wins_over_a_later_screen_outcome() {
    let cases = [
        (SAFE, deny("Data Exfiltration"), Decision::Deny),
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
        let guardian = guardian_of(&endpoint);
        let composition = preset(&server, &with(both(), &CONCURRENT), false);
        let composed = run(&composition, &guardian).await;
        assert_eq!(composed.dropped_input_tokens, None);
        // The screen's reads are kept for the record.
        assert!(composed.score("stop").is_some());
        assert_eq!(composed.outcome, Outcome::Pass);
        assert_eq!(adjudication(composed).decision, expected);
    }
}

// ---------------------------------------------------------------- the screen preset, default shape

#[tokio::test]
async fn by_default_the_screen_is_an_allow_screen_in_front_of_the_guardian() {
    let (endpoint, guardian) = slow_guardian(deny("Data Exfiltration"), Duration::ZERO).await;
    let params = [("allow_threshold", number(0.01))];

    // Clearly safe: allowed, and the guardian is never asked.
    let server = screen_server(SAFE.0, SAFE.1, SAFE.2, Duration::ZERO).await;
    let composed = run(&preset(&server, &params, false), &guardian).await;
    assert!(matches!(composed.decided, Decided::Allow { .. }));
    assert_eq!(composed.dropped_input_tokens, None);
    assert_eq!(endpoint.calls().await, 0);

    // Risky: no stop threshold, so the guardian reviews it, and the stop read is still scored.
    let server = screen_server(RISKY.0, RISKY.1, RISKY.2, Duration::ZERO).await;
    let composed = run(&preset(&server, &params, false), &guardian).await;
    assert_eq!(composed.outcome, Outcome::Pass);
    assert!((composed.score("stop").unwrap() - 0.99).abs() < 1e-6);
    assert_eq!(adjudication(composed).decision, Decision::Deny);
    assert_eq!(endpoint.calls().await, 1);
}

#[tokio::test]
async fn a_stop_escalates_only_when_stop_threshold_is_set() {
    let (endpoint, guardian) = slow_guardian(allow(0.95), Duration::ZERO).await;
    let server = screen_server(RISKY.0, RISKY.1, RISKY.2, Duration::ZERO).await;

    let composed = run(
        &preset(&server, &[("stop_threshold", number(0.9))], false),
        &guardian,
    )
    .await;
    assert!(matches!(&composed.decided, Decided::Escalate { node, .. } if node == "screen.stop"));
    // Sequential: the guardian was never asked, so nothing was dropped.
    assert_eq!(composed.dropped_input_tokens, None);
    assert_eq!(endpoint.calls().await, 0);

    let composed = run(
        &preset(&server, &[("allow_threshold", number(0.01))], false),
        &guardian,
    )
    .await;
    assert_eq!(composed.outcome, Outcome::Pass);
    assert_eq!(adjudication(composed).decision, Decision::Allow);
}

#[tokio::test]
async fn stop_takes_precedence_over_allow() {
    let (_endpoint, guardian) = slow_guardian(allow(0.95), Duration::ZERO).await;
    let server = screen_server(0.01, 0.999, 0.0001, Duration::ZERO).await;
    let params = with(both(), &[("allow_threshold", number(0.5))]);
    let composed = run(&preset(&server, &params, false), &guardian).await;
    assert_eq!(composed.outcome, Outcome::Stop);
}

#[tokio::test]
async fn steering_at_the_veto_or_an_allow_risk_at_the_threshold_passes() {
    let cases = [
        (0.99, 0.999, 0.001),
        (0.99, 0.999, 0.5),
        // The allow risk is the mass on no: 0.01 exactly.
        (0.99, 0.99, 0.0001),
    ];
    for (stop, allow_yes, steering) in cases {
        let (_endpoint, guardian) = slow_guardian(deny("Data Exfiltration"), Duration::ZERO).await;
        let server = screen_server(stop, allow_yes, steering, Duration::ZERO).await;
        let composed = run(&preset(&server, &both(), false), &guardian).await;
        assert_eq!(
            composed.outcome,
            Outcome::Pass,
            "{stop} {allow_yes} {steering}"
        );
    }
}

#[tokio::test]
async fn unsure_calls_wait_for_the_guardian() {
    let server = screen_server(UNSURE.0, UNSURE.1, UNSURE.2, Duration::ZERO).await;
    let (_endpoint, guardian) =
        slow_guardian(deny("Data Exfiltration"), Duration::from_millis(300)).await;
    let composed = run(&preset(&server, &both(), false), &guardian).await;
    assert_eq!(composed.outcome, Outcome::Pass);
    assert!(!composed.unavailable());
    assert_eq!(adjudication(composed).decision, Decision::Deny);
}

#[tokio::test]
async fn any_failed_read_leaves_the_call_to_the_guardian() {
    let garbage = || {
        ResponseTemplate::new(200).set_body_json(json!({ "logprobs": [{
            "token": "The", "logprob": 0.0,
            "top_logprobs": [{ "token": "The", "logprob": -0.01 }],
        }]}))
    };
    type Matcher = fn(&Request) -> bool;
    let marks: [(&str, Matcher); 3] = [
        ("stop", |r| {
            !is_read(r, STEERING_MARK) && !is_read(r, HARDENED_MARK)
        }),
        ("allow", |r| is_read(r, HARDENED_MARK)),
        ("steering", |r| {
            is_read(r, STEERING_MARK) && !is_read(r, HARDENED_MARK)
        }),
    ];
    for (failing, _) in marks {
        for failure in [
            ResponseTemplate::new(500),
            garbage(),
            ResponseTemplate::new(200)
                .set_body_json(answer(0.5))
                .set_delay(Duration::from_millis(600)),
        ] {
            let server = MockServer::start().await;
            for (name, matcher) in marks {
                let response = if name == failing {
                    failure.clone()
                } else {
                    ResponseTemplate::new(200).set_body_json(answer(0.0001))
                };
                Mock::given(method("POST"))
                    .and(path("/api/chat"))
                    .and(matcher)
                    .respond_with(response)
                    .mount(&server)
                    .await;
            }
            let composition = Arc::new(
                Composition::compile(
                    CompositionSpec {
                        script: Script::Preset("screen".into()),
                        model: MODEL.into(),
                        reads: BTreeMap::new(),
                        params: params_of(&with(
                            both(),
                            &[
                                ("allow_threshold", number(0.5)),
                                ("steering_veto", number(0.5)),
                            ],
                        )),
                        shadow: false,
                    },
                    vec![(MODEL.into(), reader_at(&server, Duration::from_millis(200)))],
                )
                .unwrap(),
            );
            let (_endpoint, guardian) =
                slow_guardian(deny("Data Exfiltration"), Duration::ZERO).await;
            let composed = run(&composition, &guardian).await;
            assert!(composed.unavailable(), "{failing}");
            assert_eq!(composed.outcome_name(), "unavailable");
            assert!(
                composed
                    .reads
                    .iter()
                    .any(|r| r.name == failing && r.result.is_err()),
                "{failing}: {:?}",
                composed.reads
            );
            // The failed read does not discard the others' scores.
            assert!(composed.reads.iter().any(|r| r.result.is_ok()));
            assert_eq!(adjudication(composed).decision, Decision::Deny);
        }
    }
}

#[tokio::test]
async fn without_an_allow_threshold_only_the_stop_read_is_made() {
    let (_endpoint, guardian) = slow_guardian(allow(0.95), Duration::ZERO).await;
    let server = screen_server(0.1, 0.1, 0.1, Duration::ZERO).await;
    let composed = run(
        &preset(&server, &[("stop_threshold", number(0.9))], false),
        &guardian,
    )
    .await;
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    assert!(composed.score("stop").is_some() && composed.score("allow").is_none());
}

// ---------------------------------------------------------------- shadow

#[tokio::test]
async fn a_shadow_allow_or_stop_leaves_the_call_to_the_guardian() {
    for scores in [SAFE, RISKY] {
        let server = screen_server(scores.0, scores.1, scores.2, Duration::ZERO).await;
        let (endpoint, guardian) =
            slow_guardian(deny("Data Exfiltration"), Duration::from_millis(300)).await;
        let composed = run(&preset(&server, &both(), true), &guardian).await;
        assert_eq!(composed.dropped_input_tokens, None);
        assert!(matches!(composed.outcome, Outcome::Allow | Outcome::Stop));
        // The scores of every read are logged.
        assert!(composed.score("stop").is_some());
        assert!(composed.score("allow").is_some());
        assert!(composed.score("steering").is_some());
        assert_eq!(adjudication(composed).decision, Decision::Deny);
        assert_eq!(endpoint.calls().await, 1);
    }
}

#[tokio::test]
async fn shadow_mode_starts_every_declared_read_once_even_when_the_script_reads_few() {
    let (endpoint, guardian) = slow_guardian(allow(0.95), Duration::ZERO).await;
    let server = screen_server(0.5, 0.5, 0.5, Duration::ZERO).await;
    // The script reads one name and never names the LLM.
    let composition = script(
        &server,
        r#"pub async fn guardian(call, params) { read(call, "a").await; abstain() }"#,
        true,
    );
    let composed = run(&composition, &guardian).await;
    assert_eq!(composed.reads.len(), 5);
    assert_eq!(server.received_requests().await.unwrap().len(), 5);
    assert_eq!(endpoint.calls().await, 1);
    // The abstain would have been a deny; the LLM decides anyway.
    assert!(matches!(composed.outcome, Outcome::Failed(_)));
    assert_eq!(adjudication(composed).decision, Decision::Allow);
}

// ---------------------------------------------------------------- the host module

fn compile_error(source: &str) -> String {
    let mut spec = script_spec(source, false);
    spec.reads.clear();
    Composition::compile(spec, vec![])
        .err()
        .unwrap_or_else(|| panic!("`{source}` compiled"))
        .to_string()
}

#[test]
fn a_script_cannot_build_a_verdict_a_score_or_a_call() {
    for body in [
        "Verdict { kind: 1 }",
        "Verdict::Allow",
        "Verdict::new()",
        "Score { value: 0.0 }",
        "Call { ctx: 1 }",
        "allow(call)",
        "undeclared_function(call)",
    ] {
        let source = format!("pub async fn guardian(call, params) {{ {body} }}");
        let message = compile_error(&source);
        assert!(message.contains("does not compile"), "{body}: {message}");
    }
}

#[test]
fn a_script_must_define_guardian_with_two_parameters() {
    for source in [
        "pub async fn other(call, params) { abstain() }",
        "pub async fn guardian(call) { abstain() }",
        "pub async fn guardian(call, params, more) { abstain() }",
    ] {
        let message = compile_error(source);
        assert!(
            message.contains("must define `pub async fn guardian"),
            "{message}"
        );
    }
}

async fn verdict_of_script(source: &str) -> Composed {
    let server = idle_server().await;
    let (_endpoint, guardian) = slow_guardian(allow(0.95), Duration::ZERO).await;
    run(&script(&server, source, false), &guardian).await
}

fn fail_closed(composed: &Composed) -> Option<FailClosed> {
    match &composed.decided {
        Decided::Deny { fail_closed, .. } => *fail_closed,
        other => panic!("expected a deny, got {other:?}"),
    }
}

#[tokio::test]
async fn a_return_value_that_is_not_a_verdict_is_a_deny() {
    for body in ["\"allow\"", "()", "1", "true", "#{}"] {
        let composed =
            verdict_of_script(&format!("pub async fn guardian(call, params) {{ {body} }}")).await;
        assert_eq!(fail_closed(&composed), Some(FailClosed::Error), "{body}");
        assert!(matches!(composed.outcome, Outcome::Failed(_)));
        assert!(composed.unavailable());
    }
}

#[tokio::test]
async fn allow_below_at_the_threshold_abstains_and_the_root_denies() {
    let composed = verdict_of_script(
        r#"pub async fn guardian(call, params) {
            let score = read(call, "a").await.unwrap();
            let v = allow_below("n", score, score.value());
            if v.is_abstain() { deny("below", "at the threshold") } else { v }
        }"#,
    )
    .await;
    assert!(
        matches!(&composed.decided, Decided::Deny { node, fail_closed: None, .. } if node == "below")
    );
    assert_eq!(composed.outcome, Outcome::Deny);
}

#[tokio::test]
async fn allow_below_under_the_threshold_allows() {
    let composed = verdict_of_script(
        r#"pub async fn guardian(call, params) {
            allow_below("n", read(call, "a").await.unwrap(), 0.9)
        }"#,
    )
    .await;
    assert!(
        matches!(&composed.decided, Decided::Allow { node, threshold, .. } if node == "n" && *threshold == 0.9)
    );
}

#[tokio::test]
async fn reading_an_undeclared_name_is_an_error() {
    let composed = verdict_of_script(
        r#"pub async fn guardian(call, params) {
            match read(call, "nope").await {
                Ok(_) => abstain(),
                Err(reason) => deny("undeclared", reason),
            }
        }"#,
    )
    .await;
    let Decided::Deny {
        reason,
        fail_closed: None,
        ..
    } = &composed.decided
    else {
        panic!("{:?}", composed.decided);
    };
    assert!(reason.contains("`nope` is not a declared read"), "{reason}");
}

#[tokio::test]
async fn the_call_shows_the_tool_and_whether_it_is_destructive_only() {
    let composed = verdict_of_script(
        r#"pub async fn guardian(call, params) {
            if call.tool() == "shell" && !call.destructive() { deny("facts", "shell") } else { abstain() }
        }"#,
    )
    .await;
    assert!(matches!(&composed.decided, Decided::Deny { node, .. } if node == "facts"));
}

#[tokio::test]
async fn params_reach_the_script_as_typed_values() {
    let server = idle_server().await;
    let (_endpoint, guardian) = slow_guardian(allow(0.95), Duration::ZERO).await;
    let mut spec = script_spec(
        r#"pub async fn guardian(call, params) {
            if params.n == 1.5 && params.s == "x" && params.b { match params.unset { None => deny("params", "ok"), _ => abstain() } } else { abstain() }
        }"#,
        false,
    );
    spec.params = params_of(&[
        ("n", number(1.5)),
        ("s", Param::Str("x".into())),
        ("b", Param::Bool(true)),
        ("unset", Param::Unset),
    ]);
    let composition = Arc::new(
        Composition::compile(
            spec,
            vec![(MODEL.into(), reader_at(&server, Duration::from_secs(1)))],
        )
        .unwrap(),
    );
    let composed = run(&composition, &guardian).await;
    assert!(
        matches!(&composed.decided, Decided::Deny { node, .. } if node == "params"),
        "{:?}",
        composed.decided
    );
}

// ---------------------------------------------------------------- the root

#[tokio::test]
async fn an_infinite_loop_exhausts_the_budget_and_denies() {
    let composed = verdict_of_script("pub async fn guardian(call, params) { loop {} }").await;
    assert_eq!(fail_closed(&composed), Some(FailClosed::Error));
}

#[tokio::test]
async fn a_script_error_denies() {
    let composed =
        verdict_of_script("pub async fn guardian(call, params) { let x = 1 / 0; abstain() }").await;
    assert_eq!(fail_closed(&composed), Some(FailClosed::Error));
    let composed =
        verdict_of_script(r#"pub async fn guardian(call, params) { panic!("no") }"#).await;
    assert_eq!(fail_closed(&composed), Some(FailClosed::Error));
}

#[tokio::test]
async fn a_root_abstain_is_a_deny() {
    let composed = verdict_of_script("pub async fn guardian(call, params) { abstain() }").await;
    assert_eq!(fail_closed(&composed), Some(FailClosed::Unparseable));
    assert!(matches!(composed.outcome, Outcome::Failed(_)));
}

#[tokio::test]
async fn read_stages_chained_past_the_root_bound_are_a_timeout_deny() {
    let server = screen_server(0.5, 0.5, 0.5, Duration::from_millis(1_100)).await;
    let endpoint = Endpoint::start(vec![Reply::Verdict(allow(0.95))]).await;
    let guardian = Arc::new(endpoint.guardian(fontaine_guardian::Settings {
        timeout: Duration::from_millis(100),
        ..settings()
    }));
    // The bound is the guardian's 0.1 s, one read's 1.5 s and the slack: 3.6 s at most, under the
    // four stages' 4.4 s, and every stage is within its own bound.
    let composition = script_timeout(
        &server,
        r#"pub async fn guardian(call, params) {
            read(call, "a").await;
            read(call, "b").await;
            read(call, "c").await;
            read(call, "d").await;
            llm(call).await
        }"#,
        false,
        Duration::from_millis(1_500),
    );
    let started = Instant::now();
    let composed = run(&composition, &guardian).await;
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(fail_closed(&composed), Some(FailClosed::Timeout));
    assert_eq!(endpoint.calls().await, 0);
}

#[tokio::test]
async fn a_screen_allow_never_beats_a_guardian_denial_in_hand() {
    let server = idle_server().await;
    let endpoint = Endpoint::start(vec![Reply::Verdict(deny("Data Exfiltration"))]).await;
    let guardian = guardian_of(&endpoint);
    let composed = run(
        &script(
            &server,
            r#"pub async fn guardian(call, params) {
                llm(call).await;
                allow_below("n", read(call, "a").await.unwrap(), 2.0)
            }"#,
            false,
        ),
        &guardian,
    )
    .await;
    assert_eq!(composed.outcome, Outcome::Allow);
    assert!(composed.llm_usage.is_none());
    assert_eq!(adjudication(composed).decision, Decision::Deny);
}

#[tokio::test]
async fn a_script_escalate_beats_a_guardian_allow_in_hand() {
    let server = idle_server().await;
    let endpoint = Endpoint::start(vec![Reply::Verdict(allow(0.95))]).await;
    let guardian = guardian_of(&endpoint);
    let composed = run(
        &script(
            &server,
            r#"pub async fn guardian(call, params) { llm(call).await; escalate("late", "no") }"#,
            false,
        ),
        &guardian,
    )
    .await;
    assert!(matches!(&composed.decided, Decided::Escalate { node, .. } if node == "late"));
    // The finished request's spend is handed back, not lost.
    assert!(
        composed
            .llm_usage
            .is_some_and(|u| u.input_tokens.unwrap_or(0) > 0)
    );
    assert_eq!(composed.dropped_input_tokens, None);
}

#[tokio::test]
async fn a_guardian_refusal_in_hand_beats_any_script_verdict() {
    for body in [
        r#"escalate("late", "no")"#,
        r#"deny("late", "no")"#,
        "abstain()",
    ] {
        for reply in [
            deny("Data Exfiltration"),
            common::verdict("escalate", "Destroy Pre-existing Files", 0.6),
        ] {
            let server = idle_server().await;
            let endpoint = Endpoint::start(vec![Reply::Verdict(reply.clone())]).await;
            let guardian = guardian_of(&endpoint);
            let composed = run(
                &script(
                    &server,
                    &format!("pub async fn guardian(call, params) {{ llm(call).await; {body} }}"),
                    false,
                ),
                &guardian,
            )
            .await;
            assert!(composed.llm_usage.is_none());
            let expected = if reply["decision"] == "deny" {
                Decision::Deny
            } else {
                Decision::Escalate
            };
            assert_eq!(adjudication(composed).decision, expected, "{body}");
        }
    }
}

// ---------------------------------------------------------------- concurrency and cost

#[tokio::test]
async fn first_decisive_skips_abstains_and_list_order_wins_a_tie() {
    let composed = verdict_of_script(
        r#"pub async fn guardian(call, params) {
            first_decisive([
                async { abstain() },
                async { escalate("first", "x") },
                async { deny("second", "y") },
            ]).await
        }"#,
    )
    .await;
    assert!(matches!(&composed.decided, Decided::Escalate { node, .. } if node == "first"));
    let composed = verdict_of_script(
        r#"pub async fn guardian(call, params) {
            first_decisive([async { abstain() }, async { abstain() }]).await
        }"#,
    )
    .await;
    assert_eq!(fail_closed(&composed), Some(FailClosed::Unparseable));
}

#[tokio::test]
async fn the_llm_named_twice_sends_one_request() {
    let server = idle_server().await;
    let endpoint = Endpoint::start(vec![Reply::Verdict(allow(0.95))]).await;
    let guardian = guardian_of(&endpoint);
    let composed = run(
        &script(
            &server,
            r#"pub async fn guardian(call, params) {
                let first = llm(call).await;
                let second = llm(call).await;
                if first.is_allow() && second.is_allow() { llm(call).await } else { deny("x", "y") }
            }"#,
            false,
        ),
        &guardian,
    )
    .await;
    assert_eq!(endpoint.calls().await, 1);
    assert_eq!(adjudication(composed).decision, Decision::Allow);
}

#[tokio::test]
async fn a_read_is_made_once_however_often_it_is_named() {
    let (_endpoint, guardian) = slow_guardian(allow(0.95), Duration::ZERO).await;
    let server = idle_server().await;
    let composed = run(
        &script(
            &server,
            r#"pub async fn guardian(call, params) {
                let x = read(call, "a").await.unwrap();
                let y = read(call, "a").await.unwrap();
                if x.value() == y.value() { llm(call).await } else { deny("x", "y") }
            }"#,
            false,
        ),
        &guardian,
    )
    .await;
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    assert_eq!(composed.reads.len(), 1);
}

#[tokio::test]
async fn every_started_read_is_recorded_even_after_first_decisive_settled() {
    let (endpoint, guardian) = slow_guardian(allow(0.95), Duration::from_secs(3)).await;
    let server = screen_server(0.5, 0.5, 0.5, Duration::from_millis(400)).await;
    let composed = run(
        &script(
            &server,
            r#"pub async fn guardian(call, params) {
                first_decisive([
                    async { deny("fast", "x") },
                    async { read(call, "a").await; abstain() },
                    async { read(call, "b").await; abstain() },
                ]).await
            }"#,
            false,
        ),
        &guardian,
    )
    .await;
    // The decisive verdict is ready on the first poll; the reads were started by then or never.
    assert!(matches!(
        composed.decided,
        Decided::Deny {
            fail_closed: None,
            ..
        }
    ));
    for record in &composed.reads {
        assert!(record.result.is_ok(), "{record:?}");
    }
    assert_eq!(endpoint.calls().await, 0);

    // A raced read that loses is still awaited and recorded.
    let composed = run(
        &script(
            &server,
            r#"pub async fn guardian(call, params) {
                let slow = read(call, "a");
                first_decisive([async { slow.await; abstain() }, async { deny("fast", "x") }]).await
            }"#,
            false,
        ),
        &guardian,
    )
    .await;
    assert!(matches!(composed.decided, Decided::Deny { .. }));
}

#[tokio::test]
async fn sequential_reads_take_their_turns_and_join_runs_them_together() {
    let (_endpoint, guardian) = slow_guardian(allow(0.95), Duration::ZERO).await;
    let server = screen_server(0.5, 0.5, 0.5, Duration::from_millis(300)).await;

    let started = Instant::now();
    let composed = run(
        &script(
            &server,
            r#"pub async fn guardian(call, params) {
                read(call, "a").await;
                read(call, "b").await;
                read(call, "c").await;
                abstain()
            }"#,
            false,
        ),
        &guardian,
    )
    .await;
    assert!(
        started.elapsed() >= Duration::from_millis(900),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(
        composed
            .reads
            .iter()
            .map(|r| r.name.as_str())
            .collect::<Vec<_>>(),
        ["a", "b", "c"]
    );

    let started = Instant::now();
    run(
        &script(
            &server,
            r#"pub async fn guardian(call, params) {
                std::future::join((read(call, "a"), read(call, "b"), read(call, "c"))).await;
                abstain()
            }"#,
            false,
        ),
        &guardian,
    )
    .await;
    assert!(
        started.elapsed() < Duration::from_millis(850),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn reads_started_by_a_script_are_not_cancelled_by_dropping_their_future() {
    let (_endpoint, guardian) = slow_guardian(allow(0.95), Duration::ZERO).await;
    let server = screen_server(0.5, 0.5, 0.5, Duration::from_millis(200)).await;
    let composed = run(
        &script(
            &server,
            r#"pub async fn guardian(call, params) {
                first_decisive([async { deny("x", "y") }, async { read(call, "a").await; abstain() }]).await
            }"#,
            false,
        ),
        &guardian,
    )
    .await;
    assert!(matches!(composed.decided, Decided::Deny { .. }));
    assert!(composed.reads.iter().all(|r| r.result.is_ok()));
}

// ---------------------------------------------------------------- outcomes

#[tokio::test]
async fn the_outcome_follows_the_origin_of_the_verdict() {
    let cases = [
        (r#"escalate("n", "r")"#, Outcome::Stop),
        (r#"deny("n", "r")"#, Outcome::Deny),
        (
            r#"allow_below("n", read(call, "a").await.unwrap(), 2.0)"#,
            Outcome::Allow,
        ),
        // An LLM allow returned by a sequential script is a pass, not an allow.
        ("llm(call).await", Outcome::Pass),
        (
            "let v = abstain(); if v.is_abstain() { llm(call).await } else { v }",
            Outcome::Pass,
        ),
    ];
    for (body, expected) in cases {
        let composed =
            verdict_of_script(&format!("pub async fn guardian(call, params) {{ {body} }}")).await;
        assert_eq!(composed.outcome, expected, "{body}");
    }
}

#[tokio::test]
async fn an_llm_that_wins_first_decisive_records_a_pass() {
    let server = screen_server(0.5, 0.5, 0.5, Duration::from_millis(400)).await;
    let (_endpoint, guardian) = slow_guardian(allow(0.95), Duration::ZERO).await;
    let composed = run(
        &script(
            &server,
            r#"pub async fn guardian(call, params) {
                first_decisive([llm(call), async { read(call, "a").await.unwrap(); escalate("n", "r") }]).await
            }"#,
            false,
        ),
        &guardian,
    )
    .await;
    assert_eq!(composed.outcome, Outcome::Pass);
    // Its reads are still awaited and recorded.
    assert_eq!(composed.reads.len(), 1);
    assert!(composed.reads[0].result.is_ok());
}

#[tokio::test]
async fn a_root_timeout_with_the_request_in_flight_counts_the_dropped_spend() {
    let server = screen_server(0.5, 0.5, 0.5, Duration::from_millis(1_500)).await;
    let endpoint = Endpoint::start(vec![Reply::Slow(Duration::from_secs(60), allow(0.95))]).await;
    let guardian = Arc::new(endpoint.guardian(fontaine_guardian::Settings {
        timeout: Duration::from_secs(1),
        ..settings()
    }));
    // The bound is 1 s + the 2 s read bound + the 2 s slack = 5 s. The reads take 4.5 s, so the
    // request starts late enough that its own 1 s limit would end after the root's.
    let composition = script_timeout(
        &server,
        r#"pub async fn guardian(call, params) {
            read(call, "a").await;
            read(call, "b").await;
            read(call, "c").await;
            llm(call).await
        }"#,
        false,
        Duration::from_secs(2),
    );
    let composed = run(&composition, &guardian).await;
    assert_eq!(fail_closed(&composed), Some(FailClosed::Timeout));
    assert_eq!(
        composed.dropped_input_tokens,
        Some(guardian.dropped_estimate(&request("ls", false)))
    );
    assert_eq!(composed.reads.len(), 3);
}

#[tokio::test]
async fn the_documented_example_races_a_read_against_the_guardian() {
    let source = r#"pub async fn guardian(call, params) {
    let outside = async {
        match read(call, "outside").await {
            Ok(score) if score.value() >= params.limit => {
                escalate("outside.stop", format!("risk {:.2}", score.value()))
            }
            _ => abstain(),
        }
    };
    first_decisive([llm(call), outside]).await
}"#;
    for (risk, slow_guardian_ms, expect_escalate) in [(0.99, 800, true), (0.3, 0, false)] {
        let server = screen_server(1.0 - risk, 0.5, 0.5, Duration::ZERO).await;
        let endpoint = Endpoint::start(vec![Reply::Slow(
            Duration::from_millis(slow_guardian_ms),
            allow(0.95),
        )])
        .await;
        let guardian = guardian_of(&endpoint);
        let mut spec = script_spec(source, false);
        spec.reads = BTreeMap::from([(
            "outside".into(),
            read(Prompt::Plain, Question::EmployeeAllowed),
        )]);
        spec.params = params_of(&[("limit", number(0.95))]);
        let composition = Arc::new(
            Composition::compile(
                spec,
                vec![(MODEL.into(), reader_at(&server, Duration::from_secs(5)))],
            )
            .unwrap(),
        );
        let composed = run(&composition, &guardian).await;
        if expect_escalate {
            assert!(
                matches!(&composed.decided, Decided::Escalate { node, reason } if node == "outside.stop" && reason == "risk 0.99"),
                "{:?}",
                composed.decided
            );
        } else {
            assert_eq!(adjudication(composed).decision, Decision::Allow);
        }
    }
}

#[tokio::test]
async fn an_unavailable_guardian_denies_even_when_the_screen_would_allow() {
    let server = screen_server(0.0, 1.0, 0.0, Duration::ZERO).await;
    let guardian = Arc::new(Guardian::unavailable("mock/none", "no key", settings()));
    let composed = run(&preset(&server, &both(), false), &guardian).await;
    assert!(composed.unavailable());
    let adjudication = adjudication(composed);
    assert_eq!(adjudication.decision, Decision::Deny);
    assert_eq!(adjudication.fail_closed, Some(FailClosed::Unavailable));
}
