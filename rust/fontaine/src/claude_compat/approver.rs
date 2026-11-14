//! `canUseTool`: the host program answers where a person would. It is an [`Approver`], so it is
//! reached only from the pipeline's `ask_human`, after the hooks, the floor, the rules, the mode
//! and the guardian have all had their say, and every answer it gives is one a person at the
//! terminal could have given: never an allow-always, and `updatedPermissions` only as session
//! allows or a mode change.

use std::sync::Arc;

use fontaine_core::config::PermissionMode;
use fontaine_core::permission_rules::{Rule, parse_rule};
use fontaine_core::permissions::{Answer, ApprovalRequest, Approver};
use fontaine_tools::ask_user::{Question, QuestionAnswer, QuestionReply};
use futures::future::BoxFuture;
use serde_json::{Value, json};

use super::cli::parse_mode;
use super::router::{RequestError, Router};
use super::translate::wire_name;

pub struct SdkApprover {
    router: Arc<Router>,
}

impl SdkApprover {
    pub fn new(router: Arc<Router>) -> SdkApprover {
        SdkApprover { router }
    }
}

/// The one rule an "allow session" answer would add, as Claude's `permission_suggestions`.
pub fn suggestions(proposed: Option<&str>) -> Vec<Value> {
    let Some(rule) = proposed.and_then(|text| parse_rule(text).ok()) else {
        return Vec::new();
    };
    let mut entry = json!({ "toolName": wire_name(&rule.tool) });
    if let (Some(spec), Some(map)) = (&rule.specifier, entry.as_object_mut()) {
        map.insert("ruleContent".into(), json!(spec));
    }
    vec![json!({
        "type": "addRules",
        "rules": [entry],
        "behavior": "allow",
        "destination": "session",
    })]
}

/// The `can_use_tool` request for one prompt.
pub fn permission_request(request: &ApprovalRequest) -> Value {
    let mut body = json!({
        "subtype": "can_use_tool",
        "tool_name": wire_name(&request.tool),
        "input": request.args,
        "permission_suggestions": suggestions(request.proposed_rule.as_deref()),
        "decision_reason": request.reason,
        "description": request.summary,
    });
    let map = body.as_object_mut().expect("an object");
    if let Some(id) = &request.tool_use_id {
        map.insert("tool_use_id".into(), json!(id));
    }
    if !request.agent.is_empty() && request.agent != fontaine_tools::gate::ROOT_AGENT {
        map.insert("agent_id".into(), json!(request.agent));
    }
    body
}

/// A rule of `updatedPermissions`, in Fontaine's syntax.
fn rule_of(entry: &Value) -> Result<Rule, String> {
    let tool = entry
        .get("toolName")
        .and_then(Value::as_str)
        .ok_or("a rule has no `toolName`")?;
    let text = match entry.get("ruleContent").and_then(Value::as_str) {
        Some(content) if !content.is_empty() => format!("{tool}({content})"),
        _ => tool.to_string(),
    };
    let mapped = fontaine_core::extensions::map_tool_entry(&text)?;
    parse_rule(&mapped).map_err(|e| e.to_string())
}

/// What a valid `updatedPermissions` list asks for.
#[derive(Debug, Default, PartialEq, Eq)]
struct Updates {
    grants: Vec<Rule>,
    mode: Option<PermissionMode>,
}

/// Validates the whole list before anything is applied. Only session allows and a mode change are
/// accepted; anything else fails the whole answer, because Fontaine has no session deny or ask
/// state that a resume restores and nothing a host says may reach the user's settings.
fn updates(permissions: &[Value]) -> Result<Updates, String> {
    let mut out = Updates::default();
    for update in permissions {
        let kind = update
            .get("type")
            .and_then(Value::as_str)
            .ok_or("an update has no `type`")?;
        match kind {
            "addRules" => {
                let behavior = update.get("behavior").and_then(Value::as_str);
                if behavior != Some("allow") {
                    return Err(format!(
                        "an `addRules` update with behavior `{}` is not accepted: only session allows are",
                        behavior.unwrap_or("none")
                    ));
                }
                let destination = update.get("destination").and_then(Value::as_str);
                if !matches!(destination, Some("session" | "cliArg")) {
                    return Err(format!(
                        "an allow rule for destination `{}` is not accepted: only `session` and `cliArg` are",
                        destination.unwrap_or("none")
                    ));
                }
                let rules = update
                    .get("rules")
                    .and_then(Value::as_array)
                    .ok_or("an `addRules` update has no `rules`")?;
                for rule in rules {
                    out.grants.push(rule_of(rule)?);
                }
            }
            "setMode" => {
                let mode = update
                    .get("mode")
                    .and_then(Value::as_str)
                    .ok_or("a `setMode` update has no `mode`")?;
                let mode = parse_mode(mode)?;
                if mode == PermissionMode::Afk {
                    return Err("a host cannot turn AFK mode on".to_string());
                }
                out.mode = Some(mode);
            }
            other => {
                return Err(format!("an update of type `{other}` is not accepted"));
            }
        }
    }
    Ok(out)
}

fn denied(why: impl Into<String>) -> Answer {
    Answer::Deny {
        feedback: Some(why.into()),
    }
}

/// Turns the host's reply into the answer of a person at the prompt. Anything it cannot read is a
/// denial.
pub fn decode(
    sent: &Value,
    proposed: Option<&str>,
    reply: Result<Value, RequestError>,
) -> (Answer, Option<HostInterrupt>) {
    let reply = match reply {
        Ok(reply) => reply,
        Err(e) => {
            return (
                denied(format!(
                    "the host's permission callback gave no answer: {e}"
                )),
                None,
            );
        }
    };
    match reply.get("behavior").and_then(Value::as_str) {
        Some("allow") => {
            let permissions: &[Value] = match reply.get("updatedPermissions") {
                None | Some(Value::Null) => &[],
                Some(Value::Array(list)) => list,
                Some(_) => {
                    return (
                        denied("the host's `updatedPermissions` is not a list"),
                        None,
                    );
                }
            };
            let changes = match updates(permissions) {
                Ok(changes) => changes,
                Err(why) => {
                    return (
                        denied(format!("the host's answer was refused: {why}")),
                        None,
                    );
                }
            };
            let args = match reply.get("updatedInput") {
                None | Some(Value::Null) => None,
                Some(input) if input == sent => None,
                Some(input) if input.is_object() => Some(input.clone()),
                Some(_) => {
                    return (denied("the host's `updatedInput` is not an object"), None);
                }
            };
            let answer = match (args, changes.mode, changes.grants.as_slice()) {
                (None, None, []) => Answer::AllowOnce,
                (None, None, [only]) if Some(only.to_string().as_str()) == proposed => {
                    Answer::AllowSession
                }
                (args, mode, _) => Answer::AllowUpdated {
                    args,
                    grants: changes.grants,
                    mode,
                },
            };
            (answer, None)
        }
        Some("deny") => {
            let message = reply
                .get("message")
                .and_then(Value::as_str)
                .filter(|m| !m.trim().is_empty())
                .unwrap_or("the host denied it")
                .to_string();
            let interrupt = reply
                .get("interrupt")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                .then_some(HostInterrupt);
            (
                Answer::Deny {
                    feedback: Some(message),
                },
                interrupt,
            )
        }
        other => (
            denied(format!(
                "the host's answer has the unknown behavior `{}`",
                other.unwrap_or("none")
            )),
            None,
        ),
    }
}

/// The host asked for the turn to end with its denial.
#[derive(Debug, PartialEq, Eq)]
pub struct HostInterrupt;

impl Approver for SdkApprover {
    fn approve<'a>(&'a self, request: ApprovalRequest) -> BoxFuture<'a, Answer> {
        Box::pin(async move {
            let body = permission_request(&request);
            let reply = self.router.request(body).await;
            let (answer, interrupt) =
                decode(&request.args, request.proposed_rule.as_deref(), reply);
            if interrupt.is_some() {
                // The turn ends once this answer is delivered, as the TUI's Esc does.
                let router = self.router.clone();
                tokio::spawn(async move { router.interrupt().await });
            }
            answer
        })
    }

    fn ask_user<'a>(
        &'a self,
        agent: &'a str,
        questions: Vec<Question>,
    ) -> BoxFuture<'a, QuestionReply> {
        Box::pin(async move {
            let listed: Vec<Value> = questions
                .iter()
                .map(|q| {
                    json!({
                        "question": q.question,
                        "header": q.header,
                        "options": q.options.iter().map(|o| json!({
                            "label": o.label, "description": o.description,
                        })).collect::<Vec<_>>(),
                        "multiSelect": q.multi_select,
                    })
                })
                .collect();
            let mut body = json!({
                "subtype": "can_use_tool",
                "tool_name": "AskUserQuestion",
                "input": { "questions": listed },
            });
            if !agent.is_empty() && agent != fontaine_tools::gate::ROOT_AGENT {
                body["agent_id"] = json!(agent);
            }
            let reply = self.router.request(body).await;
            decode_answers(&questions, reply)
        })
    }
}

/// The host's answers to `AskUserQuestion`: every question must be answered, or the reply is
/// unusable.
pub fn decode_answers(questions: &[Question], reply: Result<Value, RequestError>) -> QuestionReply {
    let Ok(reply) = reply else {
        return QuestionReply::Unavailable;
    };
    match reply.get("behavior").and_then(Value::as_str) {
        Some("deny") => QuestionReply::Declined,
        Some("allow") => {
            let Some(answers) = reply
                .pointer("/updatedInput/answers")
                .and_then(Value::as_object)
            else {
                return QuestionReply::Unavailable;
            };
            let mut out = Vec::new();
            for question in questions {
                let Some(text) = answers.get(&question.question).and_then(Value::as_str) else {
                    return QuestionReply::Unavailable;
                };
                let known = |label: &str| question.options.iter().any(|o| o.label == label);
                let (selected, other) = if known(text) {
                    (vec![text.to_string()], None)
                } else if question.multi_select {
                    let parts: Vec<&str> = text.split(", ").collect();
                    if parts.iter().all(|p| known(p)) {
                        (parts.iter().map(|p| p.to_string()).collect(), None)
                    } else {
                        (Vec::new(), Some(text.to_string()))
                    }
                } else {
                    (Vec::new(), Some(text.to_string()))
                };
                out.push(QuestionAnswer {
                    question: question.question.clone(),
                    selected,
                    other,
                });
            }
            QuestionReply::Answered(out)
        }
        _ => QuestionReply::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fontaine_tools::ask_user::QuestionOption;

    fn sent() -> Value {
        json!({"command": "git status"})
    }

    fn rule(text: &str) -> Value {
        let parsed = parse_rule(text).unwrap();
        let mut entry = json!({"toolName": wire_name(&parsed.tool)});
        if let Some(spec) = parsed.specifier {
            entry["ruleContent"] = json!(spec);
        }
        entry
    }

    fn allow(extra: Value) -> Result<Value, RequestError> {
        let mut reply = json!({"behavior": "allow"});
        reply
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        Ok(reply)
    }

    fn decode_with(reply: Result<Value, RequestError>) -> Answer {
        decode(&sent(), Some("shell(git status)"), reply).0
    }

    #[test]
    fn a_plain_allow_is_allow_once_whether_or_not_it_echoes_the_input() {
        assert_eq!(decode_with(allow(json!({}))), Answer::AllowOnce);
        assert_eq!(
            decode_with(allow(json!({"updatedInput": sent()}))),
            Answer::AllowOnce
        );
        // Extra keys the SDKs add are tolerated.
        assert_eq!(
            decode_with(allow(
                json!({"updatedInput": sent(), "toolUseID": "t", "decisionClassification": "user_temporary"})
            )),
            Answer::AllowOnce
        );
        assert_eq!(
            decode_with(allow(json!({"updatedPermissions": []}))),
            Answer::AllowOnce
        );
    }

    #[test]
    fn the_proposed_rule_at_session_is_allow_session() {
        let reply = allow(json!({"updatedPermissions": [{
            "type": "addRules", "behavior": "allow", "destination": "session",
            "rules": [rule("shell(git status)")],
        }]}));
        assert_eq!(decode_with(reply), Answer::AllowSession);
    }

    #[test]
    fn other_session_allows_and_a_mode_are_applied_together() {
        let reply = allow(json!({"updatedPermissions": [
            {"type": "addRules", "behavior": "allow", "destination": "session",
             "rules": [rule("shell(git log)"), rule("read")]},
            {"type": "setMode", "mode": "acceptEdits"},
        ]}));
        match decode_with(reply) {
            Answer::AllowUpdated { args, grants, mode } => {
                assert_eq!(args, None);
                assert_eq!(grants.len(), 2);
                assert_eq!(mode, Some(PermissionMode::AcceptEdits));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn changed_input_is_carried_for_a_second_look() {
        let reply = allow(json!({"updatedInput": {"command": "git log"}}));
        assert_eq!(
            decode_with(reply),
            Answer::AllowUpdated {
                args: Some(json!({"command": "git log"})),
                grants: Vec::new(),
                mode: None,
            }
        );
    }

    #[test]
    fn updates_that_loosen_further_than_a_prompt_could_fail_the_whole_answer() {
        let bad = [
            json!({"type": "addRules", "behavior": "deny", "destination": "session", "rules": [rule("read")]}),
            json!({"type": "addRules", "behavior": "ask", "destination": "session", "rules": [rule("read")]}),
            json!({"type": "addRules", "behavior": "allow", "destination": "userSettings", "rules": [rule("read")]}),
            json!({"type": "addRules", "behavior": "allow", "destination": "projectSettings", "rules": [rule("read")]}),
            json!({"type": "addRules", "behavior": "allow", "destination": "localSettings", "rules": [rule("read")]}),
            json!({"type": "replaceRules", "behavior": "allow", "destination": "session", "rules": []}),
            json!({"type": "removeRules", "behavior": "allow", "destination": "session", "rules": []}),
            json!({"type": "addDirectories", "directories": ["/"], "destination": "session"}),
            json!({"type": "removeDirectories", "directories": ["/"], "destination": "session"}),
            json!({"type": "setMode", "mode": "bypassPermissions"}),
            json!({"type": "setMode", "mode": "afk"}),
            json!({"type": "addRules", "behavior": "allow", "destination": "session", "rules": [{"toolName": "NoSuchTool"}]}),
        ];
        for update in bad {
            // One good update beside a bad one: the whole list is refused.
            let reply = allow(json!({"updatedPermissions": [
                {"type": "addRules", "behavior": "allow", "destination": "session", "rules": [rule("read")]},
                update.clone(),
            ]}));
            assert!(
                matches!(decode_with(reply), Answer::Deny { feedback: Some(_) }),
                "{update}"
            );
        }
    }

    #[test]
    fn a_deny_carries_its_message_and_may_interrupt() {
        let (answer, interrupt) = decode(
            &sent(),
            None,
            Ok(json!({"behavior": "deny", "message": "no thanks"})),
        );
        assert_eq!(
            answer,
            Answer::Deny {
                feedback: Some("no thanks".into())
            }
        );
        assert_eq!(interrupt, None);
        let (_, interrupt) = decode(
            &sent(),
            None,
            Ok(json!({"behavior": "deny", "message": "stop", "interrupt": true})),
        );
        assert_eq!(interrupt, Some(HostInterrupt));
    }

    #[test]
    fn errors_malformed_replies_and_disconnects_fail_closed() {
        for reply in [
            Err(RequestError::Host("boom".into())),
            Err(RequestError::Cancelled),
            Err(RequestError::Disconnected),
            Ok(json!({"behavior": "maybe"})),
            Ok(json!({})),
            Ok(json!("allow")),
            allow(json!({"updatedPermissions": "all"})),
            allow(json!({"updatedInput": 5})),
        ] {
            assert!(
                matches!(decode_with(reply.clone()), Answer::Deny { .. }),
                "{reply:?}"
            );
        }
    }

    #[test]
    fn suggestions_exist_only_at_session_and_only_for_a_proposed_rule() {
        assert!(suggestions(None).is_empty());
        let suggested = suggestions(Some("shell(git status)"));
        assert_eq!(suggested.len(), 1);
        assert_eq!(suggested[0]["destination"], "session");
        assert_eq!(suggested[0]["behavior"], "allow");
        assert_eq!(suggested[0]["rules"][0]["toolName"], "Bash");
        assert_eq!(suggested[0]["rules"][0]["ruleContent"], "git status");
    }

    fn question(text: &str, multi: bool) -> Question {
        Question {
            question: text.into(),
            header: "h".into(),
            options: vec![
                QuestionOption {
                    label: "A".into(),
                    description: "a".into(),
                },
                QuestionOption {
                    label: "B".into(),
                    description: "b".into(),
                },
            ],
            multi_select: multi,
        }
    }

    #[test]
    fn answers_map_to_selections_and_free_text() {
        let questions = [question("one?", false), question("two?", true)];
        let reply = Ok(
            json!({"behavior": "allow", "updatedInput": {"answers": {"one?": "A", "two?": "A, B"}}}),
        );
        let QuestionReply::Answered(answers) = decode_answers(&questions, reply) else {
            panic!("answered");
        };
        assert_eq!(answers[0].selected, ["A"]);
        assert_eq!(answers[1].selected, ["A", "B"]);
        let reply = Ok(
            json!({"behavior": "allow", "updatedInput": {"answers": {"one?": "something else", "two?": "B"}}}),
        );
        let QuestionReply::Answered(answers) = decode_answers(&questions, reply) else {
            panic!("answered");
        };
        assert_eq!(answers[0].other.as_deref(), Some("something else"));
    }

    #[test]
    fn a_deny_declines_and_anything_unusable_is_unavailable() {
        let questions = [question("one?", false)];
        assert_eq!(
            decode_answers(&questions, Ok(json!({"behavior": "deny", "message": "x"}))),
            QuestionReply::Declined
        );
        for reply in [
            Err(RequestError::Disconnected),
            Err(RequestError::Cancelled),
            Ok(json!({"behavior": "allow"})),
            Ok(json!({"behavior": "allow", "updatedInput": {"answers": {}}})),
            Ok(json!({"behavior": "allow", "updatedInput": {"answers": {"one?": 4}}})),
        ] {
            assert_eq!(
                decode_answers(&questions, reply),
                QuestionReply::Unavailable
            );
        }
    }
}
