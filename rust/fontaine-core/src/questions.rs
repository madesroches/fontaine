//! The model's questions for the human, put through the approver. What the human chose becomes
//! guardian intent, with the question and option text that framed it: the human saw exactly that
//! text in the dialog, and the guardian credits only the picked option read with its question.

use std::sync::Arc;

use fontaine_tools::ask_user::{Question, QuestionAnswer, QuestionReply, QuestionSource};
use futures::future::BoxFuture;

use crate::gate::SessionState;
use crate::ledger::{MessageOrigin, Record};
use crate::permissions::Approver;

/// What every recorded dialog answer starts with; the guardian's prompt and the evals name it.
pub const ANSWER_LINE_PREFIX: &str = "Answered the agent's question";

/// One answer as the guardian reads it. The answer is paired with the question whose text it
/// carries; with none, the picks render without descriptions.
fn answer_line(asked: &[Question], answer: &QuestionAnswer) -> String {
    let question = asked.iter().find(|q| q.question == answer.question);
    let picks: Vec<String> = answer
        .selected
        .iter()
        .map(|label| {
            let option =
                question.and_then(|q| q.options.iter().find(|option| option.label == *label));
            match option {
                Some(option) => format!("\"{label}\" ({})", option.description),
                None => format!("\"{label}\""),
            }
        })
        .collect();
    let mut parts = Vec::new();
    if !picks.is_empty() {
        parts.push(format!("picked {}", picks.join(", ")));
    }
    if let Some(other) = &answer.other {
        parts.push(format!("typed \"{other}\""));
    }
    format!(
        "{ANSWER_LINE_PREFIX} \"{}\": {}",
        answer.question,
        parts.join("; ")
    )
}

pub struct HumanQuestions {
    approver: Arc<dyn Approver>,
    state: Arc<SessionState>,
}

impl HumanQuestions {
    pub fn new(approver: Arc<dyn Approver>, state: Arc<SessionState>) -> HumanQuestions {
        HumanQuestions { approver, state }
    }
}

impl QuestionSource for HumanQuestions {
    fn ask<'a>(&'a self, agent: &'a str, questions: Vec<Question>) -> BoxFuture<'a, QuestionReply> {
        Box::pin(async move {
            let asked = questions.clone();
            let reply = self.approver.ask_user(agent, questions).await;
            if let QuestionReply::Answered(answers) = &reply {
                let lines: Vec<String> = answers
                    .iter()
                    .filter(|answer| !answer.selected.is_empty() || answer.other.is_some())
                    .map(|answer| answer_line(&asked, answer))
                    .collect();
                if !lines.is_empty() {
                    self.state.append(Record::UserMessage {
                        origin: MessageOrigin::Dialog,
                        text: lines.join("\n"),
                        attachments: Vec::new(),
                        agent: None,
                    });
                }
            }
            reply
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::AuditLog;
    use crate::config::PermissionMode;
    use crate::gate::{AutoContext, StateParams, no_guardian};
    use crate::ledger::Ledger;
    use crate::permissions::DenyAll;
    use fontaine_agents::tree::{AgentTree, Limits};
    use fontaine_tools::ask_user::{QuestionAnswer, QuestionOption};
    use fontaine_tools::gate::ROOT_AGENT;
    use fontaine_tools::scope::Scope;
    use std::sync::Mutex;

    struct Answers(Mutex<Option<QuestionReply>>);

    impl Approver for Answers {
        fn approve<'a>(
            &'a self,
            _request: crate::permissions::ApprovalRequest,
        ) -> BoxFuture<'a, crate::permissions::Answer> {
            Box::pin(async { crate::permissions::Answer::Deny { feedback: None } })
        }

        fn ask_user<'a>(
            &'a self,
            _agent: &'a str,
            _questions: Vec<Question>,
        ) -> BoxFuture<'a, QuestionReply> {
            let reply = self
                .0
                .lock()
                .unwrap()
                .take()
                .unwrap_or(QuestionReply::Declined);
            Box::pin(async move { reply })
        }
    }

    fn rig(approver: Arc<dyn Approver>) -> (HumanQuestions, Arc<Ledger>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Arc::new(Ledger::create(dir.path()).unwrap());
        let agents = Arc::new(AgentTree::new(Limits::default(), 1));
        agents.set_root_tools(|| {
            fontaine_tools::descriptor::names()
                .map(String::from)
                .collect()
        });
        let state = Arc::new(SessionState::new(StateParams {
            session_id: "questions-test".into(),
            mode: PermissionMode::Manual,
            scope: Arc::new(Scope::new(dir.path().to_path_buf(), [])),
            deny: Vec::new(),
            ask: Vec::new(),
            allow: Vec::new(),
            ledger: ledger.clone(),
            audit: Arc::new(AuditLog::create(dir.path(), "questions-test").unwrap()),
            approver: approver.clone(),
            home: None,
            protected_dirs: Vec::new(),
            scratch: None,
            memory_root: None,
            guardian: no_guardian(),
            auto: AutoContext::default(),
            agents: agents.clone(),
            agent_types: Arc::new(fontaine_tools::live::Live::new(
                fontaine_agents::registry::AgentTypes::builtin(),
            )),
            reveal_mcp_names: false,
            auto_edits: crate::gate::AutoEdits::default(),
            effort: None,
        }));
        (HumanQuestions::new(approver, state), ledger, dir)
    }

    fn question() -> Question {
        question_named("Which?")
    }

    fn question_named(text: &str) -> Question {
        let option = |label: &str, description: &str| QuestionOption {
            label: label.into(),
            description: description.into(),
        };
        Question {
            question: text.into(),
            header: "Pick".into(),
            options: vec![option("a", "first"), option("b", "second")],
            multi_select: false,
        }
    }

    fn answer(question: &str, selected: &[&str], other: Option<&str>) -> QuestionAnswer {
        QuestionAnswer {
            question: question.into(),
            selected: selected.iter().map(|s| s.to_string()).collect(),
            other: other.map(str::to_string),
        }
    }

    fn command_request(questions: &HumanQuestions) -> fontaine_guardian::AdjudicationRequest {
        crate::adjudication::build(
            &questions.state,
            fontaine_tools::gate::ROOT_AGENT,
            "shell",
            &serde_json::json!({ "command": "ls" }),
            Vec::new(),
            None,
        )
    }

    #[tokio::test]
    async fn answers_share_one_message_one_line_per_question_in_order() {
        let reply = QuestionReply::Answered(vec![
            answer("One?", &["a"], None),
            answer("Two?", &[], Some("first")),
            answer("Three?", &["b"], Some("second")),
        ]);
        let (questions, ledger, _dir) = rig(Arc::new(Answers(Mutex::new(Some(reply.clone())))));
        let asked = vec![
            question_named("One?"),
            question_named("Two?"),
            question_named("Three?"),
        ];
        assert_eq!(questions.ask(ROOT_AGENT, asked).await, reply);
        let expected = "Answered the agent's question \"One?\": picked \"a\" (first)\n\
             Answered the agent's question \"Two?\": typed \"first\"\n\
             Answered the agent's question \"Three?\": picked \"b\" (second); typed \"second\"";
        assert_eq!(ledger.view().user_messages, vec![expected]);
        assert_eq!(
            command_request(&questions).recent_user_messages,
            vec![expected]
        );
    }

    #[tokio::test]
    async fn a_picked_option_becomes_intent_with_its_question_and_description() {
        let force_push = Question {
            question: "The rebase rewrote 584-x. Force-push it to origin?".into(),
            header: "Push".into(),
            options: vec![
                QuestionOption {
                    label: "Force-push".into(),
                    description: "Overwrite origin/584-x with the rebased commits".into(),
                },
                QuestionOption {
                    label: "Skip".into(),
                    description: "Leave origin alone".into(),
                },
            ],
            multi_select: false,
        };
        let reply =
            QuestionReply::Answered(vec![answer(&force_push.question, &["Force-push"], None)]);
        let (questions, ledger, _dir) = rig(Arc::new(Answers(Mutex::new(Some(reply)))));
        questions.ask(ROOT_AGENT, vec![force_push]).await;
        let line = "Answered the agent's question \"The rebase rewrote 584-x. Force-push it to origin?\": \
             picked \"Force-push\" (Overwrite origin/584-x with the rebased commits)";
        assert_eq!(ledger.view().user_messages, vec![line]);
        assert_eq!(command_request(&questions).recent_user_messages, vec![line]);
    }

    #[tokio::test]
    async fn a_multi_select_answer_lists_every_pick() {
        let mut asked = question();
        asked.multi_select = true;
        let reply = QuestionReply::Answered(vec![answer("Which?", &["a", "b"], None)]);
        let (questions, ledger, _dir) = rig(Arc::new(Answers(Mutex::new(Some(reply)))));
        questions.ask(ROOT_AGENT, vec![asked]).await;
        assert_eq!(
            ledger.view().user_messages,
            vec!["Answered the agent's question \"Which?\": picked \"a\" (first), \"b\" (second)"]
        );
    }

    #[tokio::test]
    async fn a_label_or_question_with_no_match_renders_without_a_description() {
        let reply = QuestionReply::Answered(vec![
            answer("Which?", &["zzz"], None),
            answer("Unasked?", &["a"], None),
        ]);
        let (questions, ledger, _dir) = rig(Arc::new(Answers(Mutex::new(Some(reply)))));
        questions.ask(ROOT_AGENT, vec![question()]).await;
        assert_eq!(
            ledger.view().user_messages,
            vec![
                "Answered the agent's question \"Which?\": picked \"zzz\"\n\
                 Answered the agent's question \"Unasked?\": picked \"a\""
            ]
        );
    }

    #[tokio::test]
    async fn the_answer_record_has_the_dialog_origin() {
        let reply = QuestionReply::Answered(vec![answer("Which?", &["a"], None)]);
        let (questions, ledger, _dir) = rig(Arc::new(Answers(Mutex::new(Some(reply)))));
        questions.ask(ROOT_AGENT, vec![question()]).await;
        let entries = ledger.read().unwrap();
        assert!(entries.iter().any(|entry| matches!(
            &entry.record,
            Record::UserMessage {
                origin: MessageOrigin::Dialog,
                agent: None,
                ..
            }
        )));
        assert!(
            command_request(&questions).screen_intent.is_some(),
            "the screen reads the intent without the answer"
        );
    }

    #[tokio::test]
    async fn declined_and_unavailable_append_nothing() {
        let (questions, ledger, _dir) = rig(Arc::new(Answers(Mutex::new(None))));
        assert_eq!(
            questions.ask(ROOT_AGENT, vec![question()]).await,
            QuestionReply::Declined
        );
        let (questions, ledger2, _dir2) = rig(Arc::new(DenyAll));
        assert_eq!(
            questions.ask(ROOT_AGENT, vec![question()]).await,
            QuestionReply::Unavailable
        );
        assert!(ledger.view().user_messages.is_empty());
        assert!(ledger2.view().user_messages.is_empty());
    }
}
