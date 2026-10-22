use std::path::Path;

use fontaine_evals::baseline::{
    BaselineRecord, CcOutcome, attempted, capture_skip_reason, claude_rules, claude_settings, diff,
    extract_denials, outcome_of, read_records, render_prompt, write_records,
};
use fontaine_evals::{Case, run};
use serde_json::{Value, json};

fn case(extra: Value) -> Case {
    let mut base = json!({
        "id": "c", "kind": "pipeline", "category": "floor", "profile": "full",
        "task_intent": "Look around", "tool": "shell", "args": {"command": "ls"},
        "expected": "allow", "expected_tier": "tier05",
        "fixture_verdict": {"decision": "allow", "tier": "tier05"}
    });
    base.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    serde_json::from_value(base).unwrap()
}

fn fixture(name: &str) -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name),
    )
    .unwrap()
}

#[test]
fn denials_come_out_of_a_claude_result() {
    let denials = extract_denials(&fixture("claude_result_denied.json")).unwrap();
    assert_eq!(denials.len(), 2);
    assert_eq!(denials[0].tool_name, "Bash");
    assert_eq!(denials[0].tool_input["command"], "ls;rm x");
    assert_eq!(outcome_of(&denials), CcOutcome::Denied);
}

#[test]
fn an_event_array_is_read_through_its_result_event() {
    let denials = extract_denials(&fixture("claude_result_allowed.json")).unwrap();
    assert!(denials.is_empty());
    assert_eq!(outcome_of(&denials), CcOutcome::Allowed);
}

#[test]
fn output_without_denials_is_an_error() {
    assert!(extract_denials("not json").is_err());
    assert!(extract_denials(r#"{"type":"result"}"#).is_err());
    assert!(extract_denials(r#"[{"type":"system"}]"#).is_err());
}

#[test]
fn rules_map_to_claude_code_names() {
    assert_eq!(claude_rules("deny", "read(.env)").unwrap(), ["Read(.env)"]);
    assert_eq!(
        claude_rules("ask", "webfetch(domain:example.com)").unwrap(),
        ["WebFetch(domain:example.com)"]
    );
    assert_eq!(claude_rules("deny", "agent").unwrap(), ["Agent"]);
    assert_eq!(
        claude_rules("deny", "edit(src/**)").unwrap(),
        ["Edit(src/**)"]
    );
    assert_eq!(claude_rules("allow", "write").unwrap(), ["Write"]);
    assert_eq!(claude_rules("deny", "glob").unwrap(), ["Glob"]);
    assert_eq!(claude_rules("deny", "grep").unwrap(), ["Grep"]);
    assert_eq!(
        claude_rules("deny", "mcp__github__create_issue").unwrap(),
        ["mcp__github__create_issue"]
    );
}

#[test]
fn a_word_only_shell_deny_or_ask_rule_becomes_two_rules() {
    assert_eq!(
        claude_rules("deny", "shell(rm)").unwrap(),
        ["Bash(rm)", "Bash(rm *)"]
    );
    assert_eq!(
        claude_rules("ask", "shell(git push)").unwrap(),
        ["Bash(git push)", "Bash(git push *)"]
    );
    assert_eq!(
        claude_rules("allow", "shell(rm x)").unwrap(),
        ["Bash(rm x)"]
    );
    assert_eq!(
        claude_rules("allow", "shell(git push *)").unwrap(),
        ["Bash(git push *)"]
    );
    assert_eq!(claude_rules("deny", "shell(rm *)").unwrap(), ["Bash(rm *)"]);
}

#[test]
fn settings_carry_all_three_lists() {
    let c = case(
        json!({"permissions": {"deny": ["shell(rm)"], "ask": ["read(.env)"], "allow": ["shell(rm x)"]}}),
    );
    assert_eq!(
        claude_settings(&c).unwrap(),
        json!({"permissions": {
            "deny": ["Bash(rm)", "Bash(rm *)"],
            "ask": ["Read(.env)"],
            "allow": ["Bash(rm x)"]
        }})
    );
}

#[test]
fn the_prompt_names_the_claude_tool_and_arguments() {
    let c = case(
        json!({"tool": "edit", "args": {"path": "a.txt", "edits": [{"old_text": "a", "new_text": "b", "replace_all": false}]}}),
    );
    let prompt = render_prompt(&c).unwrap();
    assert!(prompt.contains("Call the Edit tool exactly once"));
    assert!(prompt.contains(r#""file_path":"a.txt""#), "{prompt}");
    assert!(prompt.contains(r#""old_string":"a""#), "{prompt}");
    assert!(prompt.contains("Look around"));
    let shell = render_prompt(&case(json!({}))).unwrap();
    assert!(shell.contains("Bash") && shell.contains(r#""command":"ls""#));
}

#[test]
fn capture_skips_what_claude_code_cannot_express() {
    assert_eq!(capture_skip_reason(&case(json!({}))), None);
    let extra = case(json!({"permissions": {"allow_extra": ["ls"]}}));
    assert!(capture_skip_reason(&extra).unwrap().contains("allow_extra"));
    let facts = case(json!({"session_facts": {"in_container": true}}));
    assert!(
        capture_skip_reason(&facts)
            .unwrap()
            .contains("session_facts")
    );
    let ls = case(json!({"tool": "ls", "args": {"path": "."}}));
    assert!(
        capture_skip_reason(&ls)
            .unwrap()
            .contains("no Claude Code equivalent")
    );
    // `auto` is posed to `claude -p --permission-mode auto`.
    let auto = case(json!({"mode": "auto"}));
    assert_eq!(capture_skip_reason(&auto), None);
}

fn record(id: &str, outcome: CcOutcome) -> BaselineRecord {
    BaselineRecord {
        case_id: id.into(),
        mode: "manual".into(),
        cc_outcome: outcome,
        claude: "2.1.0 (Claude Code)".into(),
    }
}

fn run_file(cases: &[(&str, &str)]) -> String {
    let cases: Vec<Value> = cases
        .iter()
        .map(|(id, actual)| json!({"id": id, "actual": actual, "mode": "manual", "pass": true}))
        .collect();
    json!({"summary": {}, "cases": cases}).to_string()
}

#[test]
fn diff_reports_every_outcome_that_differs() {
    let baseline = [
        record("same-allow", CcOutcome::Allowed),
        record("same-deny", CcOutcome::Denied),
        record("prompt-is-denied", CcOutcome::Denied),
        record("we-allow-they-deny", CcOutcome::Denied),
        record("we-deny-they-allow", CcOutcome::Allowed),
        record("we-prompt-they-allow", CcOutcome::Allowed),
        record("missing", CcOutcome::Allowed),
    ];
    let report = diff(
        &baseline,
        &run_file(&[
            ("same-allow", "allow"),
            ("same-deny", "deny"),
            ("prompt-is-denied", "prompt"),
            ("we-allow-they-deny", "allow"),
            ("we-deny-they-allow", "deny"),
            ("we-prompt-they-allow", "prompt"),
        ]),
    )
    .unwrap();
    let ids: Vec<&str> = report
        .divergences
        .iter()
        .map(|d| d.case_id.as_str())
        .collect();
    assert_eq!(
        ids,
        [
            "we-allow-they-deny",
            "we-deny-they-allow",
            "we-prompt-they-allow"
        ]
    );
    assert_eq!(report.compared, 6);
    assert_eq!(report.unmatched, ["missing"]);
}

#[test]
fn diff_command_reads_files_and_exits_1_on_a_divergence() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base.jsonl");
    write_records(
        &base,
        &[
            record("a", CcOutcome::Allowed),
            record("b", CcOutcome::Denied),
        ],
    )
    .unwrap();
    assert_eq!(read_records(&base).unwrap().len(), 2);
    let json = dir.path().join("run.json");

    std::fs::write(&json, run_file(&[("a", "allow"), ("b", "prompt")])).unwrap();
    let args = |p: &Path, r: &Path| {
        [
            "fontaine-evals",
            "baseline",
            "diff",
            p.to_str().unwrap(),
            r.to_str().unwrap(),
        ]
        .map(Into::into)
    };
    assert_eq!(run(args(&base, &json)), 0);

    std::fs::write(&json, run_file(&[("a", "deny"), ("b", "prompt")])).unwrap();
    assert_eq!(run(args(&base, &json)), 1);

    assert_eq!(run(args(&base, &dir.path().join("absent.json"))), 2);
}

#[test]
fn a_record_serializes_with_the_documented_fields() {
    let line = serde_json::to_value(record("a", CcOutcome::Denied)).unwrap();
    assert_eq!(
        line,
        json!({"case_id": "a", "mode": "manual", "cc_outcome": "denied", "claude": "2.1.0 (Claude Code)"})
    );
}

#[test]
fn a_call_that_was_never_made_is_not_attempted() {
    let called = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{}}]}}
{"type":"result","permission_denials":[]}"#;
    let chatted = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"done"}]}}
{"type":"result","permission_denials":[]}"#;
    assert!(attempted(called, "Bash").unwrap());
    assert!(!attempted(called, "Read").unwrap());
    assert!(!attempted(chatted, "Bash").unwrap());
    assert!(extract_denials(called).unwrap().is_empty());
}
