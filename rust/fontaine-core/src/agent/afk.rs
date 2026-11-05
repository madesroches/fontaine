//! AFK mode as the runtime drives it: the report, the independent review
//! of a stretch, and what happens to the running tree when the temperature ceiling stops the
//! session.

use fontaine_guardian::evasion::assess_evasion;
use fontaine_tools::error_chain::chain;
use fontaine_tools::gate::ROOT_AGENT;

use super::AgentRuntime;
use crate::afk_report::{self, AfkOutcome, AfkReport, Assessment, AssessmentResult};
use crate::gate::Blocked;
use crate::goal::PauseReason;
use crate::ledger::Record;
use crate::runtime_error::RuntimeError;

impl AgentRuntime {
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

    /// Asks the guardian's model whether a later call of the stretch that started at the ledger
    /// position `stretch_seq` pursued the effect of a denied one, and records what it said. A
    /// stretch with no denial is not assessed. The working model never grades itself, and a
    /// failed assessment is recorded as unavailable.
    pub async fn assess_afk(&self, stretch_seq: u64) {
        let report = match self.afk_reports(true) {
            Ok(reports) => reports
                .into_iter()
                .find(|report| report.stretch.started_seq == stretch_seq),
            Err(e) => {
                self.approver.warn(&chain(&e));
                return;
            }
        };
        let Some(report) = report
            .filter(AfkReport::has_denials)
            .filter(|report| report.review.assessment == Assessment::Pending)
        else {
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
            Err(e) => {
                // A timed-out or unparseable answer still cost tokens.
                self.state.add_guardian_usage(ROOT_AGENT, &e.usage, true);
                AssessmentResult::Unavailable { reason: chain(&e) }
            }
        };
        self.state.append(Record::AfkAssessment {
            stretch_seq: report.stretch.started_seq,
            result,
        });
    }

    /// What follows a stretch that stopped, blocked or ended a headless run: the final review of
    /// the running stretch, then the report of the latest one.
    pub async fn finish_afk(&self) -> Option<AfkReport> {
        if let Some(stretch) = self.state.afk().stretch {
            self.assess_afk(stretch.started_seq).await;
        }
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

    /// Whether `note_blocked` has a block to announce, so a surface can say the review is
    /// starting before it waits on it.
    pub fn block_unannounced(&self) -> bool {
        self.state.block_unannounced()
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

    /// The temperature stop the user reviewed may run again, in `manual` if AFK mode was on. The
    /// next turn's refresh brings the prompt's AFK guidance in line.
    pub fn review_afk_stop(&self) -> bool {
        self.state.afk_review()
    }
}
