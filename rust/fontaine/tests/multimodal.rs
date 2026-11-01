//! Multimodal `read` end to end against the scripted wires: an image reaches an
//! image-capable model as an image part, a text-only model and every PDF fall back to text.

mod common;

use base64::Engine;
use common::{Endpoint, Sandbox, Turn, Wire};
use serde_json::json;

/// A 1x1 PNG; the magic bytes are what `read`'s sniff recognizes.
const PNG_B64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

fn png_bytes() -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(PNG_B64)
        .unwrap()
}

/// `read` of a PNG answers with the image in front of an image-capable model: the wire
/// request after the tool call carries the base64 image part, and the transcript's
/// tool-result text names the file instead of dumping the bytes.
#[tokio::test(flavor = "multi_thread")]
async fn reading_a_png_sends_the_image_part_to_the_model() {
    let endpoint = Endpoint::start(
        Wire::Responses,
        vec![
            Turn::tool("call-1", "read", json!({"path": "pixel.png"})),
            Turn::text("a single pixel"),
        ],
    )
    .await;
    // The catalog-priced `openai` account serves gpt-4o-mini, which takes images.
    let s = Sandbox::priced(&endpoint.url());
    std::fs::write(s.work.join("pixel.png"), png_bytes()).unwrap();

    let out = s
        .headless(&[
            "--output-format",
            "stream-json",
            "Read pixel.png and tell me what you see.",
        ])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    let result = lines.iter().rev().find(|l| l["type"] == "result").unwrap();
    assert!(
        result["result"].as_str().unwrap().contains("single pixel"),
        "{lines:?}"
    );
    // The transcript shows a description, never the base64.
    let tool = lines
        .iter()
        .find(|l| l["type"] == "tool_result")
        .expect("the read result");
    assert_eq!(tool["success"], json!(true));
    let text = tool["text"].as_str().unwrap();
    assert!(text.contains("[image: PNG"), "{text}");
    assert!(text.contains("pixel.png"), "{text}");
    assert!(!text.contains(PNG_B64), "{text}");

    // The request that carried the tool result has the image part on the wire.
    let requests = endpoint.requests().await;
    assert_eq!(requests.len(), 2, "{requests:?}");
    let body = requests[1].to_string();
    assert!(body.contains(PNG_B64), "the base64 part reaches the model");
}

/// A model the catalog does not credit with images gets a text note for an image read;
/// no image bytes reach the wire.
#[tokio::test(flavor = "multi_thread")]
async fn reading_a_png_without_image_support_falls_back_to_text() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::tool("call-1", "read", json!({"path": "pixel.png"})),
            Turn::text("no picture here"),
        ],
    )
    .await;
    // The `openai-compatible` mock has no catalog row: text-only modalities.
    let s = Sandbox::compatible(&endpoint.url());
    std::fs::write(s.work.join("pixel.png"), png_bytes()).unwrap();

    let out = s
        .headless(&[
            "--output-format",
            "stream-json",
            "Read pixel.png and tell me what you see.",
        ])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    let result = lines.iter().rev().find(|l| l["type"] == "result").unwrap();
    assert!(
        result["result"]
            .as_str()
            .unwrap()
            .contains("no picture here"),
        "{lines:?}"
    );
    let tool = lines
        .iter()
        .find(|l| l["type"] == "tool_result")
        .expect("the read result");
    let text = tool["text"].as_str().unwrap();
    assert!(text.contains("does not accept images"), "{text}");

    let requests = endpoint.requests().await;
    assert_eq!(requests.len(), 2, "{requests:?}");
    let body = requests[1].to_string();
    assert!(!body.contains(PNG_B64), "no image bytes on the wire");
}

/// `read` of a PDF never sends the file, even to a model that takes PDFs: the result is a
/// stub naming the file and pointing at attach, and the wire carries no document part.
#[tokio::test(flavor = "multi_thread")]
async fn reading_a_pdf_sends_a_text_stub_not_the_file() {
    let endpoint = Endpoint::start(
        Wire::Responses,
        vec![
            Turn::tool("call-1", "read", json!({"path": "notes.pdf"})),
            Turn::text("the stub is enough"),
        ],
    )
    .await;
    let s = Sandbox::priced(&endpoint.url());
    std::fs::write(s.work.join("notes.pdf"), b"%PDF-1.7\nnot really a pdf").unwrap();

    let out = s
        .headless(&["--output-format", "stream-json", "Read notes.pdf."])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    let result = lines.iter().rev().find(|l| l["type"] == "result").unwrap();
    assert!(
        result["result"]
            .as_str()
            .unwrap()
            .contains("the stub is enough"),
        "{lines:?}"
    );
    let tool = lines
        .iter()
        .find(|l| l["type"] == "tool_result")
        .expect("the read result");
    let text = tool["text"].as_str().unwrap();
    assert!(text.contains("notes.pdf"), "{text}");
    assert!(text.contains("PDF"), "{text}");
    assert!(text.contains("attach"), "{text}");

    let requests = endpoint.requests().await;
    assert_eq!(requests.len(), 2, "{requests:?}");
    let body = requests[1].to_string();
    assert!(!body.contains("JVBER"), "no PDF bytes on the wire: {body}");
}
