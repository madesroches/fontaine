//! The verdict cache. Only verdicts the model actually returned are stored, never a
//! fail-closed outcome, and the key holds everything the guardian was shown, so any change to
//! the call, the user's words, the rules, the model or the session's history is a miss.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::Duration;

use tokio::time::Instant;

use crate::verdict::Verdict;

/// How long a verdict stays reusable.
pub const TTL: Duration = Duration::from_secs(60);
const CAPACITY: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CacheKey {
    pub tool: String,
    pub args_digest: String,
    pub model: String,
    pub profile: &'static str,
    /// A digest of the rendered system prompt: the rules, the environment and the profile's
    /// prompt asset.
    pub rules_version: u128,
    /// The rendered request, which carries the intent, hook notes, session facts and prior
    /// denials the verdict depended on.
    pub request: String,
}

/// Two differently-seeded 64-bit hashes of `text`. This labels trusted configuration text;
/// attacker-influenced text is compared whole.
pub fn digest(text: &str) -> u128 {
    let mut a = DefaultHasher::new();
    text.hash(&mut a);
    let mut b = DefaultHasher::new();
    0xf0_u8.hash(&mut b);
    text.hash(&mut b);
    (u128::from(a.finish()) << 64) | u128::from(b.finish())
}

#[derive(Default)]
pub struct VerdictCache {
    entries: HashMap<CacheKey, (Verdict, Instant)>,
}

impl VerdictCache {
    pub fn get(&mut self, key: &CacheKey, now: Instant) -> Option<Verdict> {
        match self.entries.get(key) {
            Some((verdict, at)) if now.saturating_duration_since(*at) < TTL => {
                Some(verdict.clone())
            }
            Some(_) => {
                self.entries.remove(key);
                None
            }
            None => None,
        }
    }

    pub fn insert(&mut self, key: CacheKey, verdict: Verdict, now: Instant) {
        if self.entries.len() >= CAPACITY {
            self.entries
                .retain(|_, (_, at)| now.saturating_duration_since(*at) < TTL);
            if self.entries.len() >= CAPACITY {
                self.entries.clear();
            }
        }
        self.entries.insert(key, (verdict, now));
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule_label::RuleLabel;
    use crate::verdict::Decision;

    fn key(request: &str) -> CacheKey {
        CacheKey {
            tool: "shell".into(),
            args_digest: "d".into(),
            model: "m".into(),
            profile: "full",
            rules_version: 1,
            request: request.into(),
        }
    }

    fn verdict() -> Verdict {
        Verdict {
            decision: Decision::Allow,
            rule: RuleLabel::Allow,
            explanation: "fine".into(),
            confidence: 0.9,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_verdict_expires_after_the_ttl() {
        let mut cache = VerdictCache::default();
        cache.insert(key("a"), verdict(), Instant::now());
        tokio::time::advance(TTL - Duration::from_secs(1)).await;
        assert!(cache.get(&key("a"), Instant::now()).is_some());
        tokio::time::advance(Duration::from_secs(2)).await;
        assert!(cache.get(&key("a"), Instant::now()).is_none());
        assert!(cache.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn any_difference_in_the_key_misses() {
        let mut cache = VerdictCache::default();
        cache.insert(key("a"), verdict(), Instant::now());
        assert!(cache.get(&key("b"), Instant::now()).is_none());
        let mut other = key("a");
        other.rules_version = 2;
        assert!(cache.get(&other, Instant::now()).is_none());
    }

    #[test]
    fn the_digest_tells_texts_apart() {
        assert_ne!(digest("rules a"), digest("rules b"));
        assert_eq!(digest("same"), digest("same"));
    }
}
