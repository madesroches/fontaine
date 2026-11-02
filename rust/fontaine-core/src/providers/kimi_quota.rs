//! Kimi for Coding's subscription quota: `GET <base_url>/usages`.

use std::time::{Duration, SystemTime};

use futures::FutureExt;
use futures::future::BoxFuture;
use serde_json::Value;

use crate::RuntimeError;
use crate::quota::{Quota, QuotaSource, QuotaWindow};
use crate::transport::reqwest_client;
use fontaine_tools::tls::LoadedNetwork;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// The windows the endpoint reports, headline first.
const WINDOWS: [(&str, &str); 4] = [
    ("limit_month_total", "month"),
    ("limit_month_code", "code"),
    ("limit_7d", "7d"),
    ("limit_5h", "5h"),
];

#[derive(Debug)]
pub struct KimiQuota {
    client: reqwest::Client,
    url: String,
    /// The `openai-compatible` credential is optional, so the header is sent only when present.
    key: Option<String>,
    timeout: Duration,
}

impl KimiQuota {
    pub fn new(
        network: &LoadedNetwork,
        base_url: &str,
        key: Option<String>,
    ) -> Result<Self, RuntimeError> {
        Ok(KimiQuota {
            client: reqwest_client(network)?,
            url: format!("{}/usages", base_url.trim_end_matches('/')),
            key,
            timeout: REQUEST_TIMEOUT,
        })
    }

    /// Tests only: a paused clock auto-advances during real I/O and would fire any timeout.
    #[cfg(test)]
    fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    async fn get(&self) -> Result<Quota, RuntimeError> {
        let mut request = self.client.get(&self.url).timeout(self.timeout);
        if let Some(key) = &self.key {
            request = request.bearer_auth(key);
        }
        let response = request.send().await.map_err(|e| {
            RuntimeError::caused("could not reach the quota endpoint", e.without_url())
        })?;
        let status = response.status();
        if !status.is_success() {
            return Err(RuntimeError::new(format!(
                "the quota endpoint replied {status}"
            )));
        }
        let body = response
            .text()
            .await
            .map_err(|e| RuntimeError::caused("could not read the quota reply", e.without_url()))?;
        parse_usages(&body)
    }
}

impl QuotaSource for KimiQuota {
    fn name(&self) -> &str {
        "kimi"
    }

    fn fetch(&self) -> BoxFuture<'_, Result<Quota, RuntimeError>> {
        self.get().boxed()
    }
}

pub fn parse_usages(body: &str) -> Result<Quota, RuntimeError> {
    let value: Value = serde_json::from_str(body)
        .map_err(|e| RuntimeError::caused("the quota reply is not JSON", e))?;
    let usages = value.get("usages").and_then(Value::as_object);
    let windows: Vec<QuotaWindow> = WINDOWS
        .iter()
        .filter_map(|(key, label)| {
            let entry = usages?.get(*key)?;
            let used_ratio = match entry.get("used_ratio")? {
                Value::Number(n) => n.as_f64()?,
                Value::String(s) => s.trim().parse().ok()?,
                _ => return None,
            };
            if !used_ratio.is_finite() || used_ratio < 0.0 {
                return None;
            }
            let reset_at = entry
                .get("reset_time")
                .and_then(Value::as_str)
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(SystemTime::from);
            Some(QuotaWindow {
                label: (*label).to_string(),
                used_ratio,
                reset_at,
            })
        })
        .collect();
    if windows.is_empty() {
        return Err(RuntimeError::new("the quota reply has no usable window"));
    }
    Ok(Quota { windows })
}

#[cfg(test)]
mod tests {
    use fontaine_tools::error_chain::chain;
    use fontaine_tools::tls::LoadedNetwork;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    use super::*;

    const FULL: &str = r#"{
        "usages": {
            "limit_5h": {"used_ratio": 0.82, "reset_time": "2026-11-02T15:00:00Z"},
            "limit_7d": {"used_ratio": "0.4", "reset_time": "2026-11-09T00:00:00+00:00"},
            "limit_month_total": {"used_ratio": 0.16, "reset_time": "2026-12-01T00:00:00Z"},
            "limit_month_code": {"used_ratio": 0.05},
            "limit_year": {"used_ratio": 0.9}
        },
        "boosterWallet": {"balance": 3}
    }"#;

    fn source(server: &MockServer, key: Option<&str>) -> KimiQuota {
        KimiQuota::new(
            &LoadedNetwork::default(),
            &format!("{}/coding/v1/", server.uri()),
            key.map(str::to_string),
        )
        .expect("the client builds")
    }

    #[test]
    fn a_full_reply_lists_the_headline_first() {
        let quota = parse_usages(FULL).unwrap();
        let labels: Vec<&str> = quota.windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, ["month", "code", "7d", "5h"]);
        assert_eq!(quota.windows[0].used_ratio, 0.16);
        assert_eq!(quota.windows[2].used_ratio, 0.4);
        assert!(quota.windows[0].reset_at.is_some());
        assert!(quota.windows[1].reset_at.is_none());
    }

    #[test]
    fn missing_windows_are_left_out() {
        let quota = parse_usages(r#"{"usages": {"limit_5h": {"used_ratio": 0.1}}}"#).unwrap();
        assert_eq!(quota.windows.len(), 1);
        assert_eq!(quota.windows[0].label, "5h");
    }

    #[test]
    fn bad_ratios_drop_the_entry_and_bad_timestamps_only_the_reset() {
        let quota = parse_usages(
            r#"{"usages": {
                "limit_5h": {"used_ratio": -0.1},
                "limit_7d": {"used_ratio": "lots"},
                "limit_month_code": {"used_ratio": null},
                "limit_month_total": {"used_ratio": 0.3, "reset_time": "tomorrow"}
            }}"#,
        )
        .unwrap();
        assert_eq!(quota.windows.len(), 1);
        assert_eq!(quota.windows[0].label, "month");
        assert_eq!(quota.windows[0].reset_at, None);
    }

    #[test]
    fn a_reply_with_nothing_usable_is_an_error() {
        for body in [
            "{}",
            r#"{"usages": {}}"#,
            r#"{"usages": {"limit_year": {"used_ratio": 0.5}}}"#,
            r#"{"boosterWallet": {"balance": 3}}"#,
            "not json",
        ] {
            assert!(parse_usages(body).is_err(), "{body}");
        }
    }

    #[tokio::test]
    async fn sends_the_key_to_the_usages_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/coding/v1/usages"))
            .and(header("authorization", "Bearer sk-secret"))
            .respond_with(ResponseTemplate::new(200).set_body_string(FULL))
            .expect(1)
            .mount(&server)
            .await;
        let quota = source(&server, Some("sk-secret")).fetch().await.unwrap();
        assert_eq!(quota.windows.len(), 4);
    }

    #[tokio::test]
    async fn without_a_key_no_authorization_header_is_sent() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/coding/v1/usages"))
            .respond_with(ResponseTemplate::new(200).set_body_string(FULL))
            .mount(&server)
            .await;
        source(&server, None).fetch().await.unwrap();
        let requests = server.received_requests().await.unwrap();
        assert!(
            requests
                .iter()
                .all(|r: &Request| !r.headers.contains_key("authorization"))
        );
    }

    #[tokio::test]
    async fn a_non_success_reply_is_an_error_without_the_key() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(401).set_body_string("bad key sk-secret"))
            .mount(&server)
            .await;
        let error = source(&server, Some("sk-secret"))
            .fetch()
            .await
            .unwrap_err();
        let text = chain(&error);
        assert!(text.contains("401"), "{text}");
        assert!(!text.contains("sk-secret"), "{text}");
    }

    #[tokio::test]
    async fn a_slow_endpoint_times_out() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(FULL)
                    .set_delay(Duration::from_secs(3)),
            )
            .mount(&server)
            .await;
        let slow = source(&server, None).with_timeout(Duration::from_millis(200));
        let started = std::time::Instant::now();
        assert!(slow.fetch().await.is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
