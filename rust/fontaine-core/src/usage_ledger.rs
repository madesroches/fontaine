//! What the session's model calls have cost, by the part of Fontaine that made them.

use std::sync::Mutex;

use fontaine_tools::lock::lock;
use rig_core::completion::Usage;

/// Who made a model call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageSource {
    Main,
    Subagent,
    Guardian,
    Goal,
    Compaction,
    SideQuestion,
}

impl UsageSource {
    pub const ALL: [UsageSource; 6] = [
        UsageSource::Main,
        UsageSource::Subagent,
        UsageSource::Guardian,
        UsageSource::Goal,
        UsageSource::Compaction,
        UsageSource::SideQuestion,
    ];

    pub fn index(self) -> usize {
        self as usize
    }
}

/// The tokens, cost and call count of one source.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Bucket {
    /// Model requests sent, with a guardian adjudication or a goal evaluation counting once
    /// however many requests it took.
    pub calls: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    /// Only the priced calls: a lower bound when `unpriced_tokens` is above zero.
    pub cost_usd: Option<f64>,
    /// Input and output tokens (no cache tokens) of the calls that had no price.
    pub unpriced_tokens: u64,
}

impl Bucket {
    /// Adds one call. `sent_request` is false for a record that stands for no request, such
    /// as a guardian answer served from its cache: the tokens and cost still accrue.
    pub fn add(&mut self, usage: &Usage, sent_request: bool) {
        if sent_request {
            self.calls += 1;
        }
        let (input, output) = (
            usage.input_tokens.unwrap_or(0),
            usage.output_tokens.unwrap_or(0),
        );
        self.input_tokens += input;
        self.output_tokens += output;
        self.cache_read_input_tokens += usage.cached_input_tokens.unwrap_or(0);
        self.cache_creation_input_tokens += usage.cache_creation_input_tokens.unwrap_or(0);
        match usage.cost {
            Some(cost) => *self.cost_usd.get_or_insert(0.0) += cost.total,
            None => self.unpriced_tokens += input + output,
        }
    }

    /// Adds another bucket's figures to this one.
    pub fn merge(&mut self, other: &Bucket) {
        self.calls += other.calls;
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_read_input_tokens += other.cache_read_input_tokens;
        self.cache_creation_input_tokens += other.cache_creation_input_tokens;
        self.unpriced_tokens += other.unpriced_tokens;
        if let Some(cost) = other.cost_usd {
            *self.cost_usd.get_or_insert(0.0) += cost;
        }
    }
}

/// Every model call the session has made, one bucket per source.
#[derive(Default)]
pub struct UsageLedger {
    by_source: Mutex<[Bucket; 6]>,
}

impl UsageLedger {
    pub fn record(&self, source: UsageSource, usage: &Usage, sent_request: bool) {
        lock(&self.by_source, "usage ledger")[source.index()].add(usage, sent_request);
    }

    pub fn bucket(&self, source: UsageSource) -> Bucket {
        lock(&self.by_source, "usage ledger")[source.index()]
    }

    pub fn snapshot(&self) -> [Bucket; 6] {
        *lock(&self.by_source, "usage ledger")
    }

    /// All sources summed.
    pub fn total(&self) -> Bucket {
        total(&self.snapshot())
    }
}

/// The buckets summed.
pub fn total(buckets: &[Bucket]) -> Bucket {
    let mut total = Bucket::default();
    for bucket in buckets {
        total.merge(bucket);
    }
    total
}

#[cfg(test)]
mod tests {
    use rig_core::completion::Cost;

    use super::*;

    fn usage(input: u64, output: u64, cost: Option<f64>) -> Usage {
        let mut usage = Usage::new();
        usage.input_tokens = Some(input);
        usage.output_tokens = Some(output);
        usage.cost = cost.map(Cost::from_total);
        usage
    }

    #[test]
    fn add_sums_tokens_and_cache_tokens() {
        let mut bucket = Bucket::default();
        let mut call = usage(100, 10, Some(0.5));
        call.cached_input_tokens = Some(60);
        call.cache_creation_input_tokens = Some(7);
        bucket.add(&call, true);
        bucket.add(&call, true);
        assert_eq!(bucket.calls, 2);
        assert_eq!(bucket.input_tokens, 200);
        assert_eq!(bucket.output_tokens, 20);
        assert_eq!(bucket.cache_read_input_tokens, 120);
        assert_eq!(bucket.cache_creation_input_tokens, 14);
    }

    #[test]
    fn cost_accrues_only_from_priced_calls_and_unpriced_tokens_exclude_cache() {
        let mut bucket = Bucket::default();
        bucket.add(&usage(10, 5, None), true);
        assert_eq!(bucket.cost_usd, None);
        assert_eq!(bucket.unpriced_tokens, 15);

        bucket.add(&usage(100, 10, Some(0.25)), true);
        let mut unpriced = usage(20, 2, None);
        unpriced.cached_input_tokens = Some(500);
        bucket.add(&unpriced, true);
        assert_eq!(bucket.cost_usd, Some(0.25));
        assert_eq!(bucket.unpriced_tokens, 37);
    }

    #[test]
    fn calls_count_only_usage_marked_as_a_sent_request() {
        let mut bucket = Bucket::default();
        bucket.add(&Usage::new(), false);
        bucket.add(&usage(10, 1, Some(0.1)), false);
        assert_eq!(bucket.calls, 0);
        assert_eq!(bucket.input_tokens, 10);
        assert_eq!(bucket.cost_usd, Some(0.1));
        bucket.add(&usage(10, 1, Some(0.1)), true);
        assert_eq!(bucket.calls, 1);
    }

    #[test]
    fn record_lands_in_its_source_and_total_propagates_partial() {
        let ledger = UsageLedger::default();
        ledger.record(UsageSource::Main, &usage(100, 10, Some(0.5)), true);
        ledger.record(UsageSource::Subagent, &usage(40, 5, None), true);
        ledger.record(UsageSource::Guardian, &usage(7, 1, Some(0.25)), true);

        assert_eq!(ledger.bucket(UsageSource::Main).input_tokens, 100);
        assert_eq!(ledger.bucket(UsageSource::Subagent).unpriced_tokens, 45);
        assert_eq!(ledger.bucket(UsageSource::Goal), Bucket::default());

        let total = ledger.total();
        assert_eq!(total.calls, 3);
        assert_eq!(total.input_tokens, 147);
        assert_eq!(total.cost_usd, Some(0.75));
        assert_eq!(total.unpriced_tokens, 45);
    }

    #[test]
    fn total_has_no_cost_when_nothing_was_priced() {
        let ledger = UsageLedger::default();
        ledger.record(UsageSource::Main, &usage(1, 1, None), true);
        assert_eq!(ledger.total().cost_usd, None);
    }
}
