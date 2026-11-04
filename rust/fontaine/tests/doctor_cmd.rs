mod common;

use common::{Output, Sandbox};
use serde_json::Value;

fn example(name: &str) -> String {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap().join("examples");
    let file = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let path = dir.join(file);
    assert!(path.exists(), "{} is not built", path.display());
    path.to_string_lossy().into_owned()
}

/// A sandbox whose one account has a static model list, so nothing is contacted.
fn sandbox() -> Sandbox {
    Sandbox::compatible("http://127.0.0.1:9")
}

fn report(out: &Output) -> Value {
    serde_json::from_str(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", out.stdout))
}

fn findings<'a>(report: &'a Value, title: &str) -> &'a Vec<Value> {
    report["sections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["title"] == title)
        .unwrap_or_else(|| panic!("no section {title}"))["findings"]
        .as_array()
        .unwrap()
}

fn finding<'a>(report: &'a Value, title: &str, subject: &str) -> &'a Value {
    findings(report, title)
        .iter()
        .find(|f| f["subject"] == subject)
        .unwrap_or_else(|| panic!("no finding {subject} in {title}"))
}

#[tokio::test]
async fn a_clean_setup_exits_0_with_valid_json_and_a_ready_guardian() {
    let s = sandbox();
    let out = s.run(&["doctor", "--format=json"]).await;
    assert_eq!(out.code, Some(0), "{}\n{}", out.stdout, out.stderr);
    let report = report(&out);
    assert_eq!(report["failed"], false);
    assert_eq!(finding(&report, "Trust", "workspace")["level"], "warn");
    let titles: Vec<&str> = report["sections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["title"].as_str().unwrap())
        .collect();
    assert_eq!(
        titles,
        [
            "Install",
            "Config layers",
            "Effective settings",
            "Trust",
            "Guardian and models",
            "MCP servers",
            "Instructions, hooks, skills",
            "Memory"
        ]
    );
    let guardian = finding(&report, "Guardian and models", "guardian model");
    assert_eq!(guardian["level"], "ok");
    assert!(guardian["detail"].as_str().unwrap().contains("ready"));
    let model = finding(&report, "Effective settings", "model.model");
    assert!(
        model["detail"]
            .as_str()
            .unwrap()
            .contains("mock/scripted (user)")
    );
}

#[tokio::test]
async fn a_project_key_the_scope_refuses_exits_1_and_is_named() {
    let s = sandbox();
    s.add_file(
        ".fontaine/settings.toml",
        "[permissions]\ndefault_mode = \"auto\"\n",
    );
    let out = s.run(&["doctor"]).await;
    assert_eq!(out.code, Some(1), "{}\n{}", out.stdout, out.stderr);
    assert!(
        out.stdout.contains("[fail] project:") && out.stdout.contains("permissions.default_mode"),
        "{}",
        out.stdout
    );
    // The other layers and the sections that do not need the configuration still report.
    assert!(out.stdout.contains("[ok] user:"), "{}", out.stdout);
    assert!(
        out.stdout.contains("skipped: config did not load"),
        "{}",
        out.stdout
    );
    assert!(out.stdout.contains("Memory"), "{}", out.stdout);
}

#[tokio::test]
async fn all_lists_the_settings_nothing_set() {
    let s = sandbox();
    let few = report(&s.run(&["doctor", "--format=json"]).await);
    let all = report(&s.run(&["doctor", "--all", "--format=json"]).await);
    assert!(
        findings(&all, "Effective settings").len() > findings(&few, "Effective settings").len()
    );
}

#[tokio::test]
async fn a_user_mcp_server_is_started_and_its_tools_counted() {
    let s = sandbox();
    let mut config = std::fs::read_to_string(s.config.join("config.toml")).unwrap();
    config.push_str(&format!(
        "\n[mcp.servers.repo]\ncommand = '{}'\n",
        example("repo_mcp_server")
    ));
    s.user_config(&config);
    let out = s.run(&["doctor", "--format=json"]).await;
    assert_eq!(out.code, Some(0), "{}\n{}", out.stdout, out.stderr);
    let parsed = report(&out);
    let row = finding(&parsed, "MCP servers", "repo");
    assert_eq!(row["level"], "ok");
    assert!(
        row["detail"]
            .as_str()
            .unwrap()
            .contains("connected (3 tools)"),
        "{row}"
    );

    let out = s.run(&["doctor", "--connect=no", "--format=json"]).await;
    let parsed = report(&out);
    let row = finding(&parsed, "MCP servers", "repo");
    assert!(
        row["detail"]
            .as_str()
            .unwrap()
            .contains("not started: probe skipped")
    );
}

#[tokio::test]
async fn a_repository_server_is_not_started_in_an_untrusted_workspace() {
    let s = sandbox();
    s.add_file(
        ".fontaine/settings.toml",
        &format!(
            "[mcp.servers.repo]\ncommand = '{}'\n",
            example("repo_mcp_server")
        ),
    );
    let out = s.run(&["doctor", "--format=json"]).await;
    assert_eq!(out.code, Some(0), "{}\n{}", out.stdout, out.stderr);
    let parsed = report(&out);
    let row = finding(&parsed, "MCP servers", "repo");
    assert!(
        row["detail"]
            .as_str()
            .unwrap()
            .contains("not started: the workspace is not trusted"),
        "{row}"
    );
}

#[tokio::test]
async fn a_memory_index_pointing_at_a_missing_file_fails() {
    let s = sandbox();
    let dir = fontaine_core::memory::dir(&s.data, &s.work);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("MEMORY.md"), "- [gone](gone.md)\n").unwrap();
    std::fs::write(dir.join("loose.md"), "x").unwrap();
    let out = s.run(&["doctor"]).await;
    assert_eq!(out.code, Some(1), "{}", out.stdout);
    assert!(
        out.stdout.contains("line 1 links to gone.md"),
        "{}",
        out.stdout
    );
    assert!(out.stdout.contains("[warn] loose.md"), "{}", out.stdout);
}
