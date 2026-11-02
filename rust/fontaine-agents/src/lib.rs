//! Parallel subagents on rig-agent: the `agent` and `send_message` tools, the team task board
//! and its tools, the agent-type
//! registry, per-agent budgets and the live tree of agents. Running a child is the
//! [`runner::ChildRunner`]'s job, implemented by `fontaine-core`.

pub mod budget;
pub mod effort;
pub mod mode;
pub mod registry;
pub mod root_ledger;
pub mod runner;
pub mod spawn;
pub mod tasks;
pub mod tools;
pub mod tree;
