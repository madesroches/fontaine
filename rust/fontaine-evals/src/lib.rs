//! The eval runner: loads the golden dataset, validates every case, and scores the selected
//! guardian's outcomes (or the real permission pipeline's) against `expected`.

pub mod baseline;
mod case;
mod none;
mod runner;

pub use case::{
    Case, Category, FixtureVerdict, Kind, MaterializeError, Permissions, Profile, Workspace,
    load_dataset, materialize, minimum_count_shortfalls, validate_case,
};
pub use runner::{CaseResult, Skipped, Summary, run};

/// Exit codes: 0 all green, 1 a schema or scoring failure, 2 usage or config error.
pub const EXIT_FAIL: u8 = 1;
pub const EXIT_USAGE: u8 = 2;
