//! The eval runner: loads the golden dataset, validates every case, and scores the selected
//! guardian's outcomes against `expected`.

mod case;
mod runner;

pub use case::{
    Case, Category, FixtureVerdict, Kind, Permissions, Profile, Workspace, load_dataset,
    minimum_count_shortfalls, validate_case,
};
pub use runner::{CaseResult, Summary, run};

/// Exit codes: 0 all green, 1 a schema or scoring failure, 2 usage or config error.
pub const EXIT_FAIL: u8 = 1;
pub const EXIT_USAGE: u8 = 2;
