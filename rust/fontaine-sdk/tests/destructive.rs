//! The destructive class: derived from the call itself, on the shell parser and the scope
//! validator, never from the guardian's label.

use fontaine_sdk::core::code_execution::PROGRAMS;
use fontaine_sdk::core::destructive::{
    CLOUD_CLI_WORDS, CLUSTER_CLI_WORDS, Facts, GIT_NETWORK_SUBCOMMANDS, INSTALLERS,
    NETWORK_CLIENT_WORDS, REMOTE_WRITE_SUBCOMMANDS, REMOVAL_OVERWRITE_WORDS, derive, touched_paths,
};
use fontaine_sdk::core::ledger::{LedgerView, path_key};
use fontaine_sdk::tools::scope::Scope;
use serde_json::json;

fn world() -> (tempfile::TempDir, Scope, LedgerView) {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    (dir, Scope::new(root, []), LedgerView::default())
}

fn shell_is(command: &str) -> bool {
    let (_dir, scope, view) = world();
    derive(
        "shell",
        &json!({ "command": command }),
        &Facts {
            scope: &scope,
            view: &view,
        },
    )
}

#[test]
fn argv_launchers_are_judged_by_the_program_they_wrap() {
    for command in [
        "strace -f rm -rf src",
        "strace -o out.txt -e trace=open rm x",
        "taskset -c 0 rm x",
        "/usr/bin/time rm x",
        "/usr/bin/time -f %e rm x",
        "fakeroot rm x",
        "nsenter -t 1 rm x",
        "chrt 10 rm x",
        "systemd-run --user ls",
        "parallel echo ::: a",
    ] {
        assert!(shell_is(command), "{command}");
    }
    for command in ["strace ls", "taskset -c 0 ls", "/usr/bin/time ls"] {
        assert!(!shell_is(command), "{command}");
    }
}

#[test]
fn git_config_injection_is_destructive() {
    for command in [
        "git -c alias.x='!rm -rf ~' x",
        "git -c core.pager=less log",
        "git -c core.fsmonitor=evil status",
        "git -ccore.pager=less log",
        "git --config-env=core.pager=VAR log",
        "git --exec-path=/tmp/x status",
    ] {
        assert!(shell_is(command), "{command}");
    }
    assert!(!shell_is("git -C sub status"));
}

#[test]
fn plain_builds_and_tests_are_not_destructive() {
    for command in [
        "cargo test",
        "cargo test 2>&1 | tail",
        "cargo build --release",
        "npm test",
        "git status",
        "git diff HEAD~1",
        "git log --oneline",
        "git commit -m 'fix rm and curl'",
        "ls -la src | wc -l",
        "echo hello",
        "cat Cargo.toml | head -5",
        "cargo test 2>&1",
        "sed -n '1,5p' file",
        "find . -name '*.rs'",
        "git branch -d merged",
        "git checkout -b feature",
        "git checkout",
        "git switch main",
        "git switch -c feature",
        "git stash",
        "git push-notes",
    ] {
        assert!(!shell_is(command), "{command}");
    }
}

#[test]
fn every_removal_and_network_word_is_destructive() {
    for word in REMOVAL_OVERWRITE_WORDS
        .iter()
        .chain(&NETWORK_CLIENT_WORDS)
        .chain(&CLOUD_CLI_WORDS)
        .chain(&CLUSTER_CLI_WORDS)
    {
        assert!(shell_is(&format!("{word} a b")), "{word}");
        assert!(shell_is(&format!("ls && {word} a")), "{word} in a list");
        assert!(shell_is(&format!("/usr/bin/{word} a")), "{word} by path");
    }
}

#[test]
fn every_installer_and_git_network_subcommand_is_destructive() {
    for (program, subcommands) in INSTALLERS {
        for subcommand in subcommands {
            assert!(
                shell_is(&format!("{program} {subcommand} pkg")),
                "{program} {subcommand}"
            );
        }
    }
    for subcommand in GIT_NETWORK_SUBCOMMANDS {
        assert!(shell_is(&format!("git {subcommand}")), "git {subcommand}");
        assert!(
            shell_is(&format!("git -C sub {subcommand}")),
            "git -C {subcommand}"
        );
    }
    for program in ["dnf", "yum"] {
        assert!(shell_is(&format!("{program} install pkg")), "{program}");
    }
}

#[test]
fn every_remote_write_subcommand_is_destructive_and_its_reads_are_not() {
    for (program, subcommands) in REMOTE_WRITE_SUBCOMMANDS {
        for subcommand in subcommands {
            assert!(
                shell_is(&format!("{program} {subcommand} x")),
                "{program} {subcommand}"
            );
            assert!(
                shell_is(&format!("{program} --quiet {subcommand} x")),
                "{program} --quiet {subcommand}"
            );
        }
    }
    for command in [
        "gh pr view 1",
        "gh pr checks",
        "gh issue list",
        "docker build -t app .",
        "docker ps",
        "systemctl status x",
        "tmux display-message -p x",
        "npm test",
    ] {
        assert!(!shell_is(command), "{command}");
    }
}

/// The commands the classifier prompt Claude Code's auto mode runs on names (its hard and soft
/// block rules and their definitions), each with a representative call and whether `derive`
/// flags it. A term Fontaine leaves out is asserted `false` so that changing the lists shows up
/// here; the Decisions on the guardian say why.
#[test]
fn the_commands_claude_codes_auto_mode_names_are_compared_term_by_term() {
    const TERMS: &[(&str, &str, bool)] = &[
        // Removal, overwrite and history rewrite.
        ("rm", "rm -rf build", true),
        ("rmdir", "rmdir build", true),
        ("mv", "mv a b", true),
        ("cp", "cp a b", true),
        ("sed -i", "sed -i s/a/b/ f", true),
        ("find -delete", "find . -name x -delete", true),
        ("rsync --delete", "rsync --delete a/ b/", true),
        ("git reset --hard", "git reset --hard origin/main", true),
        ("git clean", "git clean -fd", true),
        ("git checkout --", "git checkout -- .", true),
        ("git restore", "git restore .", true),
        ("git stash drop", "git stash drop", true),
        ("git push --force", "git push --force origin main", true),
        ("git push", "git push origin main", true),
        ("git pull", "git pull", true),
        (
            "git remote add",
            "git remote add x http://example.com/r.git",
            true,
        ),
        (
            "git remote set-url",
            "git remote set-url origin http://example.com/r.git",
            true,
        ),
        ("git commit --amend", "git commit --amend", true),
        (
            "git worktree remove --force",
            "git worktree remove --force wt",
            true,
        ),
        ("git branch -D", "git branch -D x", true),
        ("git filter-branch", "git filter-branch", true),
        ("crontab", "crontab -e", true),
        // Merges, reviews and other writes to a code host.
        ("gh pr merge", "gh pr merge --auto 1", true),
        ("gh pr review", "gh pr review --approve 1", true),
        ("gh pr comment", "gh pr comment 1 --body x", true),
        ("gh pr edit", "gh pr edit 1 --base main", true),
        ("gh pr ready", "gh pr ready 1", true),
        ("gh release create", "gh release create v1", true),
        ("gh repo create", "gh repo create x", true),
        ("gh repo fork", "gh repo fork x", true),
        ("gh secret set", "gh secret set X", true),
        ("gh workflow disable", "gh workflow disable ci", true),
        ("gh gist create", "gh gist create f", true),
        ("gh api", "gh api repos/a/b", true),
        // Clusters and infrastructure.
        ("kubectl apply", "kubectl apply -f x.yaml", true),
        ("kubectl delete", "kubectl delete pod x", true),
        ("kubectl exec", "kubectl exec -it pod -- sh", true),
        ("kubectl get secret", "kubectl get secret x -o yaml", true),
        ("kubectl port-forward", "kubectl port-forward pod 80", true),
        ("kubectl cordon", "kubectl cordon n", true),
        ("kubectl drain", "kubectl drain n", true),
        ("kubectl attach", "kubectl attach p", true),
        ("helm upgrade", "helm upgrade x y", true),
        ("argocd app sync", "argocd app sync x", true),
        ("scancel", "scancel 1", true),
        ("scontrol update", "scontrol update x", true),
        ("nomad job stop", "nomad job stop x", true),
        ("ray stop", "ray stop", true),
        ("docker service rm", "docker service rm s", true),
        ("terraform apply", "terraform apply", true),
        ("terraform destroy", "terraform destroy", true),
        ("terragrunt destroy", "terragrunt destroy", true),
        ("pulumi up", "pulumi up", true),
        ("pulumi destroy", "pulumi destroy", true),
        ("cdk deploy", "cdk deploy", true),
        ("cdk destroy", "cdk destroy", true),
        ("atlantis apply", "atlantis apply", true),
        // Cloud providers.
        (
            "aws eks delete-nodegroup",
            "aws eks delete-nodegroup --cluster-name c",
            true,
        ),
        ("aws s3 rm", "aws s3 rm s3://b --recursive", true),
        (
            "aws sts get-session-token",
            "aws sts get-session-token",
            true,
        ),
        ("gsutil rm", "gsutil rm gs://b/x", true),
        (
            "gcloud node-pools delete",
            "gcloud container node-pools delete p",
            true,
        ),
        (
            "gcloud auth print-access-token",
            "gcloud auth print-access-token",
            true,
        ),
        ("bq rm", "bq rm -r -d ds", true),
        // Publishing and containers.
        ("docker push", "docker push img", true),
        (
            "global option: docker -H tcp://host run img",
            "docker -H tcp://host run img",
            true,
        ),
        (
            "global option: docker --context prod push img",
            "docker --context prod push img",
            true,
        ),
        (
            "global option: tmux -L sock send-keys -t 1 ls",
            "tmux -L sock send-keys -t 1 ls",
            true,
        ),
        (
            "global option: tmux -S /tmp/s send-keys ls",
            "tmux -S /tmp/s send-keys ls",
            true,
        ),
        (
            "global option: npm --prefix dir publish",
            "npm --prefix dir publish",
            true,
        ),
        (
            "global option: systemctl -H host restart x",
            "systemctl -H host restart x",
            true,
        ),
        (
            "global option: gh pr -R o/r merge 1",
            "gh pr -R o/r merge 1",
            true,
        ),
        (
            "global option: cargo +nightly publish",
            "cargo +nightly publish",
            true,
        ),
        (
            "global option: docker --host=tcp://h run img",
            "docker --host=tcp://h run img",
            true,
        ),
        ("podman push", "podman push img", true),
        ("docker exec", "docker exec c ls", true),
        ("docker run -v /:/host", "docker run -v /:/host img", true),
        (
            "docker run --privileged",
            "docker run --privileged img",
            true,
        ),
        ("npm publish", "npm publish", true),
        ("cargo publish", "cargo publish", true),
        ("gem push", "gem push x.gem", true),
        ("twine upload", "twine upload dist/x", true),
        (
            "npm install --registry",
            "npm install --registry=http://x pkg",
            true,
        ),
        (
            "pip install --index-url",
            "pip install --index-url http://x pkg",
            true,
        ),
        // The network, tunnels and scanners.
        ("curl", "curl http://x", true),
        ("nc", "nc host 80", true),
        ("ncat", "ncat host 80", true),
        ("socat", "socat - TCP:host:80", true),
        ("ssh -R", "ssh -R 80:localhost:80 host", true),
        ("autossh", "autossh -R 80:localhost:80 host", true),
        ("ngrok", "ngrok http 80", true),
        ("cloudflared tunnel", "cloudflared tunnel run", true),
        ("localtunnel", "localtunnel --port 80", true),
        ("chisel", "chisel client x R:80", true),
        ("frp", "frp -c x", true),
        ("bore", "bore local 80 --to x", true),
        ("tailscale funnel", "tailscale funnel 80", true),
        ("nmap", "nmap host", true),
        ("masscan", "masscan host", true),
        // Code execution and the machine's own state.
        ("python -c", "python -c print(1)", true),
        ("python -m http.server", "python -m http.server", true),
        ("bash -i", "bash -i", true),
        ("sh", "sh -c ls", true),
        ("systemctl restart", "systemctl restart x", true),
        ("tmux send-keys", "tmux send-keys -t 1 ls Enter", true),
        ("nsenter", "nsenter -t 1 rm x", true),
        // Left out on purpose; see the guardian Decision on the auto-mode comparison.
        ("git add", "git add .", false),
        ("git stage", "git stage .", false),
        ("git status", "git status", false),
        ("tar -x", "tar -x -f a.tar", false),
        ("unzip -o", "unzip -o a.zip", false),
        ("aider --yes-always", "aider --yes-always", false),
        ("codex --full-auto", "codex --full-auto", false),
        (
            "claude --dangerously-skip-permissions",
            "claude --dangerously-skip-permissions",
            false,
        ),
        ("sleep", "sleep 1", false),
        ("cat", "cat f", false),
        ("head", "head f", false),
    ];
    for (term, command, expected) in TERMS {
        assert_eq!(shell_is(command), *expected, "{term}: `{command}`");
    }
}

#[test]
fn a_browser_or_scheduling_tool_is_destructive_as_any_tool_without_a_class_is() {
    let (_dir, scope, view) = world();
    let facts = Facts {
        scope: &scope,
        view: &view,
    };
    for tool in [
        "mcp__claude-in-chrome__navigate",
        "mcp__Claude_Browser__click",
        "CronCreate",
        "ScheduleWakeup",
    ] {
        assert!(derive(tool, &json!({}), &facts), "{tool}");
    }
}

#[test]
fn every_flagged_form_is_destructive() {
    for command in [
        "sed -i s/a/b/ f",
        "sed -i.bak s/a/b/ f",
        "sed -ni s/a/b/p f",
        "sed --in-place s/a/b/ f",
        "sed --in-place=.bak s/a/b/ f",
        "perl -i -pe s/a/b/ f",
        "perl -pi -e s/a/b/ f",
        "find . -name x -delete",
        "find . -exec rm {} ;",
        "find . -execdir ls ;",
        "git reset --hard",
        "git reset --hard HEAD~1",
        "git clean -fd",
        "git checkout -- .",
        "git checkout HEAD -- f",
        "git checkout .",
        "git checkout src/a.rs",
        "git checkout -f",
        "git checkout --force main",
        "git switch -f main",
        "git switch --discard-changes main",
        "git rm f",
        "git rm -r --cached d",
        "git restore f",
        "git branch -D old",
        "git push --force",
        "git push -f origin main",
        "git push --force-with-lease",
        "git push origin +main",
        "git rebase main",
        "git filter-branch --all",
        "git stash drop",
        "git stash clear",
        "git -C sub reset --hard",
        "git push",
        "git fetch",
        "git pull",
        "git clone https://example.com/r.git",
        "git remote add x https://example.com/r.git",
        "pip install x",
        "pip3 install x",
        "npm install",
        "npm add x",
        "pnpm install",
        "pnpm add x",
        "yarn install",
        "yarn add x",
        "cargo install ripgrep",
        "gem install x",
        "go install ./...",
        "apt install x",
        "apt-get install x",
        "brew install x",
        "dnf install x",
        "yum install x",
        "echo x > f",
        "echo x >> f",
        "echo x >| f",
        "echo x &> f",
        "cargo test 2>err.log",
        "ls | tee out.txt",
    ] {
        assert!(shell_is(command), "{command}");
    }
}

#[test]
fn every_broad_code_execution_entry_is_destructive() {
    for entry in PROGRAMS {
        assert!(shell_is(entry), "{entry}");
        assert!(shell_is(&format!("{entry} arg")), "{entry} arg");
        assert!(
            shell_is(&format!("ls | {entry} x")),
            "{entry} in a pipeline"
        );
    }
    assert!(!shell_is("cargo test"));
    assert!(!shell_is("npm test"));
}

#[test]
fn what_the_parser_cannot_classify_is_destructive() {
    for command in [
        "ls 'unterminated",
        "$cmd arg",
        "$(pick) arg",
        "sh -c 'a; b'",
        "echo $((1+1))",
        "if then",
    ] {
        assert!(shell_is(command), "{command}");
    }
}

#[test]
fn a_shell_outside_the_workspace_is_destructive() {
    let (dir, scope, view) = world();
    let outside = dir.path().parent().unwrap().to_string_lossy().into_owned();
    let args = json!({ "command": "ls", "cwd": outside });
    assert!(derive(
        "shell",
        &args,
        &Facts {
            scope: &scope,
            view: &view
        }
    ));
}

#[test]
fn network_and_mcp_and_unknown_tools_are_destructive() {
    let (_dir, scope, view) = world();
    let facts = Facts {
        scope: &scope,
        view: &view,
    };
    assert!(derive(
        "webfetch",
        &json!({"url": "https://example.com"}),
        &facts
    ));
    assert!(derive(
        "websearch",
        &json!({"query": "rust 2024 edition"}),
        &facts
    ));
    assert!(derive("mcp__github__list_issues", &json!({}), &facts));
    assert!(derive("agent", &json!({}), &facts));
    for tool in ["read", "ls", "glob", "grep", "skill", "todo_write"] {
        assert!(!derive(tool, &json!({"path": "."}), &facts), "{tool}");
    }
}

#[test]
fn a_write_is_an_overwrite_only_when_it_replaces_something_old() {
    use fontaine_sdk::core::ledger::{Ledger, Record};

    let (dir, scope, _) = world();
    let root = scope.primary().to_path_buf();
    std::fs::write(root.join("old.txt"), "x").unwrap();
    std::fs::write(root.join("indexed.txt"), "x").unwrap();
    std::fs::write(root.join("session.txt"), "x").unwrap();
    let ledger = Ledger::create(dir.path()).unwrap();
    let call = |tool: &str, path: &str| {
        derive(
            tool,
            &json!({ "path": path }),
            &Facts {
                scope: &scope,
                view: &ledger.view(),
            },
        )
    };

    // Not indexed and on disk: possibly pre-existing, so an overwrite.
    assert!(call("write", "old.txt"));
    // Absent on disk: a creation.
    assert!(!call("write", "new.txt"));
    // Outside every root.
    assert!(call("write", "../outside.txt"));

    ledger
        .append(Record::FileIndex {
            paths: vec![root.join("indexed.txt").to_string_lossy().into_owned()],
            truncated: false,
        })
        .unwrap();
    assert!(call("edit", "indexed.txt"));

    // Touched by an earlier action of this session and not indexed.
    ledger
        .append(Record::Action {
            tool: "write".into(),
            agent: "root".into(),
            paths: vec![root.join("session.txt").to_string_lossy().into_owned()],
            created: vec![root.join("session.txt").to_string_lossy().into_owned()],
            summary: String::new(),
            tier: None,
            tool_use_id: None,
            digest: None,
        })
        .unwrap();
    assert!(!call("write", "session.txt"));
    assert!(!call("edit", "session.txt"));
    assert!(call("write", "old.txt"), "the other file is still old");
}

#[test]
fn shell_redirect_targets_are_touched_paths() {
    let (_dir, scope, _view) = world();
    let root = scope.primary().to_path_buf();
    let keys = |args: serde_json::Value| -> Vec<String> {
        touched_paths("shell", &args, &scope, None)
            .iter()
            .map(|p| path_key(p))
            .collect()
    };
    let key = |p: std::path::PathBuf| path_key(&p.to_string_lossy());

    let reported = keys(json!({"command": "printf '# generated\\n' >> cfg.py"}));
    assert_eq!(reported.first(), Some(&key(root.join("cfg.py"))));

    std::fs::create_dir(root.join("sub")).unwrap();
    let relative = keys(json!({"command": "echo x > out.txt", "cwd": "sub"}));
    assert_eq!(relative, [key(root.join("sub").join("out.txt"))]);

    let deduped = keys(json!({"command": "tee ./f > ./f"}));
    assert_eq!(deduped, [key(root.join("f"))]);

    assert!(keys(json!({"command": "echo x > $F"})).is_empty());
}

#[test]
fn a_redirect_to_dev_null_is_not_a_touched_path_or_a_write() {
    let (_dir, scope, _view) = world();
    let command = "RIG_DIR=$(find ~/.cargo/registry/src -maxdepth 2 -type d -name \"rig-core-0.45.0\" | head -1); echo \"$RIG_DIR\"; rg -n \"enum ContentPart\" -A 40 \"$RIG_DIR/src\" 2>/dev/null | head -80";
    assert!(shell_is(command));
    let paths = touched_paths("shell", &json!({ "command": command }), &scope, None);
    assert!(paths.iter().all(|p| !p.contains("/dev/null")), "{paths:?}");

    assert!(!shell_is("rg -n x src 2>/dev/null | head -80"));
    assert!(shell_is("echo x > /dev/stderr"));
}

#[test]
fn touched_paths_are_resolved_by_the_scope() {
    let (_dir, scope, _view) = world();
    let root = scope.primary().to_path_buf();
    std::fs::write(root.join("a.txt"), "x").unwrap();
    let paths = touched_paths("write", &json!({"path": "a.txt"}), &scope, None);
    // A Windows path may or may not carry the verbatim prefix; the ledger compares without it.
    let keys: Vec<String> = paths.iter().map(|p| path_key(p)).collect();
    assert_eq!(keys, [path_key(&root.join("a.txt").to_string_lossy())]);
    let shell = touched_paths(
        "shell",
        &json!({"command": "rm -rf a.txt ./sub echo nothing"}),
        &scope,
        None,
    );
    let shell_keys: Vec<String> = shell.iter().map(|p| path_key(p)).collect();
    assert!(shell_keys.contains(&path_key(&root.join("a.txt").to_string_lossy())));
    assert!(shell.iter().any(|p| p.ends_with("sub")));
    assert!(!shell.iter().any(|p| p.ends_with("nothing")));
    assert!(
        touched_paths(
            "webfetch",
            &json!({"url": "https://a.example"}),
            &scope,
            None
        )
        .is_empty()
    );
}

#[test]
fn a_memory_call_names_its_topic_file_and_the_index_under_the_memory_root() {
    let (_dir, scope, _view) = world();
    let root = std::path::Path::new("/data/memory/project-1");
    let args = json!({"topic": "Commands", "content": "x"});
    let paths = touched_paths("memory", &args, &scope, Some(root));
    assert_eq!(
        paths,
        vec![
            root.join("commands.md").to_string_lossy().into_owned(),
            root.join(fontaine_sdk::core::memory::INDEX_FILE)
                .to_string_lossy()
                .into_owned(),
        ]
    );
    // Without a memory root (and for other tools) the call names nothing.
    assert!(touched_paths("memory", &args, &scope, None).is_empty());
    assert!(
        touched_paths("write", &json!({"path": "a.txt"}), &scope, Some(root))
            .iter()
            .all(|p| !p.contains("memory"))
    );
}

#[test]
fn a_ledger_written_before_paths_and_summaries_existed_still_loads() {
    use fontaine_sdk::core::ledger::{Entry, Record};
    let old = r#"{"seq":3,"ts_ms":1,"kind":"action","tool":"read","agent":"root"}"#;
    let entry: Entry = serde_json::from_str(old).unwrap();
    assert_eq!(
        entry.record,
        Record::Action {
            tool: "read".into(),
            agent: "root".into(),
            paths: vec![],
            created: vec![],
            summary: String::new(),
            tier: None,
            tool_use_id: None,
            digest: None,
        }
    );
    let old = r#"{"seq":4,"ts_ms":1,"kind":"denial","tool":"shell","agent":"root","reason":"no"}"#;
    let entry: Entry = serde_json::from_str(old).unwrap();
    assert!(matches!(entry.record, Record::Denial { rule: None, .. }));
}

#[test]
fn launched_and_scripted_programs_are_judged_by_what_they_run() {
    for command in [
        "timeout 5 rm -rf build",
        "timeout -s KILL 5 rm x",
        "nice rm x",
        "nice -n 5 rm x",
        "nohup rm -rf src",
        "setsid rm x",
        "stdbuf -oL rm x",
        "chroot / rm x",
        "command rm x",
        "doas rm x",
        "doas -u root rm x",
        "busybox rm x",
        "ionice -c3 rm x",
        "watch rm x",
        "flock f rm x",
        "nice nohup timeout 5 rm x",
        "timeout 5",
        "nice",
        "awk 'BEGIN{system(\"rm -rf ~\")}'",
        "gawk 'BEGIN{}'",
        "mawk 1 f",
        "php -r 'x();'",
        "lua -e 'x()'",
        "tclsh script.tcl",
        "/usr/bin/python3.12 -c 'x()'",
        "python3.11 -c 'x()'",
        "ruby3.2 -e 'x()'",
        "pwsh -c 'x'",
        "powershell -c 'x'",
        "powershell.exe -c 'x'",
        "cmd /c 'del /s x'",
    ] {
        assert!(shell_is(command), "{command}");
    }
    for command in [
        "timeout 5 ls",
        "nice cargo test",
        "nohup cargo build",
        "flock f ls",
    ] {
        assert!(!shell_is(command), "{command}");
    }
}

#[test]
fn a_monitor_is_judged_by_the_same_command_analysis_as_the_shell() {
    let (_dir, scope, view) = world();
    let facts = Facts {
        scope: &scope,
        view: &view,
    };
    for command in [
        "rm -rf build",
        "git push",
        "ls > out.txt",
        "tail -f app.log",
        "git status",
        "echo $(date)",
        "curl https://example.com",
        "cat log | grep error",
    ] {
        let args = json!({ "command": command });
        assert_eq!(
            derive("monitor", &args, &facts),
            derive("shell", &args, &facts),
            "{command}"
        );
    }
    assert!(derive("monitor", &json!({ "command": "rm x" }), &facts));
    assert!(!derive(
        "monitor",
        &json!({ "command": "git status" }),
        &facts
    ));
    assert!(!derive("job_output", &json!({}), &facts));
}

#[test]
fn a_powershell_call_is_always_destructive_and_touches_no_path() {
    let (_dir, scope, view) = world();
    let args = json!({ "command": "Get-ChildItem" });
    let facts = Facts {
        scope: &scope,
        view: &view,
    };
    assert!(derive("powershell", &args, &facts));
    assert!(touched_paths("powershell", &args, &scope, None).is_empty());
}

#[test]
fn text_operands_of_printf_and_sed_are_not_touched_paths() {
    let scope = Scope::new(std::path::PathBuf::from("/w"), []);
    let command = "printf '\\n[dependencies]\\n' >> crates/x/Cargo.toml && sed -i 's/pub fn add/pub fn sum/' crates/x/src/main.rs && sed -e 's/a/b/' -i crates/x/src/lib.rs";
    let paths: Vec<String> = touched_paths("shell", &json!({ "command": command }), &scope, None)
        .iter()
        .map(|p| p.replace('\\', "/"))
        .collect();
    assert_eq!(
        paths,
        vec![
            "/w/crates/x/Cargo.toml",
            "/w/crates/x/src/main.rs",
            "/w/crates/x/src/lib.rs",
        ]
    );
}

fn paths_of(command: &str) -> Vec<String> {
    let scope = Scope::new(std::path::PathBuf::from("/w"), []);
    touched_paths("shell", &json!({ "command": command }), &scope, None)
        .iter()
        .map(|p| p.replace('\\', "/"))
        .collect()
}

#[test]
fn a_grep_pattern_is_not_a_touched_path() {
    assert!(
        paths_of("bin=$(ls -t target/debug/deps/agents-* | grep -v '\\.d$' | head -1); ($bin t)")
            .is_empty()
    );
    assert!(paths_of("ls -t target/debug/deps/agents-* | grep -v '\\.d$' | head -1").is_empty());
    for command in [
        "grep -m 5 'a/b' sub/f",
        "grep -e 'a/b' sub/f",
        "grep --regexp='a/b' sub/f",
        "grep -ie 'a/b' sub/f",
        "grep -e'a/b' sub/f",
        "egrep -A 2 -B2 'a/b' sub/f",
        "fgrep --include='*/x' 'a/b' sub/f",
        "rg -g '*.rs' 'a/b' sub/f",
        "rg --type=rust 'a/b' sub/f",
        "rg --type rust 'a/b' sub/f",
        "rg -t rust -e 'a/b' sub/f",
        "rg -j4 'a/b' sub/f",
        "grep -- 'a/b' sub/f",
    ] {
        assert_eq!(paths_of(command), ["/w/sub/f"], "{command}");
    }
}

#[test]
fn a_grep_pattern_file_stays_a_path() {
    assert_eq!(
        paths_of("grep -f sub/pats sub/f"),
        ["/w/sub/pats", "/w/sub/f"],
        "the pattern comes from the file, so the first operand is a file too"
    );
    assert_eq!(paths_of("rg -f sub/pats g/x"), ["/w/sub/pats", "/w/g/x"]);
}

#[test]
fn rg_modes_without_a_pattern_keep_every_operand_a_path() {
    assert_eq!(paths_of("rg --files sub/src"), ["/w/sub/src"]);
    assert_eq!(paths_of("rg --type-list sub/src"), ["/w/sub/src"]);
}
