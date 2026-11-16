//! The loop breaker: a model that keeps proposing what the guardian keeps refusing is handed
//! back to the human. Not configurable.

/// Consecutive guardian denials that pause `auto`.
pub const CONSECUTIVE_LIMIT: u32 = 3;
/// Guardian denials in one session that pause `auto`.
pub const TOTAL_LIMIT: u32 = 20;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoopBreaker {
    consecutive: u32,
    total: u32,
    paused: bool,
}

impl LoopBreaker {
    /// A guardian denial, of any tool. Returns whether this one paused `auto`.
    pub fn denied(&mut self) -> bool {
        self.consecutive += 1;
        self.total += 1;
        if !self.paused && (self.consecutive >= CONSECUTIVE_LIMIT || self.total >= TOTAL_LIMIT) {
            self.paused = true;
            return true;
        }
        false
    }

    /// An allowed call ends the run of consecutive denials.
    pub fn allowed(&mut self) {
        self.consecutive = 0;
    }

    pub fn paused(&self) -> bool {
        self.paused
    }

    /// The human approved a call: `auto` resumes with fresh counts.
    pub fn resume(&mut self) {
        *self = LoopBreaker::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_in_a_row_pause() {
        let mut b = LoopBreaker::default();
        assert!(!b.denied());
        assert!(!b.denied());
        assert!(!b.paused());
        assert!(b.denied());
        assert!(b.paused());
    }

    #[test]
    fn an_allow_resets_the_run_but_not_the_total() {
        let mut b = LoopBreaker::default();
        for _ in 0..19 {
            b.denied();
            b.allowed();
        }
        assert!(!b.paused());
        assert!(b.denied());
        assert!(b.paused());
    }

    #[test]
    fn approval_resumes_with_fresh_counts() {
        let mut b = LoopBreaker::default();
        for _ in 0..3 {
            b.denied();
        }
        b.resume();
        assert!(!b.paused());
        assert!(!b.denied());
        assert!(!b.denied());
        assert!(b.denied());
    }
}
