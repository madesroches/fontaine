//! The shell working directory that persists between calls.
#![cfg(unix)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::{AllowAll, Dirs, as_root, call, env_with, registry, scope, text, tool};
use fontaine_tools::builtins::BuiltinEnv;
use fontaine_tools::gate::AgentId;
use fontaine_tools::jobs::NoPromote;
use serde_json::{Value, json};

struct Rig {
    dirs: Dirs,
    root: PathBuf,
    scratch: PathBuf,
    gate: Arc<AllowAll>,
    env: BuiltinEnv,
}

fn rig_with(scratch: bool) -> Rig {
    let dirs = Dirs::new();
    let root = dirs.dir("work");
    std::fs::create_dir_all(root.join("sub/deeper")).unwrap();
    let scratch_dir = dirs.dir("scratch");
    let gate = Arc::new(AllowAll::default());
    let mut env = env_with(scope(&root, &[&scratch_dir]), gate.clone());
    env.scratch = scratch.then(|| scratch_dir.clone());
    Rig {
        dirs,
        root,
        scratch: scratch_dir,
        gate,
        env,
    }
}

fn rig() -> Rig {
    rig_with(true)
}

async fn shell(tools: &[fontaine_tools::builtins::Registered], args: Value) -> String {
    text(&call(tools, "shell", args).await.unwrap())
}

async fn pwd(tools: &[fontaine_tools::builtins::Registered]) -> String {
    shell(tools, json!({"command": "pwd"}))
        .await
        .lines()
        .next()
        .unwrap()
        .to_string()
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[tokio::test]
async fn a_cd_carries_over_to_the_next_call() {
    let r = rig();
    let tools = registry(&r.env);
    assert_eq!(pwd(&tools).await, path_text(&r.root));
    shell(&tools, json!({"command": "cd sub"})).await;
    assert_eq!(pwd(&tools).await, path_text(&r.root.join("sub")));
    // A relative cd continues from there.
    shell(&tools, json!({"command": "cd deeper"})).await;
    assert_eq!(pwd(&tools).await, path_text(&r.root.join("sub/deeper")));
    // An explicit cwd starts the call elsewhere, and where it ends is tracked.
    shell(&tools, json!({"command": "true", "cwd": "sub"})).await;
    assert_eq!(pwd(&tools).await, path_text(&r.root.join("sub")));
    // Nothing is left behind in the scratch directory.
    assert_eq!(std::fs::read_dir(&r.scratch).unwrap().count(), 0);
}

#[tokio::test]
async fn leaving_the_roots_resets_and_says_so() {
    let r = rig();
    let tools = registry(&r.env);
    shell(&tools, json!({"command": "cd sub"})).await;
    let out = shell(&tools, json!({"command": "cd /"})).await;
    let expected = format!(
        "[shell working directory reset to {}: / is outside the workspace]",
        r.root.display()
    );
    assert!(out.contains(&expected), "{out}");
    assert_eq!(pwd(&tools).await, path_text(&r.root));
}

#[tokio::test]
async fn the_exit_code_is_kept_and_a_command_that_never_reaches_the_trailer_leaves_the_directory() {
    let r = rig();
    let tools = registry(&r.env);
    let out = shell(&tools, json!({"command": "cd sub; false"})).await;
    assert!(out.contains("[exit code 1]"), "{out}");
    assert_eq!(pwd(&tools).await, path_text(&r.root.join("sub")));

    for (command, code) in [
        ("cd deeper; exit 3", "[exit code 3]"),
        ("cd deeper; exec true", "[exit code 0]"),
        (
            "set -e; cd deeper; false; echo unreachable",
            "[exit code 1]",
        ),
    ] {
        let out = shell(&tools, json!({"command": command})).await;
        assert!(out.contains(code), "{command}: {out}");
        assert!(!out.contains("unreachable"), "{out}");
        assert_eq!(
            pwd(&tools).await,
            path_text(&r.root.join("sub")),
            "{command}"
        );
    }
}

#[tokio::test]
async fn quotes_newlines_and_heredocs_reach_the_shell_intact() {
    let r = rig();
    let tools = registry(&r.env);
    let command = "printf '%s\\n' \"a 'b' \\\"c\\\"\"\ncat <<'EOT'\nline $one\n  two\nEOT\n# trailing comment";
    let out = shell(&tools, json!({"command": command})).await;
    assert!(out.contains("a 'b' \"c\"\nline $one\n  two"), "{out}");
}

#[tokio::test]
async fn positional_parameters_behave_as_under_sh_c() {
    let r = rig();
    let tools = registry(&r.env);
    let out = shell(
        &tools,
        json!({"command": "echo \"n=$#\"; set -- a b; echo \"n=$# first=$1\"; shift; echo \"n=$# first=$1\""}),
    )
    .await;
    assert!(out.contains("n=0\nn=2 first=a\nn=1 first=b"), "{out}");
}

#[tokio::test]
async fn assigning_the_wrappers_variable_names_does_not_disturb_the_trailer() {
    let r = rig();
    std::fs::write(r.root.join("sub/one.txt"), "").unwrap();
    let tools = registry(&r.env);
    let out = shell(
        &tools,
        json!({"command": "cd sub; f=/dev/null; c=1; rc=2; for f in *.txt; do echo $f; done"}),
    )
    .await;
    assert!(out.contains("one.txt"), "{out}");
    assert_eq!(pwd(&tools).await, path_text(&r.root.join("sub")));
    // The loop variable did not turn into the trailer's output file.
    assert_eq!(std::fs::read_dir(&r.scratch).unwrap().count(), 0);
    assert_eq!(
        std::fs::read_to_string(r.root.join("sub/one.txt")).unwrap(),
        ""
    );
}

#[tokio::test]
async fn without_a_scratch_directory_the_directory_never_moves() {
    let r = rig_with(false);
    let tools = registry(&r.env);
    shell(&tools, json!({"command": "cd sub"})).await;
    assert_eq!(pwd(&tools).await, path_text(&r.root));
}

#[tokio::test]
async fn a_trailer_that_cannot_write_leaves_the_directory() {
    let r = rig();
    let tools = registry(&r.env);
    let gone = r.scratch.display();
    shell(
        &tools,
        json!({"command": format!("cd sub; rm -rf '{gone}'")}),
    )
    .await;
    assert_eq!(pwd(&tools).await, path_text(&r.root));
}

#[tokio::test]
async fn background_calls_and_calls_moved_to_the_background_do_not_move_the_directory() {
    let r = rig();
    let tools = registry(&r.env);
    let out = shell(
        &tools,
        json!({"command": "cd sub; sleep 0.3", "run_in_background": true}),
    )
    .await;
    assert!(out.contains("started in the background"), "{out}");
    let out = shell(
        &tools,
        json!({"command": "cd deeper 2>/dev/null; cd sub; sleep 2", "timeout_secs": 1}),
    )
    .await;
    assert!(out.contains("moved to background job"), "{out}");
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(pwd(&tools).await, path_text(&r.root));
}

#[tokio::test]
async fn a_subagent_has_a_directory_of_its_own() {
    let r = rig();
    let tools = registry(&r.env);
    let as_child = || {
        let mut context = as_root();
        context.insert(AgentId("child-1".to_string())).unwrap();
        context
    };
    let run = |command: &'static str| {
        let tools = &tools;
        let mut context = as_child();
        async move {
            text(
                &tool(tools, "shell")
                    .tool
                    .run_with(&mut context, json!({"command": command}))
                    .await
                    .unwrap(),
            )
        }
    };
    shell(&tools, json!({"command": "cd sub/deeper"})).await;
    // It starts at the primary root, not where the parent is.
    assert!(run("pwd").await.starts_with(&path_text(&r.root)));
    assert!(!run("pwd").await.contains("deeper"));
    run("cd sub").await;
    assert!(
        run("pwd")
            .await
            .starts_with(&path_text(&r.root.join("sub")))
    );
    // The parent did not move.
    assert_eq!(pwd(&tools).await, path_text(&r.root.join("sub/deeper")));
    // Background jobs stay with the main agent.
    let refused = tool(&tools, "shell")
        .tool
        .run_with(
            &mut as_child(),
            json!({"command": "true", "run_in_background": true}),
        )
        .await;
    assert!(refused.is_err());
}

#[tokio::test]
async fn a_harness_call_that_splices_its_output_does_not_move_the_directory() {
    let r = rig();
    let tools = registry(&r.env);
    let mut context = as_root();
    context.insert(NoPromote(true)).unwrap();
    tool(&tools, "shell")
        .tool
        .run_with(&mut context, json!({"command": "cd sub"}))
        .await
        .unwrap();
    assert_eq!(pwd(&tools).await, path_text(&r.root));
}

#[tokio::test]
async fn the_gate_sees_the_directory_the_command_will_run_in() {
    let r = rig();
    let tools = registry(&r.env);
    let sub = path_text(&r.root.join("sub"));
    shell(&tools, json!({"command": "cd sub"})).await;
    for args in [
        json!({"command": "cat .env"}),
        json!({"command": "cat .env", "cwd": null}),
    ] {
        shell(&tools, args).await;
        let seen = r.gate.seen.lock().unwrap();
        let (_, last) = seen.last().unwrap();
        assert_eq!(last, &json!({"command": "cat .env", "cwd": sub}));
    }
    // A named directory is left as the model wrote it.
    shell(&tools, json!({"command": "pwd", "cwd": "sub/deeper"})).await;
    let seen = r.gate.seen.lock().unwrap();
    assert_eq!(seen.last().unwrap().1["cwd"], "sub/deeper");
}

#[tokio::test]
async fn an_untouched_session_passes_the_arguments_through() {
    let r = rig();
    let tools = registry(&r.env);
    shell(&tools, json!({"command": "pwd"})).await;
    assert_eq!(
        r.gate.seen.lock().unwrap().last().unwrap().1,
        json!({"command": "pwd"})
    );
}

#[tokio::test]
async fn a_tracked_directory_deleted_between_calls_resets_with_a_note() {
    let r = rig();
    let tools = registry(&r.env);
    shell(&tools, json!({"command": "cd sub/deeper"})).await;
    std::fs::remove_dir(r.root.join("sub/deeper")).unwrap();
    let out = shell(&tools, json!({"command": "pwd"})).await;
    assert!(out.starts_with(&path_text(&r.root)), "{out}");
    assert!(out.contains("is not a directory]"), "{out}");
    assert!(!out.contains("No such file"), "{out}");
    assert_eq!(pwd(&tools).await, path_text(&r.root));
    drop(r.dirs);
}

#[tokio::test]
async fn the_monitor_starts_in_the_primary_root() {
    let r = rig();
    let tools = registry(&r.env);
    shell(&tools, json!({"command": "cd sub"})).await;
    let definition = tool(&tools, "monitor").tool.definition();
    assert!(definition.description.contains("primary workspace root"));
    // The monitor's command runs where the tracked directory is not consulted.
    let started = text(
        &call(
            &tools,
            "monitor",
            json!({"command": "pwd", "description": "d"}),
        )
        .await
        .unwrap(),
    );
    assert!(started.contains("monitor"), "{started}");
}
