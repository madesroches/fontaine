//! The model's questions for the human, put through the approver. Only the text the human typed
//! becomes guardian intent: option labels and question text are model-written, so recording
//! them would let the model launder its own words into what the user asked for.

use std::sync::Arc;

use fontaine_tools::ask_user::{Question, QuestionReply, QuestionSource};
use futures::future::BoxFuture;

use crate::gate::SessionState;
use crate::ledger::Record;
use crate::permissions::Approver;

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
            let reply = self.approver.ask_user(agent, questions).await;
            if let QuestionReply::Answered(answers) = &reply {
                let typed: Vec<String> = answers
                    .iter()
                    .filter_map(|answer| answer.other.as_deref())
                    .map(|text| format!("Other: \"{text}\""))
                    .collect();
                if !typed.is_empty() {
                    self.state.append(Record::UserMessage {
                        text: typed.join("\n"),
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
        let agents = Arc::new(AgentTree::new(Limits::default()));
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
            agent_types: Arc::new(fontaine_agents::registry::AgentTypes::builtin()),
            reveal_mcp_names: false,
            auto_edits: crate::gate::AutoEdits::default(),
        }));
        (HumanQuestions::new(approver, state), ledger, dir)
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

    fn answer(question: &str, selected: &[&str], other: Option<&str>) -> QuestionAnswer {
        QuestionAnswer {
            question: question.into(),
            selected: selected.iter().map(|s| s.to_string()).collect(),
            other: other.map(str::to_string),
        }
    }

    #[tokio::test]
    async fn typed_other_text_becomes_one_user_message_in_order() {
        let reply = QuestionReply::Answered(vec![
            answer("One?", &["a"], None),
            answer("Two?", &[], Some("first")),
            answer("Three?", &["b"], Some("second")),
        ]);
        let (questions, ledger, _dir) = rig(Arc::new(Answers(Mutex::new(Some(reply.clone())))));
        assert_eq!(questions.ask(ROOT_AGENT, vec![question()]).await, reply);
        assert_eq!(
            ledger.view().user_messages,
            vec!["Other: \"first\"\nOther: \"second\""]
        );
        let request = crate::adjudication::build(
            &questions.state,
            fontaine_tools::gate::ROOT_AGENT,
            "shell",
            &serde_json::json!({ "command": "ls" }),
            Vec::new(),
            None,
        );
        assert_eq!(
            request.recent_user_messages,
            vec!["Other: \"first\"\nOther: \"second\""]
        );
    }

    #[tokio::test]
    async fn selected_labels_never_become_intent() {
        let reply = QuestionReply::Answered(vec![answer("Which?", &["a"], None)]);
        let (questions, ledger, _dir) = rig(Arc::new(Answers(Mutex::new(Some(reply)))));
        questions.ask(ROOT_AGENT, vec![question()]).await;
        assert!(ledger.view().user_messages.is_empty());
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
