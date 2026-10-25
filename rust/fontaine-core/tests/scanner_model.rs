#![cfg(feature = "pg2")]

//! The PromptGuard 2 download and the startup notices around it, against a stand-in for
//! Hugging Face. No real model is involved: files that download are not a loadable model, so
//! every run here ends with the scanner unavailable.

mod common;

use std::ffi::OsString;
use std::sync::Arc;

use common::World;
use fontaine_core::scanner_model::{self, FILES, FetchError};
use fontaine_core::scanner_stage::{self, Backend, ScannerStage, ScannerStart, StartParams};
use fontaine_tools::gate::{CallView, ROOT_AGENT};
use fontaine_tools::result_pipeline::{ResultPipeline, StageKind};
use fontaine_tools::risk::RiskClass;
use rig_core::tool::ToolOutput;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const LICENSE: &str = "Llama 4 Community License";

async fn serve_files(server: &MockServer, status: u16) {
    for name in FILES {
        Mock::given(method("GET"))
            .and(path(format!("/{name}")))
            .respond_with(
                ResponseTemplate::new(status).set_body_string(format!("contents of {name}")),
            )
            .mount(server)
            .await;
    }
}

/// Anything in `dir` that is not a finished model file: a download left behind.
fn parts_in(dir: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|name| !FILES.contains(&name.as_str()))
                .collect()
        })
        .unwrap_or_default()
}

async fn started(
    w: &World,
    user_config: &str,
    token: Option<(&str, &str)>,
    base_url: Option<&str>,
) -> ScannerStart {
    w.user_file(user_config);
    let config = w.load().unwrap();
    let token: Option<(String, String)> = token.map(|(k, v)| (k.to_string(), v.to_string()));
    let env = move |name: &str| -> Option<OsString> {
        token
            .as_ref()
            .filter(|(k, _)| k == name)
            .map(|(_, v)| OsString::from(v))
    };
    scanner_stage::start(&StartParams {
        config: &config,
        data_dir: &w.data,
        scrub_env: &[],
        env: &env,
        guardian: None,
        model_base_url: base_url,
    })
    .await
}

async fn through_stage(backend: Backend) -> String {
    let pipeline =
        ResultPipeline::new().with_stage(StageKind::Scanner, Arc::new(ScannerStage::new(backend)));
    let args = json!({});
    let view = CallView {
        tool: "read",
        risk: RiskClass::ReadOnly,
        args: &args,
        agent: ROOT_AGENT,
        annotations: None,
    };
    pipeline
        .run(view, Ok(ToolOutput::text("file body")))
        .await
        .0
        .unwrap()
        .render()
}

fn assert_failed_open(start: &ScannerStart, token_var: &str) {
    assert!(
        start.notices.iter().any(|n| n.contains(LICENSE)),
        "{:?}",
        start.notices
    );
    let failure = start.notices.last().unwrap();
    assert!(failure.contains("unavailable"), "{failure}");
    assert!(failure.contains(LICENSE), "{failure}");
    assert!(failure.contains(&format!("${token_var}")), "{failure}");
    assert!(matches!(start.backend, Some(Backend::Unavailable(_))));
}

const PG2: &str = "[guardian]\nscanner = \"pg2\"\n";

#[tokio::test]
async fn the_token_comes_from_the_named_variable_and_only_missing_files_are_fetched() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(header("authorization", "Bearer secret-token"))
        .respond_with(ResponseTemplate::new(200).set_body_string("bytes"))
        .mount(&server)
        .await;
    let w = World::new();
    let dir = scanner_model::default_dir(&w.data);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.json"), "already here").unwrap();

    let start = started(
        &w,
        "[guardian]\nscanner = \"pg2\"\nscanner_hf_token_env = \"MY_HF\"\n",
        Some(("MY_HF", "secret-token")),
        Some(&server.uri()),
    )
    .await;

    let asked: Vec<String> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| r.url.path().to_string())
        .collect();
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert!(
        !asked.iter().any(|p| p.ends_with("config.json")),
        "{asked:?}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("config.json")).unwrap(),
        "already here"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("tokenizer.json")).unwrap(),
        "bytes"
    );
    assert!(parts_in(&dir).is_empty());
    // The files are not a model, so loading fails after the download, with the license first.
    assert_failed_open(&start, "MY_HF");
}

#[tokio::test]
async fn an_interrupted_response_leaves_no_final_file_and_no_temp_file() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut request = [0u8; 4096];
            let _ = socket.read(&mut request).await;
            // Promises 1000 bytes, delivers ten, and hangs up.
            let _ = socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n0123456789")
                .await;
        }
    });
    let w = World::new();
    let dir = scanner_model::default_dir(&w.data);
    let err = scanner_model::ensure(&dir, &url, Some("tok"), &Default::default())
        .await
        .unwrap_err();
    assert!(matches!(err, FetchError::Failed(_)), "{err}");
    for name in FILES {
        assert!(!dir.join(name).exists(), "{name} must not exist");
    }
    assert!(parts_in(&dir).is_empty(), "{:?}", parts_in(&dir));
}

#[tokio::test]
async fn a_stale_partial_download_is_swept_and_a_fresh_one_kept() {
    let server = MockServer::start().await;
    serve_files(&server, 200).await;
    let w = World::new();
    let dir = scanner_model::default_dir(&w.data);
    std::fs::create_dir_all(&dir).unwrap();
    let stale = dir.join(".part-model.safetensors-old");
    let fresh = dir.join(".part-model.safetensors-new");
    for p in [&stale, &fresh] {
        std::fs::write(p, "partial").unwrap();
    }
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(&stale)
        .unwrap()
        .set_modified(old)
        .unwrap();
    scanner_model::ensure(&dir, &server.uri(), Some("tok"), &Default::default())
        .await
        .unwrap();
    assert!(!stale.exists());
    assert!(fresh.exists());
}

#[tokio::test(start_paused = true)]
async fn a_server_that_never_answers_cannot_hold_startup_forever() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((socket, _)) = listener.accept().await {
            held.push(socket);
        }
    });
    let w = World::new();
    let dir = scanner_model::default_dir(&w.data);
    let err = scanner_model::ensure(&dir, &url, Some("tok"), &Default::default())
        .await
        .unwrap_err();
    assert!(matches!(err, FetchError::Failed(_)), "{err}");
    assert!(parts_in(&dir).is_empty());
}

#[tokio::test]
async fn a_missing_token_keeps_startup_going_with_a_notice_and_asks_nothing() {
    let server = MockServer::start().await;
    serve_files(&server, 200).await;
    let w = World::new();
    let start = started(&w, PG2, None, Some(&server.uri())).await;
    assert!(server.received_requests().await.unwrap().is_empty());
    assert_failed_open(&start, "HF_TOKEN");
    assert!(
        start
            .notices
            .last()
            .unwrap()
            .contains("no Hugging Face token")
    );
}

#[tokio::test]
async fn a_refusal_keeps_startup_going_with_a_notice() {
    for status in [401, 403] {
        let server = MockServer::start().await;
        serve_files(&server, status).await;
        let w = World::new();
        let start = started(&w, PG2, Some(("HF_TOKEN", "tok")), Some(&server.uri())).await;
        assert_failed_open(&start, "HF_TOKEN");
        assert!(
            start.notices.last().unwrap().contains(&status.to_string()),
            "{:?}",
            start.notices
        );
        assert!(parts_in(&scanner_model::default_dir(&w.data)).is_empty());
    }
}

#[tokio::test]
async fn a_network_error_keeps_startup_going_with_a_notice() {
    let w = World::new();
    // Nothing listens on a port that was just released.
    let url = {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    };
    let start = started(&w, PG2, Some(("HF_TOKEN", "tok")), Some(&url)).await;
    assert_failed_open(&start, "HF_TOKEN");
}

#[tokio::test]
async fn a_bad_model_directory_keeps_startup_going_and_results_carry_the_note() {
    let w = World::new();
    let models = w.root.join("models");
    std::fs::create_dir_all(&models).unwrap();
    for name in FILES {
        std::fs::write(models.join(name), "corrupt").unwrap();
    }
    let config = format!(
        "[guardian]\nscanner = \"pg2\"\nscanner_model_path = \"{}\"\n",
        models.to_string_lossy().replace('\\', "/")
    );
    let start = started(&w, &config, None, None).await;
    assert_failed_open(&start, "HF_TOKEN");

    let text = through_stage(start.backend.unwrap()).await;
    assert!(text.starts_with("file body"), "{text}");
    assert!(text.contains("unscanned and suspect"), "{text}");
}

#[tokio::test]
async fn a_model_path_that_is_missing_files_is_not_downloaded_into() {
    let server = MockServer::start().await;
    serve_files(&server, 200).await;
    let w = World::new();
    let empty = w.root.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let config = format!(
        "[guardian]\nscanner = \"pg2\"\nscanner_model_path = \"{}\"\n",
        empty.to_string_lossy().replace('\\', "/")
    );
    let start = started(&w, &config, Some(("HF_TOKEN", "tok")), Some(&server.uri())).await;
    assert!(server.received_requests().await.unwrap().is_empty());
    assert_failed_open(&start, "HF_TOKEN");
}

#[tokio::test]
async fn the_license_notice_is_given_on_every_startup_with_pg2() {
    let w = World::new();
    let start = started(&w, PG2, None, None).await;
    assert!(start.notices[0].contains(LICENSE), "{:?}", start.notices);
    let off = started(&w, "[guardian]\nscanner = \"llm\"\n", None, None).await;
    assert!(
        off.notices.iter().all(|n| !n.contains(LICENSE)),
        "{:?}",
        off.notices
    );
}

#[tokio::test]
async fn two_sessions_downloading_at_once_both_succeed_and_leave_complete_files() {
    let server = MockServer::start().await;
    serve_files(&server, 200).await;
    let w = World::new();
    let dir = scanner_model::default_dir(&w.data);
    let url = server.uri();

    let network = Default::default();
    let (a, b) = tokio::join!(
        scanner_model::ensure(&dir, &url, Some("tok"), &network),
        scanner_model::ensure(&dir, &url, Some("tok"), &network),
    );
    a.unwrap();
    b.unwrap();
    for name in FILES {
        assert_eq!(
            std::fs::read_to_string(dir.join(name)).unwrap(),
            format!("contents of {name}")
        );
    }
    assert!(parts_in(&dir).is_empty(), "{:?}", parts_in(&dir));
}
