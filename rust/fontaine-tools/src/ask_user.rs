//! The `ask_user_question` tool: lets the model stop and put 1-4 multiple-choice questions to the
//! human. The answer comes only from the [`QuestionSource`], never from a permission mode or from
//! the model, so the call is a way to show text and collect a choice and nothing more.

use std::collections::HashSet;
use std::sync::Arc;

use futures::future::BoxFuture;
use rig_core::tool::{Tool, ToolContext, ToolExecutionError};
use serde::Deserialize;
use serde_json::json;

use crate::gate::{AgentId, ROOT_AGENT};

pub const MAX_QUESTIONS: usize = 4;
pub const MIN_OPTIONS: usize = 2;
pub const MAX_OPTIONS: usize = 4;
pub const MAX_HEADER_CHARS: usize = 12;

pub const UNAVAILABLE_TEXT: &str = "User input is unavailable in this session; proceed with your best judgment and state your assumptions.";
pub const DECLINED_TEXT: &str =
    "The user declined to answer; proceed with your best judgment or ask in plain text.";

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AskUserArgs {
    pub questions: Vec<Question>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Question {
    pub question: String,
    /// A short label for the dialog's chip.
    pub header: String,
    pub options: Vec<QuestionOption>,
    #[serde(default, rename = "multiSelect", alias = "multi_select")]
    pub multi_select: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
}

/// What the human chose for one question: at least one of a selected label or some typed
/// text. A single-select answer may set both, the typed text riding along as a note on the
/// choice; a multi-select answer may set several labels and the text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestionAnswer {
    pub question: String,
    pub selected: Vec<String>,
    /// The text typed into the always-present "Other" row, alone or as a note on a choice.
    pub other: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuestionReply {
    Answered(Vec<QuestionAnswer>),
    /// The human cancelled the whole call.
    Declined,
    /// Nobody can answer: a headless run, or a surface without a dialog.
    Unavailable,
}

/// Where answers come from. `fontaine-core` implements it over the session's approver.
pub trait QuestionSource: Send + Sync {
    fn ask<'a>(&'a self, agent: &'a str, questions: Vec<Question>) -> BoxFuture<'a, QuestionReply>;
}

#[derive(Clone)]
pub struct AskUserTool {
    pub source: Arc<dyn QuestionSource>,
}

/// The shape the model has to get right, as an error it can correct.
pub fn validate(args: &AskUserArgs) -> Result<(), String> {
    let count = args.questions.len();
    if count == 0 || count > MAX_QUESTIONS {
        return Err(format!(
            "`questions` takes 1 to {MAX_QUESTIONS} questions, got {count}"
        ));
    }
    for (at, question) in args.questions.iter().enumerate() {
        let n = at + 1;
        if question.question.trim().is_empty() {
            return Err(format!("question {n} has an empty `question`"));
        }
        let header_chars = question.header.chars().count();
        if question.header.trim().is_empty() || header_chars > MAX_HEADER_CHARS {
            return Err(format!(
                "question {n}: `header` must be 1 to {MAX_HEADER_CHARS} characters"
            ));
        }
        let options = question.options.len();
        if !(MIN_OPTIONS..=MAX_OPTIONS).contains(&options) {
            return Err(format!(
                "question {n}: `options` takes {MIN_OPTIONS} to {MAX_OPTIONS} options, got {options}; \
                 an \"Other\" choice is always offered and is not listed"
            ));
        }
        let mut seen = HashSet::new();
        for option in &question.options {
            if option.label.trim().is_empty() {
                return Err(format!("question {n} has an option with an empty `label`"));
            }
            if !seen.insert(option.label.as_str()) {
                return Err(format!(
                    "question {n}: option label `{}` is used twice",
                    option.label
                ));
            }
        }
    }
    Ok(())
}

/// The tool result for a reply.
pub fn render(reply: &QuestionReply) -> String {
    match reply {
        QuestionReply::Unavailable => UNAVAILABLE_TEXT.to_string(),
        QuestionReply::Declined => DECLINED_TEXT.to_string(),
        QuestionReply::Answered(answers) => answers
            .iter()
            .map(|answer| {
                let mut parts: Vec<&str> = answer.selected.iter().map(String::as_str).collect();
                if let Some(other) = &answer.other {
                    parts.push(other);
                }
                format!("\"{}\" = \"{}\"", answer.question, parts.join(", "))
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

impl Tool for AskUserTool {
    const NAME: &'static str = "ask_user_question";
    type Args = AskUserArgs;
    type Output = String;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "Ask the user 1 to 4 multiple-choice questions when you need a decision or a missing \
         detail that you cannot reasonably infer. Each question has a short `header`, 2 to 4 \
         `options` (a `label` and a `description` each) and `multiSelect`. The user can always \
         type their own answer, so do not add an \"Other\" option. Returns the answers. In a \
         session nobody can answer, it says so and you must proceed on your best judgment."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": MAX_QUESTIONS,
                    "description": "The questions to ask, in order.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "question": { "type": "string", "description": "The complete question, ending with a question mark." },
                            "header": { "type": "string", "maxLength": MAX_HEADER_CHARS, "description": "A very short label shown as a chip." },
                            "options": {
                                "type": "array",
                                "minItems": MIN_OPTIONS,
                                "maxItems": MAX_OPTIONS,
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "label": { "type": "string", "description": "The choice, in a few words." },
                                        "description": { "type": "string", "description": "What choosing it means." },
                                    },
                                    "required": ["label", "description"],
                                    "additionalProperties": false,
                                },
                            },
                            "multiSelect": { "type": "boolean", "description": "Whether more than one option may be chosen." },
                        },
                        "required": ["question", "header", "options"],
                        "additionalProperties": false,
                    },
                },
            },
            "required": ["questions"],
            "additionalProperties": false,
        })
    }

    async fn call(
        &self,
        context: &mut ToolContext,
        args: AskUserArgs,
    ) -> Result<String, ToolExecutionError> {
        validate(&args).map_err(ToolExecutionError::invalid_args)?;
        let agent = context
            .get::<AgentId>()
            .ok()
            .flatten()
            .map_or_else(|| ROOT_AGENT.to_string(), |id| id.0);
        // A subagent must never put a dialog in front of the human.
        if agent != ROOT_AGENT {
            return Ok(render(&QuestionReply::Unavailable));
        }
        let reply = self.source.ask(&agent, args.questions).await;
        Ok(render(&reply))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Scripted {
        reply: QuestionReply,
        asked: Mutex<Vec<String>>,
    }

    impl QuestionSource for Scripted {
        fn ask<'a>(
            &'a self,
            agent: &'a str,
            _questions: Vec<Question>,
        ) -> BoxFuture<'a, QuestionReply> {
            self.asked.lock().unwrap().push(agent.to_string());
            Box::pin(async move { self.reply.clone() })
        }
    }

    fn option(label: &str) -> QuestionOption {
        QuestionOption {
            label: label.into(),
            description: "d".into(),
        }
    }

    fn question() -> Question {
        Question {
            question: "Which one?".into(),
            header: "Pick".into(),
            options: vec![option("a"), option("b")],
            multi_select: false,
        }
    }

    fn args(questions: Vec<Question>) -> AskUserArgs {
        AskUserArgs { questions }
    }

    fn tool(reply: QuestionReply) -> (AskUserTool, Arc<Scripted>) {
        let source = Arc::new(Scripted {
            reply,
            asked: Mutex::new(Vec::new()),
        });
        (
            AskUserTool {
                source: source.clone(),
            },
            source,
        )
    }

    #[test]
    fn a_well_formed_call_validates() {
        assert_eq!(validate(&args(vec![question()])), Ok(()));
        assert_eq!(validate(&args(vec![question(); 4])), Ok(()));
    }

    #[test]
    fn question_counts_are_bounded() {
        assert!(validate(&args(vec![])).is_err());
        assert!(validate(&args(vec![question(); 5])).is_err());
    }

    #[test]
    fn option_counts_are_bounded() {
        let mut one = question();
        one.options.truncate(1);
        assert!(validate(&args(vec![one])).is_err());
        let mut five = question();
        five.options = ["a", "b", "c", "d", "e"].map(option).to_vec();
        assert!(validate(&args(vec![five])).is_err());
    }

    #[test]
    fn duplicate_and_empty_labels_are_refused() {
        let mut dup = question();
        dup.options = vec![option("a"), option("a")];
        assert!(validate(&args(vec![dup])).unwrap_err().contains("twice"));
        let mut empty = question();
        empty.options = vec![option("a"), option(" ")];
        assert!(validate(&args(vec![empty])).is_err());
    }

    #[test]
    fn the_header_is_short() {
        let mut long = question();
        long.header = "x".repeat(MAX_HEADER_CHARS + 1);
        assert!(validate(&args(vec![long])).is_err());
        let mut exact = question();
        exact.header = "x".repeat(MAX_HEADER_CHARS);
        assert!(validate(&args(vec![exact])).is_ok());
    }

    #[test]
    fn unknown_fields_do_not_parse() {
        let bad = json!({ "questions": [], "extra": 1 });
        assert!(serde_json::from_value::<AskUserArgs>(bad).is_err());
        let bad = json!({ "questions": [{
            "question": "q", "header": "h", "extra": true,
            "options": [{ "label": "a", "description": "d" }],
        }] });
        assert!(serde_json::from_value::<AskUserArgs>(bad).is_err());
    }

    #[test]
    fn the_wire_name_for_multi_select_is_camel_case() {
        let parsed: AskUserArgs = serde_json::from_value(json!({ "questions": [{
            "question": "q", "header": "h", "multiSelect": true,
            "options": [{ "label": "a", "description": "d" }, { "label": "b", "description": "d" }],
        }] }))
        .unwrap();
        assert!(parsed.questions[0].multi_select);
    }

    #[test]
    fn answers_render_one_line_per_question() {
        let reply = QuestionReply::Answered(vec![
            QuestionAnswer {
                question: "Which?".into(),
                selected: vec!["a".into()],
                other: None,
            },
            QuestionAnswer {
                question: "Many?".into(),
                selected: vec!["x".into(), "y".into()],
                other: Some("z".into()),
            },
            QuestionAnswer {
                question: "Free?".into(),
                selected: vec![],
                other: Some("typed".into()),
            },
        ]);
        assert_eq!(
            render(&reply),
            "\"Which?\" = \"a\"\n\"Many?\" = \"x, y, z\"\n\"Free?\" = \"typed\""
        );
    }

    #[test]
    fn declined_and_unavailable_have_their_own_texts() {
        assert_eq!(render(&QuestionReply::Unavailable), UNAVAILABLE_TEXT);
        assert_eq!(render(&QuestionReply::Declined), DECLINED_TEXT);
        assert_ne!(UNAVAILABLE_TEXT, DECLINED_TEXT);
    }

    #[tokio::test]
    async fn the_root_agent_is_asked() {
        let (tool, source) = tool(QuestionReply::Declined);
        let out = tool
            .call(&mut ToolContext::new(), args(vec![question()]))
            .await
            .unwrap();
        assert_eq!(out, DECLINED_TEXT);
        assert_eq!(*source.asked.lock().unwrap(), vec![ROOT_AGENT.to_string()]);
    }

    #[tokio::test]
    async fn a_child_is_refused_without_asking() {
        let (tool, source) = tool(QuestionReply::Declined);
        let mut context = ToolContext::new();
        context.insert(AgentId("child-1".into())).unwrap();
        let out = tool
            .call(&mut context, args(vec![question()]))
            .await
            .unwrap();
        assert_eq!(out, UNAVAILABLE_TEXT);
        assert!(source.asked.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_malformed_call_is_an_error_the_model_can_correct() {
        let (tool, source) = tool(QuestionReply::Declined);
        let result = tool.call(&mut ToolContext::new(), args(vec![])).await;
        assert!(result.is_err());
        assert!(source.asked.lock().unwrap().is_empty());
    }
}
