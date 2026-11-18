//! Full runs against recorded provider traffic. Replay needs no keys; recording is a human
//! step, one test at a time, with the key exported into that command only:
//! `OPENROUTER_API_KEY=... RIG_PROVIDER_TEST_MODE=record cargo test -p fontaine --test cassettes <name>`
//! (add `-- --ignored` only for a test still carrying `#[ignore]`).

mod common;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use common::Sandbox;
use rig_cassette::http::{CassetteSpec, ProviderCassette};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const UPSTREAM: &str = "https://openrouter.ai/api/v1";
const KEY_ENV: &str = "OPENROUTER_API_KEY";
const PROVIDER: &str = "openrouter";
const MODEL: &str = "openai/gpt-4o-mini";
/// gpt-4o-mini misapplied the destroy rule to a new file, so the guardian uses a model the evals measured.
const GUARDIAN_MODEL: &str = "anthropic/claude-haiku-5.5";

/// Request bodies embed the per-run sandbox path, which the cassette scrubber leaves alone
/// under `/tmp`, so bodies are matched by shape.
fn spec(scenario: &'static str) -> CassetteSpec {
    CassetteSpec::new(scenario).shape_matched()
}

fn account_config(cassette: &ProviderCassette, extra: &str) -> String {
    account_config_at(&cassette.base_url(), extra)
}

/// The `[websearch]` block pins the `websearch` tool into the toolset on every host: the
/// fixtures were recorded with it, and without a container runtime the default backend would
/// drop it from the request. No `command` is set, so nothing is launched.
fn account_config_at(base_url: &str, extra: &str) -> String {
    format!(
        "[accounts.paid]\nkind = \"openrouter\"\nbase_url = \"{base_url}\"\napi_key_env = \"{KEY_ENV}\"\nmodels = [\"{MODEL}\", \"{GUARDIAN_MODEL}\"]\n\n[model]\nmodel = \"paid/{MODEL}\"\n\n[websearch]\nbackend = \"searxng\"\nbase_url = \"http://localhost:8080\"\n{extra}"
    )
}

/// A byte-for-byte forwarding proxy in front of the cassette server that keeps what the client
/// sent. Shape matching lets a replayed request differ from the recorded one, so a test that
/// asserts on the wire reads the bytes the binary actually sent, not the fixture.
struct Tap {
    base_url: String,
    sent: Arc<Mutex<Vec<u8>>>,
}

impl Tap {
    async fn start(upstream_base_url: &str) -> Tap {
        let rest = upstream_base_url
            .strip_prefix("http://")
            .expect("the cassette server speaks plain http");
        let (authority, path) = rest.split_once('/').map_or((rest, ""), |(a, p)| (a, p));
        let upstream = authority.to_string();
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the tap");
        let base_url = format!("http://{}/{path}", listener.local_addr().unwrap());
        let sent = Arc::new(Mutex::new(Vec::new()));
        let captured = sent.clone();
        tokio::spawn(async move {
            loop {
                let Ok((client, _)) = listener.accept().await else {
                    return;
                };
                let upstream = upstream.clone();
                let captured = captured.clone();
                tokio::spawn(async move {
                    let Ok(server) = TcpStream::connect(&upstream).await else {
                        return;
                    };
                    let (mut client_read, mut client_write) = client.into_split();
                    let (mut server_read, mut server_write) = server.into_split();
                    let up = async {
                        let mut buf = [0u8; 16 * 1024];
                        loop {
                            match client_read.read(&mut buf).await {
                                Ok(0) | Err(_) => break,
                                Ok(n) => {
                                    captured.lock().unwrap().extend_from_slice(&buf[..n]);
                                    if server_write.write_all(&buf[..n]).await.is_err() {
                                        break;
                                    }
                                }
                            }
                        }
                        let _ = server_write.shutdown().await;
                    };
                    let down = async {
                        let _ = tokio::io::copy(&mut server_read, &mut client_write).await;
                        let _ = client_write.shutdown().await;
                    };
                    tokio::join!(up, down);
                });
            }
        });
        Tap { base_url, sent }
    }

    fn sent(&self) -> String {
        String::from_utf8_lossy(&self.sent.lock().unwrap()).into_owned()
    }
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("cassettes")
}

/// Prompt, stream, a read tool call executed on the fast path, result, answer.
#[tokio::test(flavor = "multi_thread")]
async fn manual_mode_reads_a_file_through_the_fast_path() {
    let cassette =
        ProviderCassette::start(&root(), PROVIDER, spec("manual_read_round_trip"), UPSTREAM).await;
    let mut s = Sandbox::new();
    s.user_config(&account_config(&cassette, ""));
    s.set_env(KEY_ENV, &cassette.api_key(KEY_ENV));
    // Windows PowerShell is found in System32 without PATH and would add a `powershell` tool the
    // Linux recordings do not have.
    s.set_env("FONTAINE_POWERSHELL_PATH", "/nonexistent/pwsh.exe");
    s.add_file("note.txt", "the magic word is plugh\n");

    let out = s
        .headless(&[
            "--output-format",
            "stream-json",
            "Read note.txt and tell me the magic word.",
        ])
        .await;
    assert_eq!(
        out.code,
        Some(0),
        "stderr: {}\nstdout: {}",
        out.stderr,
        out.stdout
    );
    let lines = out.lines();
    let decision = lines
        .iter()
        .find(|l| l["type"] == "gate_decision")
        .expect("the read call was adjudicated");
    assert_eq!(decision["tier"], "read_only_fast_path");
    assert_eq!(decision["decision"], "allow");
    let result = lines.iter().find(|l| l["type"] == "tool_result").unwrap();
    assert_eq!(result["success"], json!(true));
    let last: &Value = lines.iter().rev().find(|l| l["type"] == "result").unwrap();
    assert!(last["result"].as_str().unwrap().contains("plugh"));
    cassette.finish().await;
}

/// Auto mode end to end: tool call, the guardian's verdict, the tool runs, the answer. The agent
/// and the guardian share the one recorded account.
#[tokio::test(flavor = "multi_thread")]
async fn auto_mode_has_the_guardian_allow_a_harmless_command() {
    let cassette = ProviderCassette::start(
        &root(),
        PROVIDER,
        spec("auto_guardian_round_trip"),
        UPSTREAM,
    )
    .await;
    let mut s = Sandbox::new();
    s.user_config(&account_config(
        &cassette,
        &format!("\n[guardian]\nmodel = \"paid/{GUARDIAN_MODEL}\"\n"),
    ));
    s.set_env(KEY_ENV, &cassette.api_key(KEY_ENV));
    // Windows PowerShell is found in System32 without PATH and would add a `powershell` tool the
    // Linux recordings do not have.
    s.set_env("FONTAINE_POWERSHELL_PATH", "/nonexistent/pwsh.exe");

    let out = s
        .run(&[
            "-p",
            "--permission-mode",
            "auto",
            "--output-format",
            "stream-json",
            "Use the shell tool to run exactly `echo hello > hello.txt`, then say done.",
        ])
        .await;
    assert_eq!(
        out.code,
        Some(0),
        "stderr: {}\nstdout: {}",
        out.stderr,
        out.stdout
    );
    let lines = out.lines();
    let session = lines[0]["session_id"].as_str().unwrap();
    let audit = std::fs::read_to_string(s.data.join("audit").join(format!("{session}.jsonl")))
        .expect("an audit log");
    let decision = lines
        .iter()
        .find(|l| l["type"] == "gate_decision")
        .expect("the shell call was adjudicated");
    let why = format!(
        "gate_decision: {decision}\naudit: {audit}\nstderr: {}",
        out.stderr
    );
    assert_eq!(decision["tier"], "guardian", "{why}");
    assert_eq!(decision["decision"], "allow", "{why}");
    assert_eq!(decision["rule"], "allow", "{why}");
    assert_eq!(decision["guardian_model"], format!("paid/{GUARDIAN_MODEL}"));
    assert!(s.work.join("hello.txt").exists());

    // The audit record carries the verdict.
    let record: Value = serde_json::from_str(audit.lines().next().unwrap()).unwrap();
    assert_eq!(record["tier"], "guardian");
    assert_eq!(record["guardian_verdict"], "allow");
    assert!(record["confidence"].as_f64().unwrap() > 0.0);

    let last: &Value = lines.iter().rev().find(|l| l["type"] == "result").unwrap();
    assert!(
        last["result"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("done")
    );
    cassette.finish().await;
}

/// A 1x1 PNG, as base64; the magic bytes are what `read`'s sniff recognizes.
const PNG_B64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

/// `read` of an image puts the base64 image part on the provider request: this replays a
/// recorded run against a vision model and inspects the bytes the binary sent, which unit tests
/// cannot see because they never build a provider request.
#[tokio::test(flavor = "multi_thread")]
async fn reading_an_image_sends_the_image_part_to_a_vision_model() {
    let cassette =
        ProviderCassette::start(&root(), PROVIDER, spec("read_png_image_part"), UPSTREAM).await;
    let tap = Tap::start(&cassette.base_url()).await;
    let mut s = Sandbox::new();
    s.user_config(&account_config_at(&tap.base_url, ""));
    s.set_env(KEY_ENV, &cassette.api_key(KEY_ENV));
    // Windows PowerShell is found in System32 without PATH and would add a `powershell` tool the
    // Linux recordings do not have.
    s.set_env("FONTAINE_POWERSHELL_PATH", "/nonexistent/pwsh.exe");
    use base64::Engine;
    let png = base64::engine::general_purpose::STANDARD
        .decode(PNG_B64)
        .unwrap();
    std::fs::write(s.work.join("pixel.png"), png).unwrap();

    let out = s
        .headless(&[
            "--output-format",
            "stream-json",
            "Read pixel.png and tell me what you see.",
        ])
        .await;
    assert_eq!(
        out.code,
        Some(0),
        "stderr: {}\nstdout: {}",
        out.stderr,
        out.stdout
    );
    cassette.finish().await;

    // The request that carried the tool result has the image part on the wire.
    let requests = tap.sent();
    assert!(
        requests.contains(PNG_B64),
        "the base64 image part reaches the provider"
    );
}

/// `read` of a PDF never sends the file: the requests the binary sent carry the text stub
/// and no document part.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "cassette not recorded: the model called read_mcp_resource, not read"]
async fn reading_a_pdf_replays_the_text_stub_fallback() {
    let cassette =
        ProviderCassette::start(&root(), PROVIDER, spec("read_pdf_text_stub"), UPSTREAM).await;
    let tap = Tap::start(&cassette.base_url()).await;
    let mut s = Sandbox::new();
    s.user_config(&account_config_at(&tap.base_url, ""));
    s.set_env(KEY_ENV, &cassette.api_key(KEY_ENV));
    // Windows PowerShell is found in System32 without PATH and would add a `powershell` tool the
    // Linux recordings do not have.
    s.set_env("FONTAINE_POWERSHELL_PATH", "/nonexistent/pwsh.exe");
    std::fs::write(s.work.join("notes.pdf"), b"%PDF-1.7\nnot really a pdf").unwrap();

    let out = s
        .headless(&["--output-format", "stream-json", "Read notes.pdf."])
        .await;
    assert_eq!(
        out.code,
        Some(0),
        "stderr: {}\nstdout: {}",
        out.stderr,
        out.stdout
    );
    cassette.finish().await;

    let requests = tap.sent();
    assert!(requests.contains("is a PDF"), "the stub names the file");
    assert!(!requests.contains("JVBER"), "no PDF bytes on the wire");
}
