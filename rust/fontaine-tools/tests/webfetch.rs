use std::time::Duration;

use fontaine_tools::webfetch::{Webfetch, WebfetchArgs, WebfetchConfig};
use rig_core::tool::{Tool, ToolContext, ToolErrorKind};
use serde_json::json;
use wiremock::matchers::{header, headers, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn config() -> WebfetchConfig {
    WebfetchConfig::default()
}

async fn fetch(
    config: WebfetchConfig,
    url: &str,
) -> Result<String, rig_core::tool::ToolExecutionError> {
    let tool = Webfetch::new(config).unwrap();
    tool.call(
        &mut ToolContext::new(),
        WebfetchArgs {
            url: url.to_string(),
        },
    )
    .await
}

async fn serve(route: &str, template: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(template)
        .mount(&server)
        .await;
    server
}

fn html(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body.as_bytes().to_vec(), "text/html; charset=utf-8")
}

#[test]
fn the_defaults_are_thirty_seconds_ten_megabytes_and_a_hundred_thousand_characters() {
    let defaults = config();
    assert_eq!(defaults.timeout, Duration::from_secs(30));
    assert_eq!(defaults.fetch_cap_bytes, 10 * 1024 * 1024);
    assert_eq!(defaults.output_cap_chars, 100_000);
}

#[tokio::test]
async fn html_becomes_markdown() {
    let server = serve(
        "/page",
        html("<html><head><title>T</title><script>var secret = 1;</script></head><body><h1>Hello</h1><p>See <a href=\"/docs\">the docs</a>.</p></body></html>"),
    )
    .await;
    let out = fetch(config(), &format!("{}/page", server.uri()))
        .await
        .unwrap();
    assert!(out.contains("# Hello"), "{out}");
    assert!(out.contains("[the docs](/docs)"), "{out}");
    assert!(!out.contains("secret"), "{out}");
    assert!(!out.contains("<h1>"), "{out}");
}

#[tokio::test]
async fn plain_text_is_returned_as_it_is() {
    let server = serve(
        "/t",
        ResponseTemplate::new(200).set_body_raw(b"line one\nline two".to_vec(), "text/plain"),
    )
    .await;
    let out = fetch(config(), &format!("{}/t", server.uri()))
        .await
        .unwrap();
    assert_eq!(out, "line one\nline two");
}

#[tokio::test]
async fn a_body_over_the_fetch_cap_is_cut_at_it() {
    let server = serve(
        "/big",
        ResponseTemplate::new(200).set_body_raw("a".repeat(5000).into_bytes(), "text/plain"),
    )
    .await;
    let out = fetch(
        WebfetchConfig {
            fetch_cap_bytes: 100,
            ..config()
        },
        &format!("{}/big", server.uri()),
    )
    .await
    .unwrap();
    let body: String = out.chars().take_while(|c| *c == 'a').collect();
    assert_eq!(body.len(), 100);
    assert!(out.contains("cut at 100 bytes"), "{out}");
}

#[tokio::test]
async fn output_over_the_output_cap_is_truncated_and_says_so() {
    let server = serve(
        "/long",
        ResponseTemplate::new(200).set_body_raw("é".repeat(500).into_bytes(), "text/plain"),
    )
    .await;
    let out = fetch(
        WebfetchConfig {
            output_cap_chars: 50,
            ..config()
        },
        &format!("{}/long", server.uri()),
    )
    .await
    .unwrap();
    let body: String = out.chars().take_while(|c| *c == 'é').collect();
    assert_eq!(body.chars().count(), 50);
    assert!(out.contains("truncated at 50 characters"), "{out}");
}

#[tokio::test]
async fn a_response_slower_than_the_timeout_times_out() {
    let server = serve(
        "/slow",
        ResponseTemplate::new(200)
            .set_body_string("late")
            .set_delay(Duration::from_secs(3)),
    )
    .await;
    let error = fetch(
        WebfetchConfig {
            timeout: Duration::from_millis(200),
            ..config()
        },
        &format!("{}/slow", server.uri()),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), ToolErrorKind::Timeout, "{}", error.message());
}

#[tokio::test]
async fn a_cross_host_redirect_is_returned_not_followed() {
    let server = MockServer::start().await;
    let port = server.address().port();
    Mock::given(method("GET"))
        .and(path("/start"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", format!("http://localhost:{port}/target")),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/target"))
        .respond_with(ResponseTemplate::new(200).set_body_string("reached"))
        .mount(&server)
        .await;
    let out = fetch(config(), &format!("http://127.0.0.1:{port}/start"))
        .await
        .unwrap();
    assert!(
        out.contains(&format!("http://localhost:{port}/target")),
        "{out}"
    );
    assert!(!out.contains("reached"));
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1, "the redirect target was never requested");
}

#[tokio::test]
async fn a_same_host_redirect_is_followed() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/old"))
        .respond_with(ResponseTemplate::new(301).insert_header("location", "/new"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/new"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(b"moved".to_vec(), "text/plain"))
        .mount(&server)
        .await;
    let out = fetch(config(), &format!("{}/old", server.uri()))
        .await
        .unwrap();
    assert_eq!(out, "moved");
}

#[tokio::test]
async fn same_host_redirects_keep_the_accept_header() {
    let server = MockServer::start().await;
    let accept = || headers("accept", vec!["text/markdown", "text/html", "*/*"]);
    Mock::given(method("GET"))
        .and(path("/a"))
        .and(accept())
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/b"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/b"))
        .and(accept())
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(b"# Title\n\n*x*".to_vec(), "text/markdown"),
        )
        .mount(&server)
        .await;
    let out = fetch(config(), &format!("{}/a", server.uri()))
        .await
        .unwrap();
    assert_eq!(out, "# Title\n\n*x*");
}

#[tokio::test]
async fn a_same_host_redirect_loop_stops_at_the_limit() {
    let server = serve(
        "/loop",
        ResponseTemplate::new(302).insert_header("location", "/loop"),
    )
    .await;
    let error = fetch(config(), &format!("{}/loop", server.uri()))
        .await
        .unwrap_err();
    assert!(
        error.message().contains("redirected more than"),
        "{}",
        error.message()
    );
}

#[tokio::test]
async fn a_redirect_without_a_location_is_an_error() {
    let server = serve("/bare", ResponseTemplate::new(302)).await;
    let error = fetch(config(), &format!("{}/bare", server.uri()))
        .await
        .unwrap_err();
    assert!(
        error.message().contains("no usable Location header"),
        "{}",
        error.message()
    );
}

#[tokio::test]
async fn failures_are_errors_the_model_can_read() {
    let server = serve("/missing", ResponseTemplate::new(404)).await;
    let error = fetch(config(), &format!("{}/missing", server.uri()))
        .await
        .unwrap_err();
    assert!(error.message().contains("404"), "{}", error.message());

    let error = fetch(config(), "file:///etc/passwd").await.unwrap_err();
    assert_eq!(error.kind(), ToolErrorKind::InvalidArgs);
    let error = fetch(config(), "not a url").await.unwrap_err();
    assert_eq!(error.kind(), ToolErrorKind::InvalidArgs);

    let binary = serve(
        "/img",
        ResponseTemplate::new(200).set_body_raw(vec![0u8, 1, 2], "image/png"),
    )
    .await;
    let error = fetch(config(), &format!("{}/img", binary.uri()))
        .await
        .unwrap_err();
    assert!(error.message().contains("not text"), "{}", error.message());
}

#[test]
fn only_a_url_is_accepted() {
    let ok: Result<WebfetchArgs, _> = serde_json::from_value(json!({"url": "https://example.com"}));
    assert!(ok.is_ok());
    let extra: Result<WebfetchArgs, _> =
        serde_json::from_value(json!({"url": "https://example.com", "prompt": "summarize"}));
    assert!(extra.is_err());

    let tool = Webfetch::new(config()).unwrap();
    let schema = tool.parameters();
    let properties = schema["properties"].as_object().unwrap();
    assert_eq!(properties.keys().collect::<Vec<_>>(), ["url"]);
    assert_eq!(schema["additionalProperties"], json!(false));
}

#[tokio::test]
async fn requests_carry_a_fontaine_user_agent() {
    let server = MockServer::start().await;
    let expected = concat!(
        "fontaine/",
        env!("CARGO_PKG_VERSION"),
        " (+https://github.com/madesroches/fontaine)"
    );
    Mock::given(method("GET"))
        .and(path("/ua"))
        .and(header("user-agent", expected))
        .respond_with(ResponseTemplate::new(200).set_body_raw(b"hello".to_vec(), "text/plain"))
        .mount(&server)
        .await;

    let out = fetch(config(), &format!("{}/ua", server.uri()))
        .await
        .unwrap();
    assert!(out.contains("hello"), "{out}");
}
