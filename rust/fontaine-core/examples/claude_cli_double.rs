//! A stand-in for the Claude CLI in tests, portable because it is a program rather than a shell
//! script. It answers `--version`, `auth status`, the `initialize` control request and the
//! stream-json conversation the way the adapter expects, as a scenario file tells it to.
//!
//! The scenario is `double.json` in `$CLAUDE_CONFIG_DIR`, and every launch appends what it saw
//! (argv, environment, the files it was given, each stdin frame) to `log.jsonl` there. Without
//! `CLAUDE_CONFIG_DIR` it uses the defaults and logs nothing.
//!
//! Scenario keys, all optional: `version`, `version_exit`, `version_sleep_ms`, `auth` (the JSON
//! it prints), `auth_raw`, `auth_exit`, `auth_sleep_ms`, `models`, `account` (the `account`
//! object of the `initialize` answer, default `{"email": "user@example.com", ...}`),
//! `claude_code_version` (the version that answer reports, default "2.1.319", independent of
//! `version`; either key set to JSON `null` is left out of the answer), `init_hang`, `init_extra`
//! (a
//! JSON object merged into `system/init`), `responses`: one script per model process, the last
//! repeating. A script has `text`, `thinking`, `tool_use`, `stop_reason`, `usage`,
//! `total_cost_usd`, `result_subtype`, `result_extra`, `denial_echo` (the error result after the
//! stop, as CLI 2.1.318 sends it), `denial: "mid_stream"` (CLI 2.1.319's order: the denial
//! events inside the tool block, the result after `message_stop`), `thinking_tokens` (one
//! estimate inside the thinking block), `events` (raw lines that
//! replace everything generated), `before` and `after_stop` (raw lines added), `drop_after`,
//! `exit_code`, `stderr`, `hang_before_output`, `hang_after_result`, `chunked` and `descendant`.
//!
//! `auth login` (what `claude auth login --claudeai` runs) rewrites `auth` in `double.json` to a
//! signed-in Max account, unless `login_exit` (an exit code) or `login_keeps_signed_out` (exit 0
//! with the state unchanged) says it fails.
//!
//! `claude_cli_double --descendant FILE` is the sleeping process a script's `descendant` starts.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Map, Value, json};

pub fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--descendant") {
        let file = args.get(1).expect("--descendant takes a file");
        std::fs::write(file, std::process::id().to_string()).expect("write the pid");
        std::thread::sleep(Duration::from_secs(300));
        return;
    }
    let double = Double::load();
    if args.first().map(String::as_str) == Some("--version") {
        double.pause("version_sleep_ms");
        println!(
            "{}",
            double.text("version").unwrap_or("2.1.318 (Claude Code)")
        );
        std::process::exit(double.int("version_exit") as i32);
    }
    if args.first().map(String::as_str) == Some("auth")
        && args.get(1).map(String::as_str) == Some("login")
    {
        double.login(&args);
    }
    if args.first().map(String::as_str) == Some("auth") {
        double.pause("auth_sleep_ms");
        match double.scenario.get("auth_raw").and_then(Value::as_str) {
            Some(raw) => println!("{raw}"),
            None => println!(
                "{}",
                double
                    .scenario
                    .get("auth")
                    .cloned()
                    .unwrap_or_else(|| json!({
                        "loggedIn": true,
                        "authMethod": "claude.ai",
                        "subscriptionType": "max",
                    }))
            ),
        }
        std::process::exit(double.int("auth_exit") as i32);
    }
    double.run_conversation(&args);
}

struct Double {
    scenario: Value,
    dir: Option<PathBuf>,
}

impl Double {
    fn load() -> Self {
        let dir = std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from);
        let scenario = dir
            .as_ref()
            .and_then(|dir| std::fs::read(dir.join("double.json")).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_else(|| json!({}));
        Double { scenario, dir }
    }

    /// `auth login`: logs the scenario's account in by rewriting its `auth`, so the next launch
    /// reads the signed-in state. `login_exit` makes it fail, `login_keeps_signed_out` makes it
    /// exit 0 without signing in. The arguments it was run with are logged.
    fn login(&self, args: &[String]) -> ! {
        self.log(json!({"kind": "login", "argv": args}));
        let code = self.int("login_exit") as i32;
        let keeps = self
            .scenario
            .get("login_keeps_signed_out")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if code == 0
            && !keeps
            && let Some(dir) = &self.dir
        {
            let mut scenario = self.scenario.clone();
            scenario["auth"] = json!({
                "loggedIn": true,
                "authMethod": "claude.ai",
                "subscriptionType": "max",
            });
            std::fs::write(
                dir.join("double.json"),
                serde_json::to_vec(&scenario).expect("serialize the scenario"),
            )
            .expect("rewrite the scenario");
        }
        std::process::exit(code);
    }

    fn text(&self, key: &str) -> Option<&str> {
        self.scenario.get(key).and_then(Value::as_str)
    }

    fn int(&self, key: &str) -> u64 {
        self.scenario.get(key).and_then(Value::as_u64).unwrap_or(0)
    }

    fn pause(&self, key: &str) {
        let ms = self.int(key);
        if ms > 0 {
            std::thread::sleep(Duration::from_millis(ms));
        }
    }

    fn log(&self, entry: Value) {
        let Some(dir) = &self.dir else { return };
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("log.jsonl"))
            .expect("open the log");
        // One write call: formatting straight into the unbuffered file issues a write per
        // fragment, so a reader racing a live double sees a torn line.
        file.write_all(format!("{entry}\n").as_bytes())
            .expect("write the log");
    }

    /// The number of model processes started so far, this one included.
    fn next_run(&self) -> usize {
        let Some(dir) = &self.dir else { return 1 };
        let path = dir.join("count");
        let seen: usize = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| text.trim().parse().ok())
            .unwrap_or(0);
        std::fs::write(&path, (seen + 1).to_string()).expect("write the count");
        seen + 1
    }

    fn initialize_answer(&self) -> Value {
        let mut answer = json!({"models": self.models()});
        let defaults = [
            (
                "account",
                json!({"email": "user@example.com", "subscriptionType": "max"}),
            ),
            ("claude_code_version", json!("2.1.319")),
        ];
        for (key, default) in defaults {
            match self.scenario.get(key) {
                Some(Value::Null) => {}
                Some(value) => answer[key] = value.clone(),
                None => answer[key] = default,
            }
        }
        answer
    }

    fn models(&self) -> Value {
        self.scenario.get("models").cloned().unwrap_or_else(|| {
            json!([
                {"value": "sonnet", "resolvedModel": "claude-sonnet-5-5"},
                {"value": "opus", "resolvedModel": "claude-opus-5"},
                {"value": "haiku", "resolvedModel": "claude-haiku-4-5"},
            ])
        })
    }

    fn script(&self, run: usize) -> Value {
        let scripts = self.scenario.get("responses").and_then(Value::as_array);
        scripts
            .and_then(|scripts| scripts.get(run - 1).or(scripts.last()))
            .cloned()
            .unwrap_or_else(|| json!({"text": "ok"}))
    }

    fn run_conversation(&self, args: &[String]) {
        let file_of = |flag: &str| {
            args.iter()
                .position(|a| a == flag)
                .and_then(|at| args.get(at + 1))
                .and_then(|path| std::fs::read_to_string(path).ok())
        };
        let value_of = |flag: &str| {
            args.iter()
                .position(|a| a == flag)
                .and_then(|at| args.get(at + 1))
                .cloned()
        };
        let mcp = file_of("--mcp-config").unwrap_or_default();
        let manifest = serde_json::from_str::<Value>(&mcp)
            .ok()
            .and_then(|mcp| {
                let last = mcp
                    .pointer("/mcpServers/fontaine/args")?
                    .as_array()?
                    .last()?
                    .as_str()?
                    .to_string();
                std::fs::read_to_string(last).ok()
            })
            .unwrap_or_default();
        let settings = file_of("--settings").unwrap_or_default();
        self.log(json!({
            "kind": "invocation",
            "argv": args,
            "env": std::env::vars().collect::<std::collections::BTreeMap<_, _>>(),
            "cwd": std::env::current_dir().ok(),
            "settings": settings,
            "system": file_of("--system-prompt-file"),
            "mcp": mcp,
            "tools": manifest,
        }));
        let model = value_of("--model").unwrap_or_else(|| "claude-sonnet-5-5".to_string());
        let tools: Vec<String> = serde_json::from_str::<Value>(&settings)
            .ok()
            .and_then(|s| {
                let body = s
                    .pointer("/env/CLAUDE_CODE_EXTRA_BODY")?
                    .as_str()?
                    .to_string();
                serde_json::from_str::<Value>(&body).ok()
            })
            .and_then(|body| {
                Some(
                    body.get("tools")?
                        .as_array()?
                        .iter()
                        .filter(|t| t.get("type").is_none_or(|kind| kind == "custom"))
                        .filter_map(|t| t.get("name")?.as_str().map(str::to_string))
                        .collect(),
                )
            })
            .unwrap_or_default();

        let stdin = std::io::stdin();
        let mut run = 0;
        let mut announced = false;
        for line in stdin.lock().lines() {
            let line = line.expect("read stdin");
            if line.trim().is_empty() {
                continue;
            }
            let frame: Value = serde_json::from_str(&line).expect("a JSON frame");
            if frame.get("type").and_then(Value::as_str) == Some("control_request") {
                self.log(json!({"kind": "frame", "value": frame}));
                if self.scenario.get("init_hang").and_then(Value::as_bool) == Some(true) {
                    std::thread::sleep(Duration::from_secs(300));
                }
                let reply = json!({
                    "type": "control_response",
                    "response": {
                        "subtype": "success",
                        "request_id": frame["request_id"],
                        "response": self.initialize_answer(),
                    },
                });
                emit(&[reply.to_string()], false);
                continue;
            }
            if run == 0 {
                run = self.next_run();
            }
            self.log(json!({"kind": "frame", "run": run, "value": frame}));
            let script = self.script(run);
            if !announced {
                announced = true;
                emit(&[self.init_event(&model, &tools).to_string()], false);
            }
            if frame.get("type").and_then(Value::as_str) != Some("user") {
                continue;
            }
            if frame.get("shouldQuery").and_then(Value::as_bool) == Some(false) {
                let ack = json!({
                    "type": "result", "subtype": "success", "is_error": false, "num_turns": 0,
                    "result": "", "total_cost_usd": 0,
                });
                emit(&[ack.to_string()], false);
                continue;
            }
            self.respond(&script, &model, &tools);
            return;
        }
    }

    fn init_event(&self, model: &str, tools: &[String]) -> Value {
        let mut init = json!({
            "type": "system", "subtype": "init", "model": model, "tools": tools,
            "mcp_servers": [{"name": "fontaine", "status": "connected"}],
            "plugins": [{"name": "cc-plugin-telemetry", "path": "builtin",
                         "source": "cc-plugin-telemetry@builtin"}],
            "skills": [], "slash_commands": [], "agents": [],
        });
        if let Some(extra) = self.scenario.get("init_extra").and_then(Value::as_object) {
            for (key, value) in extra {
                init[key] = value.clone();
            }
        }
        init
    }

    fn respond(&self, script: &Value, model: &str, tools: &[String]) {
        let flag = |key: &str| script.get(key).and_then(Value::as_bool) == Some(true);
        if let Some(text) = script.get("stderr").and_then(Value::as_str) {
            eprintln!("{text}");
        }
        if let Some(path) = script.get("descendant").and_then(Value::as_str) {
            // Deliberately never waited on: it must outlive this process for the test to see it
            // cleaned up.
            #[allow(clippy::zombie_processes)]
            let _descendant =
                std::process::Command::new(std::env::current_exe().expect("own path"))
                    .arg("--descendant")
                    .arg(path)
                    .spawn()
                    .expect("start the descendant");
            // Give the descendant time to record its pid before the test looks.
            std::thread::sleep(Duration::from_millis(300));
        }
        if flag("hang_before_output") {
            std::thread::sleep(Duration::from_secs(300));
        }
        let lines = match script.get("events").and_then(Value::as_array) {
            Some(events) => events.iter().map(raw_line).collect(),
            None => self.generated(script, model, tools),
        };
        let lines = match script.get("drop_after").and_then(Value::as_u64) {
            Some(n) => lines.into_iter().take(n as usize).collect(),
            None => lines,
        };
        emit(&lines, flag("chunked"));
        if script.get("drop_after").is_some() || script.get("exit_code").is_some() {
            std::process::exit(self.int_of(script, "exit_code") as i32);
        }
        if flag("hang_after_result") {
            std::thread::sleep(Duration::from_secs(300));
        }
    }

    fn int_of(&self, script: &Value, key: &str) -> u64 {
        script.get(key).and_then(Value::as_u64).unwrap_or(0)
    }

    fn generated(&self, script: &Value, model: &str, tools: &[String]) -> Vec<String> {
        let usage = script
            .get("usage")
            .cloned()
            .unwrap_or_else(|| json!({"input_tokens": 11, "output_tokens": 7}));
        let stream = |event: Value| {
            json!({
                "type": "stream_event", "event": event, "parent_tool_use_id": null,
                "session_id": "double", "uuid": "double",
            })
            .to_string()
        };
        let mut out: Vec<String> = script
            .get("before")
            .and_then(Value::as_array)
            .map(|lines| lines.iter().map(raw_line).collect())
            .unwrap_or_default();
        let start_usage = json!({
            "input_tokens": usage["input_tokens"], "output_tokens": 1,
            "cache_read_input_tokens": usage.get("cache_read_input_tokens").cloned().unwrap_or(json!(0)),
        });
        out.push(stream(json!({
            "type": "message_start",
            "message": {
                "id": "msg_double", "type": "message", "role": "assistant", "model": model,
                "content": [], "stop_reason": null, "usage": start_usage,
            },
        })));
        let mid_stream = script.get("denial").and_then(Value::as_str) == Some("mid_stream");
        let mut index = 0;
        if let Some(thinking) = script.get("thinking") {
            out.push(stream(json!({
                "type": "content_block_start", "index": index,
                "content_block": {"type": "thinking", "thinking": ""},
            })));
            out.push(stream(json!({
                "type": "content_block_delta", "index": index,
                "delta": {"type": "thinking_delta", "thinking": thinking["thinking"]},
            })));
            if script.get("thinking_tokens").and_then(Value::as_bool) == Some(true) {
                out.push(
                    json!({"type": "system", "subtype": "thinking_tokens",
                        "estimated_tokens": 12, "estimated_tokens_delta": 12,
                        "session_id": "double", "uuid": "double"})
                    .to_string(),
                );
            }
            out.push(stream(json!({
                "type": "content_block_delta", "index": index,
                "delta": {"type": "signature_delta", "signature": thinking["signature"]},
            })));
            out.push(stream(
                json!({"type": "content_block_stop", "index": index}),
            ));
            index += 1;
        }
        if let Some(text) = script.get("text").and_then(Value::as_str) {
            out.push(stream(json!({
                "type": "content_block_start", "index": index,
                "content_block": {"type": "text", "text": ""},
            })));
            let (first, second) = text.split_at(text.len() / 2);
            for part in [first, second] {
                out.push(stream(json!({
                    "type": "content_block_delta", "index": index,
                    "delta": {"type": "text_delta", "text": part},
                })));
            }
            out.push(stream(
                json!({"type": "content_block_stop", "index": index}),
            ));
            index += 1;
        }
        let mut stop = script
            .get("stop_reason")
            .and_then(Value::as_str)
            .unwrap_or("end_turn")
            .to_string();
        if let Some(call) = script.get("tool_use") {
            stop = "tool_use".to_string();
            // `$FIRST_TOOL` stands for the first tool the request defined, whatever it is named.
            let mut call = call.clone();
            if call["name"] == "$FIRST_TOOL" {
                call["name"] = json!(tools.first().cloned().unwrap_or_default());
            }
            let call = &call;
            let id = call.get("id").cloned().unwrap_or(json!("toolu_double"));
            let mut block = json!({
                "type": "tool_use", "id": id, "name": call["name"], "input": {},
            });
            if mid_stream {
                block["caller"] = json!({"type": "direct"});
            }
            out.push(stream(json!({
                "type": "content_block_start", "index": index, "content_block": block,
            })));
            let input = call.get("input").cloned().unwrap_or(json!({})).to_string();
            let (first, second) = input.split_at(input.len() / 2);
            for part in [first, second] {
                out.push(stream(json!({
                    "type": "content_block_delta", "index": index,
                    "delta": {"type": "input_json_delta", "partial_json": part},
                })));
            }
            if mid_stream {
                out.push(
                    json!({"type": "assistant", "message": {"role": "assistant", "model": model, "content": []}})
                        .to_string(),
                );
                out.push(
                    json!({"type": "system", "subtype": "permission_denied",
                        "tool_name": call["name"], "tool_use_id": id,
                        "decision_reason_type": "mode", "message": "denied",
                        "session_id": "double", "uuid": "double"})
                    .to_string(),
                );
            }
            out.push(stream(
                json!({"type": "content_block_stop", "index": index}),
            ));
            if mid_stream {
                out.push(denial_echo_line(&id));
            }
        }
        out.push(stream(json!({
            "type": "message_delta",
            "delta": {"stop_reason": stop, "stop_sequence": null},
            "usage": {"output_tokens": usage["output_tokens"]},
        })));
        out.push(stream(json!({"type": "message_stop"})));
        if mid_stream {
            out.push(json!({"type": "rate_limit_event"}).to_string());
        } else {
            out.push(
                json!({"type": "assistant", "message": {"role": "assistant", "model": model, "content": []}})
                    .to_string(),
            );
        }
        if let Some(limits) = script.get("rate_limit") {
            let ratio = |key: &str, default: f64| {
                limits.get(key).and_then(Value::as_f64).unwrap_or(default)
            };
            out.push(
                json!({"type": "rate_limit_event", "rate_limit_info": {
                    "status": "allowed", "resetsAt": 1_790_578_200u64,
                    "rateLimitType": "five_hour", "overageStatus": "rejected",
                    "overageDisabledReason": "out_of_credits", "isUsingOverage": false,
                    "unifiedWindows": {
                        "five_hour": {"utilization": ratio("five_hour", 0.09),
                            "resetsAt": 1_790_578_200u64},
                        "seven_day": {"utilization": ratio("seven_day", 0.15),
                            "resetsAt": 1_791_036_000u64},
                    }},
                    "uuid": "double", "session_id": "double"})
                .to_string(),
            );
        }
        if script.get("denial_echo").and_then(Value::as_bool) == Some(true) {
            let id = script
                .get("tool_use")
                .and_then(|call| call.get("id"))
                .cloned()
                .unwrap_or(json!("toolu_double"));
            out.push(denial_echo_line(&id));
        }
        if let Some(lines) = script.get("after_stop").and_then(Value::as_array) {
            out.extend(lines.iter().map(raw_line));
        }
        let subtype = script
            .get("result_subtype")
            .and_then(Value::as_str)
            .unwrap_or(if stop == "tool_use" {
                "error_max_turns"
            } else {
                "success"
            });
        let mut result = Map::new();
        for (key, value) in [
            ("type", json!("result")),
            ("subtype", json!(subtype)),
            (
                "is_error",
                json!(subtype != "success" && subtype != "error_max_turns"),
            ),
            ("num_turns", json!(if mid_stream { 2 } else { 1 })),
            ("result", json!("")),
            (
                "total_cost_usd",
                script.get("total_cost_usd").cloned().unwrap_or(json!(0.25)),
            ),
            ("usage", usage.clone()),
        ] {
            result.insert(key.to_string(), value);
        }
        if let Some(extra) = script.get("result_extra").and_then(Value::as_object) {
            result.extend(extra.clone());
        }
        out.push(Value::Object(result).to_string());
        out
    }
}

fn denial_echo_line(id: &Value) -> String {
    json!({"type": "user", "message": {"role": "user", "content": [
        {"type": "tool_result", "tool_use_id": id, "is_error": true, "content": "denied"}
    ]}})
    .to_string()
}

fn raw_line(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// Writes the lines as the CLI would, optionally split at awkward byte boundaries with CRLF.
fn emit(lines: &[String], chunked: bool) {
    let mut out = std::io::stdout().lock();
    for line in lines {
        if chunked {
            let bytes = format!("{line}\r\n").into_bytes();
            for piece in bytes.chunks(7) {
                out.write_all(piece).expect("write stdout");
                out.flush().expect("flush stdout");
            }
        } else {
            writeln!(out, "{line}").expect("write stdout");
            out.flush().expect("flush stdout");
        }
    }
}
