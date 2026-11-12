//! What the session's model calls have cost, by the part of Fontaine that made them.

use std::collections::BTreeMap;
use std::fmt;
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
    Advisor,
}

impl UsageSource {
    pub const ALL: [UsageSource; 7] = [
        UsageSource::Main,
        UsageSource::Subagent,
        UsageSource::Guardian,
        UsageSource::Goal,
        UsageSource::Compaction,
        UsageSource::SideQuestion,
        UsageSource::Advisor,
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

/// Which model served a call and under which role it was asked for.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Served {
    /// The role whose list the call was served from; `None` for a model named directly.
    pub role: Option<String>,
    /// `<account>/<model>`.
    pub model: String,
}

impl Served {
    pub fn new(role: Option<&str>, model: impl fmt::Display) -> Served {
        Served {
            role: role.map(str::to_string),
            model: model.to_string(),
        }
    }

    /// A model named directly, with no role.
    pub fn direct(model: impl fmt::Display) -> Served {
        Served::new(None, model)
    }
}

/// The account of an `<account>/<model>` label; a label with no slash is taken as the account.
pub fn account_of(model: &str) -> &str {
    model.split_once('/').map_or(model, |(account, _)| account)
}

impl Served {
    pub fn account(&self) -> &str {
        account_of(&self.model)
    }
}

impl fmt::Display for Served {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.role {
            Some(role) => write!(f, "{role} · {}", self.model),
            None => f.write_str(&self.model),
        }
    }
}

/// Every model call the session has made, one bucket per source and one per role and model.
#[derive(Default)]
pub struct UsageLedger {
    by_source: Mutex<[Bucket; 7]>,
    by_model: Mutex<BTreeMap<Served, Bucket>>,
}

impl UsageLedger {
    pub fn record(&self, source: UsageSource, usage: &Usage, sent_request: bool, served: &Served) {
        lock(&self.by_source, "usage ledger")[source.index()].add(usage, sent_request);
        lock(&self.by_model, "usage ledger by model")
            .entry(served.clone())
            .or_default()
            .add(usage, sent_request);
    }

    /// One bucket per role and model that served a call, in name order.
    pub fn by_model(&self) -> Vec<(Served, Bucket)> {
        lock(&self.by_model, "usage ledger by model")
            .iter()
            .map(|(served, bucket)| (served.clone(), *bucket))
            .collect()
    }

    pub fn bucket(&self, source: UsageSource) -> Bucket {
        lock(&self.by_source, "usage ledger")[source.index()]
    }

    pub fn snapshot(&self) -> [Bucket; 7] {
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
        let served = Served::direct("a/m");
        ledger.record(UsageSource::Main, &usage(100, 10, Some(0.5)), true, &served);
        ledger.record(UsageSource::Subagent, &usage(40, 5, None), true, &served);
        ledger.record(
            UsageSource::Guardian,
            &usage(7, 1, Some(0.25)),
            true,
            &served,
        );

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
        ledger.record(
            UsageSource::Main,
            &usage(1, 1, None),
            true,
            &Served::default(),
        );
        assert_eq!(ledger.total().cost_usd, None);
    }

    #[test]
    fn calls_are_also_bucketed_by_role_and_model() {
        let ledger = UsageLedger::default();
        let coding = Served::new(Some("coding"), "a/sonnet");
        let fallback = Served::new(Some("coding"), "b/kimi");
        let direct = Served::direct("a/sonnet");
        ledger.record(
            UsageSource::Subagent,
            &usage(10, 1, Some(0.5)),
            true,
            &coding,
        );
        ledger.record(
            UsageSource::Subagent,
            &usage(20, 2, Some(0.25)),
            true,
            &coding,
        );
        ledger.record(UsageSource::Subagent, &usage(5, 1, None), true, &fallback);
        ledger.record(
            UsageSource::Guardian,
            &usage(7, 1, Some(0.1)),
            true,
            &direct,
        );

        let rows = ledger.by_model();
        assert_eq!(rows.len(), 3);
        let find = |served: &Served| rows.iter().find(|(s, _)| s == served).unwrap().1;
        assert_eq!(find(&coding).calls, 2);
        assert_eq!(find(&coding).cost_usd, Some(0.75));
        assert_eq!(find(&fallback).unpriced_tokens, 6);
        assert_eq!(find(&direct).input_tokens, 7);
        assert_eq!(coding.to_string(), "coding · a/sonnet");
        assert_eq!(direct.to_string(), "a/sonnet");
        // The per-source view is unchanged by the breakdown.
        assert_eq!(ledger.bucket(UsageSource::Subagent).calls, 3);
    }
}
