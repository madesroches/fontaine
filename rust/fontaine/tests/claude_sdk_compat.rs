//! The unmodified Claude Agent SDKs (TypeScript and Python) driving the `fontaine` binary against a
//! scripted model. Each test runs one consumer program from `sdk_compat/` and asserts on the JSON
//! report it prints and on the audit log.
//!
//! Without `node`, `python3` or the pinned packages a test prints a skip notice, unless
//! `FONTAINE_SDK_COMPAT=required` (the `sdk-compat` CI job), in which case it fails.

mod common;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use common::{Endpoint, Route, Sandbox, Turn, Wire};
use serde_json::{Value, json};

#[derive(Clone, Copy, PartialEq)]
enum Lang {
    Ts,
    Py,
}

impl Lang {
    fn dir(self) -> PathBuf {
        let sub = if self == Lang::Ts { "ts" } else { "py" };
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("sdk_compat")
            .join(sub)
    }

    fn python(self) -> PathBuf {
        self.dir().join(".venv").join("bin").join("python")
    }

    /// Why the consumer programs cannot run here, if they cannot.
    fn unavailable(self) -> Option<String> {
        let probe = |program: &str| {
            std::process::Command::new(program)
                .arg("--version")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        match self {
            Lang::Ts => {
                if !probe("node") {
                    return Some("node is not installed".into());
                }
                let sdk = self
                    .dir()
                    .join("node_modules/@anthropic-ai/claude-agent-sdk");
                (!sdk.is_dir()).then(|| format!("run `npm ci` in {}", self.dir().display()))
            }
            Lang::Py => {
                let python = self.python();
                let imports = std::process::Command::new(&python)
                    .args(["-c", "import claude_agent_sdk"])
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false);
                (!imports).then(|| {
                    format!(
                        "create {}/.venv and `pip install --require-hashes -r requirements.txt`",
                        self.dir().display()
                    )
                })
            }
        }
    }
}

/// `true` when the test can run; otherwise the skip notice, or a failure when the job requires it.
fn ready(lang: Lang) -> bool {
    let Some(why) = lang.unavailable() else {
        return true;
    };
    if std::env::var("FONTAINE_SDK_COMPAT").as_deref() == Ok("required") {
        panic!("FONTAINE_SDK_COMPAT=required, but the SDK consumer programs cannot run: {why}");
    }
    eprintln!("skipping the Agent SDK conformance test: {why}");
    false
}

/// Runs a consumer program in the sandbox's workspace and returns the report it prints
/// (`null` when it printed none).
async fn run_program(s: &Sandbox, lang: Lang, program: &str, env: &[(&str, &str)]) -> Value {
    let mut cmd = match lang {
        Lang::Ts => {
            let mut c = tokio::process::Command::new("node");
            c.arg(lang.dir().join(format!("{program}.mjs")));
            c
        }
        Lang::Py => {
            let mut c = tokio::process::Command::new(lang.python());
            c.arg(lang.dir().join(format!("{program}.py")));
            c
        }
    };
    // The SDK passes its environment on to the executable: the sandbox's reaches `fontaine`.
    let sandbox = s.command(&[]);
    for (name, value) in sandbox.as_std().get_envs() {
        match value {
            Some(value) => cmd.env(name, value),
            None => cmd.env_remove(name),
        };
    }
    cmd.current_dir(&s.work)
        .env("FONTAINE_BIN", env!("CARGO_BIN_EXE_fontaine"))
        .env("CLAUDE_AGENT_SDK_SKIP_VERSION_CHECK", "1")
        .kill_on_drop(true)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (name, value) in env {
        cmd.env(name, value);
    }
    let out = tokio::time::timeout(Duration::from_secs(180), cmd.output())
        .await
        .expect("the consumer program timed out")
        .expect("the consumer program starts");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'));
    match line {
        Some(line) => serde_json::from_str(line).unwrap_or_else(|e| {
            panic!(
                "report `{line}`: {e}\n{}",
                String::from_utf8_lossy(&out.stderr)
            )
        }),
        None => Value::Null,
    }
}

/// The audit records of every session in the sandbox.
fn audit(s: &Sandbox) -> Vec<Value> {
    let dir = s.data.join("audit");
    let mut records = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return records;
    };
    for entry in entries.flatten() {
        let text = std::fs::read_to_string(entry.path()).unwrap_or_default();
        records.extend(
            text.lines()
                .filter_map(|l| serde_json::from_str::<Value>(l).ok()),
        );
    }
    records
}

fn decisions<'a>(records: &'a [Value], tool: &str) -> Vec<&'a Value> {
    records
        .iter()
        .filter(|r| r["tool"] == tool && r.get("decision").is_some())
        .collect()
}

fn shell(command: &str) -> Turn {
    Turn::tool("c1", "shell", json!({ "command": command }))
}

async fn sandbox(turns: Vec<Turn>) -> (Endpoint, Sandbox) {
    let endpoint = Endpoint::start(Wire::Chat, turns).await;
    let s = Sandbox::compatible(&endpoint.url());
    (endpoint, s)
}

macro_rules! both {
    ($name:ident, $body:ident) => {
        mod $name {
            use super::*;

            #[tokio::test(flavor = "multi_thread")]
            async fn typescript() {
                if ready(Lang::Ts) {
                    $body(Lang::Ts).await;
                }
            }

            #[tokio::test(flavor = "multi_thread")]
            async fn python() {
                if ready(Lang::Py) {
                    $body(Lang::Py).await;
                }
            }
        }
    };
}

macro_rules! ts_only {
    ($name:ident, $body:ident) => {
        #[tokio::test(flavor = "multi_thread")]
        async fn $name() {
            if ready(Lang::Ts) {
                $body(Lang::Ts).await;
            }
        }
    };
}

/// The result message of a report: the TypeScript programs keep it under `result`, the Python ones
/// among their `messages`.
fn result_of(report: &Value) -> &Value {
    if !report["result"].is_null() {
        return &report["result"];
    }
    report["messages"]
        .as_array()
        .and_then(|messages| {
            messages
                .iter()
                .rev()
                .find(|m| m["_type"] == "ResultMessage")
        })
        .unwrap_or(&Value::Null)
}

fn result_text(report: &Value) -> &str {
    result_of(report)["result"].as_str().unwrap_or_default()
}

async fn one_shot_query_streams_a_result(lang: Lang) {
    let (endpoint, s) = sandbox(vec![Turn::text("Hello from the mock")]).await;
    let report = run_program(
        &s,
        lang,
        "query_stream",
        &[("SCENARIO_PROMPT", "say hello")],
    )
    .await;
    assert_eq!(result_of(&report)["subtype"], "success", "{report}");
    assert_eq!(result_text(&report), "Hello from the mock", "{report}");
    let kinds = report["kinds"].to_string();
    assert!(
        kinds.contains("system") || kinds.contains("SystemMessage"),
        "{kinds}"
    );
    assert!(
        kinds.contains("stream_event") || kinds.contains("StreamEvent"),
        "{kinds}"
    );
    if lang == Lang::Ts {
        assert_eq!(report["init"]["compat_protocol"], "claude-sdk-1");
        assert!(
            report["stream_events"]
                .to_string()
                .contains("message_start")
        );
        assert!(report["result"]["modelUsage"].is_object());
        assert_eq!(report["result"]["permission_denials"], json!([]));
    }
    assert_eq!(endpoint.requests().await.len(), 1);
}
both!(one_shot_query, one_shot_query_streams_a_result);

async fn sequential_messages_each_get_a_result(lang: Lang) {
    let (_endpoint, s) = sandbox(vec![Turn::text("one"), Turn::text("two")]).await;
    let report = run_program(&s, lang, "streaming_input", &[]).await;
    let results = report["results"]
        .as_array()
        .unwrap_or_else(|| panic!("{report}"));
    assert_eq!(results.len(), 2, "{report}");
    assert_eq!(results[0]["result"], "one");
    assert_eq!(results[1]["result"], "two");
    assert_eq!(results[0]["session_id"], results[1]["session_id"]);
}
both!(sequential_input, sequential_messages_each_get_a_result);

async fn continue_resume_and_fork_keep_fontaine_sessions(lang: Lang) {
    let (_endpoint, s) = sandbox(vec![
        Turn::text("ok"),
        Turn::text("pelican"),
        Turn::text("pelican"),
        Turn::text("pelican"),
    ])
    .await;
    let report = run_program(&s, lang, "sessions", &[]).await;
    let id = |name: &str| {
        report[name]["session_id"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    };
    assert_eq!(report["first"]["subtype"], "success", "{report}");
    assert_eq!(id("continued"), id("first"), "{report}");
    assert_eq!(id("resumed"), id("first"), "{report}");
    assert_ne!(id("forked"), id("first"), "{report}");
}
both!(sessions, continue_resume_and_fork_keep_fontaine_sessions);

async fn the_callback_decides_a_call_that_would_prompt(lang: Lang) {
    let (_endpoint, s) = sandbox(vec![shell("touch allowed.txt"), Turn::text("done")]).await;
    let report = run_program(
        &s,
        lang,
        "can_use_tool",
        &[("SCENARIO_MODE", "allow"), ("SCENARIO_PROMPT", "run it")],
    )
    .await;
    let calls = report["calls"]
        .as_array()
        .unwrap_or_else(|| panic!("{report}"));
    assert_eq!(calls.len(), 1, "{report}");
    assert_eq!(calls[0]["tool_name"], "Bash");
    assert_eq!(calls[0]["input"]["command"], "touch allowed.txt");
    assert!(s.work.join("allowed.txt").exists());
    let records = audit(&s);
    let shell_decisions = decisions(&records, "shell");
    assert!(
        shell_decisions
            .iter()
            .any(|r| r["human_channel"] == "delegate"),
        "{shell_decisions:?}"
    );
}
both!(
    can_use_tool_allow,
    the_callback_decides_a_call_that_would_prompt
);

async fn a_callback_deny_is_a_denial(lang: Lang) {
    let (_endpoint, s) = sandbox(vec![shell("touch denied.txt"), Turn::text("done")]).await;
    let report = run_program(
        &s,
        lang,
        "can_use_tool",
        &[("SCENARIO_MODE", "deny"), ("SCENARIO_PROMPT", "run it")],
    )
    .await;
    assert_eq!(
        report["calls"].as_array().map(Vec::len),
        Some(1),
        "{report}"
    );
    assert!(!s.work.join("denied.txt").exists());
    assert_eq!(result_text(&report), "done");
}
both!(can_use_tool_deny, a_callback_deny_is_a_denial);

async fn modified_input_is_judged_again_and_runs(lang: Lang) {
    let (_endpoint, s) = sandbox(vec![shell("touch original.txt"), Turn::text("done")]).await;
    run_program(
        &s,
        lang,
        "can_use_tool",
        &[
            ("SCENARIO_MODE", "modify"),
            ("SCENARIO_PROMPT", "run it"),
            (
                "SCENARIO_UPDATED_INPUT",
                r#"{"command":"touch modified.txt"}"#,
            ),
        ],
    )
    .await;
    assert!(s.work.join("modified.txt").exists());
    assert!(!s.work.join("original.txt").exists());
}
both!(can_use_tool_modify, modified_input_is_judged_again_and_runs);

async fn a_session_allow_spares_the_next_matching_call(lang: Lang) {
    let (_endpoint, s) = sandbox(vec![
        Turn::tool("c1", "shell", json!({"command": "echo first"})),
        Turn::tool("c2", "shell", json!({"command": "echo second"})),
        Turn::text("done"),
    ])
    .await;
    let report = run_program(
        &s,
        lang,
        "can_use_tool",
        &[
            ("SCENARIO_MODE", "update"),
            ("SCENARIO_PROMPT", "run both"),
            (
                "SCENARIO_UPDATED_PERMISSIONS",
                r#"[{"type":"addRules","rules":[{"toolName":"Bash","ruleContent":"echo *"}],"behavior":"allow","destination":"session"}]"#,
            ),
        ],
    )
    .await;
    assert_eq!(
        report["calls"].as_array().map(Vec::len),
        Some(1),
        "{report}"
    );
    assert_eq!(result_text(&report), "done", "{report}");
}
both!(
    can_use_tool_update,
    a_session_allow_spares_the_next_matching_call
);

async fn a_deny_set_in_a_settings_update_fails_closed(lang: Lang) {
    // A deny rule is not an accepted update: the whole answer denies, the call does not run.
    let (_endpoint, s) = sandbox(vec![shell("touch refused.txt"), Turn::text("done")]).await;
    run_program(
        &s,
        lang,
        "can_use_tool",
        &[
            ("SCENARIO_MODE", "update"),
            ("SCENARIO_PROMPT", "run it"),
            (
                "SCENARIO_UPDATED_PERMISSIONS",
                r#"[{"type":"addRules","rules":[{"toolName":"Bash"}],"behavior":"allow","destination":"userSettings"}]"#,
            ),
        ],
    )
    .await;
    assert!(!s.work.join("refused.txt").exists());
}
both!(
    can_use_tool_update_refused,
    a_deny_set_in_a_settings_update_fails_closed
);

async fn a_callback_that_never_answers_ends_with_the_host(lang: Lang) {
    let (_endpoint, s) = sandbox(vec![shell("touch never.txt"), Turn::text("done")]).await;
    run_program(
        &s,
        lang,
        "can_use_tool",
        &[
            ("SCENARIO_MODE", "disconnect"),
            ("SCENARIO_PROMPT", "run it"),
            ("SCENARIO_DISCONNECT_MS", "3000"),
        ],
    )
    .await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(!s.work.join("never.txt").exists());
}
ts_only!(
    can_use_tool_disconnect,
    a_callback_that_never_answers_ends_with_the_host
);

async fn answers_to_a_question_reach_the_model(lang: Lang) {
    let args = json!({"questions": [{
        "question": "Which colour?",
        "header": "Colour",
        "options": [
            {"label": "red", "description": "warm"},
            {"label": "blue", "description": "cool"},
        ],
    }]});
    let (endpoint, s) = sandbox(vec![
        Turn::tool("c1", "ask_user_question", args),
        Turn::text("red it is"),
    ])
    .await;
    let report = run_program(&s, lang, "ask_question", &[("SCENARIO_PROMPT", "ask me")]).await;
    assert_eq!(
        report["asked"].as_array().map(Vec::len),
        Some(1),
        "{report}"
    );
    assert_eq!(result_text(&report), "red it is", "{report}");
    let requests = endpoint.requests().await;
    assert!(requests[1].to_string().contains("red"), "{}", requests[1]);
}
both!(ask_question, answers_to_a_question_reach_the_model);

async fn an_sdk_mcp_tool_runs_through_the_gate(lang: Lang) {
    let (endpoint, s) = sandbox(vec![
        Turn::tool("c1", "mcp__host__add", json!({"a": 2, "b": 3})),
        Turn::text("five"),
    ])
    .await;
    let report = run_program(&s, lang, "sdk_mcp", &[("SCENARIO_PROMPT", "add")]).await;
    let calls = report["calls"].to_string();
    assert!(calls.contains('2') && calls.contains('3'), "{report}");
    assert_eq!(result_text(&report), "five", "{report}");
    let records = audit(&s);
    assert!(
        !decisions(&records, "mcp__host__add").is_empty(),
        "the call was not gated"
    );
    assert!(endpoint.requests().await[1].to_string().contains('5'));
}
both!(sdk_mcp, an_sdk_mcp_tool_runs_through_the_gate);

async fn a_callback_hook_can_deny_and_never_allows(lang: Lang) {
    let (_endpoint, s) = sandbox(vec![shell("touch hooked.txt"), Turn::text("done")]).await;
    let report = run_program(
        &s,
        lang,
        "hook",
        &[("SCENARIO_HOOK", "deny"), ("SCENARIO_PROMPT", "run it")],
    )
    .await;
    assert_eq!(
        report["hook_inputs"].as_array().map(Vec::len),
        Some(1),
        "{report}"
    );
    assert!(!s.work.join("hooked.txt").exists());
}
both!(hook_deny, a_callback_hook_can_deny_and_never_allows);

async fn a_hook_allow_is_ignored(lang: Lang) {
    // The mode still decides: `manual` asks, and without a callback an ask is a denial.
    let (_endpoint, s) = sandbox(vec![shell("touch hooked.txt"), Turn::text("done")]).await;
    run_program(
        &s,
        lang,
        "hook",
        &[("SCENARIO_HOOK", "allow"), ("SCENARIO_PROMPT", "run it")],
    )
    .await;
    assert!(!s.work.join("hooked.txt").exists());
}
ts_only!(hook_allow_ignored, a_hook_allow_is_ignored);

async fn an_interrupt_ends_the_turn_and_the_run_goes_on(lang: Lang) {
    let (_endpoint, s) = sandbox(vec![shell("sleep 30"), Turn::text("done")]).await;
    let started = Instant::now();
    let report = run_program(&s, lang, "interrupt", &[("SCENARIO_PROMPT", "wait")]).await;
    assert!(started.elapsed() < Duration::from_secs(25), "{report}");
    let first = if lang == Lang::Ts {
        &report["results"][0]
    } else {
        &report["first"]
    };
    assert_eq!(first["subtype"], "error_during_execution", "{report}");
    assert_eq!(first["is_error"], true);
}
both!(interrupt, an_interrupt_ends_the_turn_and_the_run_goes_on);

async fn controls_change_mode_and_model_and_refuse_the_rest(lang: Lang) {
    let (_endpoint, s) = sandbox(vec![Turn::text("hi")]).await;
    let report = run_program(
        &s,
        lang,
        "controls",
        &[("SCENARIO_NEW_MODEL", "mock/scripted")],
    )
    .await;
    assert!(
        report["set_mode_accept_edits"].get("ok").is_some(),
        "{report}"
    );
    assert!(report["set_mode_plan"].get("ok").is_some(), "{report}");
    assert!(report["set_mode_bypass"].get("error").is_some(), "{report}");
    assert!(report["set_model"].get("ok").is_some(), "{report}");
    assert!(
        report["set_model_unknown"].get("error").is_some(),
        "{report}"
    );
    assert!(report["context_usage"].get("error").is_some(), "{report}");
    if lang == Lang::Ts {
        let afk = report["set_mode_afk"]["error"].as_str().unwrap_or_default();
        assert!(afk.to_lowercase().contains("afk"), "{report}");
    }
}
both!(controls, controls_change_mode_and_model_and_refuse_the_rest);

async fn malformed_input_ends_the_run_with_an_error(lang: Lang) {
    let (_endpoint, s) = sandbox(vec![Turn::text("never")]).await;
    let report = run_program(&s, lang, "malformed", &[]).await;
    let text = report.to_string();
    assert!(
        text.contains("error_during_execution")
            || text.contains("message.content")
            || text.contains("exit")
            || text.contains("raised"),
        "{report}"
    );
    assert!(!text.contains("never"), "{report}");
}
both!(malformed, malformed_input_ends_the_run_with_an_error);

async fn an_unsupported_option_names_the_flag(lang: Lang) {
    let (endpoint, s) = sandbox(vec![Turn::text("never")]).await;
    let report = run_program(&s, lang, "unsupported", &[]).await;
    let text = report.to_string();
    assert!(text.contains("fallback-model"), "{report}");
    if lang == Lang::Py {
        assert_eq!(report["exit_code"], 2, "{report}");
    }
    assert!(
        endpoint.requests().await.is_empty(),
        "a model was contacted"
    );
}
both!(unsupported_option, an_unsupported_option_names_the_flag);

async fn closing_the_query_stops_a_running_tool(lang: Lang) {
    let (_endpoint, s) = sandbox(vec![shell("sleep 60"), Turn::text("done")]).await;
    let report = run_program(&s, lang, "shutdown", &[("SCENARIO_PROMPT", "wait")]).await;
    let ms = report["ms"].as_u64().unwrap_or(u64::MAX);
    assert!(ms < 20_000, "{report}");
}
both!(shutdown, closing_the_query_stops_a_running_tool);

async fn a_subagent_shows_as_an_agent_call_and_its_calls_carry_its_id(lang: Lang) {
    let endpoint = Endpoint::start_routed(
        Wire::Chat,
        vec![
            Route::new(
                "root task",
                vec![
                    Turn::tool(
                        "c1",
                        "agent",
                        json!({"agent_type": "helper", "prompt": "child-X: run echo hi"}),
                    ),
                    Turn::text("spawned"),
                    Turn::text("the helper reported"),
                ],
            ),
            Route::new(
                "child-X",
                vec![
                    Turn::tool("k1", "shell", json!({"command": "echo hi"})),
                    Turn::text("child done"),
                ],
            ),
        ],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    let report = run_program(&s, lang, "subagent", &[("SCENARIO_PROMPT", "root task")]).await;
    let asked = report["asked"]
        .as_array()
        .unwrap_or_else(|| panic!("{report}"));
    assert!(
        asked
            .iter()
            .any(|a| a["tool_name"] == "Bash" && a["agent_id"].is_string()),
        "{report}"
    );
    if lang == Lang::Ts {
        assert_eq!(report["agent_use"]["name"], "Agent", "{report}");
        assert!(
            report["parent_ids"]
                .as_array()
                .unwrap()
                .iter()
                .all(Value::is_null)
        );
    }
}
both!(
    subagent,
    a_subagent_shows_as_an_agent_call_and_its_calls_carry_its_id
);

async fn a_message_sent_mid_turn_is_delivered_to_the_running_turn(_lang: Lang) {
    let (_endpoint, s) = sandbox(vec![shell("sleep 3"), Turn::text("both answered")]).await;
    let report = run_program(
        &s,
        Lang::Ts,
        "mid_turn",
        &[
            ("SCENARIO_FIRST", "run the command"),
            ("SCENARIO_SECOND", "and then say second"),
        ],
    )
    .await;
    let order = report["order"].to_string();
    assert!(order.contains("sent_second"), "{report}");
    assert!(order.contains("both answered"), "{report}");
}
ts_only!(
    mid_turn_input,
    a_message_sent_mid_turn_is_delivered_to_the_running_turn
);

const SDK_ARGS: [&str; 7] = [
    "--output-format",
    "stream-json",
    "--input-format",
    "stream-json",
    "--verbose",
    "--max-turns",
    "1",
];

fn user_line(text: &str) -> String {
    format!(
        "{}\n",
        json!({"type": "user", "message": {"role": "user", "content": text}})
    )
}

fn results(stdout: &str) -> Vec<Value> {
    stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["type"] == "result")
        .collect()
}

#[tokio::test]
async fn a_failed_turn_leaves_the_session_open_and_end_of_input_exits_zero() {
    let (_endpoint, s) = sandbox(vec![shell("echo hi"), Turn::text("second answer")]).await;
    let input = format!("{}{}", user_line("first"), user_line("second"));
    let out = s.run_with_stdin(&SDK_ARGS, &input).await;
    let all = results(&out.stdout);
    assert_eq!(all.len(), 2, "{}\n{}", out.stdout, out.stderr);
    assert_eq!(all[0]["subtype"], "error_max_turns");
    assert_eq!(out.code, Some(0), "{}", out.stderr);
}

#[tokio::test]
async fn a_subagent_can_still_spawn_after_a_failed_turn() {
    let (_endpoint, s) = sandbox(vec![
        shell("echo hi"),
        Turn::tool(
            "c1",
            "agent",
            json!({"agent_type": "helper", "prompt": "say hi"}),
        ),
        Turn::text("child done"),
        Turn::text("after"),
    ])
    .await;
    let input = format!("{}{}", user_line("first"), user_line("second"));
    let out = s.run_with_stdin(&SDK_ARGS, &input).await;
    assert_eq!(
        results(&out.stdout).len(),
        2,
        "{}\n{}",
        out.stdout,
        out.stderr
    );
    assert!(
        !out.stdout.contains("the session is ending"),
        "{}\n{}",
        out.stdout,
        out.stderr
    );
}

#[tokio::test]
async fn end_of_input_with_no_message_exits_zero() {
    let (_endpoint, s) = sandbox(vec![Turn::text("never")]).await;
    let out = s.run_with_stdin(&SDK_ARGS, "").await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
}

#[tokio::test]
async fn a_rejected_initialize_ends_the_run_without_running_the_prompt() {
    use tokio::io::AsyncWriteExt;
    let (endpoint, s) = sandbox(vec![Turn::text("never")]).await;
    let init = json!({"type": "control_request", "request_id": "r1",
        "request": {"subtype": "initialize", "title": "t"}});
    let mut cmd = s.command(&SDK_ARGS);
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(format!("{init}\n").as_bytes())
        .await
        .unwrap();
    // The host waits for the answer before it sends the prompt.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let _ = stdin.write_all(user_line("go").as_bytes()).await;
    let out = tokio::time::timeout(Duration::from_secs(30), child.wait_with_output())
        .await
        .expect("the run ends")
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stdout.contains("title"), "{stdout}");
    assert!(stderr.contains("title"), "{stderr}");
    assert!(!out.status.success());
    assert!(results(&stdout).is_empty(), "{stdout}");
    assert!(
        endpoint.requests().await.is_empty(),
        "a model was contacted"
    );
}
