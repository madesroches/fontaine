//! Instruction files that apply to a file the agent reads or writes: the nested `CLAUDE.md`,
//! `FONTAINE.md` and `AGENTS.md` of the directories on the way down from the primary root, and
//! the rules whose `paths` match. They reach the agent as a note on the tool result, once per
//! agent.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use fontaine_tools::gate::{CallView, ROOT_AGENT};
use fontaine_tools::lock::lock_recovering;
use fontaine_tools::result_pipeline::{ResultStage, ToolOutcome, append_note};
use futures::future::BoxFuture;

use super::imports::Loader;
use super::{Loaded, PathRule, Seen, Sources, has_counting_file};
use crate::destructive::touched_paths;
use crate::floor::under;
use crate::permissions::Approver;

/// What one agent has been given so far.
#[derive(Default)]
struct Entry {
    seen: Seen,
    dirs: HashSet<PathBuf>,
    loaded: Vec<PathBuf>,
}

struct State {
    sources: Sources,
    launch: Seen,
    launch_rules: Vec<PathRule>,
    /// Rules found in nested directories, registered once for every agent.
    nested_rules: Vec<PathRule>,
    scanned: HashSet<PathBuf>,
    agents_mode: bool,
    agents: HashMap<String, Entry>,
    warned: HashSet<String>,
}

impl State {
    fn fresh_entry(&self) -> Entry {
        Entry {
            seen: self.launch.clone(),
            ..Entry::default()
        }
    }
}

pub struct InstructionStage {
    // Advisory state: a poisoned lock at worst repeats or misses a note, and decides nothing
    // about permissions, so it recovers.
    state: Mutex<State>,
    approver: Arc<dyn Approver>,
}

impl InstructionStage {
    /// A stage with an empty launch set; `reseed` gives it the real one.
    pub fn new(sources: Sources, approver: Arc<dyn Approver>) -> InstructionStage {
        InstructionStage {
            state: Mutex::new(State {
                sources,
                launch: Seen::default(),
                launch_rules: Vec::new(),
                nested_rules: Vec::new(),
                scanned: HashSet::new(),
                agents_mode: false,
                agents: HashMap::new(),
                warned: HashSet::new(),
            }),
            approver,
        }
    }

    /// Takes the launch set of a rebuilt agent. Files an agent already received on demand stay
    /// received: the conversation survives a rebuild, and so do the tool results that carry them.
    pub fn reseed(&self, sources: Sources, loaded: &Loaded) {
        let mut state = lock_recovering(&self.state);
        state.sources = sources;
        state.launch = loaded.seen.clone();
        state.launch_rules = loaded.path_rules.clone();
        state.agents_mode = loaded.agents_mode;
        let launch = state.launch.clone();
        for entry in state.agents.values_mut() {
            entry.seen.union(&launch);
        }
    }

    /// The main agent's context was emptied or compacted: its on-demand files load again.
    pub fn reset_main(&self) {
        let mut state = lock_recovering(&self.state);
        let entry = state.fresh_entry();
        state.agents.insert(ROOT_AGENT.to_string(), entry);
    }

    /// The files the main agent received on demand, in the order it received them.
    pub fn loaded_on_demand(&self) -> Vec<PathBuf> {
        lock_recovering(&self.state)
            .agents
            .get(ROOT_AGENT)
            .map(|entry| entry.loaded.clone())
            .unwrap_or_default()
    }

    /// Reports each distinct warning once for the session.
    pub fn warn_once(&self, warnings: &[String]) {
        let fresh: Vec<&String> = {
            let mut state = lock_recovering(&self.state);
            warnings
                .iter()
                .filter(|w| state.warned.insert((*w).clone()))
                .collect()
        };
        for warning in fresh {
            self.approver.warn(warning);
        }
    }

    fn note_for(&self, agent: &str, tool: &str, args: &serde_json::Value) -> Option<String> {
        let (files, warnings, target) = {
            let mut guard = lock_recovering(&self.state);
            let state = &mut *guard;
            let scope = state.sources.scope.clone();
            let target = PathBuf::from(touched_paths(tool, args, &scope).into_iter().next()?);
            let relative = under(&target, scope.primary())?;
            let mut dir = scope.primary().to_path_buf();
            let mut dirs = Vec::new();
            for part in relative.parent()?.components() {
                dir.push(part);
                dirs.push(dir.clone());
            }

            let mut entry = match state.agents.remove(agent) {
                Some(entry) => entry,
                None => state.fresh_entry(),
            };
            let mut loader = Loader::new(&state.sources, std::mem::take(&mut entry.seen));
            for dir in dirs {
                let first_scan = state.scanned.insert(dir.clone());
                if !entry.dirs.insert(dir.clone()) && !first_scan {
                    continue;
                }
                let before = loader.path_rules.len();
                let agents =
                    state.agents_mode && !has_counting_file(&dir, state.sources.home.as_deref());
                loader.walk_dir(&dir, agents);
                if first_scan {
                    state.nested_rules.extend(loader.path_rules.drain(before..));
                } else {
                    loader.path_rules.truncate(before);
                }
            }
            for rule in state.launch_rules.iter().chain(&state.nested_rules) {
                if rule.matches(&target) {
                    loader.take_rule(rule);
                }
            }
            let (files, warnings, _, seen) = loader.finish();
            entry.seen = seen;
            entry
                .loaded
                .extend(files.iter().map(|file| file.path.clone()));
            state.agents.insert(agent.to_string(), entry);
            (files, warnings, target)
        };
        self.warn_once(&warnings);
        if files.is_empty() {
            return None;
        }
        let body = files
            .iter()
            .map(|file| {
                format!(
                    "Instructions from {}:\n\n{}",
                    file.path.display(),
                    file.text.trim_end()
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        Some(format!(
            "[Harness note: instruction files that apply to {}, loaded from disk.]\n\n{body}",
            display(&target)
        ))
    }
}

fn display(path: &Path) -> String {
    path.display().to_string()
}

impl ResultStage for InstructionStage {
    fn process<'a>(
        &'a self,
        call: CallView<'a>,
        outcome: ToolOutcome,
    ) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            if outcome.is_err() || !matches!(call.tool, "read" | "write" | "edit") {
                return outcome;
            }
            match self.note_for(call.agent, call.tool, call.args) {
                Some(note) => append_note(outcome, &note),
                None => outcome,
            }
        })
    }
}
