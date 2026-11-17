//! How the cost a model call reports is treated.
//!
//! The type sits outside `core` because the guardian's `Settings` carries it, and that module may
//! not name `core`; `crate::core::models` re-exports it as the path callers use.

/// How the cost a model call reports is treated: whether it is counted at all, and whether the
/// figure describes a bill or an estimate of one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CostBasis {
    /// The account bills per token at the price rig's catalog lists.
    #[default]
    Billed,
    /// The account bills in its own units, so a listed catalog price is an estimate of what the
    /// same tokens would cost on the served vendor's API.
    Estimated,
    /// No per-token price applies: any cost a response carries is dropped.
    Unpriced,
}

impl CostBasis {
    /// Whether a cost is counted at all; [`CostBasis::Estimated`] says how.
    pub fn counted(self) -> bool {
        self != CostBasis::Unpriced
    }

    /// Whether the figure is an estimate rather than a bill.
    pub fn estimated(self) -> bool {
        self == CostBasis::Estimated
    }
}
