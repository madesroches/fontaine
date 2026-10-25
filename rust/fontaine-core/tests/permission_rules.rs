mod common;

use common::pipeline::{Options, Rig, allowed};
use common::{World, no_env};
use fontaine_core::audit::Decision;
use fontaine_core::config::PermissionMode::{DontAsk, Manual, Plan};
use fontaine_core::config::{CliRules, ConfigError, LoadParams, load};
use fontaine_core::permission_rules::{
    CallContext, Posture, Rule, is_broad_code_execution, matches, parse_rule, propose,
};
use fontaine_core::permissions::Answer;
use fontaine_core::tier::Tier;
use fontaine_tools::scope::Scope;
use serde_json::{Value, json};

fn rule(text: &str) -> Rule {
    parse_rule(text).unwrap_or_else(|e| panic!("{e}"))
}

struct Fixture {
    world: World,
    scope: Scope,
}

impl Fixture {
    fn new() -> Fixture {
        let world = World::new();
        let scope = Scope::new(world.work.clone(), []);
        Fixture { world, scope }
    }

    fn ctx<'a>(&'a self, tool: &'a str, args: &'a Value) -> CallContext<'a> {
        CallContext {
            tool,
            args,
            scope: &self.scope,
            home: None,
        }
    }

    fn hit(&self, rule_text: &str, tool: &str, args: Value, posture: Posture) -> bool {
        matches(&rule(rule_text), &self.ctx(tool, &args), posture)
    }
}

fn shell(command: &str) -> Value {
    json!({ "command": command })
}

#[test]
fn the_grammar_is_checked_per_tool_kind() {
    for good in [
        "shell",
        "shell(git push)",
        "shell(npm run test *)",
        "shell(rm '*.o')",
        "read(src/**)",
        "read(/src/lib.rs)",
        "write(//etc/hosts)",
        "edit(~/notes/*.md)",
        "ls(docs)",
        "glob(**/*.rs)",
        "grep(secrets/**)",
        "webfetch(domain:example.com)",
        "webfetch(domain:*.example.com)",
        "mcp__github",
        "mcp__github__create_issue",
        "mcp__github__*",
        "agent(worker)",
        "skill(review)",
        "send_message",
        "ask_user_question",
        "todo_write",
    ] {
        parse_rule(good).unwrap_or_else(|e| panic!("{good}: {e}"));
    }
    for bad in [
        "shell(*)",
        "shell(git push; ls)",
        "shell(ls | wc)",
        "shell($HOME/x)",
        "shell(echo $(x))",
        "read(#c)",
        "read(!x)",
        "webfetch(example.com)",
        "send_message(x)",
        "ask_user_question(x)",
        "todo_write(x)",
        "mcp__github(x)",
        "agent(a b)",
        "unknown_tool",
        "bash",
    ] {
        assert!(parse_rule(bad).is_err(), "{bad}");
    }
}

#[test]
fn shell_deny_and_ask_hit_compound_wrapped_and_unparseable_commands() {
    let f = Fixture::new();
    for posture in [Posture::Deny, Posture::Ask] {
        for command in [
            "git push",
            "git push origin main",
            "ls && git push",
            "ls; git push -f",
            "a | b | git push",
            "sudo git push -f",
            "env FOO=1 git push",
            "echo $(git push)",
            "(git push)",
            "if true; then git push; fi",
            "git push 'unterminated",
            "$(echo git) push",
            "git push \"$X\"",
            "bash -c 'git push'",
        ] {
            assert!(
                f.hit("shell(git push)", "shell", shell(command), posture),
                "{command}"
            );
        }
        for command in ["git status", "gitpush", "echo git push", "git pull"] {
            // `echo git push` still holds the words as a contiguous run: that is the posture.
            let expect = command == "echo git push";
            assert_eq!(
                f.hit("shell(git push)", "shell", shell(command), posture),
                expect,
                "{command}"
            );
        }
    }
    assert!(f.hit("shell(rm)", "shell", shell("ls;rm x"), Posture::Deny));
    assert!(f.hit("shell(rm)", "shell", shell("ls && rm x"), Posture::Deny));
}

#[test]
fn shell_allow_never_matches_a_compound_command() {
    let f = Fixture::new();
    let allow =
        |rule_text: &str, command: &str| f.hit(rule_text, "shell", shell(command), Posture::Allow);
    assert!(allow("shell(git status)", "git status"));
    assert!(allow("shell(git status *)", "git status -s"));
    assert!(allow("shell(git status *)", "git status"));
    assert!(allow("shell(echo 'a b')", "echo \"a b\""));
    assert!(!allow("shell(git status)", "git status -s"));
    for command in [
        "git status; rm x",
        "git status && rm x",
        "git status | cat",
        "git status > out",
        "git status &",
        "git status $(rm x)",
        "git status $X",
        "git status *",
        "FOO=1 git status",
        "(git status)",
        "git status 'unterminated",
    ] {
        assert!(!allow("shell(git status *)", command), "{command}");
    }
}

#[test]
fn path_rules_match_the_resolved_path_and_symlinks_cannot_dodge_a_deny() {
    let f = Fixture::new();
    let work = &f.world.work;
    std::fs::create_dir_all(work.join("secret")).unwrap();
    std::fs::write(work.join("secret/key.txt"), "k").unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(work.join("secret"), work.join("alias")).unwrap();
        let args = json!({"path": "alias/key.txt"});
        // A symlink into a denied path is denied; deny and ask also look at the path as written.
        for posture in [Posture::Deny, Posture::Ask] {
            assert!(f.hit("read(secret/**)", "read", args.clone(), posture));
            assert!(f.hit("read(alias/**)", "read", args.clone(), posture));
        }
        // An allow on the alias does not match: allow sees only the resolved path.
        assert!(!f.hit("read(alias/**)", "read", args.clone(), Posture::Allow));
        assert!(f.hit("read(secret/**)", "read", args, Posture::Allow));
    }
    // `..` is folded before matching.
    let args = json!({"path": "docs/../secret/key.txt"});
    assert!(f.hit("read(secret/**)", "read", args, Posture::Deny));
}

#[test]
fn path_patterns_follow_gitignore_rules() {
    let f = Fixture::new();
    let read = |pattern: &str, path: &str| {
        f.hit(
            &format!("read({pattern})"),
            "read",
            json!({ "path": path }),
            Posture::Deny,
        )
    };
    assert!(read(".env", ".env"));
    assert!(read(".env", "sub/dir/.env"));
    assert!(read("*.pem", "keys/a.pem"));
    assert!(read("/src/lib.rs", "src/lib.rs"));
    assert!(!read("/lib.rs", "src/lib.rs"));
    assert!(read("src/**", "src/a/b.rs"));
    assert!(!read("src/**", "other/a.rs"));
    assert!(read("secrets", "secrets/a/b"));
    let absolute = f.world.work.join("a.txt");
    // Windows absolute rules are written `//C:/...`, never with the verbatim prefix.
    let absolute_rule = if cfg!(windows) {
        let text = absolute.display().to_string();
        format!("//{}", text.trim_start_matches(r"\\?\").replace('\\', "/"))
    } else {
        format!("/{}", absolute.display())
    };
    assert!(read(&absolute_rule, absolute.to_str().unwrap()));
    // Directory-scoped tools are covered by a rule for the tree.
    for tool in ["ls", "grep", "glob"] {
        let args = match tool {
            "glob" => json!({"pattern": "*", "path": "secrets"}),
            _ => json!({"pattern": "x", "path": "secrets"}),
        };
        assert!(
            f.hit(
                "ls(secrets/**)",
                "ls",
                json!({"path": "secrets"}),
                Posture::Deny
            ),
            "ls"
        );
        let rule_text = format!("{tool}(secrets/**)");
        assert!(f.hit(&rule_text, tool, args, Posture::Deny), "{tool}");
    }
}

#[test]
fn a_home_relative_pattern_anchors_at_the_home_directory() {
    let f = Fixture::new();
    let home = f.world.root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let args = json!({"path": home.join(".aws/credentials").to_str().unwrap()});
    let ctx = CallContext {
        tool: "read",
        args: &args,
        scope: &f.scope,
        home: Some(&home),
    };
    assert!(matches(&rule("read(~/.aws/**)"), &ctx, Posture::Deny));
    assert!(!matches(&rule("read(~/.ssh/**)"), &ctx, Posture::Deny));
    let ctx = CallContext { home: None, ..ctx };
    // With no home to anchor it, a deny or ask rule still matches; an allow rule does not.
    assert!(matches(&rule("read(~/.aws/**)"), &ctx, Posture::Deny));
    assert!(matches(&rule("read(~/.aws/**)"), &ctx, Posture::Ask));
    assert!(!matches(&rule("read(~/.aws/**)"), &ctx, Posture::Allow));
}

#[test]
fn webfetch_domains_are_normalized() {
    let f = Fixture::new();
    let fetch = |rule_text: &str, url: &str, posture| {
        f.hit(rule_text, "webfetch", json!({ "url": url }), posture)
    };
    assert!(fetch(
        "webfetch(domain:example.com)",
        "https://EXAMPLE.com/x",
        Posture::Allow
    ));
    assert!(fetch(
        "webfetch(domain:example.com)",
        "http://example.com:8080/",
        Posture::Allow
    ));
    assert!(!fetch(
        "webfetch(domain:example.com)",
        "https://sub.example.com/",
        Posture::Allow
    ));
    assert!(fetch(
        "webfetch(domain:*.example.com)",
        "https://a.b.example.com/",
        Posture::Allow
    ));
    assert!(!fetch(
        "webfetch(domain:*.example.com)",
        "https://example.com/",
        Posture::Allow
    ));
    assert!(!fetch(
        "webfetch(domain:*.example.com)",
        "https://badexample.com/",
        Posture::Allow
    ));
    // IDNA: the rule and the URL meet at the same ASCII form.
    assert!(fetch(
        "webfetch(domain:xn--bcher-kva.example)",
        "https://bücher.example/",
        Posture::Allow
    ));
    // An unreadable URL fails toward matching for deny and ask and away from it for allow.
    assert!(fetch(
        "webfetch(domain:example.com)",
        "not a url",
        Posture::Deny
    ));
    assert!(!fetch(
        "webfetch(domain:example.com)",
        "not a url",
        Posture::Allow
    ));
}

#[test]
fn mcp_names_match_by_server_and_tool() {
    let f = Fixture::new();
    let call = |rule_text: &str, tool: &str| f.hit(rule_text, tool, json!({}), Posture::Allow);
    assert!(call("mcp__github", "mcp__github__create_issue"));
    assert!(call("mcp__github__*", "mcp__github__create_issue"));
    assert!(call(
        "mcp__github__create_issue",
        "mcp__github__create_issue"
    ));
    assert!(!call(
        "mcp__github__create_issue",
        "mcp__github__delete_repo"
    ));
    assert!(!call("mcp__github", "mcp__gitlab__create_issue"));
    assert!(!call("mcp__git", "mcp__github__x"));
    assert!(!call("shell", "mcp__github__x"));
}

#[test]
fn agent_and_skill_rules_match_the_name() {
    let f = Fixture::new();
    assert!(f.hit("agent", "agent", json!({"agent_type": "x"}), Posture::Allow));
    assert!(f.hit(
        "agent(worker)",
        "agent",
        json!({"agent_type": "worker"}),
        Posture::Allow
    ));
    assert!(!f.hit(
        "agent(worker)",
        "agent",
        json!({"agent_type": "explore"}),
        Posture::Allow
    ));
    assert!(f.hit(
        "skill(review)",
        "skill",
        json!({"name": "review"}),
        Posture::Allow
    ));
}

#[tokio::test]
async fn an_allow_rule_never_passes_the_floor() {
    let settings = "[permissions]\nallow = [\"shell\", \"shell(rm *)\", \"write\", \"edit\", \"read\"]\ndeny = [\"read(.env)\"]\nask = [\"shell(git push)\"]\n";
    let rig = Rig::with(Options::new(DontAsk).settings(settings));
    std::fs::write(rig.work().join(".env"), "x").unwrap();
    let cases = [
        ("read", json!({"path": ".env"}), Tier::DenyRule),
        ("shell", shell("git push"), Tier::AskRule),
        ("shell", shell("rm -rf /"), Tier::CriticalPath),
        (
            "write",
            json!({"path": ".git/config", "content": "x"}),
            Tier::ProtectedPath,
        ),
    ];
    for (tool, args, tier) in cases {
        let d = rig.check(tool, args).await;
        assert!(!allowed(&d), "{tool} {tier}");
        assert_eq!(rig.audit().last().unwrap().tier, tier);
    }
    let rig = Rig::with(Options::new(Plan).settings(settings));
    let d = rig
        .check("write", json!({"path": "n.txt", "content": "x"}))
        .await;
    assert!(!allowed(&d));
    assert_eq!(rig.audit().last().unwrap().tier, Tier::PlanBlock);
}

#[tokio::test]
async fn an_allow_rule_pre_approves_in_dont_ask_and_skips_the_prompt_in_manual() {
    let settings =
        "[permissions]\nallow = [\"shell(touch x)\", \"webfetch(domain:example.com)\"]\n";
    for mode in [Manual, DontAsk, Plan] {
        let rig = Rig::with(Options::new(mode).settings(settings));
        let d = rig.check("shell", shell("touch x")).await;
        assert!(allowed(&d), "{mode:?}");
        let record = rig.audit()[0].clone();
        assert_eq!(
            (record.decision, record.tier),
            (Decision::Allow, Tier::AllowRule)
        );
        assert_eq!(record.rule.as_deref(), Some("shell(touch x) [user]"));
        assert_eq!(rig.approver.asked(), 0);
        let d = rig
            .check("webfetch", json!({"url": "https://example.com/a"}))
            .await;
        assert!(allowed(&d));
        let d = rig.check("shell", shell("touch y")).await;
        assert!(!allowed(&d) || mode == Manual, "{mode:?}");
    }
}

#[tokio::test]
async fn allow_session_lasts_for_the_session_and_is_recorded() {
    let rig = Rig::new(Manual);
    rig.approver.answer(Answer::AllowSession);
    assert!(allowed(&rig.check("shell", shell("touch 'a b'")).await));
    let request = rig.approver.requests.lock().unwrap()[0].clone();
    assert!(request.offers.session && request.offers.always);
    assert_eq!(request.proposed_rule.as_deref(), Some("shell(touch 'a b')"));

    // The next identical call needs no prompt; a different one still does.
    assert!(allowed(&rig.check("shell", shell("touch 'a b'")).await));
    assert_eq!(rig.approver.asked(), 1);
    assert!(!allowed(&rig.check("shell", shell("touch c")).await));
    assert_eq!(rig.approver.asked(), 2);

    let records = rig.audit();
    let rows: Vec<_> = records.iter().map(|r| (r.tier, r.decision)).collect();
    assert_eq!(
        &rows[..3],
        [
            (Tier::Mode, Decision::Prompt),
            (Tier::Human, Decision::Allow),
            (Tier::AllowRule, Decision::Allow)
        ]
    );
    assert_eq!(
        records[2].rule.as_deref(),
        Some("shell(touch 'a b') [session]")
    );
    let grants: Vec<_> = rig
        .session
        .ledger
        .read()
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.record {
            fontaine_core::ledger::Record::Grant { rule } => Some(rule),
            _ => None,
        })
        .collect();
    assert_eq!(grants, ["shell(touch 'a b')"]);
    // Nothing was persisted.
    assert_eq!(std::fs::read_to_string(&rig.user_config).unwrap(), "");
}

#[tokio::test]
async fn always_allow_writes_exactly_the_shown_rule_and_the_next_call_skips_the_prompt() {
    let rig = Rig::new(Manual);
    std::fs::write(&rig.user_config, "# keep me\n[model]\nmodel = \"a/b\"\n").unwrap();
    rig.approver.answer(Answer::AllowAlways);
    assert!(allowed(&rig.check("shell", shell("touch x")).await));
    let shown = rig.approver.requests.lock().unwrap()[0]
        .proposed_rule
        .clone()
        .unwrap();
    assert_eq!(shown, "shell(touch x)");
    let text = std::fs::read_to_string(&rig.user_config).unwrap();
    assert!(
        text.starts_with("# keep me\n[model]\nmodel = \"a/b\"\n"),
        "{text}"
    );
    assert!(text.contains(&format!("allow = [\"{shown}\"]")), "{text}");

    assert!(allowed(&rig.check("shell", shell("touch x")).await));
    assert_eq!(rig.approver.asked(), 1);
    assert_eq!(
        rig.audit().last().unwrap().rule.as_deref(),
        Some("shell(touch x) [user]")
    );

    // What was written loads as a user-scope allow rule.
    let config = rig.world.load().unwrap();
    assert_eq!(config.permissions.allow.len(), 1);
    assert_eq!(config.permissions.allow[0].to_string(), shown);
}

#[tokio::test]
async fn an_unwritable_settings_file_falls_back_to_a_session_grant_with_a_notice() {
    let rig = Rig::new(Manual);
    std::fs::write(&rig.user_config, "permissions = 3\n").unwrap();
    rig.approver.answer(Answer::AllowAlways);
    assert!(allowed(&rig.check("shell", shell("touch x")).await));
    assert_eq!(rig.approver.notices.lock().unwrap().len(), 1);
    assert!(allowed(&rig.check("shell", shell("touch x")).await));
    assert_eq!(rig.approver.asked(), 1);
    assert_eq!(
        std::fs::read_to_string(&rig.user_config).unwrap(),
        "permissions = 3\n"
    );
}

#[tokio::test]
async fn floor_forced_and_non_simple_prompts_offer_no_grants() {
    let settings = "[permissions]\nask = [\"read(.env)\"]\n";
    let rig = Rig::with(Options::new(Manual).settings(settings));
    std::fs::write(rig.work().join(".env"), "x").unwrap();
    for (tool, args) in [
        ("read", json!({"path": ".env"})),
        ("shell", shell("rm -rf /")),
        ("write", json!({"path": ".git/config", "content": "x"})),
        ("shell", shell("touch a; touch b")),
        ("shell", shell("touch $X")),
        ("shell", shell("cat x | wc")),
    ] {
        rig.approver.answer(Answer::AllowAlways);
        assert!(
            allowed(&rig.check(tool, args.clone()).await),
            "{tool} {args}"
        );
        let request = rig.approver.requests.lock().unwrap().pop().unwrap();
        assert!(!request.offers.session && !request.offers.always, "{args}");
        assert_eq!(request.proposed_rule, None);
    }
    // An answer the prompt did not offer is only the one call.
    assert_eq!(std::fs::read_to_string(&rig.user_config).unwrap(), settings);
    rig.approver.answer(Answer::AllowSession);
    assert!(allowed(
        &rig.check("shell", shell("touch a; touch b")).await
    ));
    rig.approver.answer(Answer::Deny { feedback: None });
    assert!(!allowed(
        &rig.check("shell", shell("touch a; touch b")).await
    ));
}

#[test]
fn proposed_rules_round_trip_through_the_parser_and_the_matcher() {
    let f = Fixture::new();
    let awkward = [
        "it's a file.txt",
        "star*.txt",
        "bracket[1].txt",
        "back\\slash.txt",
        "question?.txt",
        "trailing space ",
        "#hash.txt",
        "!bang.txt",
        "sp ace/nested dir/f.rs",
    ];
    for name in awkward {
        for tool in ["read", "write", "ls"] {
            let args = json!({ "path": name });
            let proposed = propose(&f.ctx(tool, &args)).unwrap_or_else(|| panic!("{tool} {name}"));
            let reparsed = parse_rule(&proposed.to_string()).unwrap();
            assert_eq!(reparsed, proposed);
            assert!(
                matches(&reparsed, &f.ctx(tool, &args), Posture::Allow),
                "{name}"
            );
            // And it does not cover a sibling.
            let other = json!({ "path": format!("{name}.other") });
            if tool != "ls" {
                assert!(
                    !matches(&reparsed, &f.ctx(tool, &other), Posture::Allow),
                    "{name}"
                );
            }
        }
    }
    let absolute = f.world.root.join("elsewhere/a b.txt");
    let args = json!({ "path": absolute.to_str().unwrap() });
    let proposed = propose(&f.ctx("read", &args)).unwrap();
    assert!(proposed.to_string().starts_with("read(//"), "{proposed}");
    assert!(matches(&proposed, &f.ctx("read", &args), Posture::Allow));

    for command in [
        "touch x",
        "git commit -m 'fix: it'\\''s done'",
        "echo \"two words\" 'single'",
        "grep -n 'a*b' file.txt",
        "printf '%s' ''",
        "echo '$HOME' \"~\"",
    ] {
        let args = shell(command);
        let proposed = propose(&f.ctx("shell", &args)).unwrap_or_else(|| panic!("{command}"));
        let reparsed = parse_rule(&proposed.to_string()).unwrap();
        assert!(
            matches(&reparsed, &f.ctx("shell", &args), Posture::Allow),
            "{command}"
        );
    }
    assert_eq!(propose(&f.ctx("shell", &shell("a; b"))), None);
    assert_eq!(propose(&f.ctx("shell", &shell("echo $X"))), None);

    let url = json!({"url": "https://Docs.Example.com/a?b=1"});
    let proposed = propose(&f.ctx("webfetch", &url)).unwrap();
    assert_eq!(proposed.to_string(), "webfetch(domain:docs.example.com)");
}

#[test]
fn the_broad_rule_list_names_what_auto_will_suspend() {
    for text in [
        "shell",
        "agent",
        "shell(bash)",
        "shell(python3 *)",
        "shell(npm run *)",
        "shell(xargs)",
    ] {
        assert!(is_broad_code_execution(&rule(text)), "{text}");
    }
    for text in [
        "shell(npm test)",
        "shell(python script.py)",
        "read",
        "webfetch",
    ] {
        assert!(!is_broad_code_execution(&rule(text)), "{text}");
    }
}

fn cli_params<'a>(w: &'a World, allow: &'a [String], deny: &'a [String]) -> LoadParams<'a> {
    LoadParams {
        launch_dir: &w.work,
        config_dir: &w.config,
        data_dir: &w.data,
        settings_file: None,
        cli_rules: CliRules { allow, deny },
        env: &no_env,
    }
}

#[test]
fn cli_rules_load_as_cli_scope_rules_and_are_validated_like_settings() {
    let w = World::new();
    let allow = vec!["shell(npm test)".to_string()];
    let deny = vec!["read(.env)".to_string(), "webfetch".to_string()];
    let config = load(&cli_params(&w, &allow, &deny)).unwrap();
    let labels: Vec<_> = config.permissions.deny.iter().map(|r| r.label()).collect();
    assert_eq!(labels, ["read(.env) [cli]", "webfetch [cli]"]);
    assert_eq!(config.permissions.allow[0].label(), "shell(npm test) [cli]");
    assert!(
        config
            .scopes
            .iter()
            .any(|s| s.slot == fontaine_core::config::Slot::Cli && s.loaded)
    );

    let bad = vec!["shell(".to_string()];
    let err = load(&cli_params(&w, &bad, &[])).unwrap_err();
    assert!(matches!(err, ConfigError::Usage { .. }), "{err}");
    assert!(err.to_string().contains("--allowed-tools"), "{err}");
    let err = load(&cli_params(&w, &[], &["nonsense".to_string()])).unwrap_err();
    assert!(err.to_string().contains("--disallowed-tools"), "{err}");
}

#[tokio::test]
async fn cli_rules_act_as_rules() {
    let w = World::new();
    let config = load(&cli_params(
        &w,
        &["shell(touch x)".to_string()],
        &["read(.env)".to_string()],
    ))
    .unwrap();
    assert_eq!(config.permissions.allow.len(), 1);
    // The same rules, through the pipeline, via a settings file with the same text.
    let rig = Rig::with(
        Options::new(DontAsk)
            .settings("[permissions]\nallow = [\"shell(touch x)\"]\ndeny = [\"read(.env)\"]\n"),
    );
    assert!(allowed(&rig.check("shell", shell("touch x")).await));
    assert!(!allowed(&rig.check("read", json!({"path": ".env"})).await));
}
