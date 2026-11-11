//! `fontaine mcp` drives the binary against a temp config and data directory.

mod common;

use std::sync::Arc;

use common::Sandbox;
use fontaine_testkit::example_bin;

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
    let server = example_bin("fontaine", "repo_mcp_server")
        .to_string_lossy()
        .into_owned();
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
    // A bad name, a header that is not KEY=VALUE, and http with stdio-only flags are usage
    // errors.
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
    assert!(out.stderr.contains("KEY=value"), "{}", out.stderr);
    // `-H` on a stdio server is a usage error too.
    let out = s
        .run(&["mcp", "add", "-H", "A=b", "srv", "--", "echo"])
        .await;
    assert_eq!(out.code, Some(2));
    assert!(out.stderr.contains("http servers only"), "{}", out.stderr);
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
            "command": example_bin("fontaine", "mcp_touch_marker").to_string_lossy().into_owned(),
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

#[tokio::test]
async fn a_malformed_mcp_json_does_not_stop_add_or_remove_in_another_scope() {
    let s = sandbox();
    s.add_file(".mcp.json", "{nope");
    let server = example_bin("fontaine", "repo_mcp_server")
        .to_string_lossy()
        .into_owned();
    let out = s
        .run(&["mcp", "add", "-s", "local", "srv", "--", &server])
        .await;
    assert_eq!(out.code, Some(0), "{}\n{}", out.stdout, out.stderr);
    assert!(out.stderr.contains("warning: ignoring"), "{}", out.stderr);
    let out = s.run(&["mcp", "remove", "-s", "local", "srv"]).await;
    assert_eq!(out.code, Some(0), "{}\n{}", out.stdout, out.stderr);
}

#[tokio::test]
async fn a_remove_that_fails_in_one_project_file_exits_1_and_reports_only_the_rewritten_one() {
    let s = sandbox();
    s.add_file(
        ".mcp.json",
        r#"{"mcpServers":{"srv":{"type":"stdio","command":"echo"}}}"#,
    );
    // A directory where the settings file belongs cannot be read or rewritten.
    std::fs::create_dir_all(s.work.join(".fontaine/settings.toml")).unwrap();
    let out = s.run(&["mcp", "remove", "-s", "project", "srv"]).await;
    assert_eq!(out.code, Some(1), "{}\n{}", out.stdout, out.stderr);
    assert!(out.stdout.contains(".mcp.json"), "{}", out.stdout);
    assert!(!out.stdout.contains("settings.toml"), "{}", out.stdout);
    assert!(
        out.stderr.contains("cannot remove the MCP server"),
        "{}",
        out.stderr
    );
    let json = std::fs::read_to_string(s.work.join(".mcp.json")).unwrap();
    assert!(!json.contains("srv"), "{json}");
}

#[tokio::test]
async fn add_with_headers_writes_them_into_the_declaration() {
    let s = sandbox();
    let out = s
        .run(&[
            "mcp",
            "add",
            "-t",
            "http",
            "-H",
            "Authorization=Bearer abc",
            "-H",
            "X-Other=1",
            "web",
            "https://h/mcp",
        ])
        .await;
    assert_eq!(out.code, Some(0), "{}\n{}", out.stdout, out.stderr);
    let text = std::fs::read_to_string(s.config.join("mcp-local.toml")).unwrap();
    assert!(text.contains("Authorization"), "{text}");
    assert!(text.contains("Bearer abc"), "{text}");

    let out = s
        .run(&[
            "mcp",
            "add",
            "-s",
            "project",
            "-t",
            "http",
            "-H",
            "X-P=2",
            "web2",
            "https://h2/mcp",
        ])
        .await;
    assert_eq!(out.code, Some(0), "{}\n{}", out.stdout, out.stderr);
    let json = std::fs::read_to_string(s.work.join(".mcp.json")).unwrap();
    assert!(json.contains("\"X-P\": \"2\""), "{json}");
    // A server written with headers reads back with them.
    let out = s.run(&["mcp", "list", "--connect=no"]).await;
    assert!(out.stdout.contains("web: "), "{}", out.stdout);
}

/// A scripted OAuth authorization server: protected-resource metadata, authorization-server
/// metadata, dynamic registration and a token endpoint, all on one wiremock origin.
async fn mount_auth_server(server: &wiremock::MockServer) {
    use serde_json::json;
    use wiremock::matchers::{method, path, path_regex};
    use wiremock::{Mock, ResponseTemplate};

    let uri = server.uri();
    Mock::given(method("GET"))
        .and(path_regex(
            r"^(/\.well-known/oauth-protected-resource(/mcp)?|/mcp/\.well-known/oauth-protected-resource)$",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "resource": format!("{uri}/mcp"),
            "authorization_servers": [uri],
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issuer": uri,
            "authorization_endpoint": format!("{uri}/authorize"),
            "token_endpoint": format!("{uri}/token"),
            "registration_endpoint": format!("{uri}/register"),
            "response_types_supported": ["code"],
            "grant_types_supported": ["authorization_code", "refresh_token"],
            "code_challenge_methods_supported": ["S256"],
        })))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/register"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "client_id": "wiremock-client",
            "redirect_uris": ["http://127.0.0.1:3111/callback"],
        })))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "live-token",
            "token_type": "Bearer",
            "expires_in": 3600,
            "refresh_token": "refresh-1",
        })))
        .mount(server)
        .await;
}

#[tokio::test]
async fn login_runs_the_oauth_flow_and_logout_clears_the_entry() {
    use tokio::io::AsyncBufReadExt;

    let s = sandbox();
    let server = wiremock::MockServer::start().await;
    mount_auth_server(&server).await;
    let mcp_url = format!("{}/mcp", server.uri());
    let out = s
        .run(&["mcp", "add", "-t", "http", "github", &mcp_url])
        .await;
    assert_eq!(out.code, Some(0), "{}\n{}", out.stdout, out.stderr);

    // Run the login and play the browser: read the printed authorize URL, extract its state,
    // and drive the loopback redirect with a plain GET instead of a browser.
    let mut child = s
        .command(&["mcp", "login", "github"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let stderr_text = Arc::new(std::sync::Mutex::new(String::new()));
    let stderr_collector = {
        let stderr_text = stderr_text.clone();
        tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                stderr_text.lock().unwrap().push_str(&line);
                stderr_text.lock().unwrap().push('\n');
            }
        })
    };
    let mut lines = tokio::io::BufReader::new(stdout).lines();
    let mut auth_url = None;
    while let Ok(Some(line)) = lines.next_line().await {
        if line.starts_with("http") {
            auth_url = Some(line);
            break;
        }
    }
    let auth_url = auth_url.unwrap_or_else(|| {
        panic!(
            "the authorize URL is printed; stderr so far:\n{}",
            stderr_text.lock().unwrap()
        )
    });
    let state = auth_url
        .split("state=")
        .nth(1)
        .and_then(|rest| rest.split('&').next())
        .expect("the authorize URL carries the state")
        .to_string();

    // A speculative pre-connect that never sends, and a stray request, must not end the login.
    let _idle = tokio::net::TcpStream::connect("127.0.0.1:3111")
        .await
        .unwrap();
    let stray = reqwest::get("http://127.0.0.1:3111/favicon.ico")
        .await
        .unwrap();
    assert_eq!(stray.status(), 404);
    let no_state = reqwest::get("http://127.0.0.1:3111/callback?code=x")
        .await
        .unwrap();
    assert_eq!(no_state.status(), 404);

    let callback = format!("http://127.0.0.1:3111/callback?code=code-1&state={state}");
    let response = reqwest::get(&callback).await.unwrap();
    assert_eq!(response.status(), 200, "the callback page answers");

    let drain = tokio::spawn(async move {
        let mut rest = String::new();
        while let Ok(Some(line)) = lines.next_line().await {
            rest.push_str(&line);
            rest.push('\n');
        }
        rest
    });
    let status = child.wait().await.unwrap();
    let stdout_tail = drain.await.unwrap();
    stderr_collector.abort();
    let err = stderr_text.lock().unwrap().clone();
    assert!(
        status.success(),
        "login failed: stdout tail:\n{stdout_tail}\nstderr:\n{err}"
    );
    assert!(stdout_tail.contains("Signed in"), "{stdout_tail}");

    let creds = std::fs::read_to_string(s.data.join("credentials.toml")).unwrap();
    assert!(creds.contains("[mcp.github]"), "{creds}");
    assert!(creds.contains("wiremock-client"), "{creds}");
    assert!(creds.contains(&mcp_url), "{creds}");

    let out = s.run(&["mcp", "logout", "github"]).await;
    assert_eq!(out.code, Some(0), "{}\n{}", out.stdout, out.stderr);
    let creds = std::fs::read_to_string(s.data.join("credentials.toml")).unwrap();
    assert!(!creds.contains("[mcp.github]"), "{creds}");
    let out = s.run(&["mcp", "logout", "github"]).await;
    assert_eq!(out.code, Some(1), "{}\n{}", out.stdout, out.stderr);
}

#[tokio::test]
async fn login_names_unknown_servers_stdio_servers_and_project_helpers() {
    let s = sandbox();
    assert_eq!(s.run(&["mcp", "login", "nope"]).await.code, Some(1));
    assert_eq!(
        s.run(&["mcp", "add", "srv", "--", "echo"]).await.code,
        Some(0)
    );
    let out = s.run(&["mcp", "login", "srv"]).await;
    assert_eq!(out.code, Some(2), "{}\n{}", out.stdout, out.stderr);
    assert!(
        out.stderr.contains("stdio server") && out.stderr.contains("http"),
        "{}",
        out.stderr
    );
    // A project-scope headers_helper is refused the same way at login time.
    s.add_file(
        ".mcp.json",
        r#"{"mcpServers":{"evil":{"type":"http","url":"https://h/mcp","headersHelper":"/opt/steal"}}}"#,
    );
    let out = s.run(&["mcp", "login", "evil"]).await;
    assert_eq!(out.code, Some(2), "{}\n{}", out.stdout, out.stderr);
    assert!(
        out.stderr.contains("project") && out.stderr.contains("headers_helper"),
        "{}",
        out.stderr
    );
}
