//! The device that answered a prompt reaches the ledger and the audit log, through the AFK
//! backstop that wraps the session's approver.

#[path = "core_common/mod.rs"]
mod common;

use std::sync::{Arc, OnceLock};

use common::pipeline::{Options, Rig, allowed, denied_message};
use fontaine_sdk::core::afk_report::AfkSource;
use fontaine_sdk::core::audit::Decision;
use fontaine_sdk::core::config::PermissionMode::{Afk, Manual};
use fontaine_sdk::core::ledger::{MessageOrigin, Record, RemoteSender};
use fontaine_sdk::core::permissions::{AfkApprover, Answer, ApprovalRequest, Approver};
use fontaine_sdk::core::questions::HumanQuestions;
use fontaine_sdk::tools::ask_user::{
    Question, QuestionAnswer, QuestionOption, QuestionReply, QuestionSource,
};
use fontaine_sdk::tools::gate::ROOT_AGENT;
use futures::future::BoxFuture;
use serde_json::json;

fn device() -> RemoteSender {
    RemoteSender {
        node: "devicenode0123456789".into(),
        message_id: "7".into(),
    }
}

/// Answers as a remote device would: the plain methods answer nothing useful, the `_as` ones carry
/// the device.
struct Device {
    answer: Answer,
}

impl Approver for Device {
    fn approve<'a>(&'a self, _request: ApprovalRequest) -> BoxFuture<'a, Answer> {
        Box::pin(async { Answer::Deny { feedback: None } })
    }

    fn approve_as<'a>(
        &'a self,
        _request: ApprovalRequest,
    ) -> BoxFuture<'a, (Answer, Option<RemoteSender>)> {
        let answer = self.answer.clone();
        Box::pin(async move { (answer, Some(device())) })
    }

    fn ask_user_as<'a>(
        &'a self,
        _agent: &'a str,
        questions: Vec<Question>,
    ) -> BoxFuture<'a, (QuestionReply, Option<RemoteSender>)> {
        let answer = QuestionAnswer {
            question: questions[0].question.clone(),
            selected: vec!["a".into()],
            other: None,
        };
        Box::pin(async move { (QuestionReply::Answered(vec![answer]), Some(device())) })
    }
}

/// The session state needs its approver before the `AfkApprover` that wraps it can exist.
#[derive(Default)]
struct Late(OnceLock<Arc<dyn Approver>>);

impl Late {
    fn inner(&self) -> &Arc<dyn Approver> {
        self.0.get().expect("the approver was installed")
    }
}

impl Approver for Late {
    fn approve<'a>(&'a self, request: ApprovalRequest) -> BoxFuture<'a, Answer> {
        self.inner().approve(request)
    }

    fn approve_as<'a>(
        &'a self,
        request: ApprovalRequest,
    ) -> BoxFuture<'a, (Answer, Option<RemoteSender>)> {
        self.inner().approve_as(request)
    }

    fn ask_user<'a>(
        &'a self,
        agent: &'a str,
        questions: Vec<Question>,
    ) -> BoxFuture<'a, QuestionReply> {
        self.inner().ask_user(agent, questions)
    }

    fn ask_user_as<'a>(
        &'a self,
        agent: &'a str,
        questions: Vec<Question>,
    ) -> BoxFuture<'a, (QuestionReply, Option<RemoteSender>)> {
        self.inner().ask_user_as(agent, questions)
    }
}

fn rig_answered_by(answer: Answer) -> (Rig, Arc<Late>) {
    let late = Arc::new(Late::default());
    let rig = Rig::with(Options {
        approver: Some(late.clone()),
        ..Options::new(Manual)
    });
    let afk: Arc<dyn Approver> = Arc::new(AfkApprover::new(
        Arc::new(Device { answer }),
        rig.state.clone(),
    ));
    late.0.set(afk).ok().expect("installed once");
    (rig, late)
}

fn question() -> Question {
    let option = |label: &str| QuestionOption {
        label: label.into(),
        description: "d".into(),
    };
    Question {
        question: "Which?".into(),
        header: "Pick".into(),
        options: vec![option("a"), option("b")],
        multi_select: false,
    }
}

#[tokio::test]
async fn an_approval_by_a_device_names_it_in_the_ledger_and_the_audit_explanation() {
    let (rig, _late) = rig_answered_by(Answer::AllowOnce);
    let decision = rig.check("shell", json!({ "command": "touch x" })).await;
    assert!(allowed(&decision));
    let ledger = rig.session.ledger.read().unwrap();
    assert!(ledger.iter().any(|entry| matches!(
        &entry.record,
        Record::Approval { remote: Some(by), .. } if *by == device()
    )));
    let human_allow = rig
        .audit()
        .into_iter()
        .find(|r| r.decision == Decision::Allow && r.explanation.is_some())
        .expect("the human's allow is audited");
    let text = human_allow.explanation.unwrap();
    assert!(text.contains("devicenode0123456789"), "{text}");
}

#[tokio::test]
async fn a_denial_by_a_device_names_it_in_the_ledger_and_the_audit_explanation() {
    let (rig, _late) = rig_answered_by(Answer::Deny { feedback: None });
    let decision = rig.check("shell", json!({ "command": "touch x" })).await;
    assert!(denied_message(&decision).contains("declined"));
    let ledger = rig.session.ledger.read().unwrap();
    assert!(ledger.iter().any(|entry| matches!(
        &entry.record,
        Record::Denial { remote: Some(by), .. } if *by == device()
    )));
    assert!(rig.audit().iter().any(|r| {
        r.explanation
            .as_deref()
            .is_some_and(|e| e.contains("devicenode0123456789"))
    }));
}

#[tokio::test]
async fn a_question_answered_by_a_device_marks_the_dialog_message() {
    let (rig, late) = rig_answered_by(Answer::AllowOnce);
    let questions = HumanQuestions::new(late.clone(), rig.state.clone());
    let reply = questions.ask(ROOT_AGENT, vec![question()]).await;
    assert!(matches!(reply, QuestionReply::Answered(_)));
    let ledger = rig.session.ledger.read().unwrap();
    assert!(ledger.iter().any(|entry| matches!(
        &entry.record,
        Record::UserMessage {
            origin: MessageOrigin::Dialog,
            remote: Some(by),
            ..
        } if *by == device()
    )));
}

#[tokio::test]
async fn afk_answers_for_the_user_and_names_no_device() {
    let (rig, late) = rig_answered_by(Answer::AllowOnce);
    rig.state.set_mode(Afk, AfkSource::Cycle).unwrap();
    let request = ApprovalRequest {
        tool: "shell".into(),
        agent: ROOT_AGENT.into(),
        agent_name: String::new(),
        tool_use_id: None,
        args: json!({ "command": "x" }),
        summary: "x".into(),
        tier: fontaine_sdk::core::tier::Tier::Mode,
        reason: "r".into(),
        offers: Default::default(),
        proposed_rule: None,
        preview: None,
    };
    let (answer, by) = late.approve_as(request).await;
    assert_eq!(answer, Answer::Deny { feedback: None });
    assert_eq!(by, None);
    let (reply, by) = late.ask_user_as(ROOT_AGENT, vec![question()]).await;
    assert!(matches!(reply, QuestionReply::Unavailable));
    assert_eq!(by, None);
}
