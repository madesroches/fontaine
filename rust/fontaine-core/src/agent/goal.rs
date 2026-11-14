//! `/goal` on the runtime: setting and clearing, the evaluator call and the prompt of a goal
//! turn. The state and the step logic are in `crate::goal`.

use fontaine_agents::budget::Spend;
use fontaine_tools::error_chain::chain;
use fontaine_tools::gate::ROOT_AGENT;
use rig_core::DynModel;
use rig_core::operation::Completion;
use rig_memory::load_async;
use std::sync::Arc;

use super::AgentRuntime;
use super::models::announce_fall_through;
use crate::config::PermissionMode;
use crate::gate::SessionState;
use crate::goal::{
    Applied, Cleared, Continue, Evaluation, GoalCaps, GoalError, GoalEvaluator, PauseReason,
    Pending, REASON_CHARS, Step, cut, goal_prompt_text, last_user_text, transcript_of,
    visible_transcript_of,
};
use crate::ledger::{GoalEvent, Record};
use crate::model_roles::{COMPACTION_ROLE, ModelChain};
use crate::models::{ModelError, ModelHandle, ModelRef, Switcher};
use crate::permissions::Approver;
use crate::session::{CONVERSATION_LOG, CompactReason, CompactResult, PreSummary, SummaryGate};
use crate::side_question::{Answer, SideQuestion, SideQuestionError, clean_suggestion};
use crate::turn::TurnOutcome;
use crate::usage_ledger::{Served, UsageSource};
use crate::user_hooks::CompactGate;

/// Unused suggestions in a row after which none is asked for until one is accepted.
pub const SUGGESTION_BACKOFF: u32 = 3;

/// What the surface knows that decides whether a prompt suggestion is asked for.
#[derive(Clone, Copy, Debug)]
pub struct SuggestionInputs {
    pub enabled: bool,
    pub mode: PermissionMode,
    /// The last turn ended in an answer; an error or an interrupt did not.
    pub last_turn_finished: bool,
    pub completed_turns: u32,
    /// Suggestions shown and left unused since the last one was accepted.
    pub unused_streak: u32,
}

/// Whether a prompt suggestion is worth a model call now. Headless runs and child agents never
/// call this.
pub fn should_suggest(inputs: &SuggestionInputs) -> bool {
    inputs.enabled
        && inputs.mode != PermissionMode::Plan
        && inputs.last_turn_finished
        && inputs.completed_turns >= 1
        && inputs.unused_streak < SUGGESTION_BACKOFF
}

/// Everything the suggestion call needs, owned, so it can run on a task while the runtime is
/// borrowed by the input loop.
pub struct PreparedSuggestion {
    transcript: String,
    last_user_message: Option<String>,
    model: DynModel<Completion>,
    billed: bool,
    served: Served,
    state: Arc<SessionState>,
}

impl PreparedSuggestion {
    /// Asks the model, debiting the root whatever came back, and returns the cleaned line.
    /// `None` for no suggestion: a failure or a timeout is advisory here, nothing a person
    /// would act on.
    pub async fn run(self) -> Option<String> {
        let asked = SideQuestion::prompt_suggestion(self.model, self.billed)
            .ask("", &self.transcript)
            .await;
        let usage = match &asked {
            Ok(answer) => Some(&answer.usage),
            Err(e) => e.usage(),
        };
        if let Some(usage) = usage {
            self.state.record_usage(
                UsageSource::PromptSuggestion,
                ROOT_AGENT,
                usage,
                true,
                &self.served,
            );
        }
        clean_suggestion(&asked.ok()?.text, self.last_user_message.as_deref())
    }
}

/// What a one-shot job on a role's list needs besides the job itself.
pub(super) struct ChainJob<'a> {
    pub(super) switcher: &'a Switcher,
    pub(super) chain: &'a ModelChain,
    pub(super) explicit_budget_cap: bool,
    pub(super) state: &'a SessionState,
    pub(super) approver: &'a dyn Approver,
}

impl ChainJob<'_> {
    /// Runs `call` on the list's active entry and, while it fails with a quota-class error
    /// (`falls_through`), on the others, telling the human about an entry that was skipped and
    /// about a move to another entry. `unresolved` makes the job's error out of an entry that
    /// could not be built; the active entry failing that way ends the job without walking the
    /// list.
    pub(super) async fn run<T, E, Cf>(
        &self,
        subject: &str,
        call: impl FnMut(ModelHandle) -> Cf,
        falls_through: impl Fn(&E) -> bool,
        unresolved: impl Fn(&ModelRef, ModelError) -> E,
    ) -> Result<T, E>
    where
        Cf: std::future::Future<Output = Result<T, E>>,
    {
        let start = self.chain.active_ref().clone();
        let start_unresolved = std::sync::atomic::AtomicBool::new(false);
        let through = self
            .switcher
            .run_through(
                self.chain,
                true,
                self.explicit_budget_cap,
                call,
                falls_through,
                |reference, why| {
                    let verb = if *reference == start {
                        start_unresolved.store(true, std::sync::atomic::Ordering::Relaxed);
                        "skipped"
                    } else {
                        "skipping"
                    };
                    self.approver
                        .warn(&format!("{subject}: {verb} {reference}: {}", chain(&why)));
                    unresolved(reference, why)
                },
            )
            .await;
        match through {
            Ok(done) => {
                if let Some(from) = done.from {
                    announce_fall_through(
                        self.state,
                        self.approver,
                        ROOT_AGENT,
                        subject,
                        self.chain,
                        &self.chain.entries[from],
                        &self.chain.entries[done.index],
                        "quota",
                    );
                }
                Ok(done.value)
            }
            Err(last) => {
                if !start_unresolved.load(std::sync::atomic::Ordering::Relaxed) {
                    self.approver.warn(&format!(
                        "{subject} skipped: every model in {} failed",
                        self.chain.label()
                    ));
                }
                Err(last)
            }
        }
    }
}

impl AgentRuntime {
    /// What the session has spent so far: the root's spend, which holds the main model, the
    /// guardian and the goal evaluator, plus every subagent's. A goal measures itself by the
    /// change in this.
    pub fn tree_spend(&self) -> Spend {
        let tree = self.state.agents();
        let mut spend = tree.spend_of(ROOT_AGENT);
        spend.add(&tree.subagent_spend());
        spend
    }

    /// [`Session::compact_if_needed`](crate::session::Session::compact_if_needed) on the main
    /// model, with the summarizer's usage debited to the root so spend caps count it.
    pub async fn compact_if_needed(&self) -> CompactResult {
        self.compact_with(CompactReason::Threshold).await
    }

    /// [`Session::compact_logged_via`](crate::session::Session::compact_logged_via): the
    /// conversation is measured against the main model and summarized by the `compaction` role's
    /// model, or by the main model when no such role is defined. The usage is debited to the root
    /// like [`compact_if_needed`](Self::compact_if_needed). A role's list falls through to its
    /// next entry when the summary request runs out of quota.
    pub async fn compact_with(&self, reason: CompactReason) -> CompactResult {
        let trigger = match reason {
            CompactReason::Asked { .. } => "manual",
            CompactReason::Threshold | CompactReason::Overflow => "auto",
        };
        let focus = match &reason {
            CompactReason::Asked { focus } if !focus.trim().is_empty() => Some(focus.clone()),
            _ => None,
        };
        let hooks = self.lifecycle().clone();
        let before = move || -> futures::future::BoxFuture<'static, SummaryGate> {
            let (hooks, focus) = (hooks.clone(), focus.clone());
            Box::pin(async move {
                match hooks.pre_compact(trigger, focus.as_deref()).await {
                    CompactGate::Blocked(why) => {
                        SummaryGate::Veto(format!("a pre_compact hook blocked it: {why}"))
                    }
                    CompactGate::Proceed { focus } => SummaryGate::Proceed { focus },
                }
            })
        };
        let gate = if matches!(reason, CompactReason::Asked { .. }) {
            self.state.usage_gate_idle().await
        } else {
            self.state.usage_gate().await
        };
        if let Err(ended) = gate {
            self.approver()
                .warn(&format!("compaction skipped: {ended}"));
            return CompactResult::Failed {
                usage: None,
                falls_through: false,
            };
        }
        let main = &self.models.main;
        let result = if let (Some(chain), Ok(switcher)) =
            (self.role_chain(COMPACTION_ROLE), self.switcher())
        {
            self.compact_on_chain(reason, &switcher, &chain, &before)
                .await
        } else {
            let result = self
                .session
                .compact_logged_via(
                    &main.completion,
                    None,
                    main.catalog_billed(),
                    reason,
                    self.approver(),
                    Some(&before),
                )
                .await;
            self.record_compaction(
                &result,
                &Served::new(self.models.main_chain.role.as_deref(), &main.reference),
            );
            if matches!(
                result,
                CompactResult::Failed {
                    falls_through: true,
                    ..
                }
            ) {
                self.approver()
                    .warn("compaction skipped: the model ran out of quota");
            }
            result
        };
        if let CompactResult::Compacted { summary, .. } = &result {
            // The summary replaced the turns that carried the loaded definitions' results, so
            // the model searches again.
            self.mcp.loaded.clear(ROOT_AGENT);
            self.lifecycle().post_compact(trigger, summary).await;
            self.fire_session_start("compact").await;
        }
        result
    }

    fn record_compaction(&self, result: &CompactResult, served: &Served) {
        if let Some(usage) = result.usage() {
            self.state
                .record_usage(UsageSource::Compaction, ROOT_AGENT, usage, true, served);
        }
    }

    /// The compaction on the `compaction` role's list: the active entry, then the others while
    /// the summary request runs out of quota.
    async fn compact_on_chain(
        &self,
        reason: CompactReason,
        switcher: &Switcher,
        chain: &ModelChain,
        pre: PreSummary<'_>,
    ) -> CompactResult {
        let main = &self.models.main;
        let role = chain.role.clone();
        let job = ChainJob {
            switcher,
            chain,
            explicit_budget_cap: self.setup.explicit_budget_cap,
            state: &self.state,
            approver: self.approver(),
        };
        let through = job
            .run(
                COMPACTION_ROLE,
                |handle| {
                    let reason = reason.clone();
                    let served = Served::new(role.as_deref(), &handle.reference);
                    async move {
                        let result = self
                            .session
                            .compact_logged_via(
                                &main.completion,
                                Some(&handle.completion),
                                handle.catalog_billed(),
                                reason,
                                self.approver(),
                                Some(pre),
                            )
                            .await;
                        self.record_compaction(&result, &served);
                        match result {
                            CompactResult::Failed {
                                falls_through: true,
                                ..
                            } => Err(result),
                            done => Ok(done),
                        }
                    }
                },
                |_| true,
                |_, _| CompactResult::Failed {
                    usage: None,
                    falls_through: true,
                },
            )
            .await;
        match through {
            Ok(done) | Err(done) => done,
        }
    }

    /// `/btw`: answers `question` from the saved conversation on the main model, debited to the
    /// root like a compaction, on success and on a billed answer that was unusable. Neither the
    /// question nor the answer is saved or recorded, so the conversation and the guardian's view
    /// of what the human asked are unchanged.
    pub async fn ask_side_question(&self, question: &str) -> Result<Answer, SideQuestionError> {
        let conversation = load_async(&self.session.store(), CONVERSATION_LOG)
            .await
            .map_err(|e| SideQuestionError::caused_by("the conversation could not be read", e))?;
        self.state
            .usage_gate_idle()
            .await
            .map_err(|e| SideQuestionError::caused_by("the wait for the usage window ended", e))?;
        let main = &self.models.main;
        let asked = SideQuestion::new(main.completion.clone(), main.catalog_billed())
            .ask(question, &transcript_of(&conversation))
            .await;
        let usage = match &asked {
            Ok(answer) => Some(&answer.usage),
            Err(e) => e.usage(),
        };
        if let Some(usage) = usage {
            self.state.record_usage(
                UsageSource::SideQuestion,
                ROOT_AGENT,
                usage,
                true,
                &Served::direct(&main.reference),
            );
        }
        asked
    }

    /// The skip checks of a prompt suggestion and, when none holds, the saved conversation
    /// rendered without tool results. `None` means no suggestion is asked for. The call itself
    /// is [`PreparedSuggestion::run`]; it writes no conversation or intent record.
    pub async fn prepare_prompt_suggestion(
        &self,
        inputs: &SuggestionInputs,
    ) -> Option<PreparedSuggestion> {
        // A paused window is polled, not awaited, so a suggestion never waits for the reset.
        if !should_suggest(inputs) || self.state.usage_pause().is_paused() {
            return None;
        }
        // A conversation that cannot be read is the next turn's problem to report.
        let conversation = load_async(&self.session.store(), CONVERSATION_LOG)
            .await
            .ok()?;
        let main = &self.models.main;
        Some(PreparedSuggestion {
            transcript: visible_transcript_of(&conversation),
            last_user_message: last_user_text(&conversation),
            model: main.completion.clone(),
            billed: main.catalog_billed(),
            served: Served::direct(&main.reference),
            state: Arc::clone(&self.state),
        })
    }

    fn forward_goal_warning(&self) {
        if let Some(warning) = self.session.goal().take_warning() {
            self.approver.warn(&warning);
        }
    }

    /// `/goal <condition>`: replaces any active goal. The surface then runs the first turn.
    pub fn set_goal(&self, condition: &str) -> Result<(), GoalError> {
        self.session.goal().set(condition, self.tree_spend())?;
        self.state.append(Record::Goal {
            event: GoalEvent::Set,
            evaluations: 0,
            reason: None,
        });
        Ok(())
    }

    /// `/goal clear`.
    pub fn clear_goal(&self) -> Option<Cleared> {
        let cleared = self.session.goal().clear();
        if let Some(cleared) = &cleared {
            self.state.append(Record::Goal {
                event: GoalEvent::Cleared,
                evaluations: cleared.evaluations,
                reason: None,
            });
        }
        self.forward_goal_warning();
        cleared
    }

    /// A turn ended; `None` is an interrupted one. Reminds `main` of tasks waiting on its board
    /// after a real model turn, and returns the pause this caused.
    pub fn goal_turn_ended(&self, end: Option<&TurnOutcome>) -> Option<PauseReason> {
        if matches!(end, Some(TurnOutcome::Answer(_))) {
            fontaine_agents::spawn::remind_main(self.state.agents());
        }
        let goal = self.session.goal();
        let paused = goal.after_turn(end);
        if paused.is_some() {
            self.state.append(Record::Goal {
                event: GoalEvent::Paused,
                evaluations: goal
                    .status(self.tree_spend())
                    .active
                    .map_or(0, |a| a.evaluations),
                reason: None,
            });
        }
        paused
    }

    /// AFK mode pauses the goal for `reason`, recording why in the ledger. Returns whether a goal
    /// was active.
    pub fn pause_goal(&self, reason: PauseReason) -> bool {
        let text = reason.to_string();
        if !self.session.goal().pause(reason) {
            return false;
        }
        self.state.append(Record::Goal {
            event: GoalEvent::Paused,
            evaluations: self.session.goal().evaluations(),
            reason: Some(text),
        });
        true
    }

    /// The user sent a prompt: a paused goal runs again.
    pub fn goal_user_prompt(&self) {
        self.session.goal().user_prompt(self.tree_spend());
    }

    /// The evaluation due now, if any; none while a subagent still runs.
    pub fn next_goal_step(&self) -> Option<Pending> {
        let running = self.state.agents().any_running();
        self.session.goal().next_step(running)
    }

    /// Asks the evaluator and applies its answer; `None` when the goal was cleared meanwhile.
    pub async fn evaluate_goal(&self, pending: &Pending) -> Option<Applied> {
        let evaluation = self.judge_goal(pending).await;
        let applied = self.session.goal().apply(
            evaluation,
            self.tree_spend(),
            &GoalCaps::from(&self.config.goal),
            self.models.main.priced(),
        );
        if let Some(applied) = &applied {
            let event = match &applied.step {
                Step::Done { event, .. } => Some(*event),
                Step::Pause(_) => Some(GoalEvent::Paused),
                Step::Continue(_) => None,
            };
            if let Some(event) = event {
                self.state.append(Record::Goal {
                    event,
                    evaluations: applied.evaluations,
                    reason: None,
                });
            }
        }
        self.forward_goal_warning();
        applied
    }

    async fn judge_goal(&self, pending: &Pending) -> Evaluation {
        let evaluator = match GoalEvaluator::on(self.models.guardian.as_ref()) {
            Ok(evaluator) => evaluator,
            Err(why) => return Evaluation::Failed(why),
        };
        let conversation = match load_async(&self.session.store(), CONVERSATION_LOG).await {
            Ok(conversation) => conversation,
            Err(e) => {
                return Evaluation::Failed(format!(
                    "the conversation could not be read: {}",
                    chain(&e)
                ));
            }
        };
        if let Err(ended) = self.state.usage_gate_idle().await {
            return Evaluation::Failed(ended.to_string());
        }
        let judged = evaluator
            .evaluate(&pending.condition, &transcript_of(&conversation))
            .await;
        let served = self
            .models
            .guardian
            .as_ref()
            .map_or_else(Served::default, |model| Served::direct(model.reference()));
        self.state
            .add_goal_usage(&judged.usage, judged.answered, &served);
        judged.evaluation
    }

    /// The prompt for a goal turn. It is a harness note, not the user speaking. The condition
    /// is the user's own typed text and goes through as written; the evaluator's reason is
    /// model output derived from tool results, so it passes through the result pipeline.
    pub async fn goal_prompt(&self, next: &Continue) -> String {
        // Cut before screening, so a note the scanner adds is never what the cut removes.
        let reason = self
            .screened_text("goal", &cut(&next.reason, REASON_CHARS))
            .await;
        goal_prompt_text(&reason, &next.condition)
    }
}
