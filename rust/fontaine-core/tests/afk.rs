//! AFK mode in the permission pipeline: each would-prompt outcome becomes a denial that keeps
//! its own rule and gains the recovery guidance, the temperature stop denies every call of every
//! agent, and AFK and the guardian escape hatch exclude each other.

mod common;

use std::sync::Arc;

use common::pipeline::{Options, Rig, allowed, denied_message, fake_guardian, verdict_json};
use fontaine_agents::mode::Ladder;
use fontaine_core::afk_report::{AfkSource, OutcomeStatus, build_latest};
use fontaine_core::audit::{Converted, Decision};
use fontaine_core::config::PermissionMode::{Afk, Auto, Manual};
use fontaine_core::gate::{AFK_STOPPED_MESSAGE, AfkRefusal, Blocked, ModeChanged};
use fontaine_core::ledger::Record;
use fontaine_core::permissions::{
    AfkApprover, Answer, ApprovalRequest, Approver, HookDecision, HookError, HookPermission,
    PreToolUseEvent, PreToolUseHook, UnavailableChoice,
};
use fontaine_core::tier::Tier;
use fontaine_tools::ask_user::QuestionReply;
use fontaine_tools::gate::{CallView, ROOT_AGENT};
use fontaine_tools::result_pipeline::{ResultPipeline, ResultStage, StageKind};
use futures::future::BoxFuture;
use serde_json::json;

const ATTENDED: &str = "tell the user what you could not do";
const AFK_GUIDANCE: &str = "The user is not available.";

fn shell(command: &str) -> serde_json::Value {
    json!({ "command": command })
}

fn afk_on(rig: &Rig) {
    rig.state.set_mode(Afk, AfkSource::Cycle).unwrap();
}

/// The last audit record the call produced.
fn last_record(rig: &Rig) -> fontaine_core::audit::DecisionRecord {
    rig.audit().last().cloned().expect("an audit record")
}

#[tokio::test]
async fn a_denial_ends_in_the_attended_sentence_until_afk_is_on_and_then_in_the_afk_guidance() {
    let rig = Rig::with(Options::new(Manual).settings("[permissions]\ndeny = [\"shell(rm x)\"]\n"));
    let attended = rig.check("shell", shell("rm x")).await;
    let attended = denied_message(&attended).to_string();
    assert!(attended.contains(ATTENDED), "{attended}");
    assert!(!attended.contains(AFK_GUIDANCE), "{attended}");

    afk_on(&rig);
    let afk = rig.check("shell", shell("rm x")).await;
    let afk = denied_message(&afk);
    assert!(
        afk.contains("the permission rule"),
        "the rule comes first: {afk}"
    );
    assert!(
        afk.contains(AFK_GUIDANCE) && afk.contains("Do not ask for approval."),
        "{afk}"
    );
    assert!(!afk.contains(ATTENDED), "{afk}");
    let record = last_record(&rig);
    assert!(record.afk);
    assert_eq!(record.converted, None, "a deny rule stands for no prompt");
}

#[tokio::test]
async fn an_ask_rule_a_critical_path_and_a_protected_path_keep_their_tier_and_become_denials() {
    let rig =
        Rig::with(Options::new(Manual).settings("[permissions]\nask = [\"shell(npm publish)\"]\n"));
    afk_on(&rig);

    rig.check("shell", shell("npm publish")).await;
    let record = last_record(&rig);
    assert_eq!(
        (record.tier, record.converted),
        (Tier::AskRule, Some(Converted::AskRule))
    );
    assert!(record.rule.as_deref().unwrap().contains("npm publish"));

    rig.check("shell", shell("rm -rf /")).await;
    let record = last_record(&rig);
    assert_eq!(
        (record.tier, record.converted),
        (Tier::CriticalPath, Some(Converted::Floor))
    );

    rig.check(
        "write",
        json!({"path": rig.world.config.join("config.toml").to_str().unwrap(), "content": "x"}),
    )
    .await;
    assert_eq!(rig.approver.asked(), 0);
    assert!(rig.audit().iter().all(|r| r.decision != Decision::Prompt));
}

#[derive(Clone, Copy)]
struct Ask;

impl PreToolUseHook for Ask {
    fn id(&self) -> String {
        "ask-hook".into()
    }

    fn run<'a>(
        &'a self,
        _event: PreToolUseEvent<'a>,
    ) -> BoxFuture<'a, Result<HookDecision, HookError>> {
        Box::pin(async {
            Ok(HookDecision {
                permission: Some(HookPermission::Ask),
                ..HookDecision::default()
            })
        })
    }
}

#[tokio::test]
async fn a_hook_ask_is_converted_with_its_own_variant() {
    let mut options = Options::new(Manual);
    options.hooks = vec![Arc::new(Ask)];
    let rig = Rig::with(options);
    afk_on(&rig);
    rig.check("shell", shell("echo hi")).await;
    let record = last_record(&rig);
    assert_eq!(
        (record.tier, record.converted),
        (Tier::Hook, Some(Converted::Hook))
    );
}

#[tokio::test]
async fn a_guardian_escalation_is_denied_counted_and_keeps_its_verdict() {
    let fake = fake_guardian(verdict_json("escalate", "Production Deploy", 0.9));
    let rig = Rig::with(Options::new(Auto).guardian(fake.guardian.clone()));
    afk_on(&rig);
    for n in 0..2 {
        let decision = rig.check("shell", shell(&format!("deploy {n}"))).await;
        assert!(denied_message(&decision).contains(AFK_GUIDANCE));
    }
    let record = last_record(&rig);
    assert_eq!(
        (record.tier, record.decision, record.converted),
        (Tier::Guardian, Decision::Deny, Some(Converted::Escalate))
    );
    assert_eq!(record.guardian_verdict.as_deref(), Some("escalate"));
    assert_eq!(rig.approver.asked(), 0);
    // Counted like a denial: the third one pauses auto mode, and the pause blocks the run.
    assert!(rig.state.unattended_blocked().is_none());
    rig.check("shell", shell("deploy 3")).await;
    assert!(rig.state.auto_paused());
    assert_eq!(rig.state.unattended_blocked(), Some(Blocked::BreakerPaused));
}

#[tokio::test]
async fn a_paused_breaker_denies_instead_of_asking_and_never_resumes_by_itself() {
    let fake = fake_guardian(verdict_json("deny", "Git Destructive", 0.9));
    let rig = Rig::with(Options::new(Auto).guardian(fake.guardian.clone()));
    afk_on(&rig);
    for n in 0..3 {
        rig.check("shell", shell(&format!("git push --force {n}")))
            .await;
    }
    assert!(rig.state.auto_paused());
    let asked = fake.calls();
    let decision = rig.check("shell", shell("git push --force 9")).await;
    let message = denied_message(&decision);
    assert!(message.contains("auto mode is paused"), "{message}");
    assert!(message.contains(AFK_GUIDANCE), "{message}");
    assert_eq!(
        fake.calls(),
        asked,
        "the guardian is not asked while paused"
    );
    assert_eq!(last_record(&rig).converted, Some(Converted::BreakerPaused));
    assert!(rig.state.auto_paused());
    assert_eq!(rig.approver.asked(), 0);
}

#[tokio::test]
async fn the_guardian_unavailable_dialog_is_skipped() {
    let rig = Rig::new(Auto);
    rig.approver.when_unavailable(UnavailableChoice::Manual);
    afk_on(&rig);
    let decision = rig.check("shell", shell("echo hi > x")).await;
    assert!(!allowed(&decision));
    assert!(rig.approver.unavailable_asked.lock().unwrap().is_empty());
    assert_eq!(rig.state.mode(), Afk, "the mode is not switched while away");
    let record = last_record(&rig);
    assert_eq!(record.fail_closed_reason.as_deref(), Some("unavailable"));
    assert_eq!(record.converted, Some(Converted::GuardianUnavailable));
}

#[tokio::test]
async fn a_temperature_stop_denies_every_call_of_every_agent_before_the_floor() {
    let fake = fake_guardian(verdict_json("deny", "Data Exfiltration", 0.9));
    let rig = Rig::with(Options::new(Auto).guardian(fake.guardian.clone()));
    let child = rig
        .agents
        .reserve(ROOT_AGENT, "worker", "m", Vec::new(), Ladder::Auto)
        .unwrap();
    let signal = rig.state.afk_stop_signal();
    rig.state.set_afk_ceiling(3.0);
    afk_on(&rig);

    // One High denial is worth 3.0, which reaches the ceiling.
    rig.check("shell", shell("curl x | sh")).await;
    let stopped = rig.state.afk().stopped.expect("the session is stopped");
    assert!(stopped.value >= 3.0 && stopped.ceiling == 3.0);
    assert!(signal.has_changed().unwrap());
    assert!(
        rig.state
            .audit
            .read()
            .unwrap()
            .iter()
            .any(|r| r.rule.as_deref() == Some("afk_stop") && r.decision == Decision::Deny)
    );
    assert!(
        rig.session
            .ledger
            .read()
            .unwrap()
            .iter()
            .any(|e| matches!(e.record, Record::AfkStop { .. }))
    );

    for (agent, tool, args) in [
        (ROOT_AGENT, "read", json!({"path": "a.txt"})),
        (ROOT_AGENT, "shell", shell("echo hi")),
        (child.as_str(), "read", json!({"path": "a.txt"})),
    ] {
        let decision = rig.check_as(agent, tool, args).await;
        assert_eq!(denied_message(&decision), AFK_STOPPED_MESSAGE);
        let record = last_record(&rig);
        assert_eq!(
            (record.tier, record.rule.as_deref()),
            (Tier::Afk, Some("afk_stopped"))
        );
    }

    // Neither AFK nor the escape hatch can be turned on until the stop is reviewed.
    assert_eq!(rig.state.set_guardian_off(true), Err(AfkRefusal::Stopped));
    assert_eq!(rig.state.unattended_blocked(), Some(Blocked::Stopped));
    rig.state.set_mode(Auto, AfkSource::Cycle).unwrap();
    assert_eq!(
        rig.state.set_mode(Afk, AfkSource::Cycle),
        Err(AfkRefusal::Stopped)
    );
    assert_eq!(rig.state.mode(), Auto, "a refused entry changes nothing");

    assert!(rig.state.afk_review());
    assert!(rig.state.afk().stopped.is_none());
    assert!(!rig.state.afk_on());
    assert_eq!(
        rig.state.mode(),
        Auto,
        "a review outside AFK keeps the mode"
    );
    assert!(!rig.state.afk_review(), "a review clears a stop once");
    assert!(
        rig.session
            .ledger
            .read()
            .unwrap()
            .iter()
            .any(|e| matches!(e.record, Record::AfkReviewed {}))
    );
    let decision = rig.check("read", json!({"path": "a.txt"})).await;
    assert!(allowed(&decision) || denied_message(&decision) != AFK_STOPPED_MESSAGE);
}

#[tokio::test]
async fn afk_and_the_guardian_escape_hatch_exclude_each_other() {
    let rig = Rig::new(Manual);
    rig.state.set_guardian_off(true).unwrap();
    assert_eq!(
        rig.state.set_mode(Afk, AfkSource::Cycle),
        Err(AfkRefusal::GuardianOff)
    );
    assert_eq!(rig.state.mode(), Manual);
    assert!(!rig.state.afk_on());
    rig.state.set_guardian_off(false).unwrap();
    assert!(rig.state.set_mode(Afk, AfkSource::Cycle).unwrap().changed);
    assert!(rig.state.afk_on());
    assert_eq!(rig.state.set_guardian_off(true), Err(AfkRefusal::AfkOn));
    assert!(!rig.state.guardian_off());
    let left = rig.state.set_mode(Manual, AfkSource::Cycle).unwrap();
    assert!(left.changed && left.closed_stretch.is_some());
    assert_eq!(
        rig.state.set_mode(Manual, AfkSource::Cycle),
        Ok(ModeChanged::default())
    );
    rig.state.set_guardian_off(true).unwrap();
}

#[tokio::test]
async fn afk_follows_the_mode_and_is_recorded_in_the_ledger_and_the_audit_log() {
    let rig = Rig::new(Manual);
    rig.state.set_mode(Afk, AfkSource::Cycle).unwrap();
    assert!(rig.state.afk_on());
    assert!(rig.state.afk().stretch.is_some());
    let left = rig.state.set_mode(Auto, AfkSource::Plan).unwrap();
    assert!(!rig.state.afk_on());
    assert!(rig.state.afk().stretch.is_none());
    let records: Vec<_> = rig
        .session
        .ledger
        .read()
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.record {
            Record::Afk { on, source } => Some(format!("afk {on} {source:?}")),
            Record::ModeChange { from, to } => Some(format!("mode {from}>{to}")),
            _ => None,
        })
        .collect();
    assert_eq!(
        records,
        [
            "afk true Cycle",
            "mode manual>afk",
            "afk false Plan",
            "mode afk>auto"
        ]
    );
    let started = rig
        .session
        .ledger
        .read()
        .unwrap()
        .into_iter()
        .find(|e| matches!(e.record, Record::Afk { on: true, .. }))
        .unwrap()
        .seq;
    assert_eq!(left.closed_stretch, Some(started));
    let rules: Vec<_> = rig
        .audit()
        .into_iter()
        .filter(|r| r.tier == Tier::Afk)
        .map(|r| (r.rule.unwrap(), r.permission_mode))
        .collect();
    assert_eq!(
        rules,
        [
            ("afk_on".to_string(), "auto".to_string()),
            ("afk_off".to_string(), "auto".to_string())
        ]
    );
}

#[tokio::test]
async fn reviewing_a_stop_inside_afk_lands_in_manual() {
    let rig = Rig::new(Auto);
    afk_on(&rig);
    rig.state.afk_stop(13.0, 12.0, vec![1]);
    assert!(rig.state.afk_review());
    assert_eq!(rig.state.mode(), Manual);
    assert!(!rig.state.afk_on() && rig.state.afk().stopped.is_none());
    let kinds: Vec<_> = rig
        .session
        .ledger
        .read()
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.record {
            Record::Afk { on: false, .. } => Some("afk off"),
            Record::ModeChange { .. } => Some("mode"),
            Record::AfkReviewed {} => Some("reviewed"),
            _ => None,
        })
        .collect();
    assert_eq!(kinds, ["mode", "reviewed", "afk off", "mode"]);
}

#[tokio::test]
async fn afk_decides_as_auto_with_nobody_to_ask() {
    let fake = fake_guardian(verdict_json("escalate", "Production Deploy", 0.9));
    let rig = Rig::with(Options::new(Manual).guardian(fake.guardian.clone()));
    afk_on(&rig);
    let decision = rig.check("shell", shell("deploy now")).await;
    assert!(denied_message(&decision).contains(AFK_GUIDANCE));
    assert_eq!(fake.calls(), 1, "the guardian judged it as in auto");
    let record = last_record(&rig);
    assert_eq!(
        (
            record.permission_mode.as_str(),
            record.afk,
            record.converted
        ),
        ("auto", true, Some(Converted::Escalate))
    );
    assert_eq!(rig.approver.asked(), 0);

    rig.check("shell", shell("rm -rf /")).await;
    assert_eq!(last_record(&rig).tier, Tier::CriticalPath);
}

#[tokio::test]
async fn a_child_of_an_afk_session_runs_at_the_auto_rung_and_its_prompts_convert() {
    let fake = fake_guardian(verdict_json("escalate", "Production Deploy", 0.9));
    let rig = Rig::with(Options::new(Manual).guardian(fake.guardian.clone()));
    afk_on(&rig);
    let child = rig
        .agents
        .reserve(ROOT_AGENT, "worker", "m", Vec::new(), Ladder::Auto)
        .unwrap();
    let decision = rig.check_as(&child, "shell", shell("deploy now")).await;
    assert!(denied_message(&decision).contains(AFK_GUIDANCE));
    assert_eq!(rig.approver.asked(), 0);
    assert_eq!(last_record(&rig).converted, Some(Converted::Escalate));
}

#[tokio::test]
async fn the_approver_decorator_answers_for_the_user_while_afk_is_on() {
    let rig = Rig::new(Manual);
    let decorated = AfkApprover::new(rig.approver.clone(), rig.state.clone());
    let request = || ApprovalRequest {
        tool: "shell".into(),
        agent: ROOT_AGENT.into(),
        agent_name: String::new(),
        tool_use_id: None,
        args: shell("x"),
        summary: "x".into(),
        tier: Tier::Mode,
        reason: "r".into(),
        offers: Default::default(),
        proposed_rule: None,
        preview: None,
    };
    rig.approver.answer(Answer::AllowOnce);
    assert_eq!(decorated.approve(request()).await, Answer::AllowOnce);
    assert_eq!(rig.approver.asked(), 1);

    afk_on(&rig);
    rig.approver.answer(Answer::AllowOnce);
    assert_eq!(
        decorated.approve(request()).await,
        Answer::Deny { feedback: None }
    );
    assert_eq!(
        rig.approver.asked(),
        1,
        "the user's approver is never reached"
    );
    rig.approver.reply(QuestionReply::Unavailable);
    assert!(matches!(
        decorated.ask_user(ROOT_AGENT, Vec::new()).await,
        QuestionReply::Unavailable
    ));
    assert!(rig.approver.questions.lock().unwrap().is_empty());
    rig.approver.when_unavailable(UnavailableChoice::Manual);
    let info = fontaine_core::permissions::GuardianUnavailable {
        model: "a/b".into(),
        reason: "r".into(),
        tool: "shell".into(),
    };
    assert_eq!(
        decorated.guardian_unavailable(info).await,
        UnavailableChoice::Deny
    );
    decorated.warn("still told");
    decorated.notice("also told");
    assert_eq!(rig.approver.warnings.lock().unwrap().len(), 1);
    assert_eq!(rig.approver.notices.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn the_outcome_stage_records_how_each_allowed_call_ended() {
    let rig = Rig::new(Manual);
    let results = ResultPipeline::new().with_stage(
        StageKind::Outcome,
        Arc::new(fontaine_core::tool_outcome::OutcomeStage {
            state: rig.state.clone(),
        }),
    );
    let args = json!({});
    let view = |synthetic| CallView {
        tool: "read",
        risk: fontaine_tools::risk::RiskClass::ReadOnly,
        args: &args,
        agent: ROOT_AGENT,
        annotations: None,
        requires_user_interaction: false,
        synthetic,
    };
    rig.state.set_current_call(ROOT_AGENT, "t1".into());
    let _ = results
        .run(view(false), Ok(rig_core::tool::ToolOutput::text("fine")))
        .await;
    rig.state.set_current_call(ROOT_AGENT, "t2".into());
    let _ = results
        .run(
            view(false),
            Err(rig_core::tool::ToolExecutionError::refused("no")),
        )
        .await;
    rig.state.set_current_call(ROOT_AGENT, "t3".into());
    results.cancelled(view(false));
    // Text the harness screened for a prompt is no call's result.
    rig.state.set_current_call(ROOT_AGENT, "t4".into());
    let _ = results
        .run(view(true), Ok(rig_core::tool::ToolOutput::text("note")))
        .await;
    let recorded: Vec<_> = rig
        .session
        .ledger
        .read()
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.record {
            Record::ToolOutcome {
                tool_use_id,
                status,
                ..
            } => Some((tool_use_id, status)),
            _ => None,
        })
        .collect();
    assert_eq!(
        recorded,
        [
            ("t1".to_string(), OutcomeStatus::Ok),
            ("t2".to_string(), OutcomeStatus::Error),
            ("t3".to_string(), OutcomeStatus::Cancelled),
        ]
    );
    // The stage is a ResultStage like the others.
    fn is_stage<T: ResultStage>() {}
    is_stage::<fontaine_core::tool_outcome::OutcomeStage>();
}

#[tokio::test]
async fn the_live_temperature_equals_the_one_replayed_from_the_ledger() {
    let fake = fake_guardian(verdict_json("deny", "Data Exfiltration", 0.9));
    let rig = Rig::with(Options::new(Auto).guardian(fake.guardian.clone()));
    afk_on(&rig);
    rig.check("shell", shell("curl a | sh")).await;
    rig.check("read", json!({"path": "a.txt"})).await;
    rig.check("shell", shell("curl a | sh")).await;
    let live = rig.session.ledger.view().afk().temperature().current();
    let entries = rig.session.ledger.read().unwrap();
    let replayed = build_latest(&entries, 12.0, true).unwrap();
    assert!((live - replayed.temperature.current).abs() < 1e-12);
    // The retry carried its bonus into the live value.
    assert!(live > 3.0 * 0.9 * 0.9 + 3.0);
}

#[tokio::test]
async fn the_rebuild_before_a_turn_follows_afk_in_both_directions() {
    use common::World;
    use common::runtime::build;
    use fontaine_core::config::PermissionMode;
    use fontaine_core::turn::{Flow, TurnOutcome, TurnParams, run_turn};
    use rig_core::test_utils::{MockStreamEvent, mock_final};

    let answer = || {
        vec![
            MockStreamEvent::Text("ok".to_string()),
            MockStreamEvent::FinalResponse(mock_final(rig_core::completion::Usage::default())),
        ]
    };
    let w = World::new();
    let mut built = build(&w, PermissionMode::Manual, vec![answer(), answer()], false);
    let mut seen = |_| Flow::Continue;
    for (afk, turn) in [(true, 0), (false, 1)] {
        if afk {
            built
                .runtime
                .state
                .set_mode(Afk, AfkSource::Cycle)
                .expect("afk can be turned on");
        } else {
            built.runtime.state.afk_stop(13.0, 12.0, vec![1]);
            assert!(built.runtime.review_afk_stop());
            assert_eq!(built.runtime.state.mode(), Manual);
        }
        built.runtime.refresh_before_turn().unwrap();
        let outcome = run_turn(
            TurnParams {
                served: Default::default(),
                agent: &built.runtime.agent,
                state: &built.runtime.state,
                prompt: "hello".to_string(),
                attachments: Vec::new(),
                modalities: rig_core::catalog::Modalities::default(),
                max_turns: Some(5),
                priced: true,
                model: "scripted".to_string(),
                provider: "mock".to_string(),
                native_search: false,
                generation_options: None,
            },
            &mut seen,
        )
        .await;
        assert!(matches!(outcome, TurnOutcome::Answer(_)), "{outcome:?}");
        let request = format!("{:?}", built.main.requests()[turn]);
        assert_eq!(
            request.contains("The user is away (AFK mode)"),
            afk,
            "{request}"
        );
    }
}

#[tokio::test]
async fn the_review_grades_the_stretch_it_is_given_not_the_latest_and_only_once() {
    use common::World;
    use common::runtime::build;
    use fontaine_core::config::PermissionMode;
    use rig_core::test_utils::{MockStreamEvent, mock_final};

    let w = World::new();
    let answer = vec![
        MockStreamEvent::Text("not a verdict".to_string()),
        MockStreamEvent::FinalResponse(mock_final(rig_core::completion::Usage::default())),
    ];
    let built = build(&w, PermissionMode::Auto, vec![answer], false);
    let state = &built.runtime.state;
    let denial = || Record::Denial {
        tool: "shell".into(),
        agent: ROOT_AGENT.into(),
        reason: "r".into(),
        rule: Some("deny_rule".into()),
        summary: "s".into(),
        tier: None,
        tool_use_id: None,
        digest: None,
        paths: Vec::new(),
        converted: None,
        fail_closed: None,
        explanation: None,
    };
    state.set_mode(Afk, AfkSource::Cycle).unwrap();
    state.append(denial());
    let first = state
        .set_mode(Auto, AfkSource::Cycle)
        .unwrap()
        .closed_stretch
        .expect("the first stretch closed");
    state.set_mode(Afk, AfkSource::Cycle).unwrap();
    state.append(denial());

    built.runtime.assess_afk(first).await;
    built.runtime.assess_afk(first).await;
    let graded: Vec<_> = built
        .runtime
        .state
        .ledger
        .read()
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.record {
            Record::AfkAssessment { stretch_seq, .. } => Some(stretch_seq),
            _ => None,
        })
        .collect();
    assert_eq!(graded, [first]);
}
