use super::*;
use crate::providers::claude_subscription::claude_error;

fn seed() -> SessionSeed<'static> {
    SessionSeed {
        session: "s1",
        namespace: "root",
        canonical: "claude-sonnet-5-5",
    }
}

fn request(extra: Value) -> Value {
    let mut body = json!({
        "model": "claude-sonnet-5-5",
        "max_tokens": 64,
        "system": "be brief",
        "messages": [{"role": "user", "content": "hi"}],
    });
    for (key, value) in extra.as_object().unwrap() {
        body[key] = value.clone();
    }
    body
}

fn translated(body: Value) -> Translated {
    translate(body.to_string().as_bytes(), None, &seed()).unwrap()
}

fn refused(body: Value) -> String {
    translate(body.to_string().as_bytes(), None, &seed())
        .unwrap_err()
        .to_string()
}

fn tool(name: &str) -> Value {
    json!({"name": name, "description": "d", "input_schema": {"type": "object"}})
}

#[test]
fn a_plain_request_becomes_one_querying_frame() {
    let t = translated(request(json!({})));
    assert_eq!(t.system, "be brief");
    assert_eq!(t.frames.len(), 1);
    assert!(t.frames[0].query);
    assert_eq!(t.frames[0].value["message"]["content"], "hi");
    assert!(t.frames[0].value.get("shouldQuery").is_none());
    let extra: Value = serde_json::from_str(
        t.settings["env"]["CLAUDE_CODE_EXTRA_BODY"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(extra, json!({"max_tokens": 64}));
    assert!(t.manifest.is_empty());
}

#[test]
fn history_is_replayed_in_order_and_only_the_last_frame_queries() {
    let t = translated(request(json!({
        "tools": [tool("read_file")],
        "messages": [
            {"role": "user", "content": "go"},
            {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "hm", "signature": "sig"},
                {"type": "tool_use", "id": "t1", "name": "read_file", "input": {"p": 1}},
            ]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": "data"},
            ]},
        ],
    })));
    let flags: Vec<bool> = t.frames.iter().map(|f| f.query).collect();
    assert_eq!(flags, [false, false, true]);
    assert_eq!(t.frames[0].value["shouldQuery"], false);
    assert!(t.frames[1].value.get("shouldQuery").is_none());
    let blocks = &t.frames[1].value["message"]["content"];
    assert_eq!(blocks[0]["signature"], "sig");
    assert_eq!(blocks[1]["name"], "mcp__fontaine__read_file");
    assert!(t.frames[2].value.get("shouldQuery").is_none());
}

#[test]
fn tool_names_are_aliased_the_same_way_everywhere() {
    let long = "x".repeat(80);
    let odd = "mcp.server/tool name";
    let t = translated(request(json!({
        "tools": [tool("plain"), tool(&long), tool(odd)],
        "tool_choice": {"type": "tool", "name": odd},
    })));
    let extra: Value = serde_json::from_str(
        t.settings["env"]["CLAUDE_CODE_EXTRA_BODY"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let names: Vec<&str> = extra["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names[0], "mcp__fontaine__plain");
    for name in &names {
        assert!(name.len() <= 64, "{name}");
        assert!(
            name.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        );
    }
    assert_eq!(extra["tool_choice"]["name"], names[2]);
    assert_eq!(t.aliases.restore(names[1]), Some(long.as_str()));
    assert_eq!(t.aliases.restore(names[2]), Some(odd));
    let listed: Vec<String> = t
        .manifest
        .iter()
        .map(|m| format!("{TOOL_PREFIX}{}", m["name"].as_str().unwrap()))
        .collect();
    assert_eq!(listed, names);
    assert_eq!(t.manifest[0]["inputSchema"], json!({"type": "object"}));
}

#[test]
fn effort_and_betas_and_generation_fields_are_carried() {
    let body = request(json!({
        "temperature": 0.5, "top_p": 0.9, "top_k": 5, "stop_sequences": ["x"],
        "thinking": {"type": "enabled", "budget_tokens": 2000},
        "output_config": {"effort": "high", "format": {"type": "json_schema", "schema": {}}},
    }));
    let t = translate(
        body.to_string().as_bytes(),
        Some("interleaved-thinking-2025-05-14"),
        &seed(),
    )
    .unwrap();
    assert_eq!(t.effort.as_deref(), Some("high"));
    assert_eq!(
        t.settings["env"]["ANTHROPIC_BETAS"],
        "interleaved-thinking-2025-05-14"
    );
    let extra: Value = serde_json::from_str(
        t.settings["env"]["CLAUDE_CODE_EXTRA_BODY"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(extra["temperature"], 0.5);
    assert_eq!(extra["thinking"]["budget_tokens"], 2000);
    assert_eq!(extra["stop_sequences"], json!(["x"]));
    assert!(extra["output_config"].get("effort").is_none());
    assert_eq!(extra["output_config"]["format"]["type"], "json_schema");
    let t = translate(request(json!({})).to_string().as_bytes(), Some(""), &seed()).unwrap();
    assert!(t.settings["env"].get("ANTHROPIC_BETAS").is_none());
}

#[test]
fn what_cannot_be_replayed_without_loss_is_refused() {
    assert!(
        refused(request(json!({"messages": [
            {"role": "user", "content": "a"},
            {"role": "assistant", "content": "prefill"},
        ]})))
        .contains("prefill")
    );
    assert!(
        refused(request(json!({"messages": [
            {"role": "user", "content": "a"},
            {"role": "system", "content": "mid"},
            {"role": "user", "content": "b"},
        ]})))
        .contains("role other than user or assistant")
    );
    assert!(
        refused(request(json!({"tools": [
            {"type": "web_search_20250305", "name": "web_search"}
        ]})))
        .contains("hosted tool")
    );
    assert!(refused(request(json!({"service_tier": "auto"}))).contains("service_tier"));
    assert!(
        refused(request(json!({"cache_control": {"type": "ephemeral"}}))).contains("cache_control")
    );
    assert!(
        refused(request(json!({"messages": [
            {"role": "user", "content": [{"type": "server_tool_use", "id": "x"}]},
        ]})))
        .contains("without loss")
    );
    assert!(
        refused(request(json!({"messages": [
            {"role": "user", "content": "a"},
            {"role": "assistant", "content": [{"type": "web_search_tool_result"}]},
            {"role": "user", "content": "b"},
        ]})))
        .contains("without loss")
    );
    assert!(
        refused(request(
            json!({"tool_choice": {"type": "tool", "name": "ghost"}})
        ))
        .contains("does not define")
    );
    assert!(refused(request(json!({"output_config": {"effort": "ludicrous"}}))).contains("effort"));
    assert!(refused(request(json!({"messages": []}))).contains("no messages"));
    assert!(refused(json!([1])).contains("not an object"));
}

#[test]
fn session_ids_are_stable_and_separate_callers() {
    let body = request(json!({}));
    let id = |namespace: &str, session: &str| {
        translate(
            body.to_string().as_bytes(),
            None,
            &SessionSeed {
                session,
                namespace,
                canonical: "claude-sonnet-5-5",
            },
        )
        .unwrap()
        .session_id
    };
    assert_eq!(id("root", "s1"), id("root", "s1"));
    assert_ne!(id("root", "s1"), id("root", "s2"));
    assert_ne!(id("root", "s1"), id("guardian", "s1"));
    assert_ne!(id("agent-1", "s1"), id("agent-2", "s1"));
    uuid::Uuid::parse_str(&id("root", "s1")).expect("a UUID");
    let other = translated(request(json!({"system": "different"}))).session_id;
    assert_ne!(other, id("root", "s1"));
}

fn reader(tools: &[&str]) -> Reader {
    let mut reader = Reader::new(Expect {
        models: vec!["claude-sonnet-5-5".into(), "sonnet".into()],
        aliases: Aliases::new(tools.iter().copied()),
    });
    reader.expect_response();
    reader
}

fn feed(reader: &mut Reader, value: Value) -> Result<Event, Failure> {
    reader.accept(value.to_string().as_bytes())
}

fn init() -> Value {
    json!({"type": "system", "subtype": "init", "model": "claude-sonnet-5-5",
           "tools": [],
           "mcp_servers": [{"name": "fontaine", "status": "connected"}],
           "plugins": [], "skills": []})
}

fn stream(event: Value) -> Value {
    json!({"type": "stream_event", "event": event, "parent_tool_use_id": null})
}

fn message_start() -> Value {
    stream(json!({"type": "message_start", "message": {"model": "claude-sonnet-5-5"}}))
}

fn terminal(reason: &str) -> Value {
    stream(
        json!({"type": "message_delta", "delta": {"stop_reason": reason},
                  "usage": {"output_tokens": 5}}),
    )
}

fn result(subtype: &str) -> Value {
    json!({"type": "result", "subtype": subtype, "is_error": false, "num_turns": 1,
           "usage": {"output_tokens": 5}})
}

fn accept_text_response(reader: &mut Reader) {
    for value in [
        init(),
        message_start(),
        stream(json!({"type": "content_block_start", "index": 0,
                      "content_block": {"type": "text", "text": ""}})),
        stream(json!({"type": "content_block_delta", "index": 0,
                      "delta": {"type": "text_delta", "text": "hi"}})),
        stream(json!({"type": "content_block_stop", "index": 0})),
    ] {
        feed(reader, value).unwrap();
    }
}

fn is_nothing(event: Result<Event, Failure>) -> bool {
    matches!(event, Ok(Event::Nothing))
}

fn failure(event: Result<Event, Failure>) -> String {
    event.expect_err("a failure").to_string()
}

#[test]
fn the_terminal_delta_is_held_until_the_result_is_validated() {
    let mut r = reader(&["read_file"]);
    accept_text_response(&mut r);
    assert!(is_nothing(feed(&mut r, terminal("end_turn"))));
    assert!(is_nothing(feed(
        &mut r,
        stream(json!({"type": "message_stop"}))
    )));
    assert!(is_nothing(feed(
        &mut r,
        json!({"type": "assistant", "message": {}})
    )));
    assert!(matches!(
        feed(&mut r, result("success")),
        Ok(Event::Complete)
    ));
    let frame = r.release().expect("the held delta");
    assert!(frame.as_str().contains("end_turn"));
    assert!(r.release().is_none());
}

#[test]
fn a_cut_stream_is_never_a_response() {
    // No terminal delta, then a result.
    let mut r = reader(&[]);
    accept_text_response(&mut r);
    assert!(failure(feed(&mut r, result("success"))).contains("ended") || r.started());
    // A stop without a delta.
    let mut r = reader(&[]);
    accept_text_response(&mut r);
    assert!(failure(feed(&mut r, stream(json!({"type": "message_stop"})))).contains("stop reason"));
    // A tool block left open at the stop.
    let mut r = reader(&["read_file"]);
    feed(&mut r, init()).unwrap();
    feed(&mut r, message_start()).unwrap();
    feed(
        &mut r,
        stream(json!({"type": "content_block_start", "index": 0,
                      "content_block": {"type": "tool_use", "id": "t", "name": "mcp__fontaine__read_file", "input": {}}})),
    )
    .unwrap();
    feed(&mut r, terminal("tool_use")).unwrap();
    let error = feed(&mut r, stream(json!({"type": "message_stop"}))).unwrap_err();
    assert!(matches!(*error, ProviderError::Truncated));
    assert!(error.is_retryable());
}

#[test]
fn tool_names_are_restored_and_the_stop_may_be_max_turns() {
    let mut r = reader(&["read_file"]);
    feed(&mut r, init()).unwrap();
    feed(&mut r, message_start()).unwrap();
    let Ok(Event::Frame(frame)) = feed(
        &mut r,
        stream(json!({"type": "content_block_start", "index": 0,
                      "content_block": {"type": "tool_use", "id": "t", "name": "mcp__fontaine__read_file", "input": {}}})),
    ) else {
        panic!("a frame");
    };
    assert!(
        frame.as_str().contains(r#""name":"read_file""#),
        "{}",
        frame.as_str()
    );
    feed(
        &mut r,
        stream(json!({"type": "content_block_stop", "index": 0})),
    )
    .unwrap();
    feed(&mut r, terminal("tool_use")).unwrap();
    feed(&mut r, stream(json!({"type": "message_stop"}))).unwrap();
    // A denied call's echo is tolerated, only as an error.
    assert!(is_nothing(feed(
        &mut r,
        json!({"type": "user", "message": {"content": [{"type": "tool_result", "is_error": true}]}}),
    )));
    assert!(matches!(
        feed(&mut r, result("error_max_turns")),
        Ok(Event::Complete)
    ));
}

#[test]
fn max_turns_is_only_valid_after_a_tool_use_stop() {
    let mut r = reader(&[]);
    accept_text_response(&mut r);
    feed(&mut r, terminal("end_turn")).unwrap();
    feed(&mut r, stream(json!({"type": "message_stop"}))).unwrap();
    assert!(feed(&mut r, result("error_max_turns")).is_err());
}

#[test]
fn a_tool_the_request_did_not_define_is_refused() {
    let mut r = reader(&["read_file"]);
    feed(&mut r, init()).unwrap();
    feed(&mut r, message_start()).unwrap();
    let error = failure(feed(
        &mut r,
        stream(json!({"type": "content_block_start", "index": 0,
                      "content_block": {"type": "tool_use", "id": "t", "name": "Bash", "input": {}}})),
    ));
    assert!(error.contains("did not define"), "{error}");
}

#[test]
fn end_conversation_is_control_flow_and_never_a_tool_call() {
    let mut r = reader(&["read_file"]);
    let mut announced = init();
    announced["tools"] = json!(["mcp__fontaine__read_file", "EndConversation"]);
    feed(&mut r, announced).unwrap();
    feed(&mut r, message_start()).unwrap();
    for event in [
        json!({"type": "content_block_start", "index": 0,
               "content_block": {"type": "tool_use", "id": "t", "name": "EndConversation", "input": {}}}),
        json!({"type": "content_block_delta", "index": 0,
               "delta": {"type": "input_json_delta", "partial_json": "{}"}}),
        json!({"type": "content_block_stop", "index": 0}),
    ] {
        assert!(is_nothing(feed(&mut r, stream(event))));
    }
}

#[test]
fn unexpected_native_behavior_is_refused() {
    let cases: Vec<(&str, Vec<Value>)> = vec![
        (
            "api_retry",
            vec![init(), json!({"type": "system", "subtype": "api_retry"})],
        ),
        (
            "suggestion",
            vec![init(), json!({"type": "prompt_suggestion"})],
        ),
        (
            "second response",
            vec![init(), message_start(), message_start()],
        ),
        (
            "subagent",
            vec![
                init(),
                json!({"type": "stream_event", "parent_tool_use_id": "toolu_1",
                       "event": {"type": "ping"}}),
            ],
        ),
        (
            "native tool",
            vec![
                init(),
                message_start(),
                stream(json!({"type": "content_block_start", "index": 0,
                          "content_block": {"type": "server_tool_use", "id": "x", "name": "web"}})),
            ],
        ),
        (
            "model",
            vec![
                init(),
                stream(json!({"type": "message_start", "message": {"model": "claude-opus-5"}})),
            ],
        ),
        (
            "init model",
            vec![{
                let mut bad = init();
                bad["model"] = json!("claude-opus-5");
                bad
            }],
        ),
        (
            "init tool",
            vec![{
                let mut bad = init();
                bad["tools"] = json!(["Bash"]);
                bad
            }],
        ),
        (
            "init server",
            vec![{
                let mut bad = init();
                bad["mcp_servers"] = json!([{"name": "other", "status": "connected"}]);
                bad
            }],
        ),
        (
            "init plugins",
            vec![{
                let mut bad = init();
                bad["plugins"] = json!([{"name": "p"}]);
                bad
            }],
        ),
        (
            "execution",
            vec![
                init(),
                json!({"type": "user", "message": {"content": [{"type": "tool_result", "is_error": false}]}}),
            ],
        ),
        ("unknown event", vec![init(), json!({"type": "mystery"})]),
        ("no init", vec![message_start()]),
    ];
    for (name, events) in cases {
        let mut r = reader(&["read_file"]);
        let mut last = Ok(Event::Nothing);
        for event in events {
            last = feed(&mut r, event);
            if last.is_err() {
                break;
            }
        }
        assert!(last.is_err(), "{name} was accepted");
    }
}

#[test]
fn a_second_result_and_malformed_lines_are_refused() {
    let mut r = reader(&[]);
    accept_text_response(&mut r);
    feed(&mut r, terminal("end_turn")).unwrap();
    feed(&mut r, stream(json!({"type": "message_stop"}))).unwrap();
    assert!(matches!(
        feed(&mut r, result("success")),
        Ok(Event::Complete)
    ));
    assert!(failure(feed(&mut r, result("success"))).contains("second result"));
    let mut r = reader(&[]);
    assert!(r.accept(b"not json").is_err());
    assert!(r.accept(&[0xff, 0xfe]).is_err());
}

#[test]
fn usage_that_disagrees_with_the_stream_is_refused() {
    let mut r = reader(&[]);
    accept_text_response(&mut r);
    feed(&mut r, terminal("end_turn")).unwrap();
    feed(&mut r, stream(json!({"type": "message_stop"}))).unwrap();
    let mut low = result("success");
    low["usage"] = json!({"output_tokens": 1});
    assert!(failure(feed(&mut r, low)).contains("usage"));
}

#[test]
fn history_acknowledgments_are_zero_turn_results() {
    let mut r = Reader::new(Expect {
        models: vec!["claude-sonnet-5-5".into()],
        aliases: Aliases::default(),
    });
    r.expect_ack();
    assert!(is_nothing(feed(&mut r, init())));
    assert!(matches!(
        feed(
            &mut r,
            json!({"type": "result", "subtype": "success", "num_turns": 0})
        ),
        Ok(Event::Ack)
    ));
    assert!(feed(&mut r, message_start()).is_err());
    let mut r = Reader::new(Expect {
        models: vec!["claude-sonnet-5-5".into()],
        aliases: Aliases::default(),
    });
    r.expect_ack();
    assert!(
        feed(
            &mut r,
            json!({"type": "result", "subtype": "success", "num_turns": 1})
        )
        .is_err()
    );
}

#[test]
fn native_errors_map_without_echoing_what_the_cli_said() {
    let mut r = reader(&[]);
    feed(&mut r, init()).unwrap();
    let mut failed = result("error_during_execution");
    failed["is_error"] = json!(true);
    failed["api_error_status"] = json!(429);
    failed["result"] = json!("secret detail xyz");
    let error = feed(&mut r, failed).unwrap_err();
    assert!(!error.is_retryable());
    assert!(!error.to_string().contains("secret detail"));
    assert_eq!(claude_error(&error).map(|e| e.kind), Some(ErrorKind::Limit));

    for (status, kind) in [(401, Some(ErrorKind::Auth)), (403, Some(ErrorKind::Auth))] {
        let mut r = reader(&[]);
        feed(&mut r, init()).unwrap();
        let mut failed = result("error_during_execution");
        failed["is_error"] = json!(true);
        failed["api_error_status"] = json!(status);
        let error = feed(&mut r, failed).unwrap_err();
        assert_eq!(claude_error(&error).map(|e| e.kind), kind);
        assert!(!error.is_retryable());
    }

    let mut r = reader(&[]);
    feed(&mut r, init()).unwrap();
    let mut failed = result("error_during_execution");
    failed["is_error"] = json!(true);
    failed["api_error_status"] = json!(529);
    assert!(feed(&mut r, failed).unwrap_err().is_retryable());

    let mut r = reader(&[]);
    feed(&mut r, init()).unwrap();
    feed(
        &mut r,
        json!({"type": "assistant", "error": "authentication_failed", "message": {}}),
    )
    .unwrap();
    let mut failed = result("error_during_execution");
    failed["is_error"] = json!(true);
    let error = feed(&mut r, failed).unwrap_err();
    assert_eq!(claude_error(&error).map(|e| e.kind), Some(ErrorKind::Auth));
}

#[test]
fn stderr_classifies_an_early_exit_without_being_shown() {
    assert!(!exit_failure("Error: not logged in").is_retryable());
    assert!(!exit_failure("usage limit reached for your plan").is_retryable());
    assert!(exit_failure("segfault").is_retryable());
    assert!(
        !exit_failure("usage limit")
            .to_string()
            .contains("usage limit reached")
    );
}
