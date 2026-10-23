//! The `shell` tool on Windows launches through Git Bash.
#![cfg(windows)]

mod common;

use std::sync::Arc;

use common::{AllowAll, Dirs, call, env_with, registry, scope, text};
use fontaine_tools::git_bash::{Probe, discover};
use serde_json::json;

#[tokio::test]
async fn the_shell_runs_a_posix_command_through_git_bash() {
    let env = |name: &str| std::env::var_os(name);
    let git_bash = discover(&Probe {
        env: &env,
        is_file: &|path| path.is_file(),
    })
    .expect("Git for Windows is installed on the Windows CI image");

    let d = Dirs::new();
    let mut builtin_env = env_with(scope(&d.dir("work"), &[]), Arc::new(AllowAll::default()));
    builtin_env.shell.git_bash = Some(git_bash);
    let tools = registry(&builtin_env);
    let out = call(
        &tools,
        "shell",
        json!({"command": "echo \"hello $((1 + 2))\" | cat"}),
    )
    .await
    .unwrap();
    assert!(text(&out).contains("hello 3"), "{}", text(&out));
}

#[tokio::test]
async fn a_double_backslash_reaches_git_bash_as_written() {
    let env = |name: &str| std::env::var_os(name);
    let git_bash = discover(&Probe {
        env: &env,
        is_file: &|path| path.is_file(),
    })
    .expect("Git for Windows is installed on the Windows CI image");

    let d = Dirs::new();
    let mut builtin_env = env_with(scope(&d.dir("work"), &[]), Arc::new(AllowAll::default()));
    builtin_env.shell.git_bash = Some(git_bash);
    let tools = registry(&builtin_env);
    let out = call(
        &tools,
        "shell",
        json!({"command": r"printf '%s\n' '\\a' '\\server\share' 'tail\\' '\\?\C:\x'"}),
    )
    .await
    .unwrap();
    let shown = text(&out);
    let lines: Vec<&str> = shown.lines().collect();
    for expected in [r"\\a", r"\\server\share", r"tail\\", r"\\?\C:\x"] {
        assert!(lines.contains(&expected), "{expected} in {shown}");
    }
}
