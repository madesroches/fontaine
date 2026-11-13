//! Telling a spent usage window from a transient rate limit. Both answer 429, but only the
//! first lasts until the window resets, so retrying it with a backoff is wasted and ending the
//! run over it is premature.
//!
//! Only structured fields decide: the HTTP status, the provider body's machine code and the
//! reply's headers. Never error text and never tool arguments.

use std::error::Error;
use std::time::{Duration, SystemTime};

use rig_agent::completion::PromptError;
use rig_core::ProviderResponseError;
use rig_core::error::{ErrorReport, ProviderError};

use crate::providers::claude_subscription::{ErrorKind, LimitForm, claude_error};

/// A `Retry-After` above this is a window, not a throttle: it dwarfs the retry backoff, which
/// tops out at a second.
pub const TRANSIENT_RETRY_AFTER_MAX: Duration = Duration::from_secs(300);

/// Machine codes a provider answers a spent window with. A flat table like the quota codes: a
/// new provider adds a row, not a branch. Anthropic's subscription is served only by the
/// `claude-subscription` kind, which says so structurally, so it has no row; an Anthropic
/// API-key `rate_limit_error` is a per-minute limit and stays transient.
pub const WINDOW_CODES: &[&str] = &["exceeded_current_quota_error", "GoUsageLimitError"];

/// A usage window that is spent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageLimit {
    /// When the provider said the window resets, if it said.
    pub reset_at: Option<SystemTime>,
    /// The provider's own name for the limit, when the reply carried one.
    pub window: Option<String>,
}

/// Whether `error` or anything in its `source()` chain is a spent usage window.
pub fn usage_limit(error: &(dyn Error + 'static)) -> Option<UsageLimit> {
    usage_limit_at(error, SystemTime::now())
}

/// [`usage_limit`] as of `now`, which turns a relative `Retry-After` into a time.
pub fn usage_limit_at(error: &(dyn Error + 'static), now: SystemTime) -> Option<UsageLimit> {
    let mut current = Some(error);
    while let Some(e) = current {
        if let Some(run) = e.downcast_ref::<PromptError>() {
            return prompt_usage_limit(run, now);
        }
        if let Some(provider) = e.downcast_ref::<ProviderError>() {
            return provider_usage_limit(provider, now);
        }
        current = e.source();
    }
    None
}

/// [`usage_limit`] for a failed run.
pub fn prompt_usage_limit(error: &PromptError, now: SystemTime) -> Option<UsageLimit> {
    match error {
        PromptError::Provider(e) => provider_usage_limit(e, now),
        PromptError::Report(report) => report_usage_limit(report, now),
        _ => None,
    }
}

/// [`usage_limit`] for a provider error, which is also what a one-shot completion returns.
pub fn provider_usage_limit(error: &ProviderError, now: SystemTime) -> Option<UsageLimit> {
    if let Some(claude) = claude_error(error) {
        let plan_limit =
            claude.kind == ErrorKind::Limit && claude.limit_form == Some(LimitForm::PlanLimit);
        return plan_limit.then_some(UsageLimit {
            reset_at: claude.reset_at,
            window: None,
        });
    }
    classify(
        error.provider_response_status().map(|s| s.as_u16()),
        error
            .provider_response()
            .and_then(ProviderResponseError::machine_code)
            .as_deref(),
        error.provider_response(),
        now,
    )
}

/// [`usage_limit`] for a relayed report.
pub fn report_usage_limit(report: &ErrorReport, now: SystemTime) -> Option<UsageLimit> {
    let response = report.provider_response.as_ref();
    let status = report
        .http_status
        .or_else(|| response.and_then(|r| r.status).map(|s| s.as_u16()));
    let code = report
        .code
        .clone()
        .or_else(|| response.and_then(ProviderResponseError::machine_code));
    classify(status, code.as_deref(), response, now)
}

/// Whether a machine code names a spent window.
pub fn is_window_code(code: &str) -> bool {
    WINDOW_CODES.contains(&code)
}

fn classify(
    status: Option<u16>,
    code: Option<&str>,
    response: Option<&ProviderResponseError>,
    now: SystemTime,
) -> Option<UsageLimit> {
    let retry_after = response.and_then(|r| retry_after(r, now));
    let by_code = code.is_some_and(is_window_code);
    let by_wait = matches!(status, Some(402 | 429))
        && retry_after.is_some_and(|wait| wait > TRANSIENT_RETRY_AFTER_MAX);
    if !by_code && !by_wait {
        return None;
    }
    let reset_at = retry_after
        .and_then(|wait| now.checked_add(wait))
        .or_else(|| response.and_then(body_reset));
    Some(UsageLimit {
        reset_at,
        window: code.filter(|c| is_window_code(c)).map(str::to_owned),
    })
}

/// The wait the reply's `Retry-After` header asks for: seconds or an HTTP date. Never the body's
/// prose, which rig's own reader also scans.
pub(crate) fn retry_after(response: &ProviderResponseError, now: SystemTime) -> Option<Duration> {
    let value = response
        .headers
        .as_ref()?
        .get(http::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim();
    if let Ok(secs) = value.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    let at: SystemTime = chrono::DateTime::parse_from_rfc2822(value).ok()?.into();
    Some(at.duration_since(now).unwrap_or_default())
}

/// A reset time in the body's structured fields: a numeric `reset_at` or `resets_at` (epoch
/// seconds) on the error or its metadata.
fn body_reset(response: &ProviderResponseError) -> Option<SystemTime> {
    let doc: serde_json::Value = serde_json::from_str(&response.body).ok()?;
    ["error", "metadata"]
        .into_iter()
        .filter_map(|key| doc.get(key))
        .chain(std::iter::once(&doc))
        .find_map(|scope| {
            ["reset_at", "resets_at"].into_iter().find_map(|field| {
                let secs = scope.get(field)?.as_u64()?;
                // Epoch milliseconds are the only other form providers use for a time this large.
                let secs = if secs > 100_000_000_000 {
                    secs / 1000
                } else {
                    secs
                };
                SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(secs))
            })
        })
}

#[cfg(test)]
mod tests {
    use http::{HeaderMap, HeaderValue, StatusCode};

    use super::*;

    const NOW: SystemTime = SystemTime::UNIX_EPOCH;

    fn reply(status: u16, body: &str, retry_after: Option<&str>) -> ProviderError {
        let mut response = ProviderResponseError::new(StatusCode::from_u16(status).unwrap(), body);
        if let Some(value) = retry_after {
            let mut headers = HeaderMap::new();
            headers.insert(
                http::header::RETRY_AFTER,
                HeaderValue::from_str(value).unwrap(),
            );
            response = response.with_headers(Some(headers));
        }
        ProviderError::ProviderResponse(response)
    }

    fn kimi_body() -> &'static str {
        r#"{"error":{"message":"You've reached your 5-hour usage limit. Your quota will reset when the current 5-hour window ends.","type":"exceeded_current_quota_error"}}"#
    }

    #[test]
    fn the_kimi_code_plan_body_is_a_spent_window_without_a_reset() {
        let limit = provider_usage_limit(&reply(429, kimi_body(), None), NOW).unwrap();
        assert_eq!(limit.reset_at, None);
        assert_eq!(
            limit.window.as_deref(),
            Some("exceeded_current_quota_error")
        );
    }

    #[test]
    fn the_go_capture_is_a_spent_window_with_its_retry_after() {
        let capture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../research/kimi-harness/captures/e2_go_429.json"
        ))
        .unwrap();
        let retry = capture["retry_after_s"].as_str().unwrap();
        let body = capture["body"].to_string();
        let limit = provider_usage_limit(&reply(429, &body, Some(retry)), NOW).unwrap();
        assert_eq!(limit.window.as_deref(), Some("GoUsageLimitError"));
        assert_eq!(limit.reset_at, Some(NOW + Duration::from_secs(1_301_339)));
    }

    #[test]
    fn a_long_retry_after_alone_is_a_window_and_a_short_one_is_a_throttle() {
        let body = r#"{"error":{"type":"rate_limit_error","message":"slow down"}}"#;
        let long = provider_usage_limit(&reply(429, body, Some("7200")), NOW).unwrap();
        assert_eq!(long.reset_at, Some(NOW + Duration::from_secs(7200)));
        assert_eq!(long.window, None);
        assert_eq!(
            provider_usage_limit(&reply(429, body, Some("30")), NOW),
            None
        );
        assert_eq!(
            provider_usage_limit(&reply(429, body, Some("300")), NOW),
            None
        );
        assert_eq!(provider_usage_limit(&reply(429, body, None), NOW), None);
    }

    #[test]
    fn a_long_retry_after_on_a_server_error_is_not_a_window() {
        let body = r#"{"error":{"type":"overloaded_error"}}"#;
        assert_eq!(
            provider_usage_limit(&reply(503, body, Some("7200")), NOW),
            None
        );
    }

    #[test]
    fn an_http_date_retry_after_is_read() {
        let body = r#"{"error":{"type":"rate_limit_error"}}"#;
        let limit = provider_usage_limit(
            &reply(429, body, Some("Thu, 01 Jan 1970 05:00:00 GMT")),
            NOW,
        )
        .unwrap();
        assert_eq!(limit.reset_at, Some(NOW + Duration::from_secs(5 * 3600)));
    }

    #[test]
    fn a_reset_in_the_body_is_used_when_no_header_gives_one() {
        let body = r#"{"error":{"type":"exceeded_current_quota_error","resets_at":1790578200}}"#;
        let limit = provider_usage_limit(&reply(429, body, None), NOW).unwrap();
        assert_eq!(
            limit.reset_at,
            Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_578_200))
        );
    }

    #[test]
    fn a_status_less_mid_stream_code_is_a_window() {
        let report = ErrorReport::new(rig_core::error::ErrorKind::Request, "stream error")
            .with_code("exceeded_current_quota_error");
        let limit = report_usage_limit(&report, NOW).unwrap();
        assert_eq!(limit.reset_at, None);
    }

    #[test]
    fn prose_is_never_read() {
        let body = r#"{"error":{"type":"rate_limit_error","message":"You've reached your 5-hour usage limit; try again in 4h"}}"#;
        assert_eq!(provider_usage_limit(&reply(429, body, None), NOW), None);
        let plain = ProviderError::ProviderResponse(ProviderResponseError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "usage limit exceeded, exceeded_current_quota_error",
        ));
        assert_eq!(provider_usage_limit(&plain, NOW), None);
    }

    #[test]
    fn the_chain_is_walked_to_the_provider_error() {
        #[derive(Debug)]
        struct Wrapper(ProviderError);
        impl std::fmt::Display for Wrapper {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("the guardian could not be reached")
            }
        }
        impl Error for Wrapper {
            fn source(&self) -> Option<&(dyn Error + 'static)> {
                Some(&self.0)
            }
        }
        let wrapped = Wrapper(reply(429, kimi_body(), None));
        assert!(usage_limit(&wrapped).is_some());
        let transient = Wrapper(reply(429, r#"{"error":{"type":"rate_limit_error"}}"#, None));
        assert!(usage_limit(&transient).is_none());
    }

    fn claude(form: LimitForm, reset: Option<SystemTime>) -> ProviderError {
        crate::providers::claude_subscription::ClaudeError::new(ErrorKind::Limit, "limit")
            .with_limit(form, reset)
            .provider()
    }

    #[test]
    fn only_the_plan_limit_form_of_a_claude_subscription_failure_is_a_window() {
        let reset = Some(NOW + Duration::from_secs(60));
        let plan = provider_usage_limit(&claude(LimitForm::PlanLimit, reset), NOW).unwrap();
        assert_eq!(plan.reset_at, reset);
        assert!(provider_usage_limit(&claude(LimitForm::RateLimit, reset), NOW).is_none());
        assert!(provider_usage_limit(&claude(LimitForm::Billing, reset), NOW).is_none());
    }
}
