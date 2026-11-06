//! The account's subscription usage windows, learned from the `rate_limit_event` frames the CLI
//! emits mid-stream. No request is made for them: they arrive on the model streams the session
//! already runs.

use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use futures::future::BoxFuture;
use serde_json::Value;

use crate::RuntimeError;
use crate::quota::{Quota, QuotaSource, QuotaWindow};
use fontaine_tools::lock::lock_recovering;

/// The latest quota any model of the account saw, and the source that serves it.
#[derive(Debug, Default)]
pub struct Usage {
    latest: Mutex<Option<Quota>>,
}

impl Usage {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&self, quota: Quota) {
        // Display-only state: a poisoned lock still holds a usable figure.
        *lock_recovering(&self.latest) = Some(quota);
    }

    pub fn snapshot(&self) -> Option<Quota> {
        lock_recovering(&self.latest).clone()
    }
}

impl QuotaSource for Usage {
    fn name(&self) -> &str {
        "claude"
    }

    fn fetch(&self) -> BoxFuture<'_, Result<Quota, RuntimeError>> {
        let quota = self.snapshot().unwrap_or(Quota { windows: vec![] });
        Box::pin(async move { Ok(quota) })
    }
}

/// The window label for a CLI window type, longest period last in this table's order of use.
fn label_of(kind: &str) -> Option<&'static str> {
    match kind {
        "five_hour" => Some("5h"),
        "seven_day" => Some("7d"),
        _ => None,
    }
}

fn window(
    kind: &str,
    utilization: Option<&Value>,
    resets_at: Option<&Value>,
) -> Option<QuotaWindow> {
    let label = label_of(kind)?;
    let used_ratio = utilization
        .and_then(Value::as_f64)
        .filter(|u| u.is_finite() && *u >= 0.0)?;
    // Epoch seconds, as the captured payload shows.
    let reset_at = resets_at
        .and_then(Value::as_u64)
        .map(|secs| SystemTime::UNIX_EPOCH + Duration::from_secs(secs));
    Some(QuotaWindow {
        label: label.to_string(),
        used_ratio,
        reset_at,
    })
}

/// The windows a `rate_limit_event` frame carries, longest period first. A frame without usable
/// window data yields an empty quota.
pub fn parse_rate_limit(value: &Value) -> Quota {
    let mut windows = Vec::new();
    let info = value.get("rate_limit_info");
    if let Some(unified) = info
        .and_then(|i| i.get("unifiedWindows"))
        .and_then(Value::as_object)
    {
        for kind in ["seven_day", "five_hour"] {
            if let Some(w) = unified
                .get(kind)
                .and_then(|w| window(kind, w.get("utilization"), w.get("resetsAt")))
            {
                windows.push(w);
            }
        }
    }
    if windows.is_empty()
        && let Some(info) = info
        && let Some(kind) = info.get("rateLimitType").and_then(Value::as_str)
        && let Some(w) = window(kind, info.get("utilization"), info.get("resetsAt"))
    {
        windows.push(w);
    }
    Quota { windows }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn at(secs: u64) -> Option<SystemTime> {
        Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
    }

    fn captured() -> Value {
        json!({"type":"rate_limit_event","rate_limit_info":{
            "status":"allowed","resetsAt":1790578200u64,"rateLimitType":"five_hour",
            "overageStatus":"rejected","overageDisabledReason":"out_of_credits",
            "isUsingOverage":false,
            "unifiedWindows":{
                "five_hour":{"utilization":0.09,"resetsAt":1790578200u64},
                "seven_day":{"utilization":0.15,"resetsAt":1791036000u64}}},
            "uuid":"u","session_id":"s"})
    }

    #[test]
    fn the_captured_payload_gives_both_windows_longest_first() {
        let quota = parse_rate_limit(&captured());
        assert_eq!(
            quota.windows,
            vec![
                QuotaWindow {
                    label: "7d".into(),
                    used_ratio: 0.15,
                    reset_at: at(1_791_036_000),
                },
                QuotaWindow {
                    label: "5h".into(),
                    used_ratio: 0.09,
                    reset_at: at(1_790_578_200),
                },
            ]
        );
    }

    #[test]
    fn overage_and_unknown_windows_are_skipped() {
        let frame = json!({"rate_limit_info":{"unifiedWindows":{
            "seven_day_overage_included":{"utilization":0.5,"resetsAt":1},
            "mystery":{"utilization":0.5,"resetsAt":1},
            "five_hour":{"utilization":0.2,"resetsAt":1790578200u64}}}});
        let quota = parse_rate_limit(&frame);
        assert_eq!(quota.windows.len(), 1);
        assert_eq!(quota.windows[0].label, "5h");
    }

    #[test]
    fn a_bad_utilization_drops_the_window_and_a_bad_reset_only_the_reset() {
        let frame = json!({"rate_limit_info":{"unifiedWindows":{
            "five_hour":{"utilization":"lots","resetsAt":1790578200u64},
            "seven_day":{"utilization":0.3,"resetsAt":"soon"}}}});
        let quota = parse_rate_limit(&frame);
        assert_eq!(quota.windows.len(), 1);
        assert_eq!(quota.windows[0].label, "7d");
        assert_eq!(quota.windows[0].used_ratio, 0.3);
        assert_eq!(quota.windows[0].reset_at, None);

        let negative = json!({"rate_limit_info":{"unifiedWindows":{
            "five_hour":{"utilization":-0.1}}}});
        assert!(parse_rate_limit(&negative).windows.is_empty());
    }

    #[test]
    fn the_top_level_fields_are_the_fallback() {
        let frame = json!({"rate_limit_info":{
            "status":"allowed_warning","rateLimitType":"seven_day",
            "utilization":0.8,"resetsAt":1791036000u64}});
        assert_eq!(
            parse_rate_limit(&frame).windows,
            vec![QuotaWindow {
                label: "7d".into(),
                used_ratio: 0.8,
                reset_at: at(1_791_036_000),
            }]
        );
        let overage = json!({"rate_limit_info":{"rateLimitType":"overage","utilization":0.8}});
        assert!(parse_rate_limit(&overage).windows.is_empty());
    }

    #[test]
    fn bare_and_status_only_frames_are_empty() {
        assert!(
            parse_rate_limit(&json!({"type":"rate_limit_event"}))
                .windows
                .is_empty()
        );
        assert!(
            parse_rate_limit(&json!({"rate_limit_info":{"status":"allowed"}}))
                .windows
                .is_empty()
        );
        assert!(parse_rate_limit(&json!("junk")).windows.is_empty());
    }

    #[tokio::test]
    async fn fetch_is_empty_until_recorded() {
        let usage = Usage::new();
        assert_eq!(usage.name(), "claude");
        assert!(usage.fetch().await.unwrap().windows.is_empty());
        let quota = parse_rate_limit(&captured());
        usage.record(quota.clone());
        assert_eq!(usage.fetch().await.unwrap(), quota);
    }
}
