//! Per-agent spend and the caps it is held to. A cap that cannot be measured is inactive: a
//! cost cap on a model with no catalog price never fires, and a token cap, if set, still binds.

/// What one agent has used: its own model calls plus the guardian calls made for it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Spend {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// `None` until a priced call is counted.
    pub cost_usd: Option<f64>,
    /// Tokens of the calls that carried no price, which `cost_usd` therefore leaves out.
    pub unpriced_tokens: u64,
}

impl Spend {
    pub fn tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }

    pub fn add(&mut self, other: &Spend) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.unpriced_tokens += other.unpriced_tokens;
        if let Some(cost) = other.cost_usd {
            *self.cost_usd.get_or_insert(0.0) += cost;
        }
    }

    /// Whether `cost_usd` is a lower bound: some calls were priced and some were not.
    pub fn is_partial_cost(&self) -> bool {
        self.cost_usd.is_some() && self.unpriced_tokens > 0
    }
}

/// A cap on one agent, or on the whole tree.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Budget {
    pub tokens: Option<u64>,
    pub cost_usd: Option<f64>,
}

/// Why an agent was stopped.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Exhausted {
    Tokens { spent: u64, cap: u64 },
    Cost { spent: f64, cap: f64 },
    SessionTokens { spent: u64, cap: u64 },
    SessionCost { spent: f64, cap: f64 },
}

impl Exhausted {
    pub fn describe(&self) -> String {
        self.describe_with(|n| n.to_string())
    }

    /// Like `describe`, formatting token counts with `count`; cost figures stay as they are.
    pub fn describe_with(&self, count: impl Fn(u64) -> String) -> String {
        match self {
            Exhausted::Tokens { spent, cap } => {
                format!(
                    "its token budget is spent ({} of {})",
                    count(*spent),
                    count(*cap)
                )
            }
            Exhausted::Cost { spent, cap } => {
                format!("its cost budget is spent (${spent:.4} of ${cap:.2})")
            }
            Exhausted::SessionTokens { spent, cap } => {
                format!(
                    "the session's agent token budget is spent ({} of {})",
                    count(*spent),
                    count(*cap)
                )
            }
            Exhausted::SessionCost { spent, cap } => {
                format!("the session's agent cost budget is spent (${spent:.4} of ${cap:.2})")
            }
        }
    }
}

impl Budget {
    /// Whether `spend` has used up this budget. The cap is crossed when the spend is above it.
    pub fn exhausted(&self, spend: &Spend, session: bool) -> Option<Exhausted> {
        if let Some(cap) = self.tokens
            && spend.tokens() > cap
        {
            let spent = spend.tokens();
            return Some(if session {
                Exhausted::SessionTokens { spent, cap }
            } else {
                Exhausted::Tokens { spent, cap }
            });
        }
        if let (Some(cap), Some(spent)) = (self.cost_usd, spend.cost_usd)
            && spent > cap
        {
            return Some(if session {
                Exhausted::SessionCost { spent, cap }
            } else {
                Exhausted::Cost { spent, cap }
            });
        }
        None
    }

    /// Whether a cost cap is set but this spend has no price to measure it by.
    pub fn cost_cap_inactive(&self, spend: &Spend) -> bool {
        self.cost_usd.is_some() && spend.cost_usd.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_with_formats_token_counts_only() {
        let why = Exhausted::Tokens {
            spent: 90_887,
            cap: 50_000,
        };
        assert_eq!(why.describe(), "its token budget is spent (90887 of 50000)");
        assert_eq!(
            why.describe_with(|n| format!("<{n}>")),
            "its token budget is spent (<90887> of <50000>)"
        );
        let cost = Exhausted::Cost {
            spent: 1.0,
            cap: 2.0,
        };
        assert_eq!(cost.describe_with(|n| format!("<{n}>")), cost.describe());
    }

    fn spend(input: u64, output: u64, cost: Option<f64>) -> Spend {
        Spend {
            input_tokens: input,
            output_tokens: output,
            cost_usd: cost,
            ..Spend::default()
        }
    }

    fn unpriced(tokens: u64) -> Spend {
        Spend {
            input_tokens: tokens,
            unpriced_tokens: tokens,
            ..Spend::default()
        }
    }

    #[test]
    fn the_token_cap_binds_with_or_without_a_price() {
        let budget = Budget {
            tokens: Some(100),
            cost_usd: Some(1.0),
        };
        assert_eq!(budget.exhausted(&spend(60, 40, None), false), None);
        assert!(matches!(
            budget.exhausted(&spend(60, 41, None), false),
            Some(Exhausted::Tokens { .. })
        ));
    }

    #[test]
    fn an_unpriced_spend_never_trips_the_cost_cap() {
        let budget = Budget {
            tokens: None,
            cost_usd: Some(0.5),
        };
        assert_eq!(budget.exhausted(&spend(10_000_000, 0, None), false), None);
        assert!(budget.cost_cap_inactive(&spend(1, 1, None)));
        assert!(!budget.cost_cap_inactive(&spend(1, 1, Some(0.0))));
        assert!(matches!(
            budget.exhausted(&spend(1, 1, Some(0.6)), false),
            Some(Exhausted::Cost { .. })
        ));
    }

    #[test]
    fn spends_add_and_a_price_appears_with_the_first_priced_call() {
        let mut total = Spend::default();
        total.add(&spend(1, 2, None));
        assert_eq!(total.cost_usd, None);
        total.add(&spend(3, 4, Some(0.25)));
        total.add(&spend(0, 0, Some(0.25)));
        assert_eq!(total.tokens(), 10);
        assert_eq!(total.cost_usd, Some(0.5));
    }

    #[test]
    fn unpriced_tokens_add_up_and_mark_a_priced_total_as_partial() {
        let mut total = Spend::default();
        total.add(&unpriced(10));
        assert_eq!(total.unpriced_tokens, 10);
        assert!(!total.is_partial_cost(), "no price at all is not partial");
        total.add(&unpriced(5));
        total.add(&spend(3, 4, Some(0.25)));
        assert_eq!(total.unpriced_tokens, 15);
        assert!(total.is_partial_cost());
    }

    #[test]
    fn an_all_priced_spend_is_not_partial() {
        let mut total = Spend::default();
        total.add(&spend(3, 4, Some(0.25)));
        total.add(&spend(1, 1, Some(0.1)));
        assert!(!total.is_partial_cost());
    }
}
