mod common;

use std::sync::Arc;

use common::{AllowAll, Dirs, call, env_with, registry, scope, text};
use fontaine_tools::scope::{Scope, ScopeError};
use serde_json::json;

#[cfg(unix)]
fn link(target: &std::path::Path, at: &std::path::Path) {
    std::os::unix::fs::symlink(target, at).unwrap();
}

#[test]
fn paths_inside_the_root_are_in_scope_whether_or_not_they_exist() {
    let d = Dirs::new();
    let root = d.dir("work");
    std::fs::write(root.join("a.txt"), "a").unwrap();
    let s = scope(&root, &[]);
    for args in [
        json!({"path": "a.txt"}),
        json!({"path": "./sub/../a.txt"}),
        json!({"path": root.join("a.txt").to_str().unwrap()}),
        json!({"path": "new/file.txt"}),
    ] {
        let located = s.locate("read", &args).unwrap();
        assert_eq!(located.root, 0, "{args}");
    }
}

#[test]
fn traversal_and_absolute_paths_outside_the_root_are_out_of_scope() {
    let d = Dirs::new();
    let root = d.dir("work");
    d.dir("other");
    let s = scope(&root, &[]);
    for path in [
        "../other/x",
        "../../etc/passwd",
        "sub/../../other/x",
        "/etc/passwd",
        d.base.join("other").to_str().unwrap(),
    ] {
        assert!(
            matches!(
                s.locate("read", &json!({ "path": path })),
                Err(ScopeError::OutOfScope(_))
            ),
            "{path}"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_symlink_that_leaves_the_root_is_out_of_scope() {
    let d = Dirs::new();
    let root = d.dir("work");
    let outside = d.dir("outside");
    std::fs::write(outside.join("secret.txt"), "s").unwrap();
    link(&outside, &root.join("escape"));
    link(&outside.join("secret.txt"), &root.join("file-link"));
    let s = scope(&root, &[]);
    for path in ["escape", "escape/secret.txt", "file-link", "escape/new.txt"] {
        assert!(
            s.locate("read", &json!({ "path": path })).is_err(),
            "{path}"
        );
    }
}

#[test]
fn unreadable_arguments_are_out_of_scope() {
    let d = Dirs::new();
    let s = scope(&d.dir("work"), &[]);
    for (tool, args) in [
        ("read", json!({})),
        ("read", json!({"path": 3})),
        ("read", json!({"path": ["a"]})),
        ("read", json!("a.txt")),
        ("glob", json!({"path": "."})),
        ("mystery", json!({"path": "a"})),
        ("webfetch", json!({"url": "https://example.com"})),
        ("websearch", json!({"query": "rust 2024 edition"})),
    ] {
        assert!(
            matches!(s.locate(tool, &args), Err(ScopeError::Unreadable(_))),
            "{tool} {args}"
        );
    }
}

#[test]
fn optional_paths_default_to_the_primary_root() {
    let d = Dirs::new();
    let s = scope(&d.dir("work"), &[]);
    for tool in ["ls", "grep", "shell"] {
        let located = s.locate(tool, &json!({})).unwrap();
        assert_eq!(located.root, 0, "{tool}");
        assert!(s.locate(tool, &json!(null)).is_ok(), "{tool}");
    }
}

#[test]
fn a_shell_cwd_is_resolved_like_any_other_path() {
    let d = Dirs::new();
    let root = d.dir("work");
    d.dir("other");
    let s = scope(&root, &[]);
    assert!(
        s.locate("shell", &json!({"command": "ls", "cwd": "."}))
            .is_ok()
    );
    assert!(
        s.locate("shell", &json!({"command": "ls", "cwd": "sub/.."}))
            .is_ok()
    );
    for cwd in ["..", "../other", "/", "/tmp"] {
        assert!(
            matches!(
                s.locate("shell", &json!({"command": "ls", "cwd": cwd})),
                Err(ScopeError::OutOfScope(_))
            ),
            "{cwd}"
        );
    }
}

#[test]
fn glob_patterns_are_anchored_at_their_literal_directory() {
    let d = Dirs::new();
    let root = d.dir("work");
    let extra = d.dir("extra");
    let s = scope(&root, &[&extra]);
    assert_eq!(
        s.locate("glob", &json!({"pattern": "**/*.rs"}))
            .unwrap()
            .root,
        0
    );
    assert_eq!(
        s.locate("glob", &json!({"pattern": "src/*.rs"}))
            .unwrap()
            .root,
        0
    );
    let absolute = format!("{}/**/*.rs", extra.display());
    let located = s.locate("glob", &json!({ "pattern": absolute })).unwrap();
    assert_eq!(located.root, 1);
    assert_eq!(located.args["pattern"], "**/*.rs");
    assert_eq!(located.args["path"], extra.to_str().unwrap());
    for pattern in ["../other/*.rs", "/etc/*.conf", "/*.conf"] {
        assert!(
            s.locate("glob", &json!({ "pattern": pattern })).is_err(),
            "{pattern}"
        );
    }
    // The path argument is itself checked, and the pattern's directory is joined under it.
    assert!(
        s.locate("glob", &json!({"pattern": "*.rs", "path": "../x"}))
            .is_err()
    );
    assert!(
        s.locate("glob", &json!({"pattern": "../*.rs", "path": "sub"}))
            .is_ok()
    );
}

#[test]
fn a_path_in_an_added_root_reaches_that_root() {
    let d = Dirs::new();
    let root = d.dir("work");
    let extra = d.dir("extra");
    let s = scope(&root, &[&extra]);
    assert_eq!(s.roots(), [root.clone(), extra.clone()]);

    let file = extra.join("lib.rs");
    let located = s
        .locate("read", &json!({"path": file.to_str().unwrap()}))
        .unwrap();
    assert_eq!(located.root, 1);

    // A relative path that climbs into the added root is read as the absolute path it means,
    // because the added root's instance would read the relative path against itself.
    let sibling = format!("../{}/lib.rs", extra.file_name().unwrap().to_str().unwrap());
    let located = s.locate("read", &json!({ "path": sibling })).unwrap();
    assert_eq!(located.root, 1);
    assert_eq!(located.args["path"], file.to_str().unwrap());

    // A path in the primary root is left as the model wrote it.
    let located = s.locate("read", &json!({"path": "a.txt"})).unwrap();
    assert_eq!(located.args, json!({"path": "a.txt"}));
}

#[cfg(unix)]
#[test]
fn a_symlink_from_one_root_into_another_stays_in_scope() {
    let d = Dirs::new();
    let root = d.dir("work");
    let extra = d.dir("extra");
    let lonely = d.dir("lonely");
    std::fs::write(extra.join("lib.rs"), "x").unwrap();
    link(&extra, &root.join("shared"));
    link(&lonely, &root.join("elsewhere"));
    let s = scope(&root, &[&extra]);
    let located = s.locate("read", &json!({"path": "shared/lib.rs"})).unwrap();
    assert_eq!(located.root, 1);
    assert!(s.locate("read", &json!({"path": "elsewhere/x"})).is_err());
}

#[test]
fn duplicate_roots_collapse_and_the_primary_stays_first() {
    let d = Dirs::new();
    let root = d.dir("work");
    let extra = d.dir("extra");
    let s = Scope::new(root.clone(), [extra.clone(), root.clone(), extra.clone()]);
    assert_eq!(s.roots(), [root.clone(), extra]);
    assert_eq!(s.primary(), root);
}

#[tokio::test]
async fn every_tool_reaches_the_instance_of_the_root_that_holds_its_path() {
    let d = Dirs::new();
    let root = d.dir("work");
    let extra = d.dir("extra");
    std::fs::write(extra.join("lib.rs"), "fn added() {}\n").unwrap();
    std::fs::write(root.join("main.rs"), "fn primary() {}\n").unwrap();
    let s = scope(&root, &[&extra]);
    let gate = Arc::new(AllowAll::default());
    let tools = registry(&env_with(s.clone(), gate));

    let read = call(
        &tools,
        "read",
        json!({"path": extra.join("lib.rs").to_str().unwrap()}),
    )
    .await
    .unwrap();
    assert!(text(&read).contains("fn added"));
    let read = call(&tools, "read", json!({"path": "main.rs"}))
        .await
        .unwrap();
    assert!(text(&read).contains("fn primary"));

    let listed = call(&tools, "ls", json!({"path": extra.to_str().unwrap()}))
        .await
        .unwrap();
    assert!(text(&listed).contains("lib.rs"));
    let found = call(
        &tools,
        "grep",
        json!({"pattern": "added", "path": extra.to_str().unwrap()}),
    )
    .await
    .unwrap();
    assert!(text(&found).contains("lib.rs"));
    let pattern = format!("{}/*.rs", extra.display());
    let globbed = call(&tools, "glob", json!({ "pattern": pattern }))
        .await
        .unwrap();
    assert!(text(&globbed).contains("lib.rs"));

    let target = extra.join("written.txt");
    call(
        &tools,
        "write",
        json!({"path": target.to_str().unwrap(), "content": "hi"}),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "hi");
    call(&tools, "read", json!({"path": target.to_str().unwrap()}))
        .await
        .unwrap();
    call(
        &tools,
        "edit",
        json!({"path": target.to_str().unwrap(), "edits": [{"old_text": "hi", "new_text": "bye"}]}),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "bye");
}

#[cfg(unix)]
#[tokio::test]
async fn a_shell_in_an_added_root_runs_there_and_one_outside_every_root_does_not_run() {
    let d = Dirs::new();
    let root = d.dir("work");
    let extra = d.dir("extra");
    let outside = d.dir("outside");
    let s = scope(&root, &[&extra]);
    let tools = registry(&env_with(s, Arc::new(AllowAll::default())));

    let here = call(&tools, "shell", json!({"command": "pwd -P"}))
        .await
        .unwrap();
    assert!(
        text(&here).contains(root.to_str().unwrap()),
        "{}",
        text(&here)
    );
    let there = call(
        &tools,
        "shell",
        json!({"command": "pwd -P", "cwd": extra.to_str().unwrap()}),
    )
    .await
    .unwrap();
    assert!(
        text(&there).contains(extra.to_str().unwrap()),
        "{}",
        text(&there)
    );

    let marker = outside.join("ran");
    let refused = call(
        &tools,
        "shell",
        json!({"command": "touch ran", "cwd": outside.to_str().unwrap()}),
    )
    .await;
    assert!(refused.is_err());
    assert!(!marker.exists());
}

#[tokio::test]
async fn no_registered_tool_can_add_a_root() {
    let d = Dirs::new();
    let root = d.dir("work");
    let extra = d.dir("extra");
    let s = scope(&root, &[]);
    let tools = registry(&env_with(s.clone(), Arc::new(AllowAll::default())));
    assert!(
        tools
            .iter()
            .all(|t| !t.name.contains("dir") && !t.name.contains("root"))
    );

    // Reaching for another directory, by any tool, leaves the roots as they were.
    let outside = extra.to_str().unwrap();
    let _ = call(&tools, "ls", json!({"path": outside})).await;
    let _ = call(&tools, "shell", json!({"command": "ls", "cwd": outside})).await;
    assert_eq!(s.roots(), [root]);
}

#[cfg(windows)]
mod windows {
    use super::*;

    fn plain(path: &std::path::Path) -> String {
        path.to_string_lossy()
            .trim_start_matches(r"\\?\")
            .to_string()
    }

    #[test]
    fn drive_letter_paths_are_judged_by_the_root_that_holds_them() {
        let d = Dirs::new();
        let root = d.dir("work");
        let other = d.dir("other");
        let s = scope(&root, &[]);
        let inside = plain(&root.join("a.txt"));
        assert!(s.locate("read", &json!({ "path": inside })).is_ok());
        let outside = plain(&other.join("a.txt"));
        assert!(s.locate("read", &json!({ "path": outside })).is_err());
    }

    #[test]
    fn comparison_ignores_case() {
        let d = Dirs::new();
        let root = d.dir("work");
        let s = scope(&root, &[]);
        let shouted = plain(&root.join("A.TXT")).to_uppercase();
        assert!(s.locate("read", &json!({ "path": shouted })).is_ok());
    }

    #[test]
    fn unc_paths_outside_the_roots_are_out_of_scope() {
        let d = Dirs::new();
        let s = scope(&d.dir("work"), &[]);
        for path in [r"\\server\share\x.txt", "//server/share/x.txt"] {
            assert!(
                s.locate("read", &json!({ "path": path })).is_err(),
                "{path}"
            );
        }
    }

    #[test]
    fn msys_style_paths_translate_and_untranslatable_ones_are_out_of_scope() {
        let d = Dirs::new();
        let root = d.dir("work");
        let s = scope(&root, &[]);
        let plain_root = plain(&root);
        let (drive, rest) = plain_root.split_once(':').unwrap();
        let msys = format!(
            "/{}{}/a.txt",
            drive.to_ascii_lowercase(),
            rest.replace('\\', "/")
        );
        let located = s.locate("read", &json!({ "path": msys })).unwrap();
        assert_eq!(located.root, 0);
        assert!(located.args["path"].as_str().unwrap().contains(':'));
        for path in ["/etc/passwd", "/usr/bin/sh", "/tmp/x"] {
            assert!(
                s.locate("read", &json!({ "path": path })).is_err(),
                "{path}"
            );
        }
    }

    #[test]
    fn a_junction_that_leaves_the_root_is_out_of_scope() {
        let d = Dirs::new();
        let root = d.dir("work");
        let outside = d.dir("outside");
        let junction = root.join("escape");
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&outside)
            .output()
            .unwrap();
        assert!(made.status.success(), "{made:?}");
        let s = scope(&root, &[]);
        assert!(
            s.locate("read", &json!({"path": "escape/secret.txt"}))
                .is_err()
        );
    }

    #[test]
    fn a_symlink_that_leaves_the_root_is_out_of_scope_when_symlinks_are_allowed() {
        let d = Dirs::new();
        let root = d.dir("work");
        let outside = d.dir("outside");
        if std::os::windows::fs::symlink_dir(&outside, root.join("escape")).is_err() {
            // Creating symlinks needs a privilege this account may lack.
            return;
        }
        let s = scope(&root, &[]);
        assert!(
            s.locate("read", &json!({"path": "escape/secret.txt"}))
                .is_err()
        );
    }
}
