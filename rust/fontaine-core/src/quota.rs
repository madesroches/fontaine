//! A provider's own subscription quota, polled slowly for display. Advisory only: a failure
//! never reaches the guardian, the floor or a turn.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use futures::future::BoxFuture;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::RuntimeError;

/// How long the poller waits between fetches.
pub const POLL_INTERVAL: Duration = Duration::from_secs(60);

/// How long the last good value outlives failing fetches before it is dropped.
pub const STALE_AFTER: Duration = Duration::from_secs(10 * 60);

#[derive(Clone, Debug, PartialEq)]
pub struct QuotaWindow {
    pub label: String,
    /// Above 1.0 when the provider reports more than the limit.
    pub used_ratio: f64,
    pub reset_at: Option<SystemTime>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Quota {
    /// The longest-period window first; the status bar shows them in reverse.
    pub windows: Vec<QuotaWindow>,
}

pub trait QuotaSource: Send + Sync + std::fmt::Debug {
    /// The short label the status bar shows, such as `kimi`.
    fn name(&self) -> &str;

    fn fetch(&self) -> BoxFuture<'_, Result<Quota, RuntimeError>>;
}

/// Whose quota an [`AccountUsage`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageRole {
    Main,
    Guardian,
}

impl UsageRole {
    pub fn as_str(self) -> &'static str {
        match self {
            UsageRole::Main => "main",
            UsageRole::Guardian => "guardian",
        }
    }
}

/// One account's usage-window state, as every surface shows it. Only an account with a quota
/// source has one.
#[derive(Clone, Debug, PartialEq)]
pub struct AccountUsage {
    pub role: UsageRole,
    /// The configured account name.
    pub account: String,
    /// [`QuotaSource::name`], such as `kimi`.
    pub label: String,
    /// `None` when the fetch failed or has not answered yet.
    pub quota: Option<Quota>,
}

impl AccountUsage {
    /// The one serialisation, shared by headless output and the custom status line. A window
    /// follows Claude Code's `rate_limits` fields: `used_percentage` is 0-100 and `resets_at`
    /// is Unix epoch seconds. Nothing is guessed: an absent value is omitted.
    pub fn json(&self) -> serde_json::Value {
        let mut entry = serde_json::json!({
            "role": self.role.as_str(),
            "account": self.account,
            "label": self.label,
        });
        let Some(quota) = &self.quota else {
            return entry;
        };
        let windows: Vec<serde_json::Value> = quota
            .windows
            .iter()
            .map(|window| {
                let mut value = serde_json::json!({
                    "label": window.label,
                    "used_percentage": (window.used_ratio.clamp(0.0, 1.0) * 100.0).round() as u32,
                });
                let resets_at = window
                    .reset_at
                    .and_then(|at| at.duration_since(SystemTime::UNIX_EPOCH).ok());
                if let Some(at) = resets_at {
                    value["resets_at"] = at.as_secs().into();
                }
                value
            })
            .collect();
        entry["windows"] = windows.into();
        entry
    }
}

/// What the poller last published.
#[derive(Clone, Debug, Default)]
pub struct QuotaSnapshot {
    pub name: String,
    /// The last good value; `None` before the first success and once it went stale.
    pub quota: Option<Quota>,
    /// The latest fetch's failure, cleared by the next success.
    pub error: Option<Arc<RuntimeError>>,
}

/// Polls one [`QuotaSource`] on its own task, so a fetch is never awaited by a turn or a draw.
/// The task starts with the first source and is aborted when the poller drops.
pub struct QuotaPoller {
    source: watch::Sender<Option<Arc<dyn QuotaSource>>>,
    snapshot: watch::Sender<QuotaSnapshot>,
    task: Option<JoinHandle<()>>,
}

impl Default for QuotaPoller {
    fn default() -> Self {
        QuotaPoller {
            source: watch::channel(None).0,
            snapshot: watch::channel(QuotaSnapshot::default()).0,
            task: None,
        }
    }
}

impl QuotaPoller {
    pub fn new() -> Self {
        Self::default()
    }

    /// Switches to `source`, fetching at once. The same source (by `Arc` identity) or `None`
    /// again changes nothing. Needs a Tokio runtime when `source` is `Some`.
    pub fn point(&mut self, source: Option<&Arc<dyn QuotaSource>>) {
        let unchanged = match (&*self.source.borrow(), source) {
            (None, None) => true,
            (Some(current), Some(next)) => Arc::ptr_eq(current, next),
            _ => false,
        };
        if unchanged {
            return;
        }
        self.source.send_replace(source.cloned());
        // At once, so a draw right after never shows the previous account's figures.
        self.snapshot.send_replace(QuotaSnapshot::default());
        if source.is_some() && self.task.is_none() {
            self.task = Some(tokio::spawn(run(
                self.source.subscribe(),
                self.snapshot.clone(),
            )));
        }
    }

    /// A receiver that wakes when the snapshot changes.
    pub fn subscribe(&self) -> watch::Receiver<QuotaSnapshot> {
        self.snapshot.subscribe()
    }
}

impl Drop for QuotaPoller {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

async fn run(
    mut sources: watch::Receiver<Option<Arc<dyn QuotaSource>>>,
    snapshot: watch::Sender<QuotaSnapshot>,
) {
    let mut last_good: Option<(Quota, Instant)> = None;
    loop {
        let source = sources.borrow_and_update().clone();
        let Some(source) = source else {
            last_good = None;
            snapshot.send_replace(QuotaSnapshot::default());
            if sources.changed().await.is_err() {
                return;
            }
            continue;
        };
        let fetched = tokio::select! {
            fetched = source.fetch() => fetched,
            changed = sources.changed() => {
                if changed.is_err() {
                    return;
                }
                last_good = None;
                snapshot.send_replace(QuotaSnapshot::default());
                continue;
            }
        };
        let mut published = QuotaSnapshot {
            name: source.name().to_string(),
            ..QuotaSnapshot::default()
        };
        match fetched {
            Ok(quota) => last_good = Some((quota, Instant::now())),
            Err(e) => {
                if last_good
                    .as_ref()
                    .is_some_and(|(_, at)| at.elapsed() > STALE_AFTER)
                {
                    last_good = None;
                }
                published.error = Some(Arc::new(e));
            }
        }
        published.quota = last_good.as_ref().map(|(quota, _)| quota.clone());
        // select! picks randomly when the fetch and a re-point are both ready.
        if sources.has_changed().unwrap_or(true) {
            last_good = None;
            snapshot.send_replace(QuotaSnapshot::default());
            continue;
        }
        snapshot.send_replace(published);
        tokio::select! {
            () = tokio::time::sleep(POLL_INTERVAL) => {}
            changed = sources.changed() => {
                if changed.is_err() {
                    return;
                }
                last_good = None;
                snapshot.send_replace(QuotaSnapshot::default());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[derive(Debug)]
    struct Fake {
        name: &'static str,
        calls: AtomicUsize,
        script: Mutex<VecDeque<Result<f64, &'static str>>>,
    }

    impl Fake {
        fn new(name: &'static str, script: &[Result<f64, &'static str>]) -> Arc<Fake> {
            Arc::new(Fake {
                name,
                calls: AtomicUsize::new(0),
                script: Mutex::new(script.iter().cloned().collect()),
            })
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl QuotaSource for Fake {
        fn name(&self) -> &str {
            self.name
        }

        fn fetch(&self) -> BoxFuture<'_, Result<Quota, RuntimeError>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let next = self
                .script
                .lock()
                .expect("test lock")
                .pop_front()
                .unwrap_or(Err("script exhausted"));
            Box::pin(async move {
                match next {
                    Ok(ratio) => Ok(Quota {
                        windows: vec![QuotaWindow {
                            label: "month".into(),
                            used_ratio: ratio,
                            reset_at: None,
                        }],
                    }),
                    Err(why) => Err(RuntimeError::new(why)),
                }
            })
        }
    }

    fn usage(quota: Option<Quota>) -> AccountUsage {
        AccountUsage {
            role: UsageRole::Guardian,
            account: "moon".into(),
            label: "kimi".into(),
            quota,
        }
    }

    #[test]
    fn usage_json_follows_the_rate_limits_fields() {
        let quota = Quota {
            windows: vec![
                QuotaWindow {
                    label: "month".into(),
                    used_ratio: 0.165,
                    reset_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)),
                },
                QuotaWindow {
                    label: "5h".into(),
                    used_ratio: 1.4,
                    reset_at: None,
                },
            ],
        };
        assert_eq!(
            usage(Some(quota)).json(),
            serde_json::json!({
                "role": "guardian",
                "account": "moon",
                "label": "kimi",
                "windows": [
                    {"label": "month", "used_percentage": 17, "resets_at": 1_800_000_000u64},
                    {"label": "5h", "used_percentage": 100},
                ],
            })
        );
    }

    #[test]
    fn usage_without_data_has_no_windows() {
        assert_eq!(
            usage(None).json(),
            serde_json::json!({"role": "guardian", "account": "moon", "label": "kimi"})
        );
    }

    fn erased(fake: &Arc<Fake>) -> Arc<dyn QuotaSource> {
        fake.clone()
    }

    async fn settle() {
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
    }

    fn ratio(rx: &watch::Receiver<QuotaSnapshot>) -> Option<f64> {
        rx.borrow()
            .quota
            .as_ref()
            .map(|quota| quota.windows[0].used_ratio)
    }

    #[tokio::test(start_paused = true)]
    async fn fetches_at_once_then_every_interval() {
        let fake = Fake::new("k", &[Ok(0.1), Ok(0.2), Ok(0.3)]);
        let mut poller = QuotaPoller::new();
        let rx = poller.subscribe();
        poller.point(Some(&erased(&fake)));
        settle().await;
        assert_eq!(fake.calls(), 1);
        assert_eq!(ratio(&rx), Some(0.1));
        assert_eq!(rx.borrow().name, "k");

        tokio::time::advance(POLL_INTERVAL + Duration::from_secs(1)).await;
        settle().await;
        assert_eq!(fake.calls(), 2);
        assert_eq!(ratio(&rx), Some(0.2));
    }

    #[tokio::test(start_paused = true)]
    async fn pointing_at_the_same_source_does_not_refetch() {
        let fake = Fake::new("k", &[Ok(0.1), Ok(0.2)]);
        let source = erased(&fake);
        let mut poller = QuotaPoller::new();
        poller.point(Some(&source));
        settle().await;
        poller.point(Some(&source));
        settle().await;
        assert_eq!(fake.calls(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_new_source_is_fetched_at_once() {
        let a = Fake::new("a", &[Ok(0.1)]);
        let b = Fake::new("b", &[Ok(0.7)]);
        let mut poller = QuotaPoller::new();
        let rx = poller.subscribe();
        poller.point(Some(&erased(&a)));
        settle().await;
        poller.point(Some(&erased(&b)));
        settle().await;
        assert_eq!(b.calls(), 1);
        assert_eq!(ratio(&rx), Some(0.7));
        assert_eq!(rx.borrow().name, "b");
    }

    #[tokio::test(start_paused = true)]
    async fn a_failure_keeps_the_last_good_value_until_it_is_stale() {
        let fake = Fake::new("k", &[Ok(0.4)]);
        let mut poller = QuotaPoller::new();
        let rx = poller.subscribe();
        poller.point(Some(&erased(&fake)));
        settle().await;

        tokio::time::advance(POLL_INTERVAL + Duration::from_secs(1)).await;
        settle().await;
        assert_eq!(ratio(&rx), Some(0.4));
        let error = rx.borrow().error.clone().expect("the failure is published");
        assert_eq!(error.to_string(), "script exhausted");

        // The value is from t=0 and the fetches fall at 61 s, 121 s and so on: the one at 541 s
        // still finds it fresh, the one at 601 s finds it past ten minutes.
        for _ in 0..8 {
            tokio::time::advance(POLL_INTERVAL).await;
            settle().await;
        }
        assert_eq!(ratio(&rx), Some(0.4));
        tokio::time::advance(POLL_INTERVAL).await;
        settle().await;
        assert_eq!(ratio(&rx), None);
        assert!(rx.borrow().error.is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn a_success_clears_the_failure() {
        let fake = Fake::new("k", &[Err("down"), Ok(0.5)]);
        let mut poller = QuotaPoller::new();
        let rx = poller.subscribe();
        poller.point(Some(&erased(&fake)));
        settle().await;
        assert!(rx.borrow().error.is_some());
        assert_eq!(ratio(&rx), None);

        tokio::time::advance(POLL_INTERVAL + Duration::from_secs(1)).await;
        settle().await;
        assert!(rx.borrow().error.is_none());
        assert_eq!(ratio(&rx), Some(0.5));
    }

    #[tokio::test(start_paused = true)]
    async fn no_source_clears_the_slot() {
        let fake = Fake::new("k", &[Ok(0.4)]);
        let mut poller = QuotaPoller::new();
        let rx = poller.subscribe();
        poller.point(Some(&erased(&fake)));
        settle().await;
        assert_eq!(ratio(&rx), Some(0.4));

        poller.point(None);
        settle().await;
        assert_eq!(ratio(&rx), None);
        assert!(rx.borrow().name.is_empty());

        tokio::time::advance(POLL_INTERVAL * 3).await;
        settle().await;
        assert_eq!(fake.calls(), 1);
    }
}
