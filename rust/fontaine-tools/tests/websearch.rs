use std::time::Duration;

use fontaine_tools::websearch::{ApiKey, BackendConfig, Websearch, WebsearchArgs, WebsearchConfig};
use rig_core::tool::{Tool, ToolContext, ToolErrorKind, ToolExecutionError};
use serde_json::{Value, json};
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const KEY: &str = "tvly-secret-key";

fn tavily(server: &MockServer) -> WebsearchConfig {
    WebsearchConfig::new(BackendConfig::Tavily {
        api_key: ApiKey::new(KEY),
        endpoint: format!("{}/search", server.uri()).parse().unwrap(),
    })
}

fn searxng(server: &MockServer) -> WebsearchConfig {
    WebsearchConfig::new(BackendConfig::Searxng {
        base_url: server.uri().parse().unwrap(),
        launch: None,
    })
}

fn args(query: &str) -> WebsearchArgs {
    WebsearchArgs {
        query: query.to_string(),
        allowed_domains: None,
        blocked_domains: None,
    }
}

async fn run(config: WebsearchConfig, args: WebsearchArgs) -> Result<String, ToolExecutionError> {
    let tool = Websearch::new(config, &Default::default(), Default::default()).unwrap();
    tool.call(&mut ToolContext::new(), args).await
}

fn results(items: &[(&str, &str, &str)]) -> Value {
    json!({
        "results": items
            .iter()
            .map(|(title, url, content)| json!({"title": title, "url": url, "content": content}))
            .collect::<Vec<_>>()
    })
}

async fn serve_tavily(body: Value) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;
    server
}

async fn serve_searxng(body: Value) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;
    server
}

fn sample() -> Value {
    results(&[
        (
            "Rust 2024",
            "https://blog.rust-lang.org/2024",
            "The edition.",
        ),
        ("Docs", "https://docs.rs/x", "Crate\ndocs   here."),
        ("Other", "https://example.com/rust", "Elsewhere."),
    ])
}

#[tokio::test]
async fn tavily_gets_a_bearer_authenticated_post_and_the_results_are_listed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/search"))
        .and(header("authorization", format!("Bearer {KEY}").as_str()))
        .and(body_json(json!({"query": "rust 2024", "max_results": 10})))
        .respond_with(ResponseTemplate::new(200).set_body_json(sample()))
        .expect(1)
        .mount(&server)
        .await;
    let out = run(tavily(&server), args("rust 2024")).await.unwrap();
    assert!(
        out.starts_with(
            "1. Rust 2024\n   https://blog.rust-lang.org/2024\n   The edition.\n\n2. Docs"
        ),
        "{out}"
    );
    assert!(
        out.contains("   Crate docs here."),
        "snippet whitespace is folded: {out}"
    );
    assert!(out.contains("3. Other"), "{out}");
    assert!(!out.contains(KEY));
}

#[tokio::test]
async fn searxng_gets_a_json_format_query_on_its_search_path() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .and(query_param("q", "rust & edition"))
        .and(query_param("format", "json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [
                {"title": "A", "url": "https://a.example/", "content": null},
                {"url": "https://b.example/", "content": "only a url and a snippet"}
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let out = run(searxng(&server), args("rust & edition")).await.unwrap();
    assert!(out.contains("1. A\n   https://a.example/"), "{out}");
    assert!(
        out.contains("2. https://b.example/\n   https://b.example/\n   only a url"),
        "a missing title falls back to the URL: {out}"
    );
}

#[tokio::test]
async fn a_searxng_base_url_with_a_path_keeps_it() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/searx/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(sample()))
        .expect(1)
        .mount(&server)
        .await;
    let config = WebsearchConfig::new(BackendConfig::Searxng {
        base_url: format!("{}/searx/", server.uri()).parse().unwrap(),
        launch: None,
    });
    run(config, args("x")).await.unwrap();
}

#[tokio::test]
async fn allowed_domains_keep_the_host_and_its_subdomains_only() {
    let server = serve_tavily(sample()).await;
    let mut a = args("rust");
    a.allowed_domains = Some(vec!["rust-lang.org".into(), "docs.rs".into()]);
    let out = run(tavily(&server), a).await.unwrap();
    assert!(
        out.contains("blog.rust-lang.org") && out.contains("docs.rs/x"),
        "{out}"
    );
    assert!(!out.contains("example.com"), "{out}");
    assert!(
        out.starts_with("1. Rust 2024"),
        "results are renumbered: {out}"
    );
}

#[tokio::test]
async fn blocked_domains_drop_the_host_and_its_subdomains() {
    let server = serve_searxng(sample()).await;
    let mut a = args("rust");
    a.blocked_domains = Some(vec!["rust-lang.org".into()]);
    let out = run(searxng(&server), a).await.unwrap();
    assert!(!out.contains("rust-lang.org"), "{out}");
    assert!(
        out.contains("docs.rs") && out.contains("example.com"),
        "{out}"
    );
}

#[tokio::test]
async fn a_domain_is_not_matched_as_a_suffix_of_a_longer_name() {
    let server = serve_tavily(results(&[("T", "https://notrust-lang.org/", "s")])).await;
    let mut a = args("x");
    a.allowed_domains = Some(vec!["rust-lang.org".into()]);
    let out = run(tavily(&server), a).await.unwrap();
    assert!(out.starts_with("No results"), "{out}");
}

#[tokio::test]
async fn both_domain_lists_together_are_invalid_arguments() {
    let server = serve_tavily(sample()).await;
    let mut a = args("rust");
    a.allowed_domains = Some(vec!["a.com".into()]);
    a.blocked_domains = Some(vec!["b.com".into()]);
    let error = run(tavily(&server), a).await.unwrap_err();
    assert_eq!(error.kind(), ToolErrorKind::InvalidArgs);
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn an_empty_query_is_invalid_arguments() {
    let server = serve_tavily(sample()).await;
    let error = run(tavily(&server), args("  ")).await.unwrap_err();
    assert_eq!(error.kind(), ToolErrorKind::InvalidArgs);
}

#[tokio::test]
async fn more_results_than_the_cap_are_cut_with_a_notice() {
    let items: Vec<(String, String)> = (0..8)
        .map(|i| (format!("T{i}"), format!("https://h{i}.example/")))
        .collect();
    let refs: Vec<(&str, &str, &str)> = items
        .iter()
        .map(|(t, u)| (t.as_str(), u.as_str(), "s"))
        .collect();
    let server = serve_searxng(results(&refs)).await;
    let mut config = searxng(&server);
    config.max_results = 3;
    let out = run(config, args("x")).await.unwrap();
    assert!(out.contains("3. T2") && !out.contains("4. T3"), "{out}");
    assert!(out.ends_with("[showing the first 3 of 8 results]"), "{out}");
}

#[tokio::test]
async fn long_output_is_truncated_with_a_notice() {
    let long = "word ".repeat(400);
    let server = serve_tavily(results(&[("T", "https://a.example/", long.as_str())])).await;
    let mut config = tavily(&server);
    config.output_cap_chars = 100;
    let out = run(config, args("x")).await.unwrap();
    assert!(
        out.contains("[the output was truncated at 100 characters]"),
        "{out}"
    );
    assert!(out.chars().count() < 200, "{}", out.chars().count());
}

#[tokio::test]
async fn no_results_is_a_plain_successful_answer() {
    let server = serve_tavily(json!({"results": []})).await;
    let out = run(tavily(&server), args("zzz")).await.unwrap();
    assert_eq!(out, "No results were found for that query.");
}

async fn answering(status: u16, tavily_backend: bool) -> Result<String, ToolExecutionError> {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(status).set_body_string(format!("body with {KEY}")))
        .mount(&server)
        .await;
    let config = if tavily_backend {
        tavily(&server)
    } else {
        searxng(&server)
    };
    run(config, args("x")).await
}

#[tokio::test]
async fn statuses_map_to_distinct_messages_and_never_echo_the_key_or_the_body() {
    let mut seen = Vec::new();
    for status in [401, 403, 429, 500] {
        let error = answering(status, true).await.unwrap_err();
        let message = error.message().to_string();
        assert!(message.contains(&status.to_string()), "{message}");
        assert!(!message.contains(KEY), "{message}");
        seen.push(message);
    }
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 4, "{seen:?}");
}

#[tokio::test]
async fn a_searxng_403_says_the_json_format_is_probably_disabled() {
    let message = answering(403, false)
        .await
        .unwrap_err()
        .message()
        .to_string();
    assert!(message.contains("json"), "{message}");
}

#[tokio::test]
async fn a_malformed_body_is_an_error_naming_the_backend() {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>not json</html>"))
        .mount(&server)
        .await;
    let error = run(searxng(&server), args("x")).await.unwrap_err();
    assert!(error.message().contains("SearXNG"), "{}", error.message());
    let error = run(tavily(&server), args("x")).await.unwrap_err();
    assert!(error.message().contains("Tavily"), "{}", error.message());

    let wrong = serve_tavily(json!({"answer": "no results key"})).await;
    let error = run(tavily(&wrong), args("x")).await.unwrap_err();
    assert!(error.message().contains("Tavily"), "{}", error.message());
}

#[tokio::test]
async fn a_slow_backend_times_out() {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
        .mount(&server)
        .await;
    let mut config = tavily(&server);
    config.timeout = Duration::from_millis(200);
    let error = run(config, args("x")).await.unwrap_err();
    assert_eq!(error.kind(), ToolErrorKind::Timeout, "{}", error.message());
}

#[tokio::test]
async fn a_refused_connection_is_a_network_error_without_the_key() {
    let closed = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap()
    };
    let config = WebsearchConfig::new(BackendConfig::Tavily {
        api_key: ApiKey::new(KEY),
        endpoint: format!("http://{closed}/search").parse().unwrap(),
    });
    let error = run(config, args("x")).await.unwrap_err();
    assert_eq!(error.kind(), ToolErrorKind::Network, "{}", error.message());
    assert!(!error.message().contains(KEY));
    assert!(
        !error.message().contains(&closed.to_string()),
        "the URL is dropped"
    );
}

#[tokio::test]
async fn a_redirect_is_an_error_and_is_never_followed() {
    let elsewhere = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(200).set_body_json(sample()))
        .mount(&elsewhere)
        .await;
    for status in [301, 302, 307, 308] {
        for tavily_backend in [true, false] {
            let origin = MockServer::start().await;
            Mock::given(wiremock::matchers::any())
                .respond_with(
                    ResponseTemplate::new(status)
                        .insert_header("location", format!("{}/search", elsewhere.uri()).as_str()),
                )
                .mount(&origin)
                .await;
            let config = if tavily_backend {
                tavily(&origin)
            } else {
                searxng(&origin)
            };
            let error = run(config, args("a secret query")).await.unwrap_err();
            assert!(error.message().contains("redirect"), "{}", error.message());
            assert!(!error.message().contains(KEY));
        }
    }
    assert!(elsewhere.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn the_description_and_schema_name_the_arguments() {
    let server = serve_tavily(sample()).await;
    let tool = Websearch::new(tavily(&server), &Default::default(), Default::default()).unwrap();
    let schema = tool.parameters();
    assert_eq!(schema["required"], json!(["query"]));
    for key in ["query", "allowed_domains", "blocked_domains"] {
        assert!(schema["properties"].get(key).is_some(), "{key}");
    }
    assert!(tool.description().contains("Tavily"));
    assert!(!tool.description().contains(KEY));
    let unknown = serde_json::from_value::<WebsearchArgs>(json!({"query": "x", "n": 1}));
    assert!(unknown.is_err());
}

#[cfg(unix)]
mod launch {
    use super::*;
    use fontaine_tools::websearch::{Launch, SearxngChild};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn launching(
        server: &MockServer,
        script: &Path,
        dir: &Path,
        startup: Duration,
    ) -> WebsearchConfig {
        WebsearchConfig::new(BackendConfig::Searxng {
            base_url: server.uri().parse().unwrap(),
            launch: Some(Launch {
                argv: vec![script.display().to_string(), dir.display().to_string()],
                startup_timeout: startup,
                env: fontaine_tools::child_env::base_env(&|name| std::env::var_os(name)),
            }),
        })
    }

    /// The script may still be writing when the first probe is answered.
    async fn read_when_written(path: PathBuf) -> String {
        for _ in 0..100 {
            if let Ok(text) = std::fs::read_to_string(&path)
                && text.ends_with('\n')
            {
                return text;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("{} was never written", path.display());
    }

    fn alive(pid: &str) -> bool {
        let out = std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", pid])
            .output()
            .unwrap();
        let state = String::from_utf8_lossy(&out.stdout);
        let state = state.trim();
        !state.is_empty() && !state.starts_with('Z')
    }

    #[tokio::test]
    async fn the_instance_starts_once_for_several_searches_and_dies_with_the_tool() {
        let server = serve_searxng(sample()).await;
        let dir = tempfile::tempdir().unwrap();
        let start = script(
            dir.path(),
            "start.sh",
            "echo started >> \"$1/starts\"\necho $$ > \"$1/pid\"\nexec sleep 300",
        );
        let child = Arc::new(SearxngChild::default());
        let tool = Websearch::new(
            launching(&server, &start, dir.path(), Duration::from_secs(20)),
            &Default::default(),
            child.clone(),
        )
        .unwrap();
        for _ in 0..3 {
            tool.call(&mut ToolContext::new(), args("x")).await.unwrap();
        }
        let starts = read_when_written(dir.path().join("starts")).await;
        assert_eq!(starts.lines().count(), 1, "{starts}");
        assert!(child.is_running().await);

        // A rebuilt tool sharing the handle reuses the process.
        let rebuilt = Websearch::new(
            launching(&server, &start, dir.path(), Duration::from_secs(20)),
            &Default::default(),
            child.clone(),
        )
        .unwrap();
        rebuilt
            .call(&mut ToolContext::new(), args("x"))
            .await
            .unwrap();
        let starts = std::fs::read_to_string(dir.path().join("starts")).unwrap();
        assert_eq!(starts.lines().count(), 1, "{starts}");

        let pid = read_when_written(dir.path().join("pid"))
            .await
            .trim()
            .to_string();
        assert!(alive(&pid));
        drop((tool, rebuilt, child));
        for _ in 0..100 {
            if !alive(&pid) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the instance outlived its handle");
    }

    #[tokio::test]
    async fn a_command_that_exits_before_answering_is_a_start_failure_with_its_stderr() {
        let closed = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            format!("http://{}", listener.local_addr().unwrap())
        };
        let dir = tempfile::tempdir().unwrap();
        let start = script(dir.path(), "fail.sh", "echo boom >&2\nexit 3");
        let config = WebsearchConfig::new(BackendConfig::Searxng {
            base_url: closed.parse().unwrap(),
            launch: Some(Launch {
                argv: vec![
                    start.display().to_string(),
                    dir.path().display().to_string(),
                ],
                startup_timeout: Duration::from_secs(20),
                env: fontaine_tools::child_env::base_env(&|name| std::env::var_os(name)),
            }),
        });
        let error = run(config, args("x")).await.unwrap_err();
        let message = error.message();
        assert!(
            message.contains("exited") && message.contains('3'),
            "{message}"
        );
        assert!(message.contains("boom"), "{message}");
    }

    #[tokio::test]
    async fn a_command_that_never_answers_is_a_readiness_timeout() {
        let closed = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            format!("http://{}", listener.local_addr().unwrap())
        };
        let dir = tempfile::tempdir().unwrap();
        let start = script(dir.path(), "idle.sh", "echo waiting >&2\nexec sleep 300");
        let config = WebsearchConfig::new(BackendConfig::Searxng {
            base_url: closed.parse().unwrap(),
            launch: Some(Launch {
                argv: vec![
                    start.display().to_string(),
                    dir.path().display().to_string(),
                ],
                startup_timeout: Duration::from_millis(600),
                env: fontaine_tools::child_env::base_env(&|name| std::env::var_os(name)),
            }),
        });
        let error = run(config, args("x")).await.unwrap_err();
        let message = error.message();
        assert!(message.contains("did not answer"), "{message}");
        assert!(message.contains("waiting"), "{message}");
    }

    #[tokio::test]
    async fn a_search_dropped_during_startup_terminates_the_process_group() {
        let closed = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            format!("http://{}", listener.local_addr().unwrap())
        };
        let dir = tempfile::tempdir().unwrap();
        let start = script(
            dir.path(),
            "trap.sh",
            "trap 'echo x > \"$1/termed\"; exit 0' TERM\necho x > \"$1/ready\"\nwhile :; do sleep 0.05; done",
        );
        let config = WebsearchConfig::new(BackendConfig::Searxng {
            base_url: closed.parse().unwrap(),
            launch: Some(Launch {
                argv: vec![
                    start.display().to_string(),
                    dir.path().display().to_string(),
                ],
                startup_timeout: Duration::from_secs(60),
                env: fontaine_tools::child_env::base_env(&|name| std::env::var_os(name)),
            }),
        });
        let ready = dir.path().join("ready");
        let cancelled = tokio::time::timeout(Duration::from_millis(1500), async {
            let _ = run(config, args("x")).await;
        })
        .await;
        assert!(cancelled.is_err(), "the search should still be starting");
        assert!(ready.exists(), "the fake child never started");
        // The group is stopped by a detached task, so the trap may run just after the drop.
        let termed = dir.path().join("termed");
        for _ in 0..100 {
            if termed.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(termed.exists(), "the child saw SIGTERM");
    }

    #[tokio::test]
    async fn a_5xx_answer_does_not_count_as_up_until_it_turns_into_a_real_one() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(502))
            .up_to_n_times(2)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(2)
            .with_priority(2)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200))
            .with_priority(3)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(sample()))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let start = script(dir.path(), "start.sh", "exec sleep 300");
        let config = launching(&server, &start, dir.path(), Duration::from_secs(20));
        run(config, args("x")).await.unwrap();
        let probes = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path() == "/")
            .count();
        assert_eq!(probes, 5, "four 5xx answers were polled past");
    }

    #[tokio::test]
    async fn a_command_that_cannot_be_spawned_names_it_and_its_cause() {
        let server = serve_searxng(sample()).await;
        let config = WebsearchConfig::new(BackendConfig::Searxng {
            base_url: server.uri().parse().unwrap(),
            launch: Some(Launch {
                argv: vec!["/nonexistent/searxng-launcher".into()],
                startup_timeout: Duration::from_secs(5),
                env: Vec::new(),
            }),
        });
        let error = run(config, args("x")).await.unwrap_err();
        let message = error.message();
        assert!(
            message.contains("/nonexistent/searxng-launcher"),
            "{message}"
        );
        assert!(message.contains("No such file"), "{message}");
    }

    #[tokio::test]
    async fn the_child_gets_a_scrubbed_environment() {
        let server = serve_searxng(sample()).await;
        let dir = tempfile::tempdir().unwrap();
        let start = script(dir.path(), "env.sh", "env > \"$1/env\"\nexec sleep 300");
        // SAFETY: the variable is set before any thread of this test reads it, and no other
        // test in this binary uses it.
        unsafe { std::env::set_var("FONTAINE_TEST_SECRET_TOKEN", "hunter2") };
        let tool = Websearch::new(
            launching(&server, &start, dir.path(), Duration::from_secs(20)),
            &Default::default(),
            Default::default(),
        )
        .unwrap();
        tool.call(&mut ToolContext::new(), args("x")).await.unwrap();
        let env = read_when_written(dir.path().join("env")).await;
        assert!(env.contains("PATH="), "{env}");
        assert!(!env.contains("hunter2"), "{env}");
    }
}
