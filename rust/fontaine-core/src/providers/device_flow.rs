//! An OAuth 2.0 device-authorization client (RFC 8628): the sign-in that needs a browser
//! somewhere but no listener and no key typed into a terminal.

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::time::Instant;
use url::Url;

use super::token_source::now_unix;
use crate::credentials::OAuthTokens;

const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// Added to the poll interval each time the server answers `slow_down`.
const SLOW_DOWN_STEP: Duration = Duration::from_secs(5);

/// Used when the server names no lifetime for a token.
const DEFAULT_TOKEN_LIFETIME_SECS: u64 = 3600;

pub struct DeviceFlowEndpoints {
    pub code_url: String,
    pub token_url: String,
    pub client_id: &'static str,
    /// Extra fields of the device-code request.
    pub code_params: Vec<(&'static str, Value)>,
}

/// What the user must be shown to approve the sign-in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    /// The URL with the code already filled in.
    pub verification_url: String,
    pub expires_in: Duration,
    pub interval: Duration,
}

#[derive(Debug)]
pub enum FlowError {
    /// The request did not complete; carries the transport error without its URL.
    Request(reqwest::Error),
    /// The server refused with an OAuth error code (`access_denied`, `expired_token`, ...).
    Rejected(String),
    /// The code outlived its lifetime before it was approved.
    Expired,
    /// The server sent a verification URL that is not http(s), or no usable reply at all.
    Invalid(String),
}

impl fmt::Display for FlowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FlowError::Request(_) => f.write_str("the sign-in server could not be reached"),
            FlowError::Rejected(code) => write!(f, "the sign-in was refused: {code}"),
            FlowError::Expired => f.write_str("the sign-in code expired"),
            FlowError::Invalid(why) => {
                write!(f, "the sign-in server sent an unusable reply: {why}")
            }
        }
    }
}

impl std::error::Error for FlowError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            FlowError::Request(e) => Some(e),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub enum RefreshError {
    /// The refresh token is no longer valid: the user has to sign in again.
    SignedOut,
    Failed(FlowError),
}

impl fmt::Display for RefreshError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RefreshError::SignedOut => f.write_str("the sign-in has ended"),
            RefreshError::Failed(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for RefreshError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RefreshError::Failed(e) => Some(e),
            RefreshError::SignedOut => None,
        }
    }
}

#[derive(Deserialize)]
struct CodeReply {
    device_code: String,
    user_code: String,
    verification_uri_complete: Option<String>,
    verification_uri: Option<String>,
    expires_in: Option<u64>,
    interval: Option<u64>,
}

#[derive(Deserialize)]
struct TokenReply {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
    org_id: Option<String>,
    error: Option<String>,
}

pub struct DeviceFlow {
    endpoints: DeviceFlowEndpoints,
    http: reqwest::Client,
}

impl DeviceFlow {
    /// The HTTP client is built here, per flow, so nothing pooled outlives the runtime that ran
    /// it.
    pub fn new(endpoints: DeviceFlowEndpoints) -> Result<Self, String> {
        Ok(DeviceFlow {
            endpoints,
            http: crate::transport::reqwest_client()?,
        })
    }

    async fn post(&self, url: &str, body: &Value) -> Result<(u16, Vec<u8>), FlowError> {
        let response = self
            .http
            .post(url)
            .header("content-type", "application/json")
            .header("accept", "application/json")
            .header("user-agent", fontaine_tools::FONTAINE_USER_AGENT)
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| FlowError::Request(e.without_url()))?;
        let status = response.status().as_u16();
        let bytes = response
            .bytes()
            .await
            .map_err(|e| FlowError::Request(e.without_url()))?;
        Ok((status, bytes.to_vec()))
    }

    pub async fn start(&self) -> Result<DeviceCode, FlowError> {
        let mut body = json!({ "client_id": self.endpoints.client_id });
        for (key, value) in &self.endpoints.code_params {
            body[*key] = value.clone();
        }
        let (status, bytes) = self.post(&self.endpoints.code_url, &body).await?;
        let reply: CodeReply = serde_json::from_slice(&bytes).map_err(|_| {
            FlowError::Invalid(format!("the device-code request answered HTTP {status}"))
        })?;
        let url = reply
            .verification_uri_complete
            .or(reply.verification_uri)
            .ok_or_else(|| FlowError::Invalid("no verification URL".to_string()))?;
        let parsed = Url::parse(&url)
            .map_err(|_| FlowError::Invalid("the verification URL is not a URL".to_string()))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(FlowError::Invalid(
                "the verification URL is not http or https".to_string(),
            ));
        }
        Ok(DeviceCode {
            device_code: reply.device_code,
            user_code: reply.user_code,
            verification_url: url,
            expires_in: Duration::from_secs(reply.expires_in.unwrap_or(600)),
            interval: Duration::from_secs(reply.interval.unwrap_or(5).max(1)),
        })
    }

    /// Waits for the user to approve `code`.
    pub async fn poll(&self, code: &DeviceCode) -> Result<OAuthTokens, FlowError> {
        self.poll_with(code, tokio::time::sleep).await
    }

    /// [`poll`](Self::poll) with the wait between requests supplied, so a caller can see how
    /// long it was asked to wait.
    pub async fn poll_with<W, F>(
        &self,
        code: &DeviceCode,
        mut wait: W,
    ) -> Result<OAuthTokens, FlowError>
    where
        W: FnMut(Duration) -> F,
        F: Future<Output = ()>,
    {
        let deadline = Instant::now() + code.expires_in;
        let mut interval = code.interval;
        let body = json!({
            "grant_type": DEVICE_GRANT,
            "device_code": code.device_code,
            "client_id": self.endpoints.client_id,
        });
        loop {
            wait(interval).await;
            if Instant::now() >= deadline {
                return Err(FlowError::Expired);
            }
            let (status, bytes) = self.post(&self.endpoints.token_url, &body).await?;
            let reply = parse_token_reply(status, &bytes)?;
            match reply.error.as_deref() {
                Some("authorization_pending") => continue,
                Some("slow_down") => {
                    interval += SLOW_DOWN_STEP;
                    continue;
                }
                Some(other) => return Err(FlowError::Rejected(other.to_string())),
                None => {}
            }
            return tokens_of(reply, None);
        }
    }

    /// Exchanges `refresh_token` for a new pair. The server rotates the refresh token.
    pub async fn refresh(&self, refresh_token: &str) -> Result<OAuthTokens, RefreshError> {
        let body = json!({
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
            "client_id": self.endpoints.client_id,
        });
        let (status, bytes) = self
            .post(&self.endpoints.token_url, &body)
            .await
            .map_err(RefreshError::Failed)?;
        let reply = parse_token_reply(status, &bytes).map_err(RefreshError::Failed)?;
        match reply.error.as_deref() {
            Some("invalid_grant") => return Err(RefreshError::SignedOut),
            Some(other) => {
                return Err(RefreshError::Failed(FlowError::Rejected(other.to_string())));
            }
            None => {}
        }
        tokens_of(reply, Some(refresh_token)).map_err(RefreshError::Failed)
    }
}

fn parse_token_reply(status: u16, bytes: &[u8]) -> Result<TokenReply, FlowError> {
    serde_json::from_slice(bytes)
        .map_err(|_| FlowError::Invalid(format!("the token request answered HTTP {status}")))
}

fn tokens_of(reply: TokenReply, previous_refresh: Option<&str>) -> Result<OAuthTokens, FlowError> {
    let access = reply
        .access_token
        .ok_or_else(|| FlowError::Invalid("no access token".to_string()))?;
    let refresh = reply
        .refresh_token
        .or_else(|| previous_refresh.map(str::to_string))
        .ok_or_else(|| FlowError::Invalid("no refresh token".to_string()))?;
    let mut extra = BTreeMap::new();
    if let Some(org) = reply.org_id {
        extra.insert("org_id".to_string(), org);
    }
    Ok(OAuthTokens {
        access,
        refresh,
        expires_at: now_unix() + reply.expires_in.unwrap_or(DEFAULT_TOKEN_LIFETIME_SECS),
        extra,
    })
}
