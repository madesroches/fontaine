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
fn history_may_call_a_tool_the_request_omits() {
    let t = translated(request(json!({
        "tools": [tool("read_file")],
        "messages": [
            {"role": "user", "content": "go"},
            {"role": "assistant", "content": [
                {"type": "tool_use", "id": "t1", "name": "ask_user_question", "input": {}},
            ]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": "a"},
            ]},
        ],
    })));
    assert_eq!(
        t.frames[1].value["message"]["content"][0]["name"],
        "mcp__fontaine__ask_user_question"
    );
    assert!(t.aliases.full("ask_user_question").is_none());
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
    let mut reader = Reader::new(
        Expect {
            models: vec!["claude-sonnet-5-5".into(), "sonnet".into()],
            aliases: Aliases::new(tools.iter().copied()),
        },
        None,
    );
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
    assert!(
        failure(feed(&mut r, result("success"))).contains("ended before the provider ended it")
    );
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
        json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "t", "is_error": true}]}}),
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
    assert!(
        error.contains("did not define: `Bash`; this request defines read_file"),
        "{error}"
    );
}

#[test]
fn an_undefined_call_lists_a_capped_set_of_the_defined_tools() {
    let names: Vec<String> = (0..45).map(|i| format!("tool_{i:02}")).collect();
    let mut r = reader(&names.iter().map(String::as_str).collect::<Vec<_>>());
    feed(&mut r, init()).unwrap();
    feed(&mut r, message_start()).unwrap();
    let long = "x".repeat(500);
    let error = failure(feed(
        &mut r,
        stream(json!({"type": "content_block_start", "index": 0,
                      "content_block": {"type": "tool_use", "id": "t", "name": long, "input": {}}})),
    ));
    assert!(error.contains(&format!("`{}`;", "x".repeat(80))), "{error}");
    assert!(!error.contains(&"x".repeat(81)), "{error}");
    assert!(error.contains("tool_39, and 5 more"), "{error}");

    let mut none = reader(&[]);
    feed(&mut none, init()).unwrap();
    feed(&mut none, message_start()).unwrap();
    let error = failure(feed(
        &mut none,
        stream(json!({"type": "content_block_start", "index": 0,
                      "content_block": {"type": "tool_use", "id": "t", "name": "Task", "input": {}}})),
    ));
    assert!(
        error.contains("`Task`; this request defines no tools"),
        "{error}"
    );
}

#[test]
fn an_init_offering_an_undefined_tool_names_it() {
    let error = init_error(init_with("tools", json!(["Bash"])));
    assert!(error.contains("did not define: `Bash`"), "{error}");
}

#[test]
fn an_init_without_exactly_the_inventory_names_what_the_cli_reported() {
    let failed = init_error(init_with(
        "mcp_servers",
        json!([{"name": "fontaine", "status": "failed"}]),
    ));
    assert!(
        failed.contains("reported MCP servers: fontaine (failed)"),
        "{failed}"
    );
    assert!(
        failed.contains("could not start or talk to the inventory"),
        "{failed}"
    );

    let extra = init_error(init_with(
        "mcp_servers",
        json!([{"name": "fontaine", "status": "connected"}, {"name": "other"}]),
    ));
    assert!(
        extra.contains("reported MCP servers: fontaine (connected), other (?)"),
        "{extra}"
    );
    assert!(!extra.contains("could not start"), "{extra}");

    let none = init_error(init_with("mcp_servers", json!([])));
    assert!(none.contains("reported MCP servers: none"), "{none}");
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

fn init_with(key: &str, value: Value) -> Value {
    let mut init = init();
    init[key] = value;
    init
}

fn init_error(init: Value) -> String {
    let mut r = reader(&[]);
    feed(&mut r, init).unwrap_err().to_string()
}

#[test]
fn bundled_plugins_are_accepted_and_the_stream_decodes() {
    let plugins: Vec<Value> = [
        "cc-plugin-agents-md",
        "cc-plugin-telemetry",
        "cc-plugin-privacy",
        "cc-plugin-plugin-authoring",
    ]
    .iter()
    .map(|n| json!({"name": n, "path": "builtin", "source": format!("{n}@builtin")}))
    .collect();
    let mut r = reader(&[]);
    feed(&mut r, init_with("plugins", json!(plugins))).unwrap();
    for value in [
        message_start(),
        stream(json!({"type": "content_block_start", "index": 0,
                      "content_block": {"type": "text", "text": ""}})),
    ] {
        feed(&mut r, value).unwrap();
    }
}

#[test]
fn a_user_plugin_beside_bundled_ones_is_named() {
    let message = init_error(init_with(
        "plugins",
        json!([
            {"name": "cc-plugin-telemetry", "path": "builtin", "source": "cc-plugin-telemetry@builtin"},
            {"name": "foo", "path": "/p", "source": "foo@market"},
        ]),
    ));
    assert!(message.contains("plugins"), "{message}");
    assert!(message.contains("foo@market"), "{message}");
    assert!(!message.contains("cc-plugin-telemetry"), "{message}");
}

#[test]
fn both_bundled_markers_are_required() {
    for plugin in [
        json!({"path": "builtin", "source": "x@market"}),
        json!({"path": "/p", "source": "x@builtin"}),
    ] {
        let mut r = reader(&[]);
        assert!(feed(&mut r, init_with("plugins", json!([plugin]))).is_err());
    }
}

#[test]
fn skills_are_refused_by_name() {
    for entry in [json!("my-skill"), json!({"name": "my-skill"})] {
        let message = init_error(init_with("skills", json!([entry])));
        assert!(message.contains("skills"), "{message}");
        assert!(message.contains("my-skill"), "{message}");
    }
}

#[test]
fn listed_entries_are_capped_in_number_and_length() {
    let plugins: Vec<Value> = (0..7)
        .map(|i| json!({"name": format!("p{i}"), "path": "/p"}))
        .collect();
    let message = init_error(init_with("plugins", json!(plugins)));
    assert!(message.contains("p4"), "{message}");
    assert!(!message.contains("p5"), "{message}");
    assert!(message.contains("and 2 more"), "{message}");

    let long = "é".repeat(500);
    let message = init_error(init_with("plugins", json!([{"name": long}])));
    assert!(message.contains(&"é".repeat(80)), "{message}");
    assert!(!message.contains(&"é".repeat(81)), "{message}");
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
    let mut r = Reader::new(
        Expect {
            models: vec!["claude-sonnet-5-5".into()],
            aliases: Aliases::default(),
        },
        None,
    );
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
    let mut r = Reader::new(
        Expect {
            models: vec!["claude-sonnet-5-5".into()],
            aliases: Aliases::default(),
        },
        None,
    );
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
fn the_reported_category_backs_an_empty_stderr() {
    assert!(!exit_failure("", Some(&Native::Auth)).is_retryable());
    assert!(exit_failure("", Some(&Native::Server)).is_retryable());
}

#[test]
fn stderr_classifies_an_early_exit_without_being_shown() {
    assert!(!exit_failure("Error: not logged in", None).is_retryable());
    assert!(!exit_failure("usage limit reached for your plan", None).is_retryable());
    assert!(exit_failure("segfault", None).is_retryable());
    assert!(
        !exit_failure("usage limit", None)
            .to_string()
            .contains("usage limit reached")
    );
}

const READ: &str = "mcp__fontaine__read_file";

fn tool_start(r: &mut Reader, index: u64, id: &str, name: &str) -> Result<Event, Failure> {
    feed(
        r,
        stream(json!({"type": "content_block_start", "index": index,
                      "content_block": {"type": "tool_use", "id": id, "name": name,
                                        "caller": {"type": "direct"}, "input": {}}})),
    )
}

fn stop_block(r: &mut Reader, index: u64) {
    feed(
        r,
        stream(json!({"type": "content_block_stop", "index": index})),
    )
    .unwrap();
}

fn denied(id: &str, name: &str) -> Value {
    json!({"type": "system", "subtype": "permission_denied", "tool_name": name,
           "tool_use_id": id, "decision_reason_type": "mode", "message": "UNIQUE-RAW"})
}

fn echo(ids: &[&str]) -> Value {
    let blocks: Vec<Value> = ids
        .iter()
        .map(|id| json!({"type": "tool_result", "tool_use_id": id, "is_error": true}))
        .collect();
    json!({"type": "user", "message": {"content": blocks}})
}

fn started(tools: &[&str]) -> Reader {
    let mut r = reader(tools);
    feed(&mut r, init()).unwrap();
    feed(&mut r, message_start()).unwrap();
    r
}

fn finish_tool_use(r: &mut Reader) -> Result<Event, Failure> {
    feed(r, terminal("tool_use")).unwrap();
    feed(r, stream(json!({"type": "message_stop"}))).unwrap();
    feed(r, result("error_max_turns"))
}

#[test]
fn the_mid_stream_denial_order_of_cli_2_1_319_completes() {
    let mut r = started(&["read_file"]);
    tool_start(&mut r, 0, "toolu_A", READ).unwrap();
    feed(&mut r, json!({"type": "assistant", "message": {}})).unwrap();
    assert!(is_nothing(feed(&mut r, denied("toolu_A", READ))));
    stop_block(&mut r, 0);
    assert!(is_nothing(feed(&mut r, echo(&["toolu_A"]))));
    feed(&mut r, json!({"type": "rate_limit_event"})).unwrap();
    assert!(matches!(finish_tool_use(&mut r), Ok(Event::Complete)));
}

#[test]
fn parallel_calls_interleave_their_denials_and_echoes() {
    let mut r = started(&["read_file"]);
    tool_start(&mut r, 0, "toolu_A", READ).unwrap();
    assert!(is_nothing(feed(&mut r, denied("toolu_A", READ))));
    stop_block(&mut r, 0);
    tool_start(&mut r, 1, "toolu_B", READ).unwrap();
    assert!(is_nothing(feed(&mut r, echo(&["toolu_A"]))));
    assert!(is_nothing(feed(&mut r, denied("toolu_B", READ))));
    stop_block(&mut r, 1);
    assert!(is_nothing(feed(&mut r, echo(&["toolu_B"]))));
    assert!(matches!(finish_tool_use(&mut r), Ok(Event::Complete)));
}

#[test]
fn a_denial_is_matched_to_a_call_the_response_made() {
    let mut r = started(&["read_file"]);
    tool_start(&mut r, 0, "toolu_A", READ).unwrap();
    let unknown = failure(feed(&mut r, denied("toolu_other", READ)));
    assert!(
        unknown.contains("(event: system/permission_denied)"),
        "{unknown}"
    );
    assert!(!unknown.contains("UNIQUE-RAW"));
    assert!(feed(&mut r, denied("toolu_A", "mcp__fontaine__other")).is_err());
    assert!(is_nothing(feed(&mut r, denied("toolu_A", READ))));
    assert!(feed(&mut r, denied("toolu_A", READ)).is_err());
}

#[test]
fn an_echo_must_answer_an_unanswered_call_with_an_error() {
    let mut r = started(&["read_file"]);
    tool_start(&mut r, 0, "toolu_A", READ).unwrap();
    assert!(feed(&mut r, echo(&["toolu_other"])).is_err());
    assert!(feed(&mut r, echo(&[])).is_err());
    let mixed = json!({"type": "user", "message": {"content": [
        {"type": "tool_result", "tool_use_id": "toolu_A", "is_error": true},
        {"type": "tool_result", "tool_use_id": "toolu_A", "is_error": false},
    ]}});
    assert!(feed(&mut r, mixed).is_err());
    assert!(is_nothing(feed(&mut r, echo(&["toolu_A"]))));
    assert!(feed(&mut r, echo(&["toolu_A"])).is_err());
}

#[test]
fn a_denial_next_to_an_end_turn_stop_contradicts_the_response() {
    let mut r = started(&["read_file"]);
    tool_start(&mut r, 0, "toolu_A", READ).unwrap();
    feed(&mut r, denied("toolu_A", READ)).unwrap();
    stop_block(&mut r, 0);
    feed(&mut r, terminal("end_turn")).unwrap();
    feed(&mut r, stream(json!({"type": "message_stop"}))).unwrap();
    let error = failure(feed(&mut r, result("success")));
    assert!(error.contains("contradicts"), "{error}");
}

#[test]
fn a_suppressed_end_conversation_call_may_be_denied() {
    let mut r = started(&["read_file"]);
    assert!(is_nothing(tool_start(
        &mut r,
        0,
        "toolu_E",
        "EndConversation"
    )));
    assert!(is_nothing(feed(
        &mut r,
        denied("toolu_E", "EndConversation")
    )));
    stop_block(&mut r, 0);
    assert!(is_nothing(feed(&mut r, echo(&["toolu_E"]))));
    assert!(matches!(finish_tool_use(&mut r), Ok(Event::Complete)));
}

#[test]
fn a_missing_or_reused_tool_call_id_is_refused() {
    let mut r = started(&["read_file"]);
    let missing = feed(
        &mut r,
        stream(json!({"type": "content_block_start", "index": 0,
                      "content_block": {"type": "tool_use", "name": READ, "input": {}}})),
    );
    assert!(failure(missing).contains("no id"));
    tool_start(&mut r, 0, "toolu_A", READ).unwrap();
    assert!(failure(tool_start(&mut r, 1, "toolu_A", READ)).contains("reused"));
}

#[test]
fn thinking_estimates_are_accepted_in_a_response_only() {
    let estimate = json!({"type": "system", "subtype": "thinking_tokens",
                          "estimated_tokens": 5, "estimated_tokens_delta": 5});
    let mut r = started(&[]);
    assert!(is_nothing(feed(&mut r, estimate.clone())));
    let mut r = Reader::new(
        Expect {
            models: vec!["claude-sonnet-5-5".into()],
            aliases: Aliases::default(),
        },
        None,
    );
    r.expect_ack();
    feed(&mut r, init()).unwrap();
    assert!(feed(&mut r, estimate).is_err());
}

#[test]
fn other_system_events_are_refused_by_name() {
    let mut r = started(&[]);
    for subtype in ["model_fallback", "notification"] {
        let error = failure(feed(&mut r, json!({"type": "system", "subtype": subtype})));
        assert!(
            error.contains("does not accept")
                && error.ends_with(&format!("(event: system/{subtype})")),
            "{error}"
        );
    }
}

#[test]
fn rejected_events_are_named_without_echoing_raw_output() {
    let mut r = started(&[]);
    let unknown = failure(feed(&mut r, stream(json!({"type": "surprise"}))));
    assert!(
        unknown.ends_with("(event: stream_event/surprise)"),
        "{unknown}"
    );
    for subtype in ["x".repeat(65), "a b".to_string()] {
        let error = failure(feed(&mut r, json!({"type": "system", "subtype": subtype})));
        assert!(error.ends_with("(event: system/?)"), "{error}");
    }
    let none = failure(feed(&mut r, json!({"type": "system"})));
    assert!(none.ends_with("(event: system/(none))"), "{none}");
    let text = r.accept(b"not json").unwrap_err().to_string();
    assert!(text.ends_with("(event: (not JSON))"), "{text}");
    // A truncation is not a protocol failure and is not relabelled.
    let mut r = started(&["read_file"]);
    tool_start(&mut r, 0, "toolu_A", READ).unwrap();
    feed(&mut r, terminal("tool_use")).unwrap();
    let cut = feed(&mut r, stream(json!({"type": "message_stop"}))).unwrap_err();
    assert!(matches!(*cut, ProviderError::Truncated));
}

#[test]
fn the_trail_collapses_repeats_and_drops_the_oldest() {
    let mut r = started(&[]);
    assert_eq!(r.trail(), "system/init → stream_event/message_start");
    for _ in 0..3 {
        feed(&mut r, json!({"type": "rate_limit_event"})).unwrap();
    }
    assert!(r.trail().ends_with("rate_limit_event ×3"), "{}", r.trail());
    let mut r = started(&[]);
    for _ in 0..80 {
        feed(&mut r, json!({"type": "system", "subtype": "status"})).unwrap();
        feed(&mut r, json!({"type": "rate_limit_event"})).unwrap();
    }
    let trail = r.trail();
    assert!(trail.starts_with("… → "), "{trail}");
    assert_eq!(trail.split(" → ").count(), 65);
}

#[test]
fn a_rate_limit_frame_is_recorded_and_still_nothing() {
    let usage = Arc::new(Usage::new());
    let mut r = Reader::new(
        Expect {
            models: vec!["claude-sonnet-5-5".into()],
            aliases: Aliases::default(),
        },
        Some(usage.clone()),
    );
    r.expect_response();
    feed(&mut r, init()).unwrap();
    feed(&mut r, message_start()).unwrap();
    let frame = json!({"type": "rate_limit_event", "rate_limit_info": {"unifiedWindows": {
        "five_hour": {"utilization": 0.09, "resetsAt": 1790578200u64},
        "seven_day": {"utilization": 0.15, "resetsAt": 1791036000u64}}}});
    assert!(is_nothing(feed(&mut r, frame)));
    let recorded = usage.snapshot().expect("recorded");
    assert_eq!(recorded.windows.len(), 2);

    assert!(is_nothing(feed(
        &mut r,
        json!({"type": "rate_limit_event"})
    )));
    assert_eq!(usage.snapshot(), Some(recorded));
}
