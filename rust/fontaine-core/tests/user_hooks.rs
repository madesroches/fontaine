//! User hooks as real processes: the wire format, the exit codes, how answers compose, and what
//! a hook can never do. The hook is the `hook_double` example, a program rather than a shell
//! script, so the same test runs on every platform.

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use common::pipeline::{
    Options, Rig, allow_verdict, allowed, denied_message, fake_guardian, verdict_json,
};
use fontaine_agents::tasks::{QualityGate, Task, TaskStatus};
use fontaine_core::audit::args_digest;
use fontaine_core::config::PermissionMode::{Auto, Manual};
use fontaine_core::config::{Hook, HookEvent, Origin, Slot};
use fontaine_core::permissions::{PermissionDeniedHook, PreToolUseHook};
use fontaine_core::tier::Tier;
use fontaine_core::user_hooks::{HookDeps, HookProcess, PostHookStage, TeamGate, UserHook};
use fontaine_tools::gate::{CallView, ROOT_AGENT};
use fontaine_tools::result_pipeline::{ResultPipeline, StageKind};
use fontaine_tools::risk::RiskClass;
use rig_core::tool::ToolOutput;
use serde_json::{Value, json};

fn double() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap().join("examples");
    let name = if cfg!(windows) {
        "hook_double.exe"
    } else {
        "hook_double"
    };
    let path = dir.join(name);
    assert!(path.exists(), "{} is not built", path.display());
    path
}

/// A shell word that keeps `text` as one argument, on every platform.
fn word(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

fn command(args: &[&str]) -> String {
    let mut line = word(&double().to_string_lossy());
    for arg in args {
        line.push(' ');
        line.push_str(&word(arg));
    }
    line
}

fn process(scrub: Vec<std::ffi::OsString>) -> Arc<HookProcess> {
    process_with_env(scrub, Vec::new())
}

fn process_with_env(
    scrub: Vec<std::ffi::OsString>,
    set_env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
) -> Arc<HookProcess> {
    Arc::new(HookProcess {
        scrub_env: scrub,
        set_env,
        shell: common::command_shell(),
        project_dir: std::env::temp_dir(),
    })
}

fn user() -> Origin {
    Origin::default()
}

fn project() -> Origin {
    Origin {
        slot: Slot::Project,
        user_trust: false,
    }
}

fn hook(matcher: Option<&str>, command: String, origin: Origin) -> Hook {
    Hook {
        timeout_secs: Some(10),
        origin,
        ..Hook::command(matcher, &command)
    }
}

/// A command hook of the named event, with nothing to tell and no model.
fn command_hook(event: &str, hook: &Hook, process: Arc<HookProcess>) -> UserHook {
    let event = HookEvent::ALL
        .into_iter()
        .find(|e| e.config_key() == event)
        .expect("a hook event");
    UserHook::new(event, hook, &HookDeps::new(process))
}

fn pre(h: &Hook) -> Arc<dyn PreToolUseHook> {
    Arc::new(command_hook("pre_tool_use", h, process(Vec::new())))
}

fn reply(permission: &str) -> String {
    json!({"hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "permissionDecision": permission,
        "permissionDecisionReason": format!("because {permission}"),
    }})
    .to_string()
}

fn rig(mode: fontaine_core::config::PermissionMode, settings: &str, hooks: Vec<Hook>) -> Rig {
    let mut options = Options::new(mode).settings(settings);
    options.hooks = hooks.iter().map(pre).collect();
    Rig::with(options)
}

fn shell(command: &str) -> Value {
    json!({ "command": command })
}

fn last(rig: &Rig) -> fontaine_core::audit::DecisionRecord {
    rig.audit().last().cloned().expect("a record")
}

#[tokio::test]
async fn the_event_carries_claude_codes_fields_and_ours() {
    let dir = tempfile::tempdir().unwrap();
    let record = dir.path().join("event.json");
    let h = hook(
        None,
        command(&["--record", &record.to_string_lossy()]),
        user(),
    );
    let rig = rig(Manual, "", vec![h]);
    std::fs::write(rig.work().join("a.txt"), "a").unwrap();
    rig.check("read", json!({"path": "a.txt"})).await;

    let event: Value = serde_json::from_str(&std::fs::read_to_string(&record).unwrap()).unwrap();
    assert_eq!(event["hook_event_name"], "PreToolUse");
    assert_eq!(event["tool_name"], "read");
    assert_eq!(event["tool_input"], json!({"path": "a.txt"}));
    assert_eq!(event["session_id"], rig.state.session_id);
    assert_eq!(event["agent_id"], ROOT_AGENT);
    assert_eq!(event["permission_mode"], "manual");
    assert_eq!(
        event["cwd"].as_str().unwrap(),
        rig.work().display().to_string()
    );
    assert_eq!(
        event["workspace_roots"][0].as_str().unwrap(),
        rig.work().display().to_string()
    );
    assert!(event.get("transcript_path").is_none());
    assert!(event.get("tool_response").is_none());
}

#[tokio::test]
async fn a_double_backslash_in_a_hook_command_reaches_the_hook_as_written() {
    let text = r"a\\b\\";
    let h = hook(None, command(&["--stderr", text, "--exit", "2"]), user());
    let rig = rig(Manual, "", vec![h]);
    let d = rig.check("shell", shell("git push")).await;
    assert!(
        denied_message(&d).contains(&format!("({text})")),
        "{}",
        denied_message(&d)
    );
}

#[tokio::test]
async fn a_verbatim_windows_path_in_a_hook_command_reaches_the_hook() {
    let dir = tempfile::tempdir().unwrap();
    let record = dir.path().canonicalize().unwrap().join("event.json");
    let h = hook(
        None,
        command(&["--record", &record.to_string_lossy()]),
        user(),
    );
    let rig = rig(Manual, "", vec![h]);
    std::fs::write(rig.work().join("a.txt"), "a").unwrap();
    rig.check("read", json!({"path": "a.txt"})).await;
    assert!(record.exists());
}

#[tokio::test]
async fn updated_input_is_what_the_floor_the_rules_and_the_audit_see() {
    let settings = "[permissions]\ndeny = [\"read(.env)\"]\n";
    let rewrite = json!({"hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "updatedInput": {"path": ".env"},
    }})
    .to_string();
    let rig = rig(
        Manual,
        settings,
        vec![hook(None, command(&["--reply", &rewrite]), user())],
    );
    let original = json!({"path": "a.txt"});
    let d = rig.check("read", original.clone()).await;
    assert!(!allowed(&d));
    let record = last(&rig);
    assert_eq!(record.tier, Tier::DenyRule);
    assert_eq!(record.args_digest, args_digest(&json!({"path": ".env"})));
    assert_eq!(record.original_args_digest, Some(args_digest(&original)));

    // The floor judges the rewrite too.
    let to_rm = json!({"hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "updatedInput": {"command": "rm -rf /"},
    }})
    .to_string();
    let rig2 = self::rig(
        Manual,
        "",
        vec![hook(None, command(&["--reply", &to_rm]), user())],
    );
    rig2.check("shell", shell("ls")).await;
    // The prompt's answer is a second record; the first is the floor's.
    assert_eq!(rig2.audit()[0].tier, Tier::CriticalPath);
}

#[tokio::test]
async fn deny_beats_ask_beats_allow_and_the_exit_code_two_denies_with_its_stderr() {
    let hooks = vec![
        hook(None, command(&["--reply", &reply("allow")]), user()),
        hook(None, command(&["--reply", &reply("ask")]), user()),
    ];
    let rig = rig(Manual, "", hooks.clone());
    rig.approver
        .answer(fontaine_core::permissions::Answer::AllowOnce);
    assert!(allowed(&rig.check("shell", shell("cargo test")).await));
    assert_eq!(rig.approver.asked(), 1, "ask beat allow");

    let mut with_deny = hooks;
    with_deny.push(hook(None, command(&["--reply", &reply("deny")]), user()));
    let rig = self::rig(Manual, "", with_deny);
    let d = rig.check("shell", shell("cargo test")).await;
    assert!(denied_message(&d).contains("because deny"));
    assert_eq!(last(&rig).tier, Tier::Hook);

    let exit_two = hook(
        None,
        command(&["--stderr", "no pushing on fridays", "--exit", "2"]),
        user(),
    );
    let rig = self::rig(Manual, "", vec![exit_two]);
    let d = rig.check("shell", shell("git push")).await;
    assert!(denied_message(&d).contains("no pushing on fridays"));
    assert_eq!(last(&rig).tier, Tier::Hook);
}

#[tokio::test]
async fn a_hook_that_fails_times_out_or_answers_unreadably_decides_nothing() {
    let mut slow = hook(
        None,
        command(&["--sleep", "30", "--reply", &reply("deny")]),
        user(),
    );
    slow.timeout_secs = Some(1);
    let cases = [
        hook(
            None,
            command(&["--exit", "1", "--reply", &reply("deny")]),
            user(),
        ),
        hook(None, command(&["--reply", "not json at all"]), user()),
        hook(
            None,
            command(&[
                "--reply",
                r#"{"hookSpecificOutput":{"permissionDecision":"maybe"}}"#,
            ]),
            user(),
        ),
        hook(
            None,
            command(&[
                "--reply",
                r#"{"hookSpecificOutput":{"hookEventName":"PostToolUse","permissionDecision":"deny"}}"#,
            ]),
            user(),
        ),
        slow,
        hook(None, "fontaine-no-such-hook-program".to_string(), user()),
    ];
    for h in cases {
        let rig = rig(Manual, "", vec![h.clone()]);
        // With no hook decision the call is what the mode says: a prompt, denied by the
        // scripted approver. The point is that it is not the hook's tier.
        let d = rig.check("shell", shell("cargo test")).await;
        assert!(!allowed(&d), "{h:?}");
        assert_ne!(last(&rig).tier, Tier::Hook, "{h:?}");
        assert_eq!(rig.approver.asked(), 1, "the call fell to the mode: {h:?}");
    }
}

#[tokio::test]
async fn a_hook_allow_never_passes_a_deny_rule_or_the_critical_path() {
    let allow = command(&["--reply", &reply("allow")]);
    let rig = rig(
        Manual,
        "[permissions]\ndeny = [\"shell(git push)\"]\n",
        vec![hook(None, allow.clone(), user())],
    );
    assert!(!allowed(&rig.check("shell", shell("git push")).await));
    assert_eq!(last(&rig).tier, Tier::DenyRule);

    let rig = self::rig(Manual, "", vec![hook(None, allow, user())]);
    let _ = rig.check("shell", shell("rm -rf /")).await;
    assert_eq!(rig.audit()[0].tier, Tier::CriticalPath);
}

#[tokio::test]
async fn in_auto_a_narrow_matchers_allow_skips_the_guardian_and_a_broad_one_is_suspended() {
    let allow = command(&["--reply", &reply("allow")]);

    let fake = fake_guardian(allow_verdict());
    let mut options = Options::new(Auto).guardian(fake.guardian.clone());
    options.hooks = vec![pre(&hook(Some("shell(cargo test)"), allow.clone(), user()))];
    let narrow = Rig::with(options);
    assert!(allowed(&narrow.check("shell", shell("cargo test")).await));
    assert_eq!(last(&narrow).tier, Tier::Hook);
    assert_eq!(fake.calls(), 0);

    for matcher in [Some("shell"), Some("shell(python *)"), None] {
        let fake = fake_guardian(allow_verdict());
        let mut options = Options::new(Auto).guardian(fake.guardian.clone());
        options.hooks = vec![pre(&hook(matcher, allow.clone(), user()))];
        let broad = Rig::with(options);
        assert!(allowed(
            &broad.check("shell", shell("python script.py")).await
        ));
        assert_eq!(fake.calls(), 1, "{matcher:?} is suspended in auto");
        assert_eq!(last(&broad).tier, Tier::Guardian);
    }
}

#[tokio::test]
async fn a_later_hooks_rewrite_drops_an_earlier_hooks_allow() {
    let rewrite = json!({"hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "updatedInput": {"command": "cargo build"},
    }})
    .to_string();
    let rig = rig(
        Manual,
        "",
        vec![
            hook(None, command(&["--reply", &reply("allow")]), user()),
            hook(None, command(&["--reply", &rewrite]), project()),
        ],
    );
    rig.check("shell", shell("cargo test")).await;
    assert_ne!(last(&rig).tier, Tier::Hook);
    assert_eq!(rig.approver.asked(), 1);
}

#[tokio::test]
async fn a_project_hooks_allow_is_ignored_but_its_deny_stands() {
    let rig = rig(
        Manual,
        "",
        vec![hook(
            None,
            command(&["--reply", &reply("allow")]),
            project(),
        )],
    );
    rig.check("shell", shell("cargo test")).await;
    assert_ne!(last(&rig).tier, Tier::Hook);
    assert_eq!(rig.approver.asked(), 1);

    let rig = self::rig(
        Manual,
        "",
        vec![hook(None, command(&["--reply", &reply("deny")]), project())],
    );
    assert!(!allowed(&rig.check("shell", shell("cargo test")).await));
    assert_eq!(last(&rig).tier, Tier::Hook);
}

#[tokio::test]
async fn a_hook_only_runs_for_the_calls_its_matcher_covers() {
    let dir = tempfile::tempdir().unwrap();
    let record = dir.path().join("event.json");
    let rig = rig(
        Manual,
        "",
        vec![hook(
            Some("shell(git *)"),
            command(&["--record", &record.to_string_lossy()]),
            user(),
        )],
    );
    std::fs::write(rig.work().join("a.txt"), "a").unwrap();
    rig.check("read", json!({"path": "a.txt"})).await;
    assert!(!record.exists(), "a read is not a git command");
    rig.check("shell", shell("git status")).await;
    assert!(record.exists());
}

#[tokio::test]
async fn hook_children_get_the_scrubbed_environment() {
    let names = [
        "FONTAINE_TEST_PROVIDER_KEY",
        "FONTAINE_TEST_TELEMETRY_KEY",
        "FONTAINE_TEST_HF_TOKEN",
        "MICROMEGAS_TEST_INGESTION",
        "FONTAINE_TEST_KEPT",
    ];
    // Setting a variable is unsafe on its own terms; these names are this test's alone.
    for name in names {
        unsafe { std::env::set_var(name, "x") };
    }
    let w = common::World::new();
    w.user_file(
        "[accounts.a]\nkind = \"openai\"\napi_key_env = \"FONTAINE_TEST_PROVIDER_KEY\"\n[telemetry]\napi_key_env = \"FONTAINE_TEST_TELEMETRY_KEY\"\n[guardian]\nscanner = \"llm\"\nscanner_hf_token_env = \"FONTAINE_TEST_HF_TOKEN\"\n",
    );
    let config = w.load().unwrap();
    let scrub = fontaine_core::tools::scrubbed_env(
        &config,
        std::env::vars_os()
            .map(|(name, _)| name)
            .collect::<Vec<_>>(),
    );
    let dir = tempfile::tempdir().unwrap();
    let report = dir.path().join("report.txt");
    let mut report_args = vec!["--report", report.to_str().unwrap()];
    report_args.extend(names);
    let h = hook(None, command(&report_args), user());
    let rig = Rig::with({
        let mut options = Options::new(Manual);
        options.hooks = vec![Arc::new(command_hook("pre_tool_use", &h, process(scrub)))];
        options
    });
    rig.check("shell", shell("cargo test")).await;
    let text = std::fs::read_to_string(&report).unwrap();
    for gone in [
        "FONTAINE_TEST_PROVIDER_KEY",
        "FONTAINE_TEST_TELEMETRY_KEY",
        "FONTAINE_TEST_HF_TOKEN",
        "MICROMEGAS_TEST_INGESTION",
    ] {
        assert!(text.contains(&format!("{gone}=absent")), "{text}");
    }
    assert!(text.contains("FONTAINE_TEST_KEPT=present"), "{text}");
}

#[tokio::test]
async fn hooks_get_the_configured_env_and_the_real_project_dir() {
    let dir = tempfile::tempdir().unwrap();
    let values = dir.path().join("values.txt");
    let h = hook(
        None,
        command(&[
            "--values",
            values.to_str().unwrap(),
            "HOOK_TEST_SET",
            "FONTAINE_PROJECT_DIR",
        ]),
        user(),
    );
    let rig = Rig::with({
        let mut options = Options::new(Manual);
        options.hooks = vec![Arc::new(command_hook(
            "pre_tool_use",
            &h,
            process_with_env(
                Vec::new(),
                vec![("HOOK_TEST_SET".into(), "$HOME/literal".into())],
            ),
        ))];
        options
    });
    rig.check("shell", shell("cargo test")).await;
    let text = std::fs::read_to_string(&values).unwrap();
    assert!(text.contains("HOOK_TEST_SET=$HOME/literal"), "{text}");
    let project = std::env::temp_dir();
    assert!(
        text.contains(&format!("FONTAINE_PROJECT_DIR={}", project.display())),
        "{text}"
    );
}

#[tokio::test]
async fn guardian_context_reaches_the_guardian_as_an_untrusted_note() {
    let note = json!({"hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "guardian_context": "the lint hook saw a dirty tree",
    }})
    .to_string();
    let fake = fake_guardian(allow_verdict());
    let mut options = Options::new(Auto).guardian(fake.guardian.clone());
    options.hooks = vec![pre(&hook(None, command(&["--reply", &note]), user()))];
    let rig = Rig::with(options);
    assert!(allowed(
        &rig.check("shell", shell("python script.py")).await
    ));
    let sent = format!("{:?}", fake.mock.requests());
    assert!(sent.contains("the lint hook saw a dirty tree"));
    assert!(sent.contains("hook_context"));
}

fn post_stage(hooks: Vec<Hook>, rig: &Rig) -> ResultPipeline {
    let hooks: Vec<UserHook> = hooks
        .iter()
        .map(|h| command_hook("post_tool_use", h, process(Vec::new())))
        .collect();
    ResultPipeline::new().with_stage(
        StageKind::PostToolUse,
        Arc::new(PostHookStage::new(hooks, rig.state.clone())),
    )
}

async fn through(
    pipeline: &ResultPipeline,
    tool: &str,
    outcome: Result<ToolOutput, rig_core::tool::ToolExecutionError>,
) -> String {
    let args = json!({"path": "a.txt"});
    let view = CallView {
        tool,
        risk: RiskClass::ReadOnly,
        args: &args,
        agent: ROOT_AGENT,
        annotations: None,
        requires_user_interaction: false,
        synthetic: false,
    };
    match pipeline.run(view, outcome).await.0 {
        Ok(output) => output.render(),
        Err(e) => e.model_output().render(),
    }
}

#[tokio::test]
async fn a_post_hooks_additional_context_is_added_to_the_result() {
    let rig = Rig::new(Manual);
    let context = json!({"hookSpecificOutput": {
        "hookEventName": "PostToolUse",
        "additionalContext": "remember to run the formatter",
    }})
    .to_string();
    let dir = tempfile::tempdir().unwrap();
    let record = dir.path().join("event.json");
    let pipeline = post_stage(
        vec![hook(
            None,
            command(&["--record", &record.to_string_lossy(), "--reply", &context]),
            user(),
        )],
        &rig,
    );
    let text = through(&pipeline, "read", Ok(ToolOutput::text("file body"))).await;
    assert_eq!(text, "file body\n\nremember to run the formatter");
    let event: Value = serde_json::from_str(&std::fs::read_to_string(&record).unwrap()).unwrap();
    assert_eq!(event["hook_event_name"], "PostToolUse");
    assert_eq!(event["tool_response"]["output"], "file body");
    assert_eq!(event["tool_response"]["is_error"], false);
}

#[tokio::test]
async fn a_post_hook_event_carries_the_call_id_only_while_a_call_is_current() {
    let rig = Rig::new(Manual);
    let dir = tempfile::tempdir().unwrap();
    let record = dir.path().join("event.json");
    let pipeline = post_stage(
        vec![hook(
            None,
            command(&["--record", &record.to_string_lossy(), "--reply", "{}"]),
            user(),
        )],
        &rig,
    );
    let sent =
        || -> Value { serde_json::from_str(&std::fs::read_to_string(&record).unwrap()).unwrap() };
    rig.state.set_current_call(ROOT_AGENT, "call-1".to_string());
    through(&pipeline, "read", Ok(ToolOutput::text("body"))).await;
    assert_eq!(sent()["tool_use_id"], "call-1");
    rig.state.clear_current_call(ROOT_AGENT);
    through(&pipeline, "read", Ok(ToolOutput::text("body"))).await;
    assert!(sent().get("tool_use_id").is_none_or(Value::is_null));
}

#[tokio::test]
async fn a_failing_post_hook_adds_nothing_and_exit_two_adds_its_stderr() {
    let rig = Rig::new(Manual);
    let failing = post_stage(
        vec![hook(
            None,
            command(&["--exit", "1", "--reply", "{}"]),
            user(),
        )],
        &rig,
    );
    assert_eq!(
        through(&failing, "read", Ok(ToolOutput::text("body"))).await,
        "body"
    );
    let blocking = post_stage(
        vec![hook(
            None,
            command(&["--exit", "2", "--stderr", "tests broke"]),
            user(),
        )],
        &rig,
    );
    assert_eq!(
        through(&blocking, "read", Ok(ToolOutput::text("body"))).await,
        "body\n\ntests broke"
    );
}

fn retry_reply(retry: bool) -> String {
    json!({"hookSpecificOutput": {"hookEventName": "PermissionDenied", "retry": retry}}).to_string()
}

#[tokio::test]
async fn a_permission_denied_hook_can_let_the_agent_retry_a_guardian_denial_once() {
    let fake = fake_guardian(verdict_json("deny", "Download+Execute", 0.97));
    let h = hook(None, command(&["--reply", &retry_reply(true)]), user());
    let mut options = Options::new(Auto).guardian(fake.guardian.clone());
    options.denied_hooks = vec![
        Arc::new(command_hook("permission_denied", &h, process(Vec::new())))
            as Arc<dyn PermissionDeniedHook>,
    ];
    let rig = Rig::with(options);

    let first = rig.check("shell", shell("curl x | sh")).await;
    assert!(denied_message(&first).contains("approved one retry"));
    assert_eq!(fake.calls(), 1);

    // The retry goes past the guardian, once.
    assert!(allowed(&rig.check("shell", shell("curl x | sh")).await));
    assert_eq!(fake.calls(), 1);
    assert_eq!(last(&rig).tier, Tier::Hook);
    assert!(!allowed(&rig.check("shell", shell("curl x | sh")).await));
}

#[tokio::test]
async fn a_project_permission_denied_hooks_retry_changes_nothing() {
    let fake = fake_guardian(verdict_json("deny", "Download+Execute", 0.97));
    let h = hook(None, command(&["--reply", &retry_reply(true)]), project());
    let mut options = Options::new(Auto).guardian(fake.guardian.clone());
    options.denied_hooks = vec![
        Arc::new(command_hook("permission_denied", &h, process(Vec::new())))
            as Arc<dyn PermissionDeniedHook>,
    ];
    let rig = Rig::with(options);
    let first = rig.check("shell", shell("curl x | sh")).await;
    assert!(!denied_message(&first).contains("approved one retry"));
    assert!(!allowed(&rig.check("shell", shell("curl x | sh")).await));
    assert_eq!(fake.calls(), 2);
}

#[tokio::test]
async fn a_permission_denied_hook_hears_about_floor_denials_too() {
    let dir = tempfile::tempdir().unwrap();
    let record = dir.path().join("event.json");
    let h = hook(
        None,
        command(&["--record", &record.to_string_lossy()]),
        user(),
    );
    let mut options =
        Options::new(Manual).settings("[permissions]\ndeny = [\"shell(git push)\"]\n");
    options.denied_hooks = vec![
        Arc::new(command_hook("permission_denied", &h, process(Vec::new())))
            as Arc<dyn PermissionDeniedHook>,
    ];
    let rig = Rig::with(options);
    assert!(!allowed(&rig.check("shell", shell("git push")).await));
    let event: Value = serde_json::from_str(&std::fs::read_to_string(&record).unwrap()).unwrap();
    assert_eq!(event["hook_event_name"], "PermissionDenied");
    assert_eq!(event["tier"], "deny_rule");
    assert!(event["reason"].as_str().unwrap().contains("git push"));
}

async fn run_directly(
    hook: &UserHook,
) -> Result<fontaine_core::permissions::HookDecision, fontaine_core::permissions::HookError> {
    let args = json!({});
    let cwd = std::env::temp_dir();
    PreToolUseHook::run(
        hook,
        fontaine_core::permissions::PreToolUseEvent {
            tool: "shell",
            args: &args,
            agent: ROOT_AGENT,
            tool_use_id: None,
            session_id: "s",
            cwd: &cwd,
            permission_mode: "manual",
            workspace_roots: &[],
        },
    )
    .await
}

#[tokio::test]
async fn a_hook_that_cannot_start_keeps_the_io_error_as_its_source() {
    let missing = tempfile::tempdir().unwrap().path().join("gone");
    let process = Arc::new(HookProcess {
        scrub_env: Vec::new(),
        set_env: Vec::new(),
        shell: common::command_shell(),
        project_dir: missing,
    });
    let h = hook(None, command(&[]), user());
    let error = run_directly(&command_hook("pre_tool_use", &h, process))
        .await
        .expect_err("no working directory to start in");
    assert!(matches!(
        error,
        fontaine_core::permissions::HookError::Failed { .. }
    ));
    let source = std::error::Error::source(&error).expect("a cause");
    assert!(
        source.downcast_ref::<std::io::Error>().is_some(),
        "{source}"
    );
}

#[tokio::test]
async fn malformed_hook_output_keeps_the_json_error_as_its_source() {
    let h = hook(None, command(&["--reply", "this is not json"]), user());
    let error = run_directly(&command_hook("pre_tool_use", &h, process(Vec::new())))
        .await
        .expect_err("stdout is not JSON");
    assert!(matches!(
        error,
        fontaine_core::permissions::HookError::Malformed { .. }
    ));
    let source = std::error::Error::source(&error).expect("a cause");
    assert!(
        source.downcast_ref::<serde_json::Error>().is_some(),
        "{source}"
    );
}

// ---- the task boards' quality gates ----

fn task() -> Task {
    Task {
        id: "t1".into(),
        title: "ship it".into(),
        description: String::new(),
        status: TaskStatus::InProgress,
        owner: Some("agent-1".into()),
        blocked_by: Vec::new(),
        created_by: "root".into(),
    }
}

fn gate(rig: &Rig, completed: &[Hook], idle: &[Hook]) -> TeamGate {
    let build = |event: &str, hooks: &[Hook]| -> Vec<UserHook> {
        hooks
            .iter()
            .map(|h| command_hook(event, h, process(Vec::new())))
            .collect()
    };
    TeamGate::new(
        build("task_completed", completed),
        build("teammate_idle", idle),
        rig.state.clone(),
        rig.approver.clone(),
    )
}

#[tokio::test]
async fn a_task_completed_hook_that_exits_2_blocks_with_its_stderr_and_sees_the_task() {
    let rig = Rig::new(Manual);
    let dir = tempfile::tempdir().unwrap();
    let record = dir.path().join("event.json");
    let blocking = hook(
        None,
        command(&[
            "--record",
            &record.to_string_lossy(),
            "--exit",
            "2",
            "--stderr",
            "tests are red",
        ]),
        user(),
    );
    let gate = gate(&rig, &[blocking], &[]);
    let reason = gate.task_completed("root", &task()).await;
    assert_eq!(reason.as_deref(), Some("tests are red"));
    let event: Value = serde_json::from_str(&std::fs::read_to_string(&record).unwrap()).unwrap();
    assert_eq!(event["hook_event_name"], "TaskCompleted");
    assert_eq!(event["task"]["id"], "t1");
    assert_eq!(event["task"]["title"], "ship it");
    assert_eq!(
        event["agent_id"], "root",
        "the agent completing it, not the owner"
    );
    assert!(event["session_id"].is_string());
    assert!(event["cwd"].is_string());
    assert!(
        event.get("tool_name").is_none(),
        "no tool name, so no matcher"
    );
}

#[tokio::test]
async fn a_teammate_idle_hook_that_exits_2_blocks_and_is_told_who_went_idle() {
    let rig = Rig::new(Manual);
    let dir = tempfile::tempdir().unwrap();
    let record = dir.path().join("event.json");
    let blocking = hook(
        None,
        command(&[
            "--record",
            &record.to_string_lossy(),
            "--exit",
            "2",
            "--stderr",
            "run the linter",
        ]),
        user(),
    );
    let gate = gate(&rig, &[], &[blocking]);
    let reason = gate.teammate_idle("agent-3", "linter-1").await;
    assert_eq!(reason.as_deref(), Some("run the linter"));
    let event: Value = serde_json::from_str(&std::fs::read_to_string(&record).unwrap()).unwrap();
    assert_eq!(event["hook_event_name"], "TeammateIdle");
    assert_eq!(event["teammate_name"], "linter-1");
    assert_eq!(event["agent_id"], "agent-3");
}

#[tokio::test]
async fn a_gate_hook_that_fails_or_times_out_warns_and_blocks_nothing() {
    let rig = Rig::new(Manual);
    let crashes = hook(None, command(&["--exit", "1"]), user());
    let mut slow = hook(None, command(&["--sleep", "5"]), user());
    slow.timeout_secs = Some(1);
    let missing = hook(None, "definitely-not-a-command-xyz".to_string(), user());
    let gate = gate(&rig, &[crashes, slow, missing], &[]);
    assert_eq!(gate.task_completed("agent-1", &task()).await, None);
    assert_eq!(rig.approver.warnings.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn a_gate_hook_answer_of_allow_changes_nothing() {
    let rig = Rig::new(Manual);
    let allows = hook(None, command(&["--reply", &reply("allow")]), user());
    let gate = gate(
        &rig,
        std::slice::from_ref(&allows),
        std::slice::from_ref(&allows),
    );
    assert_eq!(gate.task_completed("agent-1", &task()).await, None);
    assert_eq!(gate.teammate_idle("agent-1", "a").await, None);
    assert!(rig.approver.warnings.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_first_blocking_gate_hook_wins_and_later_ones_do_not_run() {
    let rig = Rig::new(Manual);
    let dir = tempfile::tempdir().unwrap();
    let second = dir.path().join("second.json");
    let first = hook(None, command(&["--exit", "2", "--stderr", "first"]), user());
    let after = hook(
        None,
        command(&["--record", &second.to_string_lossy()]),
        user(),
    );
    let gate = gate(&rig, &[first, after], &[]);
    assert_eq!(
        gate.task_completed("agent-1", &task()).await.as_deref(),
        Some("first")
    );
    assert!(!second.exists());
}

// ---- the plugin layer: what a plugin reload swaps ----

fn plugin() -> Origin {
    Origin {
        slot: Slot::Plugin,
        user_trust: false,
    }
}

fn plugin_layer(
    hooks: &[Hook],
) -> Arc<fontaine_tools::live::Live<fontaine_core::user_hooks::PluginLayer>> {
    let layer = fontaine_core::user_hooks::PluginLayer {
        pre: hooks.iter().map(pre).collect(),
        ..Default::default()
    };
    Arc::new(fontaine_tools::live::Live::new(layer))
}

#[tokio::test]
async fn a_swapped_plugin_layer_changes_plugin_hooks_and_leaves_user_hooks_alone() {
    let live = plugin_layer(&[hook(
        Some("read"),
        command(&["--reply", &reply("deny")]),
        plugin(),
    )]);
    let mut options = Options::new(Manual);
    options.plugin_hooks = live.clone();
    options.hooks = vec![pre(&hook(
        Some("shell"),
        command(&["--reply", &reply("deny")]),
        user(),
    ))];
    let rig = Rig::with(options);
    std::fs::write(rig.work().join("a.txt"), "a").unwrap();

    assert!(!allowed(&rig.check("read", json!({"path": "a.txt"})).await));
    assert_eq!(last(&rig).tier, Tier::Hook);
    assert!(!allowed(&rig.check("shell", shell("cargo test")).await));

    // Unloading the plugin empties the layer.
    live.set(plugin_layer(&[]).get());
    assert!(allowed(&rig.check("read", json!({"path": "a.txt"})).await));
    assert!(
        !allowed(&rig.check("shell", shell("cargo test")).await),
        "the user's hook still denies"
    );
}

#[tokio::test]
async fn a_plugin_hooks_allow_never_counts() {
    let live = plugin_layer(&[hook(None, command(&["--reply", &reply("allow")]), plugin())]);
    let mut options = Options::new(Manual);
    options.plugin_hooks = live;
    let rig = Rig::with(options);
    rig.approver
        .answer(fontaine_core::permissions::Answer::AllowOnce);
    assert!(allowed(&rig.check("shell", shell("cargo test")).await));
    assert_eq!(
        rig.approver.asked(),
        1,
        "the plugin's allow decided nothing"
    );
}

#[tokio::test]
async fn swapped_plugin_rules_act_at_the_floor_and_leave_other_rules_alone() {
    use fontaine_core::permission_rules::{RuleSource, SourcedRule, parse_rule};
    let rig = Rig::with(Options::new(Manual).settings("[permissions]\ndeny = [\"shell(rm *)\"]\n"));
    let rule = |text: &str| SourcedRule::new(parse_rule(text).unwrap(), RuleSource::Plugin);

    rig.state
        .set_plugin_rules(vec![rule("shell(git push *)")], Vec::new());
    let denied = rig.check("shell", shell("git push origin")).await;
    assert!(!allowed(&denied));
    assert_eq!(last(&rig).tier, Tier::DenyRule);

    rig.state.set_plugin_rules(Vec::new(), Vec::new());
    rig.approver
        .answer(fontaine_core::permissions::Answer::AllowOnce);
    assert!(allowed(&rig.check("shell", shell("git push origin")).await));
    assert!(
        !allowed(&rig.check("shell", shell("rm -rf x")).await),
        "the user's own deny rule is not part of the layer"
    );
}

#[tokio::test]
async fn swapped_guardian_prose_reaches_the_next_adjudication() {
    let fake = fake_guardian(allow_verdict());
    let rig = Rig::with(Options::new(Auto).guardian(fake.guardian.clone()));
    assert!(allowed(&rig.check("shell", shell("python one.py")).await));
    let mut auto = rig.state.auto_context();
    auto.rules
        .hard_deny
        .push("never touch the moon base".into());
    rig.state.set_auto_context(auto);
    assert!(allowed(&rig.check("shell", shell("python two.py")).await));
    let requests = fake.mock.requests();
    assert_eq!(requests.len(), 2);
    assert!(!format!("{:?}", requests[0]).contains("moon base"));
    assert!(format!("{:?}", requests[1]).contains("never touch the moon base"));
}

#[tokio::test]
async fn plugin_post_and_team_hooks_follow_the_layer() {
    let rig = Rig::new(Manual);
    let context = |text: &str| {
        json!({"hookSpecificOutput": {"hookEventName": "PostToolUse", "additionalContext": text}})
            .to_string()
    };
    let h = hook(
        None,
        command(&["--reply", &context("from the plugin")]),
        plugin(),
    );
    let live = Arc::new(fontaine_tools::live::Live::new(
        fontaine_core::user_hooks::PluginLayer {
            post: vec![command_hook("post_tool_use", &h, process(Vec::new()))],
            ..Default::default()
        },
    ));
    let stage = PostHookStage::new(Vec::new(), rig.state.clone()).with_plugins(live.clone());
    let pipeline = ResultPipeline::new().with_stage(StageKind::PostToolUse, Arc::new(stage));
    assert_eq!(
        through(&pipeline, "read", Ok(ToolOutput::text("body"))).await,
        "body\n\nfrom the plugin"
    );
    live.set(Arc::default());
    assert_eq!(
        through(&pipeline, "read", Ok(ToolOutput::text("body"))).await,
        "body"
    );

    let blocking = hook(
        None,
        command(&["--stderr", "tests first", "--exit", "2"]),
        plugin(),
    );
    let live = Arc::new(fontaine_tools::live::Live::new(
        fontaine_core::user_hooks::PluginLayer {
            task_completed: vec![command_hook(
                "task_completed",
                &blocking,
                process(Vec::new()),
            )],
            ..Default::default()
        },
    ));
    let gate = TeamGate::new(
        Vec::new(),
        Vec::new(),
        rig.state.clone(),
        rig.approver.clone(),
    )
    .with_plugins(live.clone());
    let task = task();
    assert_eq!(
        gate.task_completed(ROOT_AGENT, &task).await.as_deref(),
        Some("tests first")
    );
    live.set(Arc::default());
    assert_eq!(gate.task_completed(ROOT_AGENT, &task).await, None);
}
