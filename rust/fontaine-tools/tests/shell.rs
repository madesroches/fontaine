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
