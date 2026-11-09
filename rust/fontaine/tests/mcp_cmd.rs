//! `fontaine mcp` drives the binary against a temp config and data directory.

mod common;

use common::Sandbox;

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

fn sandbox() -> Sandbox {
    Sandbox::compatible("http://127.0.0.1:9")
}

fn trust_workspace(s: &Sandbox) {
    fontaine_core::trust::record(&s.data, &s.work).unwrap();
}

#[tokio::test]
async fn add_list_get_and_remove_work_in_each_scope() {
    let s = sandbox();
    trust_workspace(&s);
    let server = example("repo_mcp_server");
    for scope in ["local", "user"] {
        let name = format!("{scope}-srv");
        let out = s
            .run(&["mcp", "add", "-s", scope, &name, "--", &server])
            .await;
        assert_eq!(out.code, Some(0), "{}\n{}", out.stdout, out.stderr);
        assert!(
            out.stdout.contains(&format!(
                "Added stdio MCP server `{name}` ({server}) to {scope} config"
            )),
            "{}",
            out.stdout
        );
        assert!(out.stdout.contains("File modified: "), "{}", out.stdout);
    }
    // Project scope is a repository file the user's own session starts after approval.
    let out = s
        .run(&["mcp", "add", "-s", "project", "project-srv", "--", &server])
        .await;
    assert_eq!(out.code, Some(0), "{}\n{}", out.stdout, out.stderr);
    let json = std::fs::read_to_string(s.work.join(".mcp.json")).unwrap();
    assert!(json.contains("\"type\": \"stdio\""), "{json}");

    let out = s.run(&["mcp", "list"]).await;
    assert_eq!(out.code, Some(0), "{}\n{}", out.stdout, out.stderr);
    let lines: Vec<&str> = out.stdout.lines().collect();
    assert_eq!(lines.len(), 3, "{}", out.stdout);
    assert_eq!(
        lines[0],
        format!("local-srv: {server} - connected (4 tools)")
    );
    assert_eq!(
        lines[1],
        format!("project-srv: {server} - pending approval (run fontaine to approve)")
    );
    assert_eq!(
        lines[2],
        format!("user-srv: {server} - connected (4 tools)")
    );

    let out = s.run(&["mcp", "list", "--connect=no"]).await;
    assert!(out.stdout.contains("local-srv: ") && out.stdout.contains("- not checked"));

    let out = s.run(&["mcp", "get", "local-srv"]).await;
    assert_eq!(out.code, Some(0), "{}\n{}", out.stdout, out.stderr);
    assert!(out.stdout.contains("Scope: local ("), "{}", out.stdout);
    assert!(out.stdout.contains("Type: stdio"), "{}", out.stdout);
    assert!(
        out.stdout
            .contains("To remove this server, run: fontaine mcp remove local-srv -s local"),
        "{}",
        out.stdout
    );

    for (name, scope) in [
        ("local-srv", "local"),
        ("project-srv", "project"),
        ("user-srv", "user"),
    ] {
        let out = s.run(&["mcp", "remove", name]).await;
        assert_eq!(out.code, Some(0), "{scope}: {}\n{}", out.stdout, out.stderr);
        assert!(
            out.stdout
                .contains(&format!("Removed MCP server `{name}` from {scope} config")),
            "{}",
            out.stdout
        );
    }
    let out = s.run(&["mcp", "list"]).await;
    assert_eq!(
        out.stdout.trim(),
        "No MCP servers configured. Use fontaine mcp add to add one."
    );
}

#[tokio::test]
async fn get_masks_environment_values_and_names_pass_through_variables() {
    let s = sandbox();
    let out = s
        .run(&[
            "mcp",
            "add",
            "-s",
            "user",
            "-e",
            "TOKEN=hunter2",
            "--env-var",
            "HOME",
            "srv",
            "--",
            "echo",
            "hi",
        ])
        .await;
    assert_eq!(out.code, Some(0), "{}\n{}", out.stdout, out.stderr);
    let out = s.run(&["mcp", "get", "srv", "--connect=no"]).await;
    assert_eq!(out.code, Some(0), "{}\n{}", out.stdout, out.stderr);
    assert!(out.stdout.contains("TOKEN=***"), "{}", out.stdout);
    assert!(!out.stdout.contains("hunter2"), "{}", out.stdout);
    assert!(out.stdout.contains("HOME"), "{}", out.stdout);
}

#[tokio::test]
async fn a_name_in_two_scopes_makes_remove_exit_1_and_name_both_commands() {
    let s = sandbox();
    for scope in ["local", "user"] {
        let out = s
            .run(&["mcp", "add", "-s", scope, "dup", "--", "echo"])
            .await;
        assert_eq!(out.code, Some(0), "{}", out.stderr);
    }
    let out = s.run(&["mcp", "remove", "dup"]).await;
    assert_eq!(out.code, Some(1), "{}\n{}", out.stdout, out.stderr);
    assert!(
        out.stderr.contains("fontaine mcp remove dup -s local")
            && out.stderr.contains("fontaine mcp remove dup -s user"),
        "{}",
        out.stderr
    );
    // Nothing was removed.
    let out = s.run(&["mcp", "remove", "dup", "-s", "user"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let out = s.run(&["mcp", "remove", "dup"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
}

#[tokio::test]
async fn duplicates_unknown_names_and_bad_usage_exit_with_the_documented_codes() {
    let s = sandbox();
    assert_eq!(
        s.run(&["mcp", "add", "foo", "--", "echo"]).await.code,
        Some(0)
    );
    let out = s.run(&["mcp", "add", "foo", "--", "echo"]).await;
    assert_eq!(out.code, Some(1));
    assert!(
        out.stderr
            .contains("MCP server `foo` already exists in local config"),
        "{}",
        out.stderr
    );
    assert_eq!(s.run(&["mcp", "get", "nope"]).await.code, Some(1));
    assert_eq!(s.run(&["mcp", "remove", "nope"]).await.code, Some(1));
    // A bad name, flags for a later issue, and http with stdio-only flags are usage errors.
    assert_eq!(
        s.run(&["mcp", "add", "bad__name", "--", "echo"]).await.code,
        Some(2)
    );
    let out = s
        .run(&[
            "mcp",
            "add",
            "-t",
            "http",
            "-e",
            "A=b",
            "web",
            "https://h/mcp",
        ])
        .await;
    assert_eq!(out.code, Some(2), "{}", out.stderr);
    let out = s
        .run(&["mcp", "add", "-t", "sse", "web", "https://h/sse"])
        .await;
    assert_eq!(out.code, Some(2));
    assert!(out.stderr.contains("#36"), "{}", out.stderr);
    let out = s
        .run(&[
            "mcp",
            "add",
            "-H",
            "A: b",
            "-t",
            "http",
            "web",
            "https://h/mcp",
        ])
        .await;
    assert_eq!(out.code, Some(2));
    assert!(out.stderr.contains("#32"), "{}", out.stderr);
}

#[tokio::test]
async fn list_shows_an_unapproved_repository_server_as_pending_and_never_starts_it() {
    let s = sandbox();
    trust_workspace(&s);
    let marker = s.work.join("marker.txt");
    s.add_file(
        ".mcp.json",
        &serde_json::json!({"mcpServers": {"repo": {
            "type": "stdio",
            "command": example("touch_marker"),
            "args": [marker.to_string_lossy()],
        }}})
        .to_string(),
    );
    let out = s.run(&["mcp", "list"]).await;
    assert_eq!(out.code, Some(0), "{}\n{}", out.stdout, out.stderr);
    let line = out
        .stdout
        .lines()
        .find(|l| l.starts_with("repo: "))
        .unwrap_or_else(|| panic!("{}", out.stdout));
    assert!(
        line.ends_with("pending approval (run fontaine to approve)"),
        "{line}"
    );
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert!(!marker.exists(), "the repository's server was started");
}

#[tokio::test]
async fn an_untrusted_workspace_lists_its_repository_servers_as_not_started() {
    let s = sandbox();
    s.add_file(
        ".mcp.json",
        r#"{"mcpServers":{"repo":{"type":"stdio","command":"echo"}}}"#,
    );
    let out = s.run(&["mcp", "list"]).await;
    assert!(
        out.stdout
            .contains("repo: echo - not started: workspace not trusted"),
        "{}\n{}",
        out.stdout,
        out.stderr
    );
}

#[tokio::test]
async fn a_malformed_mcp_json_warns_and_does_not_stop_the_command() {
    let s = sandbox();
    s.add_file(".mcp.json", "{nope");
    let out = s.run(&["mcp", "list"]).await;
    assert_eq!(out.code, Some(0), "{}\n{}", out.stdout, out.stderr);
    assert!(
        out.stderr.contains("fontaine: warning: ignoring"),
        "{}",
        out.stderr
    );
}
