//! The `webfetch` tool: one GET, HTML turned into markdown. It never follows a redirect to
//! another host; the model is told where the redirect points and may ask again.

use std::time::Duration;

use rig_core::tool::{Tool, ToolContext, ToolExecutionError};
use serde::Deserialize;
use serde_json::json;
use url::Url;

/// Redirects within one host that a fetch follows before giving up.
const MAX_SAME_HOST_REDIRECTS: usize = 10;

/// Claude Code's WebFetch value, so sites tuned for it respond the same way.
const ACCEPT: &str = "text/markdown, text/html, */*";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebfetchConfig {
    pub timeout: Duration,
    /// Bytes of the response body read; the rest is cut.
    pub fetch_cap_bytes: usize,
    /// Characters of converted text returned.
    pub output_cap_chars: usize,
}

impl Default for WebfetchConfig {
    fn default() -> Self {
        WebfetchConfig {
            timeout: Duration::from_secs(30),
            fetch_cap_bytes: 10 * 1024 * 1024,
            output_cap_chars: 100_000,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebfetchArgs {
    pub url: String,
}

#[derive(Clone, Debug)]
pub struct Webfetch {
    client: reqwest::Client,
    config: WebfetchConfig,
}

impl Webfetch {
    pub fn new(
        config: WebfetchConfig,
        network: &crate::tls::LoadedNetwork,
    ) -> Result<Self, reqwest::Error> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::ACCEPT,
            reqwest::header::HeaderValue::from_static(ACCEPT),
        );
        let client = crate::tls::build_client(network, || {
            reqwest::Client::builder()
                .user_agent(crate::FONTAINE_USER_AGENT)
                .default_headers(headers.clone())
                .redirect(reqwest::redirect::Policy::none())
                .timeout(config.timeout)
        })?;
        Ok(Webfetch { client, config })
    }

    pub fn config(&self) -> &WebfetchConfig {
        &self.config
    }

    async fn fetch(&self, start: Url) -> Result<String, ToolExecutionError> {
        let mut url = start;
        for _ in 0..=MAX_SAME_HOST_REDIRECTS {
            let mut response = self
                .client
                .get(url.clone())
                .send()
                .await
                .map_err(|e| self.request_error(e))?;
            let status = response.status();
            if status.is_redirection() {
                let target = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|location| url.join(location).ok());
                let Some(target) = target else {
                    return Err(ToolExecutionError::other(format!(
                        "{url} answered {status} with no usable Location header"
                    )));
                };
                if target.host_str() != url.host_str() {
                    return Ok(format!(
                        "{url} redirects to a different host: {target}\nNothing was fetched from \
                         it. Call webfetch again with that URL if you want it."
                    ));
                }
                url = target;
                continue;
            }
            if !status.is_success() {
                return Err(ToolExecutionError::other(format!(
                    "{url} answered with HTTP status {status}"
                )));
            }
            let kind = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(str::to_ascii_lowercase);
            let textual = kind.as_deref().is_none_or(|kind| {
                ["text/", "html", "json", "xml", "javascript", "markdown"]
                    .iter()
                    .any(|marker| kind.contains(marker))
            });
            if !textual {
                return Err(ToolExecutionError::other(format!(
                    "{url} is not text ({}), so it was not fetched",
                    kind.as_deref().unwrap_or("unknown type")
                )));
            }
            let is_html = kind.as_deref().is_some_and(|kind| kind.contains("html"));

            let mut body = Vec::new();
            let mut cut = false;
            while let Some(chunk) = response.chunk().await.map_err(|e| self.request_error(e))? {
                let room = self.config.fetch_cap_bytes - body.len();
                if chunk.len() > room {
                    body.extend_from_slice(&chunk[..room]);
                    cut = true;
                    break;
                }
                body.extend_from_slice(&chunk);
            }
            let text = String::from_utf8_lossy(&body).into_owned();
            let mut text = if is_html || looks_like_html(&text) {
                convert(&text)?
            } else {
                text
            };
            let mut notes = Vec::new();
            if cut {
                notes.push(format!(
                    "the response was cut at {} bytes",
                    self.config.fetch_cap_bytes
                ));
            }
            if let Some((end, _)) = text.char_indices().nth(self.config.output_cap_chars) {
                text.truncate(end);
                notes.push(format!(
                    "the output was truncated at {} characters",
                    self.config.output_cap_chars
                ));
            }
            if !notes.is_empty() {
                text.push_str(&format!("\n\n[{}]", notes.join("; ")));
            }
            return Ok(text);
        }
        Err(ToolExecutionError::other(format!(
            "{url} redirected more than {MAX_SAME_HOST_REDIRECTS} times"
        )))
    }

    fn request_error(&self, error: reqwest::Error) -> ToolExecutionError {
        if error.is_timeout() {
            return ToolExecutionError::timeout(format!(
                "the request timed out after {} seconds",
                self.config.timeout.as_secs_f64().max(0.001)
            ));
        }
        ToolExecutionError::network(format!("the request failed: {}", describe(error)))
    }
}

/// The error without its URL, which the model already has, and with its causes.
fn describe(error: reqwest::Error) -> String {
    crate::error_chain::chain(&error.without_url())
}

fn looks_like_html(text: &str) -> bool {
    let head: String = text
        .chars()
        .take(512)
        .collect::<String>()
        .to_ascii_lowercase();
    let head = head.trim_start();
    head.starts_with("<!doctype html") || head.starts_with("<html")
}

fn convert(html: &str) -> Result<String, ToolExecutionError> {
    htmd::HtmlToMarkdown::builder()
        .skip_tags(vec!["script", "style", "head"])
        .build()
        .convert(html)
        .map_err(|e| ToolExecutionError::other(format!("the page could not be converted: {e}")))
}

impl Tool for Webfetch {
    const NAME: &'static str = "webfetch";
    type Args = WebfetchArgs;
    type Output = String;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        format!(
            "Fetch a web page with one GET and return it as markdown (HTML is converted; other \
             text is returned as it is). Takes only a `url` (http or https). Reads at most {} MB \
             and returns at most {} characters, saying so when it cuts. A redirect to another \
             host is not followed: the new URL is returned, and you may call again with it. \
             Times out after {} seconds.",
            self.config.fetch_cap_bytes / (1024 * 1024),
            self.config.output_cap_chars,
            self.config.timeout.as_secs()
        )
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "The http or https URL to fetch." },
            },
            "required": ["url"],
            "additionalProperties": false,
        })
    }

    fn annotations(&self) -> rig_core::completion::ToolAnnotations {
        rig_core::completion::ToolAnnotations::default().with_title("Web fetch")
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: WebfetchArgs,
    ) -> Result<String, ToolExecutionError> {
        let url = Url::parse(&args.url).map_err(|e| {
            ToolExecutionError::invalid_args(format!("`{}` is not a URL: {e}", args.url))
        })?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(ToolExecutionError::invalid_args(format!(
                "only http and https URLs can be fetched, not `{}`",
                url.scheme()
            )));
        }
        self.fetch(url).await
    }
}
