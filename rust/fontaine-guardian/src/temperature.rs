//! The session temperature: how much pressure the denials of an AFK stretch put on the
//! permission floor, as a number that cools only while independent work is allowed. It counts
//! gate decisions, not seconds, so the same ledger always gives the same value. It feeds no allow
//! path and removes no evidence; its one effect is to stop a session that runs too hot.

/// How much a denial adds, by how serious the thing it refused was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Low,
    Medium,
    High,
}

impl Severity {
    pub fn weight(self) -> f64 {
        match self {
            Severity::Low => 1.0,
            Severity::Medium => 2.0,
            Severity::High => 3.0,
        }
    }
}

/// The factor every earlier denial is multiplied by on each later gate decision.
pub const DECAY: f64 = 0.9;
/// Added to a denied call that repeats an earlier denied call exactly.
pub const EXACT_RETRY_BONUS: f64 = 2.0;
/// Added to a denied call that reaches the target of an earlier denial by another tool.
pub const SAME_TARGET_BONUS: f64 = 3.0;
/// What a denial that only shows guardian trouble (an error, a timeout, an unparseable answer)
/// adds, since it says little about the agent.
pub const FAIL_CLOSED_WEIGHT: f64 = 0.5;
/// The temperature at which an AFK stretch is stopped.
pub const DEFAULT_CEILING: f64 = 12.0;

/// One denial and what it added.
#[derive(Clone, Debug, PartialEq)]
pub struct Contribution {
    /// The ledger sequence number of the denial.
    pub seq: u64,
    /// Severity plus bonuses, before any cooling.
    pub weight: f64,
    /// The decision count when it was added, which places it for cooling.
    pub step: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Temperature {
    current: f64,
    peak: f64,
    peak_step: u64,
    peak_seq: Option<u64>,
    step: u64,
    contributions: Vec<Contribution>,
}

impl Temperature {
    /// One gate decision, allowed or denied, has been made: everything cools by [`DECAY`].
    pub fn step(&mut self) {
        self.step += 1;
        self.current *= DECAY;
    }

    /// A denial of `weight` at ledger position `seq`. Call [`Temperature::step`] for the
    /// decision first. Returns the new current value.
    pub fn denial(&mut self, seq: u64, weight: f64) -> f64 {
        self.current += weight;
        self.contributions.push(Contribution {
            seq,
            weight,
            step: self.step,
        });
        if self.current > self.peak {
            self.peak = self.current;
            self.peak_step = self.step;
            self.peak_seq = Some(seq);
        }
        self.current
    }

    pub fn current(&self) -> f64 {
        self.current
    }

    /// The highest value reached; cooling never lowers it.
    pub fn peak(&self) -> f64 {
        self.peak
    }

    pub fn peak_step(&self) -> u64 {
        self.peak_step
    }

    /// The denial that set the peak.
    pub fn peak_seq(&self) -> Option<u64> {
        self.peak_seq
    }

    /// Every denial of the stretch, oldest first. Cooling never removes one.
    pub fn contributions(&self) -> &[Contribution] {
        &self.contributions
    }

    /// What a contribution is worth now, after the decisions since it was added.
    pub fn remaining(&self, contribution: &Contribution) -> f64 {
        contribution.weight * DECAY.powi((self.step - contribution.step) as i32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn a_denial_cools_by_the_decay_on_every_later_decision() {
        let mut t = Temperature::default();
        t.step();
        t.denial(1, Severity::Medium.weight());
        assert!(approx(t.current(), 2.0));
        t.step();
        t.step();
        assert!(approx(t.current(), 2.0 * DECAY * DECAY));
    }

    #[test]
    fn the_peak_never_falls_when_the_value_cools() {
        let mut t = Temperature::default();
        t.step();
        t.denial(1, 3.0);
        let peak = t.peak();
        for _ in 0..30 {
            t.step();
        }
        assert!(t.current() < 0.2);
        assert!(approx(t.peak(), peak));
        assert_eq!(t.peak_seq(), Some(1));
    }

    #[test]
    fn contributions_survive_cooling() {
        let mut t = Temperature::default();
        t.step();
        t.denial(7, 2.0);
        for _ in 0..50 {
            t.step();
        }
        assert_eq!(t.contributions().len(), 1);
        assert_eq!(t.contributions()[0].seq, 7);
        assert!(t.remaining(&t.contributions()[0]) < 0.02);
    }

    #[test]
    fn a_cluster_of_high_denials_outscores_one_isolated_low_denial() {
        let mut cluster = Temperature::default();
        for seq in 0..4 {
            cluster.step();
            cluster.denial(seq, Severity::High.weight());
        }
        let mut isolated = Temperature::default();
        isolated.step();
        isolated.denial(0, Severity::Low.weight());
        for _ in 0..3 {
            isolated.step();
        }
        assert!(cluster.current() > isolated.current() * 5.0);
        assert!(cluster.peak() > DEFAULT_CEILING * 0.5);
    }

    #[test]
    fn bonuses_are_part_of_the_weight_a_caller_passes() {
        let mut t = Temperature::default();
        t.step();
        t.denial(1, Severity::Medium.weight() + EXACT_RETRY_BONUS);
        assert!(approx(t.current(), 4.0));
    }
}
