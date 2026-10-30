//! The destructive class: derived from the call itself, on the shell parser and the scope
//! validator, never from the guardian's label.

use fontaine_core::code_execution::PROGRAMS;
use fontaine_core::destructive::{
    Facts, GIT_NETWORK_SUBCOMMANDS, INSTALLERS, NETWORK_CLIENT_WORDS, REMOVAL_OVERWRITE_WORDS,
    derive, touched_paths,
};
use fontaine_core::ledger::{LedgerView, path_key};
use fontaine_tools::scope::Scope;
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
    for word in REMOVAL_OVERWRITE_WORDS.iter().chain(&NETWORK_CLIENT_WORDS) {
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
    use fontaine_core::ledger::{Ledger, Record};

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
        touched_paths("shell", &args, &scope)
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
fn touched_paths_are_resolved_by_the_scope() {
    let (_dir, scope, _view) = world();
    let root = scope.primary().to_path_buf();
    std::fs::write(root.join("a.txt"), "x").unwrap();
    let paths = touched_paths("write", &json!({"path": "a.txt"}), &scope);
    // A Windows path may or may not carry the verbatim prefix; the ledger compares without it.
    let keys: Vec<String> = paths.iter().map(|p| path_key(p)).collect();
    assert_eq!(keys, [path_key(&root.join("a.txt").to_string_lossy())]);
    let shell = touched_paths(
        "shell",
        &json!({"command": "rm -rf a.txt ./sub echo nothing"}),
        &scope,
    );
    let shell_keys: Vec<String> = shell.iter().map(|p| path_key(p)).collect();
    assert!(shell_keys.contains(&path_key(&root.join("a.txt").to_string_lossy())));
    assert!(shell.iter().any(|p| p.ends_with("sub")));
    assert!(!shell.iter().any(|p| p.ends_with("nothing")));
    assert!(touched_paths("webfetch", &json!({"url": "https://a.example"}), &scope).is_empty());
}

#[test]
fn a_ledger_written_before_paths_and_summaries_existed_still_loads() {
    use fontaine_core::ledger::{Entry, Record};
    let old = r#"{"seq":3,"ts_ms":1,"kind":"action","tool":"read","agent":"root"}"#;
    let entry: Entry = serde_json::from_str(old).unwrap();
    assert_eq!(
        entry.record,
        Record::Action {
            tool: "read".into(),
            agent: "root".into(),
            paths: vec![],
            created: vec![],
            summary: String::new()
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
    assert!(touched_paths("powershell", &args, &scope).is_empty());
}
