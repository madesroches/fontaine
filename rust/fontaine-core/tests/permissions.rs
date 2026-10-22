mod common;

use common::pipeline::{Options, Rig, allowed, denied_message};
use fontaine_core::audit::Decision;
use fontaine_core::config::PermissionMode::{self, AcceptEdits, DontAsk, Manual, Plan};
use fontaine_core::permissions::Answer;
use fontaine_core::tier::Tier;
use serde_json::{Value, json};

const MODES: [PermissionMode; 4] = [Manual, AcceptEdits, Plan, DontAsk];

fn write_args(path: &str) -> Value {
    json!({"path": path, "content": "x"})
}

fn edit_args(path: &str) -> Value {
    json!({"path": path, "edits": [{"old_text": "a", "new_text": "b", "replace_all": false}]})
}

fn shell(command: &str) -> Value {
    json!({"command": command})
}

/// The first decision record of the only call a rig has seen.
async fn first(rig: &Rig, tool: &str, args: Value) -> (Decision, Tier) {
    let before = rig.audit().len();
    rig.check(tool, args).await;
    let record = rig.last_first(before);
    (record.decision, record.tier)
}

#[tokio::test]
async fn the_mode_and_risk_class_matrix() {
    use Decision::{Allow, Deny, Prompt};
    use Tier::{DontAsk as Dont, Mode, PlanBlock, ReadOnlyFastPath, Tier05};
    type Row = (&'static str, Value, [(Decision, Tier); 4]);
    let rows: Vec<Row> = vec![
        (
            "read",
            json!({"path": "a.txt"}),
            [(Allow, ReadOnlyFastPath); 4],
        ),
        (
            "read",
            json!({"path": "../outside.txt"}),
            [(Prompt, Mode), (Prompt, Mode), (Prompt, Mode), (Deny, Dont)],
        ),
        (
            "write",
            write_args("new.txt"),
            [
                (Prompt, Mode),
                (Allow, Mode),
                (Deny, PlanBlock),
                (Deny, Dont),
            ],
        ),
        (
            "edit",
            edit_args("a.txt"),
            [
                (Prompt, Mode),
                (Allow, Mode),
                (Deny, PlanBlock),
                (Deny, Dont),
            ],
        ),
        (
            "write",
            write_args("../outside.txt"),
            [
                (Prompt, Mode),
                (Prompt, Mode),
                (Deny, PlanBlock),
                (Deny, Dont),
            ],
        ),
        (
            "shell",
            shell("touch x"),
            [(Prompt, Mode), (Prompt, Mode), (Prompt, Mode), (Deny, Dont)],
        ),
        ("shell", shell("ls"), [(Allow, Tier05); 4]),
        (
            "webfetch",
            json!({"url": "https://example.com/"}),
            [(Prompt, Mode), (Prompt, Mode), (Prompt, Mode), (Deny, Dont)],
        ),
    ];
    for (tool, args, expected) in rows {
        for (mode, want) in MODES.into_iter().zip(expected) {
            let rig = Rig::new(mode);
            std::fs::write(rig.work().join("a.txt"), "a\n").unwrap();
            let got = first(&rig, tool, args.clone()).await;
            assert_eq!(got, want, "{tool} {args} in {mode:?}");
        }
    }
}

#[tokio::test]
async fn headless_turns_every_prompt_into_a_denial() {
    for mode in [Manual, AcceptEdits, Plan] {
        let rig = Rig::with(Options::new(mode).headless());
        let d = rig.check("shell", shell("touch x")).await;
        assert!(denied_message(&d).contains("denied"));
        assert!(denied_message(&d).contains("headless"));
        assert_eq!(
            rig.approver.asked(),
            0,
            "nothing is asked in a headless run"
        );
        let records = rig.audit();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].decision, Decision::Deny);
        assert_eq!(records[0].tier, Tier::Mode);
        assert!(records[0].headless);
    }
}

#[tokio::test]
async fn plan_mode_blocks_edits_outright_and_sends_the_rest_to_the_interim_prompt() {
    let rig = Rig::new(Plan);
    let d = rig.check("write", write_args("n.txt")).await;
    assert!(denied_message(&d).contains("plan mode"));
    assert_eq!(rig.approver.asked(), 0, "an edit is never put to the human");
    assert_eq!(rig.audit()[0].tier, Tier::PlanBlock);

    // The same in a headless run, and an allow rule does not let an edit run.
    let headless = Rig::with(
        Options::new(Plan)
            .headless()
            .settings("[permissions]\nallow = [\"write\", \"edit\"]\n"),
    );
    for (tool, args) in [("write", write_args("n.txt")), ("edit", edit_args("n.txt"))] {
        let d = headless.check(tool, args).await;
        assert!(!allowed(&d));
    }
    assert!(headless.audit().iter().all(|r| r.tier == Tier::PlanBlock));

    // A consequential non-edit call reaches the prompt in the guardian's place.
    rig.approver.answer(Answer::AllowOnce);
    let d = rig.check("shell", shell("touch x")).await;
    assert!(allowed(&d));
    let records = rig.audit();
    assert_eq!(
        (records[1].decision, records[1].tier),
        (Decision::Prompt, Tier::Mode)
    );
    assert_eq!(
        (records[2].decision, records[2].tier),
        (Decision::Allow, Tier::Human)
    );

    // In-scope `ls` proceeds through the pre-classifier.
    assert!(allowed(&rig.check("shell", shell("ls")).await));
    assert_eq!(rig.audit().last().unwrap().tier, Tier::Tier05);
}

#[tokio::test]
async fn a_symlink_out_of_the_workspace_is_not_read_only_in_scope() {
    #[cfg(unix)]
    {
        let rig = Rig::new(Manual);
        let outside = rig.world.root.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "s").unwrap();
        std::os::unix::fs::symlink(&outside, rig.work().join("linked")).unwrap();
        for (tool, args) in [
            ("read", json!({"path": "linked/secret.txt"})),
            ("ls", json!({"path": "linked"})),
            ("shell", shell("ls linked/")),
            ("shell", shell("cat linked/secret.txt")),
        ] {
            let got = first(&rig, tool, args.clone()).await;
            assert_eq!(got, (Decision::Prompt, Tier::Mode), "{tool} {args}");
        }
    }
}

#[tokio::test]
async fn a_shell_cwd_outside_the_roots_disqualifies_the_pre_classifier() {
    let rig = Rig::new(Manual);
    let outside = rig.world.root.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("n.txt"), "n").unwrap();
    let args = json!({"command": "cat n.txt", "cwd": outside.to_str().unwrap()});
    assert_eq!(
        first(&rig, "shell", args).await,
        (Decision::Prompt, Tier::Mode)
    );
    std::fs::write(rig.work().join("n.txt"), "n").unwrap();
    assert_eq!(
        first(&rig, "shell", shell("cat n.txt")).await,
        (Decision::Allow, Tier::Tier05)
    );
}

#[tokio::test]
async fn a_pre_classified_call_is_audited_and_ls_outside_asks_the_human_once() {
    let rig = Rig::new(Manual);
    assert!(allowed(&rig.check("shell", shell("ls")).await));
    let records = rig.audit();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].tier, Tier::Tier05);

    rig.approver.answer(Answer::AllowOnce);
    let d = rig.check("shell", shell("ls ../")).await;
    assert!(allowed(&d));
    assert_eq!(rig.approver.asked(), 1);
}

#[tokio::test]
async fn sequential_calls_are_adjudicated_one_at_a_time_and_a_denial_does_not_stop_the_rest() {
    let rig = Rig::with(Options::new(Manual).headless());
    std::fs::write(rig.work().join("a.txt"), "a").unwrap();
    let d1 = rig.check("write", write_args("n.txt")).await;
    let d2 = rig.check("read", json!({"path": "a.txt"})).await;
    let d3 = rig.check("shell", shell("rm n.txt")).await;
    assert!(!allowed(&d1) && allowed(&d2) && !allowed(&d3));
    let ids: Vec<_> = rig
        .audit()
        .iter()
        .map(|r| r.tool_use_id.clone().unwrap())
        .collect();
    assert_eq!(ids, ["call-1", "call-2", "call-3"]);
}

#[tokio::test]
async fn a_deny_rule_holds_in_every_mode() {
    for mode in MODES {
        for headless in [false, true] {
            let mut options = Options::new(mode)
                .settings("[permissions]\ndeny = [\"read(.env)\", \"shell(rm)\", \"write\"]\nallow = [\"read\", \"shell(rm x)\"]\n");
            options.headless = headless;
            let rig = Rig::with(options);
            std::fs::write(rig.work().join(".env"), "S=1").unwrap();
            for (tool, args) in [
                ("read", json!({"path": ".env"})),
                ("shell", shell("rm x")),
                ("write", write_args("anything")),
            ] {
                let d = rig.check(tool, args.clone()).await;
                assert!(!allowed(&d), "{tool} in {mode:?} headless={headless}");
                let record = rig.audit().last().unwrap().clone();
                assert_eq!(record.tier, Tier::DenyRule, "{tool} in {mode:?}");
                assert!(record.rule.unwrap().ends_with("[user]"));
            }
            assert_eq!(rig.approver.asked(), 0);
        }
    }
}

#[tokio::test]
async fn an_ask_rule_forces_a_prompt_in_manual_and_denies_in_dont_ask() {
    let settings = "[permissions]\nask = [\"read(.env)\", \"shell(git push)\"]\nallow = [\"shell(git push *)\"]\n";
    let rig = Rig::with(Options::new(Manual).settings(settings));
    std::fs::write(rig.work().join(".env"), "S=1").unwrap();
    for (tool, args) in [
        ("read", json!({"path": ".env"})),
        ("shell", shell("git push origin main")),
    ] {
        assert_eq!(
            first(&rig, tool, args).await,
            (Decision::Prompt, Tier::AskRule)
        );
    }
    let strict = Rig::with(Options::new(DontAsk).settings(settings));
    std::fs::write(strict.work().join(".env"), "S=1").unwrap();
    let d = strict.check("read", json!({"path": ".env"})).await;
    assert!(!allowed(&d));
    assert_eq!(strict.audit()[0].tier, Tier::AskRule);
    assert_eq!(strict.audit()[0].decision, Decision::Deny);
}

#[tokio::test]
async fn critical_paths_prompt_in_the_interactive_modes_and_deny_in_dont_ask_and_headless() {
    let commands = [
        "rm -rf /",
        "rm -rf ~",
        "rmdir /usr",
        "rm -rf *",
        "rm -rf $TARGET",
        "rm -rf $(pwd)",
        "ls; rm -rf /",
        "echo ok && rm -rf /tmp",
        "rm -rf /home/x '",
        "sudo rm -rf /",
    ];
    let allow_all = "[permissions]\nallow = [\"shell\", \"shell(rm *)\"]\n";
    for command in commands {
        for mode in [Manual, AcceptEdits, Plan] {
            let rig = Rig::with(Options::new(mode).settings(allow_all));
            let got = first(&rig, "shell", shell(command)).await;
            assert_eq!(
                got,
                (Decision::Prompt, Tier::CriticalPath),
                "{command} {mode:?}"
            );
            assert!(!rig.approver.requests.lock().unwrap()[0].offers.session);
        }
        let rig = Rig::with(Options::new(DontAsk).settings(allow_all));
        assert_eq!(
            first(&rig, "shell", shell(command)).await,
            (Decision::Deny, Tier::CriticalPath),
            "{command}"
        );
        let rig = Rig::with(Options::new(Manual).headless().settings(allow_all));
        assert_eq!(
            first(&rig, "shell", shell(command)).await,
            (Decision::Deny, Tier::CriticalPath),
            "{command}"
        );
    }
    // An ordinary removal is not critical.
    let rig = Rig::new(Manual);
    assert_eq!(
        first(&rig, "shell", shell("rm build/out.o")).await,
        (Decision::Prompt, Tier::Mode)
    );
}

#[tokio::test]
async fn critical_paths_cover_every_workspace_root_and_its_parents() {
    let rig = Rig::new(Manual);
    let work = rig.work().to_path_buf();
    let parent = work.parent().unwrap().to_path_buf();
    for target in [work.to_str().unwrap(), parent.to_str().unwrap(), ".", ".."] {
        let got = first(&rig, "shell", shell(&format!("rm -rf '{target}'"))).await;
        assert_eq!(got, (Decision::Prompt, Tier::CriticalPath), "{target}");
    }
    let nested = rig.work().join("sub");
    std::fs::create_dir_all(&nested).unwrap();
    assert_eq!(
        first(&rig, "shell", shell("rm -rf sub")).await,
        (Decision::Prompt, Tier::Mode)
    );
}

#[tokio::test]
async fn critical_paths_resolve_against_the_call_cwd() {
    let rig = Rig::new(Manual);
    std::fs::create_dir_all(rig.work().join("sub")).unwrap();
    let args = json!({"command": "rm -rf ..", "cwd": "sub"});
    assert_eq!(
        first(&rig, "shell", args).await,
        (Decision::Prompt, Tier::CriticalPath)
    );
    let args = json!({"command": "rm -rf x", "cwd": "sub"});
    assert_eq!(
        first(&rig, "shell", args).await,
        (Decision::Prompt, Tier::Mode)
    );
}

#[tokio::test]
async fn the_home_directory_is_critical() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().canonicalize().unwrap().join("home");
    std::fs::create_dir_all(home.join("docs")).unwrap();
    let mut options = Options::new(Manual);
    options.home = Some(home.clone());
    let rig = Rig::with(options);
    assert_eq!(
        first(
            &rig,
            "shell",
            shell(&format!("rm -rf '{}'", home.display()))
        )
        .await,
        (Decision::Prompt, Tier::CriticalPath)
    );
    assert_eq!(
        first(
            &rig,
            "shell",
            shell(&format!("rm -rf '{}/docs'", home.display()))
        )
        .await,
        (Decision::Prompt, Tier::Mode)
    );
}

#[tokio::test]
async fn protected_paths_prompt_or_deny_by_mode() {
    let paths = [
        ".git/config",
        "sub/.git/hooks/pre-commit",
        ".fontaine/settings.toml",
        ".vscode/settings.json",
        ".idea/x.xml",
        ".husky/pre-commit",
        ".gitconfig",
        ".gitmodules",
        ".bashrc",
        ".bash_profile",
        ".zshrc",
        ".zprofile",
        ".profile",
        ".ripgreprc",
        ".mcp.json",
    ];
    for path in paths {
        for (tool, args) in [("write", write_args(path)), ("edit", edit_args(path))] {
            for mode in [Manual, AcceptEdits] {
                let rig = Rig::with(
                    Options::new(mode).settings("[permissions]\nallow = [\"write\", \"edit\"]\n"),
                );
                assert_eq!(
                    first(&rig, tool, args.clone()).await,
                    (Decision::Prompt, Tier::ProtectedPath),
                    "{tool} {path} {mode:?}"
                );
            }
            let rig = Rig::new(DontAsk);
            assert_eq!(
                first(&rig, tool, args.clone()).await,
                (Decision::Deny, Tier::ProtectedPath),
                "{tool} {path}"
            );
            let rig = Rig::with(Options::new(Manual).headless());
            assert_eq!(
                first(&rig, tool, args.clone()).await,
                (Decision::Deny, Tier::ProtectedPath),
                "{tool} {path}"
            );
        }
    }
    let rig = Rig::new(AcceptEdits);
    assert_eq!(
        first(&rig, "write", write_args(".github/ci.yml")).await,
        (Decision::Allow, Tier::Mode)
    );
}

#[tokio::test]
async fn every_decision_appends_an_audit_record_with_the_schema_fields() {
    let rig = Rig::with(Options::new(Manual).settings("[permissions]\ndeny = [\"read(.env)\"]\n"));
    std::fs::write(rig.work().join("a.txt"), "a").unwrap();
    rig.check("read", json!({"path": "a.txt"})).await;
    rig.check("read", json!({"path": ".env"})).await;
    let records = rig.audit();
    assert_eq!(records.len(), 2);
    let allow = &records[0];
    assert_eq!(allow.session_id, rig.session.id);
    assert_eq!(allow.agent_id, "root");
    assert_eq!(allow.tool_use_id.as_deref(), Some("call-1"));
    assert_eq!(allow.tool, "read");
    assert_eq!(allow.args_digest.len(), 64);
    assert!(allow.ts.ends_with('Z') && allow.ts.contains('T'));
    assert_eq!(allow.permission_mode, "manual");
    assert_eq!(allow.rule, None);
    assert!(allow.original_args_digest.is_none());
    let deny = &records[1];
    assert_eq!(deny.tier, Tier::DenyRule);
    assert_eq!(deny.rule.as_deref(), Some("read(.env) [user]"));
    assert!(deny.explanation.as_deref().unwrap().contains("denied"));

    // The raw arguments are never logged.
    let raw = std::fs::read_to_string(rig.session.audit.path()).unwrap();
    assert!(!raw.contains("a.txt") && !raw.contains(".env\""), "{raw}");
    for line in raw.lines() {
        let value: Value = serde_json::from_str(line).unwrap();
        for key in [
            "ts",
            "session_id",
            "agent_id",
            "tool_use_id",
            "tool",
            "args_digest",
            "tier",
            "decision",
            "rule",
            "confidence",
            "guardian_model",
            "latency_ms",
        ] {
            assert!(value.get(key).is_some(), "{key} in {line}");
        }
    }
}

#[tokio::test]
async fn a_human_answer_adds_a_second_record_with_tier_human() {
    let rig = Rig::new(Manual);
    rig.approver.answer(Answer::AllowOnce);
    rig.approver.answer(Answer::Deny {
        feedback: Some("not that file".to_string()),
    });
    let d = rig.check("write", write_args("a.txt")).await;
    assert!(allowed(&d));
    let d = rig.check("write", write_args("b.txt")).await;
    assert!(denied_message(&d).contains("not that file"));
    let rows: Vec<_> = rig.audit().iter().map(|r| (r.decision, r.tier)).collect();
    assert_eq!(
        rows,
        [
            (Decision::Prompt, Tier::Mode),
            (Decision::Allow, Tier::Human),
            (Decision::Prompt, Tier::Mode),
            (Decision::Deny, Tier::Human),
        ]
    );
}

#[tokio::test]
async fn the_ledger_gets_an_action_or_denial_for_every_decision() {
    use fontaine_core::ledger::Record;
    let rig = Rig::with(Options::new(Manual).headless());
    std::fs::write(rig.work().join("a.txt"), "a").unwrap();
    rig.check("read", json!({"path": "a.txt"})).await;
    rig.check("shell", shell("touch x")).await;
    let records: Vec<Record> = rig
        .session
        .ledger
        .read()
        .unwrap()
        .into_iter()
        .map(|e| e.record)
        .collect();
    assert!(
        matches!(&records[0], Record::Action { tool, agent } if tool == "read" && agent == "root")
    );
    assert!(matches!(&records[1], Record::Denial { tool, .. } if tool == "shell"));
}

#[tokio::test]
async fn an_out_of_scope_file_call_says_why_and_how_to_fix_it() {
    let rig = Rig::with(Options::new(Manual).headless());
    let d = rig.check("read", json!({"path": "../outside.txt"})).await;
    let message = denied_message(&d);
    assert!(message.contains("outside the workspace roots"), "{message}");
    assert!(message.contains("/add-dir"), "{message}");
}

#[tokio::test]
async fn a_mode_change_takes_effect_on_the_next_call() {
    let rig = Rig::new(Manual);
    assert_eq!(
        first(&rig, "write", write_args("a.txt")).await,
        (Decision::Prompt, Tier::Mode)
    );
    rig.state.set_mode(AcceptEdits);
    assert_eq!(
        first(&rig, "write", write_args("a.txt")).await,
        (Decision::Allow, Tier::Mode)
    );
    rig.state.set_mode(Plan);
    assert_eq!(
        first(&rig, "write", write_args("a.txt")).await,
        (Decision::Deny, Tier::PlanBlock)
    );
    let ledger: Vec<_> = rig
        .session
        .ledger
        .read()
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.record {
            fontaine_core::ledger::Record::ModeChange { from, to } => Some((from, to)),
            _ => None,
        })
        .collect();
    assert_eq!(
        ledger,
        [
            ("manual".to_string(), "acceptEdits".to_string()),
            ("acceptEdits".to_string(), "plan".to_string())
        ]
    );
}
