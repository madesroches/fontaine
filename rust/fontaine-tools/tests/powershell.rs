//! The `powershell` tool on Windows runs the command exactly as written.
#![cfg(windows)]

mod common;

use std::sync::Arc;

use common::{AllowAll, Dirs, call, env_with, registry, scope, text};
use fontaine_tools::builtins::Registered;
use fontaine_tools::powershell::{Probe, discover};
use serde_json::json;

fn tools(d: &Dirs) -> Vec<Registered> {
    let env = |name: &str| std::env::var_os(name);
    let powershell = discover(&Probe {
        env: &env,
        is_file: &|path| path.is_file(),
    })
    .expect("Windows PowerShell is installed on every Windows image");
    let mut builtin_env = env_with(scope(&d.dir("work"), &[]), Arc::new(AllowAll::default()));
    builtin_env.shell.powershell = Some(powershell);
    registry(&builtin_env)
}

#[tokio::test]
async fn output_comes_back_as_utf8() {
    let d = Dirs::new();
    let out = call(
        &tools(&d),
        "powershell",
        json!({"command": "Write-Output 'é€ü'"}),
    )
    .await
    .unwrap();
    assert!(text(&out).contains("é€ü"), "{}", text(&out));
}

#[tokio::test]
async fn the_command_runs_in_the_working_directory() {
    let d = Dirs::new();
    let work = d.dir("work");
    let sub = d.dir("work/sub");
    std::fs::write(sub.join("marker.txt"), "x").unwrap();
    let tools = tools(&d);
    let out = call(
        &tools,
        "powershell",
        json!({"command": "(Get-Location).Path"}),
    )
    .await
    .unwrap();
    let shown = text(&out).to_lowercase();
    let expected = std::fs::canonicalize(&work).unwrap();
    let expected = expected.to_string_lossy().to_lowercase();
    assert!(
        shown.contains(expected.trim_start_matches(r"\\?\")),
        "{shown}"
    );
    let out = call(
        &tools,
        "powershell",
        json!({"command": "Get-ChildItem -Name", "cwd": "sub"}),
    )
    .await
    .unwrap();
    assert!(text(&out).contains("marker.txt"), "{}", text(&out));
}

#[tokio::test]
async fn a_leading_using_statement_runs() {
    let d = Dirs::new();
    let out = call(
        &tools(&d),
        "powershell",
        json!({"command": "using namespace System.IO\n[Path]::GetFileName('a\\b.txt')"}),
    )
    .await
    .unwrap();
    assert!(text(&out).contains("b.txt"), "{}", text(&out));
    assert!(!text(&out).contains("ParserError"), "{}", text(&out));
}

#[tokio::test]
async fn a_literal_exit_code_is_kept() {
    let d = Dirs::new();
    let out = call(&tools(&d), "powershell", json!({"command": "exit 7"}))
        .await
        .unwrap();
    assert!(text(&out).contains("exit code 7"), "{}", text(&out));
}

#[tokio::test]
async fn quotes_and_backslashes_reach_powershell_unchanged() {
    let d = Dirs::new();
    let command = r#"Write-Output 'it''s' "say ""hi""" '\\server\share' 'tail\\' 'C:\x y\z'"#;
    let out = call(&tools(&d), "powershell", json!({ "command": command }))
        .await
        .unwrap();
    let shown = text(&out);
    let lines: Vec<&str> = shown.lines().collect();
    for expected in [
        "it's",
        r#"say "hi""#,
        r"\\server\share",
        r"tail\\",
        r"C:\x y\z",
    ] {
        assert!(lines.contains(&expected), "{expected} in {shown}");
    }
}

#[tokio::test]
async fn a_subagent_waits_in_the_foreground_and_may_not_start_a_background_job() {
    use fontaine_tools::gate::AgentId;
    use rig_core::tool::ToolContext;

    let d = Dirs::new();
    let tools = tools(&d);
    let run = |args: serde_json::Value| {
        let tool = &common::tool(&tools, "powershell").tool;
        async move {
            let mut context = ToolContext::new();
            context
                .insert(AgentId("agent-1".to_string()))
                .expect("the agent id serializes");
            tool.run_with(&mut context, args).await
        }
    };
    let out = run(json!({"command": "Write-Output sub-ok"}))
        .await
        .unwrap();
    assert!(text(&out).contains("sub-ok"), "{}", text(&out));
    let refused = run(json!({"command": "Write-Output x", "run_in_background": true})).await;
    assert!(refused.is_err());
    let out = run(json!({"command": "Start-Sleep -Seconds 30", "timeout_secs": 1}))
        .await
        .unwrap();
    assert!(text(&out).contains("timed out after 1s"), "{}", text(&out));
}
