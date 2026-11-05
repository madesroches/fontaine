//! AFK mode as the runtime drives it: turning it on and off, the report, the independent review
//! of a stretch, and what happens to the running tree when the temperature ceiling stops the
//! session.

use fontaine_guardian::evasion::assess_evasion;
use fontaine_tools::error_chain::chain;
use fontaine_tools::gate::ROOT_AGENT;

use super::AgentRuntime;
use crate::afk_report::{self, AfkOutcome, AfkReport, AfkSource, AssessmentResult};
use crate::gate::{AfkRefusal, Blocked};
use crate::goal::PauseReason;
use crate::ledger::Record;
use crate::runtime_error::RuntimeError;

impl AgentRuntime {
    /// Turns AFK mode on or off. The base prompt is rebuilt so the model reads the guidance that
    /// matches, which costs one prompt-cache miss per toggle. Returns whether anything changed.
    pub fn set_afk(&mut self, on: bool, source: AfkSource) -> Result<bool, AfkRefusal> {
        let changed = self.state.set_afk(on, source)?;
        if changed && let Err(e) = self.rebuild() {
            // The pipeline enforces AFK either way; only the prompt's wording is stale.
            self.approver.warn(&format!(
                "the system prompt could not be rebuilt for AFK mode: {}",
                chain(&e)
            ));
        }
        Ok(changed)
    }

    /// Why a turn nobody typed may not start.
    pub fn unattended_blocked(&self) -> Option<Blocked> {
        self.state.unattended_blocked()
    }

    /// The report of the latest AFK stretch, or of every stretch, built from the whole ledger.
    pub fn afk_reports(&self, all: bool) -> Result<Vec<AfkReport>, RuntimeError> {
        let entries = self
            .state
            .ledger
            .read()
            .map_err(|e| RuntimeError::caused("the session ledger could not be read", e))?;
        let mut reports =
            afk_report::build_all(&entries, self.state.afk_ceiling(), self.state.afk_on());
        if !all {
            reports = reports.pop().into_iter().collect();
        }
        Ok(reports)
    }

    /// Asks the guardian's model whether a later call of the latest stretch pursued the effect
    /// of a denied one, and records what it said. A stretch with no denial is not assessed. The
    /// working model never grades itself, and a failed assessment is recorded as unavailable.
    pub async fn assess_afk(&self) {
        let report = match self.afk_reports(false) {
            Ok(mut reports) => reports.pop(),
            Err(e) => {
                self.approver.warn(&chain(&e));
                return;
            }
        };
        let Some(report) = report.filter(AfkReport::has_denials) else {
            return;
        };
        let guardian = self.state.guardian();
        let result = match assess_evasion(&guardian, &report.assessment_input()).await {
            Ok(assessed) => {
                self.state
                    .add_guardian_usage(ROOT_AGENT, &assessed.usage, true);
                AssessmentResult::Completed {
                    suspected: assessed.suspected,
                }
            }
            Err(e) => AssessmentResult::Unavailable { reason: chain(&e) },
        };
        self.state.append(Record::AfkAssessment {
            stretch_seq: report.stretch.started_seq,
            result,
        });
    }

    /// What follows turning AFK off or ending a headless run: the final review of the stretch,
    /// then its report.
    pub async fn finish_afk(&self) -> Option<AfkReport> {
        self.assess_afk().await;
        self.afk_reports(false).ok()?.pop()
    }

    /// The part of a temperature stop that cannot run inside the turn it ends: every running or
    /// idle agent and every running job is killed one by one (not with the tree's latching
    /// shutdown, which would outlive a review), the goal is paused, and the stretch is reviewed.
    /// Returns the report for the surface to show.
    pub async fn handle_afk_stop(&self) -> Option<AfkReport> {
        let tree = self.state.agents();
        let mut ids = tree.running_ids();
        for id in tree.idle_ids() {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        for id in ids {
            self.kill_agent(&id);
        }
        for job in self.setup.jobs.snapshot().into_iter().filter(|j| j.running) {
            self.setup.jobs.kill_by_user(&job.id);
        }
        self.pause_goal(PauseReason::AfkStopped);
        self.finish_afk().await
    }

    /// The first time the denial breaker has blocked an AFK session: the goal is paused, the
    /// stretch is reviewed, and its report comes back for the surface to announce. `None` on
    /// every later call, and while the session is not blocked.
    pub async fn note_blocked(&self) -> Option<AfkReport> {
        if !self.state.announce_blocked() {
            return None;
        }
        // The breaker may have paused before this stretch began, in which case nothing recorded
        // the block.
        let recorded = matches!(
            self.state.ledger.view().afk().latest(),
            Some(AfkOutcome::Blocked | AfkOutcome::Stopped)
        );
        if !recorded {
            self.state.append(Record::AfkBlocked {});
        }
        self.pause_goal(PauseReason::Blocked(
            "the denial breaker tripped while the user was away".to_string(),
        ));
        self.finish_afk().await
    }

    /// The temperature stop the user reviewed may run again: AFK stays off.
    pub fn review_afk_stop(&self) -> bool {
        self.state.afk_review()
    }
}
