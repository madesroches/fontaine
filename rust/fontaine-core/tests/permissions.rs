mod common;

#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::sync::Arc;

use common::pipeline::{Options, Rig, allow_verdict, allowed, denied_message, fake_guardian};
use fontaine_core::audit::Decision;
use fontaine_core::config::PermissionMode::{self, AcceptEdits, DontAsk, Manual, Plan};
use fontaine_core::permissions::Answer;
#[cfg(unix)]
use fontaine_core::permissions::ApprovalRequest;
use fontaine_core::tier::Tier;
use serde_json::{Value, json};

const MODES: [PermissionMode; 4] = [Manual, AcceptEdits, Plan, DontAsk];

fn write_args(path: &str) -> Value {
    json!({"path": path, "content": "x"})
}

fn edit_args(path: &str) -> Value {
    json!({"path": path, "edits": [{"old_text": "a", "new_text": "b", "replace_all": false}]})
}

fn notebook_args(path: &str) -> Value {
    json!({"path": path, "cell_id": "cell-0", "new_source": "x"})
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
    use Tier::{DontAsk as Dont, Guardian, Mode, PlanBlock, ReadOnlyFastPath, Tier05};
    type Row = (&'static str, Value, [(Decision, Tier); 4]);
    let rows: Vec<Row> = vec![
        (
            "read",
            json!({"path": "a.txt"}),
            [(Allow, ReadOnlyFastPath); 4],
        ),
        (
            "todo_write",
            json!({"todos": [{"content": "a", "status": "pending"}]}),
            [(Allow, ReadOnlyFastPath); 4],
        ),
        (
            "read",
            json!({"path": "../outside.txt"}),
            [
                (Prompt, Mode),
                (Prompt, Mode),
                (Deny, Guardian),
                (Deny, Dont),
            ],
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
            "notebook_edit",
            notebook_args("a.ipynb"),
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
            [(Deny, Mode), (Deny, Mode), (Deny, PlanBlock), (Deny, Dont)],
        ),
        (
            "shell",
            shell("touch x"),
            [
                (Prompt, Mode),
                (Prompt, Mode),
                (Deny, Guardian),
                (Deny, Dont),
            ],
        ),
        ("shell", shell("ls"), [(Allow, Tier05); 4]),
        ("shell", shell("cat a.txt"), [(Allow, Tier05); 4]),
        (
            "read",
            json!({"path": ".env"}),
            [
                (Prompt, Mode),
                (Prompt, Mode),
                (Deny, Guardian),
                (Deny, Dont),
            ],
        ),
        (
            "grep",
            json!({"pattern": "K", "path": ".env"}),
            [
                (Prompt, Mode),
                (Prompt, Mode),
                (Deny, Guardian),
                (Deny, Dont),
            ],
        ),
        (
            "lsp",
            json!({"operation": "hover", "path": ".env", "line": 1, "character": 1}),
            [
                (Prompt, Mode),
                (Prompt, Mode),
                (Deny, Guardian),
                (Deny, Dont),
            ],
        ),
        (
            "shell",
            shell("cat .env"),
            [
                (Prompt, Mode),
                (Prompt, Mode),
                (Deny, Guardian),
                (Deny, Dont),
            ],
        ),
        (
            "shell",
            shell("head .env"),
            [
                (Prompt, Mode),
                (Prompt, Mode),
                (Deny, Guardian),
                (Deny, Dont),
            ],
        ),
        (
            "shell",
            shell("grep -f.env x"),
            [
                (Prompt, Mode),
                (Prompt, Mode),
                (Deny, Guardian),
                (Deny, Dont),
            ],
        ),
        (
            "read",
            json!({"path": ".env.example"}),
            [(Allow, ReadOnlyFastPath); 4],
        ),
        ("shell", shell("cat .env.example"), [(Allow, Tier05); 4]),
        (
            "ls",
            json!({"path": ".env"}),
            [(Allow, ReadOnlyFastPath); 4],
        ),
        (
            "glob",
            json!({"pattern": "*", "path": ".env"}),
            [(Allow, ReadOnlyFastPath); 4],
        ),
        (
            "webfetch",
            json!({"url": "https://example.com/"}),
            [
                (Prompt, Mode),
                (Prompt, Mode),
                (Deny, Guardian),
                (Deny, Dont),
            ],
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
    for mode in [Manual, AcceptEdits] {
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
async fn plan_mode_blocks_edits_outright_and_sends_the_rest_to_the_guardian() {
    let fake = fake_guardian(allow_verdict());
    let rig = Rig::with(Options::new(Plan).guardian(fake.guardian.clone()));
    let d = rig.check("write", write_args("n.txt")).await;
    assert!(denied_message(&d).contains("plan mode"));
    assert_eq!(rig.approver.asked(), 0, "an edit is never put to the human");
    assert_eq!(rig.audit()[0].tier, Tier::PlanBlock);
    assert_eq!(fake.calls(), 0, "no verdict can reach past the edit block");

    // The same in a headless run, and an allow rule does not let an edit run, whatever the
    // guardian would say.
    let headless = Rig::with(
        Options::new(Plan)
            .headless()
            .guardian(fake.guardian.clone())
            .settings("[permissions]\nallow = [\"write\", \"edit\"]\n"),
    );
    for (tool, args) in [
        ("write", write_args("n.txt")),
        ("edit", edit_args("n.txt")),
        ("notebook_edit", notebook_args("n.ipynb")),
    ] {
        let d = headless.check(tool, args).await;
        assert!(!allowed(&d));
    }
    assert!(headless.audit().iter().all(|r| r.tier == Tier::PlanBlock));
    assert_eq!(fake.calls(), 0);

    // Every other consequential call goes to the guardian, as in auto: write-capable shell,
    // webfetch and MCP, and out-of-scope shell reads.
    for (tool, args) in [
        ("shell", shell("touch x")),
        ("webfetch", json!({"url": "https://example.com/"})),
        ("mcp__srv__tool", json!({})),
        ("shell", shell("ls ../")),
    ] {
        let before = fake.calls();
        let d = rig.check(tool, args.clone()).await;
        assert!(allowed(&d), "{tool} {args}");
        assert_eq!(fake.calls(), before + 1, "{tool} {args}");
        let record = rig.audit().last().cloned().unwrap();
        assert_eq!(
            (record.decision, record.tier),
            (Decision::Allow, Tier::Guardian)
        );
    }
    assert_eq!(rig.approver.asked(), 0);

    // In-scope `ls` proceeds through the pre-classifier without the guardian.
    let before = fake.calls();
    assert!(allowed(&rig.check("shell", shell("ls")).await));
    assert_eq!(rig.audit().last().unwrap().tier, Tier::Tier05);
    assert_eq!(fake.calls(), before);
}

#[tokio::test]
async fn a_symlink_out_of_the_workspace_is_adjudicated_not_fast_pathed() {
    #[cfg(unix)]
    {
        let rig = Rig::new(Manual);
        let outside = rig.world.root.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "s").unwrap();
        std::os::unix::fs::symlink(&outside, rig.work().join("linked")).unwrap();
        // An out-of-root read no longer takes the containment denial: it follows the same
        // pipeline as any other unapproved call, so in manual mode it asks. The shell has
        // no such confinement and still asks.
        for (tool, args, decision) in [
            (
                "read",
                json!({"path": "linked/secret.txt"}),
                Decision::Prompt,
            ),
            ("ls", json!({"path": "linked"}), Decision::Prompt),
            ("shell", shell("ls linked/"), Decision::Prompt),
            ("shell", shell("cat linked/secret.txt"), Decision::Prompt),
        ] {
            let got = first(&rig, tool, args.clone()).await;
            assert_eq!(got, (decision, Tier::Mode), "{tool} {args}");
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
async fn external_reads_follow_the_mode_matrix_and_never_take_a_fast_path() {
    use Decision::{Allow, Deny, Prompt};
    use Tier::{DontAsk as Dont, Guardian, Mode, ReadOnlyFastPath};
    // An in-root read keeps its fast path in every mode.
    for mode in MODES {
        let rig = Rig::new(mode);
        std::fs::write(rig.work().join("a.txt"), "a").unwrap();
        assert_eq!(
            first(&rig, "read", json!({"path": "a.txt"})).await,
            (Allow, ReadOnlyFastPath),
            "in {mode:?}"
        );
    }

    // An external read follows the ordinary pipeline: it asks in the interactive modes,
    // reaches the guardian in auto and plan, and denies in dontAsk.
    let cases: Vec<(PermissionMode, (Decision, Tier))> = vec![
        (Manual, (Prompt, Mode)),
        (AcceptEdits, (Prompt, Mode)),
        (Plan, (Deny, Guardian)),
        (DontAsk, (Deny, Dont)),
    ];
    for (mode, want) in cases {
        let rig = Rig::new(mode);
        let got = first(&rig, "read", json!({"path": "../outside.txt"})).await;
        assert_eq!(got, want, "in {mode:?}");
    }

    // With a working guardian, auto and plan allow it at the guardian tier.
    for mode in [fontaine_core::config::PermissionMode::Auto, Plan] {
        let fake = common::pipeline::fake_guardian(common::pipeline::allow_verdict());
        let rig = Rig::with(Options::new(mode).guardian(fake.guardian.clone()));
        let got = first(&rig, "read", json!({"path": "../outside.txt"})).await;
        assert_eq!(got, (Allow, Guardian), "in {mode:?}");
        assert!(
            !rig.audit()
                .iter()
                .any(|r| r.tier == Tier::ReadOnlyFastPath || r.tier == Tier::Tier05)
        );
    }

    // Headless turns the prompt into a denial, as for any other call.
    let rig = Rig::with(Options::new(Manual).headless());
    let d = rig.check("read", json!({"path": "../outside.txt"})).await;
    assert!(!allowed(&d));
    assert!(denied_message(&d).contains("headless"));
    assert_eq!(rig.audit()[0].tier, Mode);
}

#[cfg(unix)]
async fn symlinked_external_reads_keep_the_floor() {
    // A workspace symlink pointing outside every root, and a home directory the rules
    // anchor at without knowing either temp world's layout. The TempDir guard outlives
    // the rig: dropping it deletes the home the symlink points at.
    fn rig_with(rule_list: &str, rule: &str) -> (Rig, PathBuf, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let home = std::fs::canonicalize(tmp.path()).unwrap();
        std::fs::create_dir_all(home.join("outside")).unwrap();
        std::fs::write(home.join("outside").join("secret.txt"), "s").unwrap();
        let mut options = Options::new(Manual);
        options.home = Some(home.clone());
        options.settings = format!("[permissions]\n{rule_list} = [\"{rule}\"]\n");
        let rig = Rig::with(options);
        std::os::unix::fs::symlink(&home, rig.work().join("linked")).unwrap();
        (rig, home, tmp)
    }

    // A deny rule anchored at the resolved target wins, whatever the mode.
    for mode in MODES {
        let tmp = tempfile::tempdir().unwrap();
        let home = std::fs::canonicalize(tmp.path()).unwrap();
        std::fs::create_dir_all(home.join("outside")).unwrap();
        std::fs::write(home.join("outside").join("secret.txt"), "s").unwrap();
        let mut options = Options::new(mode);
        options.home = Some(home.clone());
        options.settings = "[permissions]\ndeny = [\"read(~/outside/secret.txt)\"]\n".into();
        let rig = Rig::with(options);
        {
            std::os::unix::fs::symlink(&home, rig.work().join("linked")).unwrap();
            let d = rig
                .check("read", json!({"path": "linked/outside/secret.txt"}))
                .await;
            assert!(!allowed(&d), "{mode:?}");
            assert_eq!(rig.audit()[0].tier, Tier::DenyRule, "{mode:?}");
        }
    }

    // An ask rule forces the human even in auto.
    let fake = common::pipeline::fake_guardian(common::pipeline::allow_verdict());
    let tmp = tempfile::tempdir().unwrap();
    let home = std::fs::canonicalize(tmp.path()).unwrap();
    std::fs::create_dir_all(home.join("outside")).unwrap();
    std::fs::write(home.join("outside").join("secret.txt"), "s").unwrap();
    let mut options = Options::new(fontaine_core::config::PermissionMode::Auto);
    options.home = Some(home.clone());
    options.guardian = Some(fake.guardian.clone());
    options.settings = "[permissions]\nask = [\"read(~/outside/secret.txt)\"]\n".into();
    let rig = Rig::with(options);
    std::os::unix::fs::symlink(&home, rig.work().join("linked")).unwrap();
    rig.approver.answer(Answer::AllowOnce);
    assert!(allowed(
        &rig.check("read", json!({"path": "linked/outside/secret.txt"}))
            .await
    ));
    assert_eq!(rig.approver.asked(), 1);
    assert_eq!(fake.calls(), 0);

    // A home-anchored allow rule runs the call at the allow_rule tier, and the allowed
    // arguments are the resolved target, never the alias the call was written with.
    let (rig, home, _tmp) = rig_with("allow", "read(~/outside/secret.txt)");
    let decision = rig
        .check("read", json!({"path": "linked/outside/secret.txt"}))
        .await;
    match decision {
        fontaine_tools::gate::GateDecision::Allow { args } => {
            assert_eq!(
                args,
                json!({"path": home.join("outside/secret.txt").to_str().unwrap()})
            );
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(rig.audit()[0].tier, Tier::AllowRule);
}

#[tokio::test]
async fn external_reads_keep_the_floor_and_their_preapproved_tiers() {
    #[cfg(unix)]
    symlinked_external_reads_keep_the_floor().await;

    // dontAsk denies an external read no rule covers.
    let rig = Rig::new(DontAsk);
    let d = rig.check("read", json!({"path": "../outside.txt"})).await;
    assert!(!allowed(&d));
    assert_eq!(rig.audit()[0].tier, Tier::DontAsk);
}

#[tokio::test]
async fn malformed_external_read_arguments_are_an_argument_error_after_the_floor() {
    for mode in MODES {
        let fake = common::pipeline::fake_guardian(common::pipeline::allow_verdict());
        let rig = Rig::with(Options::new(mode).guardian(fake.guardian.clone()));
        for args in [json!({"path": 3}), json!({}), json!("a.txt")] {
            let d = rig.check("read", args.clone()).await;
            assert!(!allowed(&d), "{args} {mode:?}");
            let message = denied_message(&d);
            assert!(message.contains("arguments could not be read"), "{message}");
            assert_eq!(
                rig.audit().last().unwrap().tier,
                Tier::Mode,
                "{args} {mode:?}"
            );
        }
        assert_eq!(fake.calls(), 0, "{mode:?}: the guardian never sees them");
        assert_eq!(rig.approver.asked(), 0, "{mode:?}: neither does a human");
    }
    // A deny rule still precedes the argument error.
    let rig = Rig::with(Options::new(Manual).settings("[permissions]\ndeny = [\"read\"]\n"));
    let d = rig.check("read", json!({"path": 3})).await;
    assert!(!allowed(&d));
    assert_eq!(rig.audit()[0].tier, Tier::DenyRule);
}

#[tokio::test]
async fn an_external_read_prompt_shows_the_resolved_grep_and_glob_target() {
    let rig = Rig::new(Manual);
    let outside = rig.world.root.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("lib.rs"), "fn x() {}\n").unwrap();
    for (tool, args, summary_path) in [
        (
            "grep",
            json!({"pattern": "fn x", "path": "../outside"}),
            outside.to_str().unwrap(),
        ),
        (
            "glob",
            json!({"pattern": "../outside/*.rs"}),
            outside.to_str().unwrap(),
        ),
        (
            "ls",
            json!({"path": "../outside"}),
            outside.to_str().unwrap(),
        ),
        (
            "read",
            json!({"path": "../outside/lib.rs"}),
            outside.join("lib.rs").to_str().unwrap(),
        ),
    ] {
        rig.approver.answer(Answer::AllowOnce);
        assert!(allowed(&rig.check(tool, args).await), "{tool}");
        let request = rig.approver.last_request();
        assert!(
            request.summary.contains(summary_path),
            "{}",
            request.summary
        );
    }
}

/// An approver that retargets the symlink after the pipeline has resolved the call, so an
/// approval granted mid-decision cannot redirect the call to the new target.
#[cfg(unix)]
struct Retargeting {
    link: std::sync::Mutex<Option<std::path::PathBuf>>,
    new_target: std::sync::Mutex<Option<std::path::PathBuf>>,
    requests: std::sync::Mutex<Vec<ApprovalRequest>>,
}

#[cfg(unix)]
impl Retargeting {
    fn aim(&self, link: std::path::PathBuf, new_target: std::path::PathBuf) {
        *self.link.lock().unwrap() = Some(link);
        *self.new_target.lock().unwrap() = Some(new_target);
    }
}

#[cfg(unix)]
impl fontaine_core::permissions::Approver for Retargeting {
    fn approve<'a>(&'a self, request: ApprovalRequest) -> futures::future::BoxFuture<'a, Answer> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(request);
            let (link, new_target) = (
                self.link.lock().unwrap().clone(),
                self.new_target.lock().unwrap().clone(),
            );
            if let (Some(link), Some(new_target)) = (link, new_target) {
                std::fs::remove_file(&link).unwrap();
                std::os::unix::fs::symlink(&new_target, &link).unwrap();
            }
            Answer::AllowOnce
        })
    }
}

#[cfg(unix)]
#[tokio::test]
async fn an_approval_cannot_be_redirected_by_retargeting_the_symlink() {
    let retargeting = Arc::new(Retargeting {
        link: std::sync::Mutex::new(None),
        new_target: std::sync::Mutex::new(None),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let mut options = Options::new(Manual);
    options.approver = Some(retargeting.clone());
    let rig = Rig::with(options);
    let outside = rig.world.root.join("outside");
    let swapped = rig.world.root.join("swapped");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::create_dir_all(&swapped).unwrap();
    std::fs::write(outside.join("secret.txt"), "ORIGINAL").unwrap();
    std::fs::write(swapped.join("secret.txt"), "SWAPPED").unwrap();
    std::os::unix::fs::symlink(&outside, rig.work().join("linked")).unwrap();
    retargeting.aim(rig.work().join("linked"), swapped);

    match rig
        .check("read", json!({"path": "linked/secret.txt"}))
        .await
    {
        fontaine_tools::gate::GateDecision::Allow { args } => {
            assert_eq!(
                args,
                json!({"path": outside.join("secret.txt").to_str().unwrap()}),
                "the allowed arguments stay with the resolved target"
            );
        }
        other => panic!("{other:?}"),
    }
    // The approval prompt itself showed the resolved target.
    let request = retargeting.requests.lock().unwrap()[0].clone();
    assert!(
        request
            .summary
            .contains(outside.join("secret.txt").to_str().unwrap()),
        "{}",
        request.summary
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
async fn lsp_takes_the_read_only_fast_path_inside_the_roots_and_path_rules_reach_it() {
    let hover = |path: &str| json!({"operation": "hover", "path": path, "line": 1, "character": 1});
    for mode in MODES {
        let rig = Rig::new(mode);
        assert_eq!(
            first(&rig, "lsp", hover("src/a.rs")).await,
            (Decision::Allow, Tier::ReadOnlyFastPath),
            "in {mode:?}"
        );
    }
    // A path rule matches it like it matches `read`, in any mode, and an allow rule for the
    // tool does not outrank the deny.
    let settings = "[permissions]\ndeny = [\"lsp(secrets/**)\"]\nallow = [\"lsp\"]\n";
    for mode in MODES {
        let rig = Rig::with(Options::new(mode).settings(settings));
        let d = rig.check("lsp", hover("secrets/key.rs")).await;
        assert!(!allowed(&d), "{mode:?}");
        assert_eq!(rig.audit()[0].tier, Tier::DenyRule, "{mode:?}");
        assert!(
            allowed(&rig.check("lsp", hover("src/a.rs")).await),
            "{mode:?}"
        );
    }
}

#[tokio::test]
async fn a_deny_rule_on_the_task_list_beats_its_fast_path() {
    let rig = Rig::with(Options::new(Manual).settings("[permissions]\ndeny = [\"todo_write\"]\n"));
    let args = json!({"todos": []});
    let d = rig.check("todo_write", args).await;
    assert!(!allowed(&d));
    assert_eq!(rig.audit().last().unwrap().tier, Tier::DenyRule);
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
        for (tool, args) in [
            ("write", write_args(path)),
            ("edit", edit_args(path)),
            ("notebook_edit", notebook_args(path)),
        ] {
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
        matches!(&records[0], Record::Action { tool, agent, .. } if tool == "read" && agent == "root")
    );
    assert!(matches!(&records[1], Record::Denial { tool, .. } if tool == "shell"));
}

#[tokio::test]
async fn an_out_of_scope_file_call_says_why_and_how_to_fix_it() {
    let rig = Rig::with(Options::new(Manual).headless());
    // A read outside the roots now follows the ordinary pipeline: headless, its prompt
    // becomes a denial.
    let d = rig.check("read", json!({"path": "../outside.txt"})).await;
    let message = denied_message(&d);
    assert!(message.contains("needs the user's approval"), "{message}");
    assert!(message.contains("headless"), "{message}");

    // A write outside the roots keeps the containment denial with its guidance.
    let d = rig.check("write", write_args("../outside.txt")).await;
    let message = denied_message(&d);
    assert!(message.contains("outside the workspace roots"), "{message}");
    assert!(message.contains("/add-dir"), "{message}");
}

#[tokio::test]
async fn the_scratch_root_takes_file_writes_without_a_prompt() {
    let rig = Rig::with(Options::new(AcceptEdits).scratch());
    let scratch = rig.scratch.clone().unwrap();
    for (tool, args) in [
        (
            "write",
            write_args(&scratch.join("helper.sh").display().to_string()),
        ),
        (
            "edit",
            edit_args(&scratch.join("helper.sh").display().to_string()),
        ),
    ] {
        assert_eq!(
            first(&rig, tool, args).await,
            (Decision::Allow, Tier::Mode),
            "{tool}"
        );
    }
    let manual = Rig::with(Options::new(Manual).scratch());
    let path = manual.scratch.clone().unwrap().join("helper.sh");
    let (decision, tier) = first(&manual, "write", write_args(&path.display().to_string())).await;
    assert_eq!((decision, tier), (Decision::Prompt, Tier::Mode));
}

#[tokio::test]
async fn a_protected_file_name_in_the_scratch_root_still_hits_the_floor() {
    let rig = Rig::with(Options::new(AcceptEdits).scratch());
    let path = rig.scratch.clone().unwrap().join(".bashrc");
    assert_eq!(
        first(&rig, "write", write_args(&path.display().to_string())).await,
        (Decision::Prompt, Tier::ProtectedPath)
    );
}

#[tokio::test]
async fn an_out_of_roots_denial_names_the_scratch_directory() {
    let rig = Rig::with(Options::new(Manual).headless().scratch());
    let outside = rig.world.root.join("outside.txt");
    let d = rig
        .check("write", write_args(&outside.display().to_string()))
        .await;
    let message = denied_message(&d);
    let scratch = rig.scratch.clone().unwrap();
    assert!(
        message.contains(&scratch.display().to_string()),
        "{message}"
    );
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

#[tokio::test]
async fn read_rules_cover_the_paths_of_read_only_shell_commands() {
    for mode in [Manual, AcceptEdits, Plan, DontAsk] {
        let rig =
            Rig::with(Options::new(mode).settings("[permissions]\ndeny = [\"read(.env)\"]\n"));
        for command in [
            "cat .env",
            "head -n 3 ./.env",
            "grep --file=.env x",
            "grep -f.env -r .",
            "grep -o -f.env -r src",
            "grep -of.env -r src",
            "grep -f .env x",
            "rg -f.env x",
            "file -m.env x",
            "cat sub/../.env",
        ] {
            assert_eq!(
                first(&rig, "shell", shell(command)).await,
                (Decision::Deny, Tier::DenyRule),
                "{command} {mode:?}"
            );
        }
    }
    let rig =
        Rig::with(Options::new(Manual).settings("[permissions]\nask = [\"read(secrets/**)\"]\n"));
    assert_eq!(
        first(&rig, "shell", shell("grep x secrets/a")).await,
        (Decision::Prompt, Tier::AskRule)
    );
    assert_eq!(
        first(&rig, "shell", shell("cat README.md")).await,
        (Decision::Allow, Tier::Tier05)
    );
    let rig = Rig::with(Options::new(Manual).settings("[permissions]\ndeny = [\"read(.env)\"]\n"));
    assert_eq!(
        first(&rig, "shell", shell("cat README.md")).await,
        (Decision::Allow, Tier::Tier05)
    );
}

#[tokio::test]
async fn an_allow_rule_restores_the_skip_for_secret_reads() {
    let allow = "[permissions]\nallow = [\"read(.env)\"]\n";
    for mode in [Manual, AcceptEdits, Plan, DontAsk] {
        let rig = Rig::with(Options::new(mode).settings(allow));
        for (tool, args) in [
            ("read", json!({"path": ".env"})),
            ("shell", shell("cat .env")),
            ("shell", shell("cat .env .env")),
        ] {
            assert_eq!(
                first(&rig, tool, args.clone()).await,
                (Decision::Allow, Tier::AllowRule),
                "{tool} {args} {mode:?}"
            );
        }
    }
    // A deny rule still wins over the allow.
    let rig = Rig::with(
        Options::new(Manual)
            .settings("[permissions]\nallow = [\"read(.env)\"]\ndeny = [\"read(.env)\"]\n"),
    );
    for (tool, args) in [
        ("read", json!({"path": ".env"})),
        ("shell", shell("cat .env")),
    ] {
        assert_eq!(
            first(&rig, tool, args).await,
            (Decision::Deny, Tier::DenyRule)
        );
    }
    // One operand's allow does not vouch for the secret beside it.
    let rig =
        Rig::with(Options::new(Manual).settings("[permissions]\nallow = [\"read(README.md)\"]\n"));
    assert_eq!(
        first(&rig, "shell", shell("cat .env README.md")).await,
        (Decision::Prompt, Tier::Mode)
    );
    assert_eq!(
        first(&rig, "shell", shell("cat README.md")).await,
        (Decision::Allow, Tier::Tier05)
    );
}

#[tokio::test]
async fn a_secret_read_is_found_through_the_shell_cwd() {
    let rig = Rig::new(Manual);
    std::fs::create_dir(rig.work().join("sub")).unwrap();
    std::fs::write(rig.work().join("sub/.env"), "k").unwrap();
    assert_eq!(
        first(&rig, "shell", json!({"command": "cat .env", "cwd": "sub"})).await,
        (Decision::Prompt, Tier::Mode)
    );
}

#[tokio::test]
async fn fontaines_own_directories_are_protected() {
    for mode in [Manual, AcceptEdits] {
        let rig = Rig::with(
            Options::new(mode).settings("[permissions]\nallow = [\"write\", \"edit\"]\n"),
        );
        for dir in [&rig.world.config, &rig.world.data] {
            let path = dir.join("settings.toml");
            let path = path.to_str().unwrap();
            assert_eq!(
                first(&rig, "write", write_args(path)).await,
                (Decision::Prompt, Tier::ProtectedPath),
                "{path} {mode:?}"
            );
        }
    }
}

#[tokio::test]
async fn an_edit_prompt_carries_a_preview_that_decides_nothing() {
    let rig = Rig::new(Manual);
    std::fs::write(rig.world.work.join("a.txt"), "one\ntwo\n").unwrap();
    let args = json!({"path": "a.txt", "edits": [{"old_text": "two", "new_text": "2"}]});

    rig.approver.answer(Answer::AllowOnce);
    assert!(allowed(&rig.check("edit", args.clone()).await));
    let preview = rig.approver.last_request().preview.expect("a preview");
    assert_eq!((preview.added, preview.removed), (1, 1));

    // The same call with nothing to preview is asked and decided the same way.
    std::fs::remove_file(rig.world.work.join("a.txt")).unwrap();
    rig.approver.answer(Answer::AllowOnce);
    assert!(allowed(&rig.check("edit", args.clone()).await));
    assert!(rig.approver.last_request().preview.is_none());

    assert!(!allowed(&rig.check("edit", args).await));
    assert_eq!(rig.approver.asked(), 3);
}

fn preview_edit(old: &str, new: &str) -> Value {
    json!({"path": "a.txt", "edits": [{"old_text": old, "new_text": new, "replace_all": false}]})
}

#[tokio::test]
async fn an_edit_allowed_without_a_prompt_publishes_its_diff_apart_from_the_audit_record() {
    for options in [
        Options::new(AcceptEdits),
        Options::new(Manual).settings("[permissions]\nallow = [\"edit\"]\n"),
    ] {
        let rig = Rig::with(options);
        std::fs::write(rig.work().join("a.txt"), "one\ntwo\n").unwrap();
        let d = rig.check("edit", preview_edit("two", "2")).await;
        assert!(allowed(&d));
        assert_eq!(rig.approver.asked(), 0);
        let mut feed = rig.state.feed().await;
        let (id, preview) = feed.try_next_preview().expect("a preview");
        assert_eq!(id, "call-1");
        assert_eq!((preview.added, preview.removed), (1, 1));
        assert!(feed.try_next_preview().is_none());
        drop(feed);
        let raw = std::fs::read_to_string(rig.session.audit.path()).unwrap();
        assert!(
            !raw.contains("preview") && !raw.contains("\"lines\""),
            "{raw}"
        );
    }
}

#[tokio::test]
async fn a_prompted_edit_publishes_the_diff_it_was_shown() {
    let rig = Rig::new(Manual);
    std::fs::write(rig.work().join("a.txt"), "one\ntwo\n").unwrap();
    rig.approver.answer(Answer::AllowOnce);
    let d = rig.check("edit", preview_edit("two", "2")).await;
    assert!(allowed(&d));
    let shown = rig
        .approver
        .last_request()
        .preview
        .expect("the prompt's diff");
    let mut feed = rig.state.feed().await;
    let (_, preview) = feed.try_next_preview().expect("a preview");
    assert_eq!(preview, shown);
}

#[tokio::test]
async fn an_interrupted_prompt_leaves_no_diff_for_the_next_call() {
    let rig = Rig::new(Manual);
    std::fs::write(rig.work().join("a.txt"), "one\ntwo\n").unwrap();
    rig.approver
        .hang
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let interrupted = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        rig.check("edit", preview_edit("two", "2")),
    )
    .await;
    assert!(interrupted.is_err());
    assert_eq!(rig.approver.asked(), 1);

    rig.state.set_mode(AcceptEdits);
    let d = rig.check("edit", preview_edit("one", "1")).await;
    assert!(allowed(&d));
    let (id, preview) = rig
        .state
        .feed()
        .await
        .try_next_preview()
        .expect("a preview");
    assert_eq!(id, "call-2");
    assert!(preview.lines.iter().any(|l| l.text == "1"));
    assert!(preview.lines.iter().all(|l| l.text != "2"));
}

#[tokio::test]
async fn a_denied_edit_and_other_tools_publish_no_diff() {
    let rig = Rig::new(DontAsk);
    std::fs::write(rig.work().join("a.txt"), "one\ntwo\n").unwrap();
    let d = rig.check("edit", preview_edit("two", "2")).await;
    assert!(!allowed(&d));
    let manual = Rig::new(Manual);
    std::fs::write(manual.work().join("a.txt"), "one\ntwo\n").unwrap();
    manual.approver.answer(Answer::Deny { feedback: None });
    assert!(!allowed(
        &manual.check("edit", preview_edit("two", "2")).await
    ));
    assert!(allowed(
        &manual.check("read", json!({"path": "a.txt"})).await
    ));
    for r in [&rig, &manual] {
        assert!(r.state.feed().await.try_next_preview().is_none());
    }
}

fn memory_file(rig: &Rig) -> String {
    rig.memory
        .as_ref()
        .expect("a rig with memory")
        .join("MEMORY.md")
        .display()
        .to_string()
}

#[tokio::test]
async fn memory_writes_are_a_protected_path_outcome_in_every_mode() {
    for (tool, make) in [
        ("write", write_args as fn(&str) -> Value),
        ("edit", edit_args),
    ] {
        for mode in [Manual, AcceptEdits] {
            // Allow rules for the tools do not approve the store.
            let rig = Rig::with(
                Options::new(mode)
                    .memory()
                    .settings("[permissions]\nallow = [\"write\", \"edit\"]\n"),
            );
            let path = memory_file(&rig);
            assert_eq!(
                first(&rig, tool, make(&path)).await,
                (Decision::Prompt, Tier::ProtectedPath),
                "{tool} {mode:?}"
            );
        }
        let rig = Rig::with(Options::new(DontAsk).memory());
        let path = memory_file(&rig);
        assert_eq!(
            first(&rig, tool, make(&path)).await,
            (Decision::Deny, Tier::ProtectedPath),
            "{tool} dontAsk"
        );
        let rig = Rig::with(Options::new(Manual).memory().headless());
        let path = memory_file(&rig);
        assert_eq!(
            first(&rig, tool, make(&path)).await,
            (Decision::Deny, Tier::ProtectedPath),
            "{tool} headless"
        );
    }
}

#[tokio::test]
async fn a_shell_write_to_the_memory_store_takes_the_ordinary_shell_path() {
    let rig = Rig::with(Options::new(Manual).memory());
    let path = memory_file(&rig);
    let (decision, tier) = first(&rig, "shell", shell(&format!("echo x >> {path}"))).await;
    assert_ne!(tier, Tier::ProtectedPath);
    assert_eq!(decision, Decision::Prompt);
}

#[tokio::test]
async fn the_out_of_scope_error_points_at_the_memory_directory() {
    let rig = Rig::with(Options::new(Manual).memory());
    // Only writes keep the containment denial; it still points at the memory directory.
    let d = rig.check("write", write_args("/etc/hostname")).await;
    let message = denied_message(&d);
    let memory = rig.memory.as_ref().unwrap().display().to_string();
    assert!(message.contains(&memory), "{message}");
}

/// Every path-naming write tool must be known to each hand-maintained path site, or a new tool
/// would slip past containment, rules or the protected-path floor.
#[tokio::test]
async fn every_path_naming_write_tool_is_handled_by_each_path_site() {
    use fontaine_core::destructive::touched_paths;
    use fontaine_core::permission_rules::{CallContext, Posture, matches, parse_rule};
    use fontaine_tools::descriptor::TOOLS;
    use fontaine_tools::risk::RiskClass;
    use fontaine_tools::scope::Scope;

    let rows = TOOLS
        .iter()
        .filter(|d| d.risk == RiskClass::WorkspaceWrite && !d.path_free);
    for d in rows {
        let rig = Rig::new(Manual);
        let scope = Scope::new(rig.work().to_path_buf(), []);
        let args = json!({"path": "target.txt"});
        assert!(scope.locate(d.name, &args).is_ok(), "{}: scope", d.name);
        let rule = parse_rule(&format!("{}(target.txt)", d.name)).unwrap();
        let call = CallContext {
            tool: d.name,
            args: &args,
            scope: &scope,
            home: None,
        };
        assert!(matches(&rule, &call, Posture::Deny), "{}: rules", d.name);
        assert!(
            !touched_paths(d.name, &args, &scope).is_empty(),
            "{}: touched paths",
            d.name
        );
        let protected = rig.check(d.name, json!({"path": ".git/config"})).await;
        assert!(!allowed(&protected), "{}: protected path", d.name);
        assert_eq!(rig.audit()[0].tier, Tier::ProtectedPath, "{}", d.name);
    }
}

#[tokio::test]
async fn a_powershell_call_is_never_preclassified() {
    // `ls` is a read-only shell command that skips every later tier; in PowerShell no command
    // is read as safe.
    let rig = Rig::new(Manual);
    for command in ["ls", "Get-ChildItem", "Get-Date", "git status"] {
        assert_eq!(
            first(&rig, "powershell", shell(command)).await,
            (Decision::Prompt, Tier::Mode),
            "{command}"
        );
    }
    let fake = fake_guardian(allow_verdict());
    let rig = Rig::with(Options::new(Manual).guardian(fake.guardian.clone()));
    assert_eq!(
        first(&rig, "shell", shell("ls")).await,
        (Decision::Allow, Tier::Tier05)
    );
}

#[tokio::test]
async fn powershell_deletes_of_critical_paths_prompt_in_every_mode() {
    // Each rig has its own workspace, so the paths are filled in from the rig under test.
    let commands = [
        "Remove-Item -Recurse -Force '{parent}'",
        "rm -r '{work}'",
        "Remove-Item $env:USERPROFILE -Recurse",
        "Get-ChildItem | Remove-Item",
        "iex $payload",
    ];
    let fill = |rig: &Rig, command: &str| {
        let work = rig.work();
        command
            .replace("{work}", &work.display().to_string())
            .replace("{parent}", &work.parent().unwrap().display().to_string())
    };
    let allow_all = "[permissions]
allow = [\"powershell\"]
";
    for template in commands {
        for mode in [Manual, AcceptEdits, Plan] {
            let rig = Rig::with(Options::new(mode).settings(allow_all));
            let command = &fill(&rig, template);
            assert_eq!(
                first(&rig, "powershell", shell(command)).await,
                (Decision::Prompt, Tier::CriticalPath),
                "{command} {mode:?}"
            );
        }
        let rig = Rig::with(Options::new(DontAsk).settings(allow_all));
        let command = &fill(&rig, template);
        assert_eq!(
            first(&rig, "powershell", shell(command)).await,
            (Decision::Deny, Tier::CriticalPath),
            "{command}"
        );
    }
    // An ordinary removal inside the workspace is not critical.
    let rig = Rig::new(Manual);
    assert_eq!(
        first(&rig, "powershell", shell("Remove-Item build/out.o")).await,
        (Decision::Prompt, Tier::Mode)
    );
}

#[tokio::test]
async fn a_shell_deny_rule_stops_powershell_and_a_specifier_rule_does_not() {
    let settings = "[permissions]
deny = [\"shell\"]
";
    let rig = Rig::with(Options::new(Manual).settings(settings));
    assert_eq!(
        first(&rig, "powershell", shell("Get-Date")).await,
        (Decision::Deny, Tier::DenyRule)
    );
    let settings = "[permissions]
deny = [\"shell(git push)\"]
";
    let rig = Rig::with(Options::new(Manual).settings(settings));
    assert_eq!(
        first(&rig, "powershell", shell("git push")).await,
        (Decision::Prompt, Tier::Mode)
    );
}

/// A tracked shell directory is written into the call as `cwd` before the gate sees it, so a
/// relative operand resolves against it exactly as an explicit `cwd` does today.
#[tokio::test]
async fn a_relative_operand_resolves_against_the_tracked_directory() {
    for mode in MODES {
        let rig = Rig::new(mode);
        let sub = rig.work().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join(".env"), "S=1").unwrap();
        let sub = sub.to_string_lossy().into_owned();
        let in_sub = |command: &str| json!({"command": command, "cwd": sub});

        let secret = first(&rig, "shell", in_sub("cat .env")).await;
        assert_eq!(
            secret,
            first(&rig, "shell", shell("cat .env")).await,
            "{mode:?}"
        );
        assert_ne!(secret.1, Tier::Tier05, "{mode:?}");
        assert_eq!(
            first(&rig, "shell", in_sub("cat .env.example")).await,
            (Decision::Allow, Tier::Tier05),
            "{mode:?}"
        );
        // `..` out of the tracked directory is still judged against the real path.
        let up = first(&rig, "shell", in_sub("cat ../sub/.env")).await;
        assert_ne!(up.1, Tier::Tier05, "{mode:?}");
        // A directory outside every root disqualifies the fast path, tracked or explicit.
        let outside = json!({"command": "cat a.txt", "cwd": "/"});
        assert_ne!(
            first(&rig, "shell", outside).await.1,
            Tier::Tier05,
            "{mode:?}"
        );
    }
}

#[test]
fn the_approval_summary_names_a_directory_only_when_the_call_has_one() {
    use fontaine_core::permissions::summarize;
    assert_eq!(summarize("shell", &json!({"command": "ls"})), "ls");
    assert_eq!(
        summarize("shell", &json!({"command": "ls", "cwd": null})),
        "ls"
    );
    assert_eq!(
        summarize("shell", &json!({"command": "ls", "cwd": "/w/sub"})),
        "in /w/sub: ls"
    );
    assert_eq!(
        summarize("powershell", &json!({"command": "dir", "cwd": "C:/w/sub"})),
        "in C:/w/sub: dir"
    );
}
