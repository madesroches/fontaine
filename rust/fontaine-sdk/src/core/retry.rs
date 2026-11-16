//! How a failed model call is retried: attempts, exponential backoff with jitter and a bound on
//! the total time spent waiting. A pure policy; `turn::drive` owns the loop and the sleeping.

use std::time::Duration;

/// A quota-class failure (402, 429, 529) is a model-list fall-through candidate, so its retries
/// are cut to what a throttle needs to clear.
const QUOTA_MAX_RETRIES: u32 = 2;
const QUOTA_MAX_TOTAL_DELAY: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Retries on top of the first attempt.
    pub max_retries: u32,
    /// The wait before the first retry; it doubles each time.
    pub base_delay: Duration,
    /// The longest single wait, `Retry-After` included.
    pub max_delay: Duration,
    /// The longest the retries of one turn may wait in total.
    pub max_total_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy {
            max_retries: 4,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(30),
            max_total_delay: Duration::from_secs(120),
        }
    }
}

impl RetryPolicy {
    /// A policy that retries without waiting, for tests that script several failures.
    pub fn immediate(max_retries: u32) -> RetryPolicy {
        RetryPolicy {
            max_retries,
            base_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
            max_total_delay: Duration::ZERO,
        }
    }

    /// The tighter budget for quota-class failures.
    pub fn for_quota(&self) -> RetryPolicy {
        RetryPolicy {
            max_retries: self.max_retries.min(QUOTA_MAX_RETRIES),
            max_total_delay: self.max_total_delay.min(QUOTA_MAX_TOTAL_DELAY),
            ..*self
        }
    }

    /// The wait before retry number `attempt + 1`, or `None` when the failure should settle now:
    /// the wait would pass `max_delay` (a `Retry-After` longer than that is the server asking for
    /// more than we will give) or push the total past `max_total_delay`. `jitter` is the factor
    /// applied to the exponential step, clamped to `[0.5, 1.0]`.
    pub fn next_delay(
        &self,
        attempt: u32,
        retry_after: Option<Duration>,
        elapsed: Duration,
        jitter: f64,
    ) -> Option<Duration> {
        let factor = if jitter.is_nan() {
            1.0
        } else {
            jitter.clamp(0.5, 1.0)
        };
        let step = self
            .base_delay
            .saturating_mul(1u32.checked_shl(attempt).unwrap_or(u32::MAX))
            .min(self.max_delay);
        let mut delay = step.mul_f64(factor);
        if let Some(asked) = retry_after {
            delay = delay.max(asked);
        }
        if delay > self.max_delay || elapsed.saturating_add(delay) > self.max_total_delay {
            return None;
        }
        Some(delay)
    }
}

/// A jitter factor in `[0.5, 1.0)` from a fresh v4 uuid, which saves a random-number dependency.
pub fn jitter() -> f64 {
    let bytes = *uuid::Uuid::new_v4().as_bytes();
    let fraction = f64::from(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        / (f64::from(u32::MAX) + 1.0);
    0.5 + fraction / 2.0
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZERO: Duration = Duration::ZERO;

    #[test]
    fn the_delay_grows_exponentially() {
        let p = RetryPolicy::default();
        let d = |a| p.next_delay(a, None, ZERO, 1.0).unwrap();
        assert_eq!(d(0), Duration::from_millis(500));
        assert_eq!(d(1), Duration::from_secs(1));
        assert_eq!(d(2), Duration::from_secs(2));
    }

    #[test]
    fn the_delay_is_capped_by_max_delay() {
        let p = RetryPolicy::default();
        assert_eq!(p.next_delay(10, None, ZERO, 1.0), Some(p.max_delay));
        assert_eq!(p.next_delay(200, None, ZERO, 1.0), Some(p.max_delay));
    }

    #[test]
    fn jitter_stays_within_bounds() {
        let p = RetryPolicy::default();
        assert_eq!(
            p.next_delay(0, None, ZERO, 0.0),
            Some(Duration::from_millis(250))
        );
        assert_eq!(
            p.next_delay(0, None, ZERO, 7.0),
            Some(Duration::from_millis(500))
        );
        for _ in 0..100 {
            let j = jitter();
            assert!((0.5..1.0).contains(&j), "{j}");
        }
    }

    #[test]
    fn a_retry_after_raises_the_wait() {
        let p = RetryPolicy::default();
        let asked = Duration::from_secs(7);
        assert_eq!(p.next_delay(0, Some(asked), ZERO, 1.0), Some(asked));
    }

    #[test]
    fn a_retry_after_above_max_delay_settles_the_failure() {
        let p = RetryPolicy::default();
        let asked = p.max_delay + Duration::from_secs(1);
        assert_eq!(p.next_delay(0, Some(asked), ZERO, 1.0), None);
    }

    #[test]
    fn the_total_delay_cap_settles_the_failure() {
        let p = RetryPolicy::default();
        let nearly = p.max_total_delay - Duration::from_millis(100);
        assert_eq!(p.next_delay(0, None, nearly, 1.0), None);
        assert!(p.next_delay(0, None, ZERO, 1.0).is_some());
    }

    #[test]
    fn quota_failures_get_a_shorter_budget() {
        let q = RetryPolicy::default().for_quota();
        assert_eq!(q.max_retries, 2);
        assert_eq!(q.max_total_delay, Duration::from_secs(10));
        let tight = RetryPolicy {
            max_retries: 1,
            max_total_delay: Duration::from_secs(3),
            ..RetryPolicy::default()
        };
        assert_eq!(tight.for_quota(), tight);
    }
}
