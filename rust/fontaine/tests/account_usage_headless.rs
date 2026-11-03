//! `fontaine -p --output-format stream-json`: the `result` event reports the usage window of each
//! account that has a quota source.

mod common;

use std::path::Path;

use common::{Endpoint, Sandbox, Turn, Wire};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

fn validator() -> jsonschema::Validator {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/schemas/headless-v1.schema.json");
    let schema: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    jsonschema::validator_for(&schema).unwrap()
}

fn assert_valid(value: &Value) {
    let errors: Vec<String> = validator()
        .iter_errors(value)
        .map(|e| format!("{} at {}", e, e.instance_path()))
        .collect();
    assert!(errors.is_empty(), "{value}\n{}", errors.join("\n"));
}

async fn serve_usages(endpoint: &Endpoint, status: u16, month: f64) {
    Mock::given(method("GET"))
        .and(path("/usages"))
        .respond_with(ResponseTemplate::new(status).set_body_json(json!({
            "usages": {
                "limit_month_total": {"used_ratio": month, "reset_time": "2030-01-01T00:00:00Z"},
                "limit_5h": {"used_ratio": 0.82},
            }
        })))
        .mount(&endpoint.server)
        .await;
}

fn config(agent: &Endpoint, judge: &Endpoint, agent_quota: bool, judge_quota: bool) -> String {
    let quota = |on| if on { "quota = \"kimi\"\n" } else { "" };
    format!(
        "[accounts.agent]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\nmodels = [\"scripted\"]\n{}\n[accounts.judge]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\nmodels = [\"scripted\"]\n{}\n[model]\nmodel = \"agent/scripted\"\n\n[guardian]\nmodel = \"judge/scripted\"\n",
        agent.url(),
        quota(agent_quota),
        judge.url(),
        quota(judge_quota),
    )
}

async fn result_of(s: &Sandbox) -> (Value, String) {
    let out = s.run(&["-p", "--output-format", "stream-json", "go"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let result = out
        .lines()
        .into_iter()
        .find(|l| l["type"] == "result")
        .expect("a result event");
    assert_valid(&result);
    (result, out.stderr)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_result_reports_main_and_guardian_usage() {
    let agent = Endpoint::start(Wire::Chat, vec![Turn::text("done")]).await;
    let judge = Endpoint::start(Wire::Chat, vec![]).await;
    serve_usages(&agent, 200, 0.164).await;
    serve_usages(&judge, 200, 0.5).await;
    let s = Sandbox::new();
    s.user_config(&config(&agent, &judge, true, true));

    let (result, _) = result_of(&s).await;
    assert_eq!(
        result["account_usage"],
        json!([
            {
                "role": "main", "account": "agent", "label": "kimi",
                "windows": [
                    {"label": "month", "used_percentage": 16, "resets_at": 1_893_456_000u64},
                    {"label": "5h", "used_percentage": 82},
                ],
            },
            {
                "role": "guardian", "account": "judge", "label": "kimi",
                "windows": [
                    {"label": "month", "used_percentage": 50, "resets_at": 1_893_456_000u64},
                    {"label": "5h", "used_percentage": 82},
                ],
            },
        ])
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_account_without_a_source_has_no_entry_and_none_means_no_field() {
    let agent = Endpoint::start(Wire::Chat, vec![Turn::text("done")]).await;
    let judge = Endpoint::start(Wire::Chat, vec![]).await;
    serve_usages(&judge, 200, 0.5).await;
    let s = Sandbox::new();
    s.user_config(&config(&agent, &judge, false, true));
    let (result, _) = result_of(&s).await;
    let entries = result["account_usage"].as_array().expect("one entry");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["role"], "guardian");

    s.user_config(&config(&agent, &judge, false, false));
    let (result, _) = result_of(&s).await;
    assert!(result.get("account_usage").is_none(), "{result}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_fetch_gives_an_entry_without_windows_and_one_warning() {
    let agent = Endpoint::start(Wire::Chat, vec![Turn::text("done")]).await;
    let judge = Endpoint::start(Wire::Chat, vec![]).await;
    serve_usages(&agent, 500, 0.1).await;
    let s = Sandbox::new();
    s.user_config(&config(&agent, &judge, true, false));
    let (result, stderr) = result_of(&s).await;
    assert_eq!(
        result["account_usage"],
        json!([{"role": "main", "account": "agent", "label": "kimi"}])
    );
    assert_eq!(
        stderr.matches("could not read the kimi quota").count(),
        1,
        "{stderr}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_guardian_on_the_main_account_is_not_reported_twice() {
    let agent = Endpoint::start(Wire::Chat, vec![Turn::text("done")]).await;
    serve_usages(&agent, 200, 0.2).await;
    let s = Sandbox::new();
    s.user_config(&format!(
        "[accounts.agent]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\nmodels = [\"scripted\"]\nquota = \"kimi\"\n\n[model]\nmodel = \"agent/scripted\"\n\n[guardian]\nmodel = \"agent/scripted\"\n",
        agent.url()
    ));
    let (result, _) = result_of(&s).await;
    let entries = result["account_usage"].as_array().expect("one entry");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["role"], "main");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_fetch_warns_once_per_account_across_turns() {
    let agent = Endpoint::start(
        Wire::Chat,
        vec![Turn::text("first answer"), Turn::text("second answer")],
    )
    .await;
    let judge = Endpoint::start(Wire::Chat, vec![]).await;
    serve_usages(&agent, 500, 0.1).await;
    let s = Sandbox::new();
    s.user_config(&config(&agent, &judge, true, false));
    let stdin: String = ["first question", "second question"]
        .iter()
        .map(|q| {
            format!(
                "{}\n",
                json!({"type": "user", "message": {"role": "user", "content": q}})
            )
        })
        .collect();
    let out = s
        .run_with_stdin(
            &[
                "-p",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
            ],
            &stdin,
        )
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    let results: Vec<&Value> = lines.iter().filter(|l| l["type"] == "result").collect();
    assert_eq!(results.len(), 2);
    for result in results {
        assert_valid(result);
        assert_eq!(
            result["account_usage"],
            json!([{"role": "main", "account": "agent", "label": "kimi"}])
        );
    }
    assert_eq!(
        out.stderr.matches("could not read the kimi quota").count(),
        1,
        "{}",
        out.stderr
    );
}
