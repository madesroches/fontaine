//! `fontaine -p` on a Windows machine where Git for Windows cannot be found: it starts, and
//! PowerShell is the command shell. The CI image has Git installed, so the run hides it.
#![cfg(windows)]

mod common;

use common::{Endpoint, Sandbox, Turn, Wire};
use serde_json::{Value, json};

/// A sandbox whose environment has no Git: `PATH` holds only the system directory, and the
/// install locations discovery searches, scoop's roots included, point at an empty directory.
fn without_git(config: &str) -> Sandbox {
    let mut s = Sandbox::new();
    s.user_config(config);
    let empty = s.runtime.join("no-git");
    std::fs::create_dir_all(&empty).unwrap();
    let system_root = std::env::var("SystemRoot").expect("Windows sets SystemRoot");
    s.set_env("PATH", &format!(r"{system_root}\System32"));
    // Windows derives a 64-bit process's `ProgramFiles` from `ProgramW6432` and ignores a
    // `ProgramFiles` set on its own, so the real Program Files stays visible unless both move.
    for var in [
        "ProgramFiles",
        "ProgramW6432",
        "ProgramFiles(x86)",
        "LocalAppData",
        "SCOOP",
        "SCOOP_GLOBAL",
        "USERPROFILE",
        "ProgramData",
    ] {
        s.set_env(var, &empty.to_string_lossy());
    }
    s.remove_env("FONTAINE_GIT_BASH_PATH");
    s.remove_env("FONTAINE_POWERSHELL_PATH");
    s
}

fn accounts(agent: &Endpoint, judge: &Endpoint) -> String {
    format!(
        "[accounts.agent]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\nmodels = [\"scripted\"]\n\n[accounts.judge]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\nmodels = [\"scripted\"]\n\n[model]\nmodel = \"agent/scripted\"\n\n[guardian]\nmodel = \"judge/scripted\"\n",
        agent.url(),
        judge.url()
    )
}

fn tool_results(lines: &[Value]) -> Vec<&Value> {
    lines
        .iter()
        .filter(|l| l["type"] == "tool_result")
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn it_starts_without_git_for_windows_and_runs_powershell() {
    let agent = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::tool(
                "c1",
                "powershell",
                json!({"command": "Write-Output 'from-powershell'"}),
            ),
            Turn::text("done"),
        ],
    )
    .await;
    let judge = Endpoint::start(
        Wire::Chat,
        vec![Turn::tool(
            "g1",
            "submit",
            json!({
                "decision": "allow",
                "rule": "allow",
                "explanation": "allow because of allow",
                "confidence": 0.95,
            }),
        )],
    )
    .await;
    let s = without_git(&accounts(&agent, &judge));
    let out = s
        .run(&[
            "-p",
            "--permission-mode",
            "auto",
            "--output-format",
            "stream-json",
            "run it",
        ])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert!(
        !out.stderr.contains("Git for Windows was not found"),
        "{}",
        out.stderr
    );
    let lines = out.lines();
    let results = tool_results(&lines);
    assert_eq!(results[0]["success"], json!(true), "{}", results[0]);
    assert!(
        results[0]["text"]
            .as_str()
            .unwrap()
            .contains("from-powershell"),
        "{}",
        results[0]
    );
    // The model is offered `powershell` and no `shell`.
    let first = &endpoint_tools(&agent).await;
    assert!(first.contains(&"powershell".to_string()), "{first:?}");
    assert!(!first.contains(&"shell".to_string()), "{first:?}");
    assert!(!first.contains(&"monitor".to_string()), "{first:?}");
}

async fn endpoint_tools(endpoint: &Endpoint) -> Vec<String> {
    endpoint.requests().await[0]["tools"]
        .as_array()
        .expect("the request carries tools")
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pre_tool_use_hook_that_exits_one_denies_the_call() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::tool("c1", "read", json!({"path": "a.txt"})),
            Turn::text("done"),
        ],
    )
    .await;
    let s = without_git(&format!(
        "[accounts.mock]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\nmodels = [\"scripted\"]\n\n[model]\nmodel = \"mock/scripted\"\n\n[[hooks.pre_tool_use]]\ncommand = \"exit 1\"\n",
        endpoint.url()
    ));
    s.add_file("a.txt", "contents");
    let out = s
        .headless(&["--output-format", "stream-json", "read it"])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    let results = tool_results(&lines);
    assert_eq!(results[0]["success"], json!(false), "{}", results[0]);
    let text = results[0]["text"].as_str().unwrap();
    assert!(
        text.contains("blocking because Git Bash was not found, so only exit code 0 or 2 counts as this hook's verdict"),
        "{text}"
    );
    assert!(!text.contains("contents"));
}

#[tokio::test(flavor = "multi_thread")]
async fn with_neither_git_bash_nor_powershell_startup_names_what_to_install_or_set() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("done")]).await;
    let mut s = without_git(&format!(
        "[accounts.mock]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\nmodels = [\"scripted\"]\n\n[model]\nmodel = \"mock/scripted\"\n",
        endpoint.url()
    ));
    s.set_env(
        "FONTAINE_POWERSHELL_PATH",
        r"C:\definitely\not\here\pwsh.exe",
    );
    let out = s.headless(&["hello"]).await;
    assert_eq!(out.code, Some(2), "{}", out.stderr);
    assert!(
        out.stderr.contains(
            r"no shell found: install Git for Windows, or set FONTAINE_GIT_BASH_PATH; FONTAINE_POWERSHELL_PATH=C:\definitely\not\here\pwsh.exe is not a file"
        ),
        "{}",
        out.stderr
    );
}
