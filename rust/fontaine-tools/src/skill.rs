//! The `skill` tool: loads a skill's instructions into the conversation. It takes a skill name,
//! resolved against the index built at startup, so it can read nothing else, and an optional
//! argument string substituted as for `/name args`; what the skill's body then asks for goes
//! through the permission pipeline like any other call.

use std::sync::Arc;

use futures::future::BoxFuture;
use rig_core::tool::{Tool, ToolContext, ToolExecutionError};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

use crate::gate::{AgentId, ROOT_AGENT};

/// Where skill text comes from. `fontaine-core` implements it over the loaded extension files.
pub trait SkillSource: Send + Sync {
    /// The text of the skill `name`, with `arguments` substituted, as `agent` loads it, or why it
    /// cannot be loaded.
    fn load<'a>(
        &'a self,
        name: &'a str,
        arguments: &'a str,
        agent: &'a str,
    ) -> BoxFuture<'a, Result<String, String>>;
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillArgs {
    pub name: String,
    // Models guess the field name; a call that fails to parse drops the input. `name` gets no
    // aliases because permission rules match on its raw key.
    #[serde(
        default,
        alias = "arguments",
        alias = "argument",
        alias = "input",
        alias = "params",
        deserialize_with = "lenient_args"
    )]
    pub args: String,
}

fn lenient_args<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    Ok(match Value::deserialize(deserializer)? {
        Value::String(text) => text,
        Value::Null => String::new(),
        Value::Array(items) => items
            .iter()
            .map(|item| match item {
                Value::String(text) => quote_word(text),
                other => quote_word(&other.to_string()),
            })
            .collect::<Vec<_>>()
            .join(" "),
        other => other.to_string(),
    })
}

// `split_arguments` has no escape character, so a word is protected with the quote it does not
// contain; one holding both kinds can only be approximated.
fn quote_word(word: &str) -> String {
    let needs_quotes = word.is_empty()
        || word
            .chars()
            .any(|c| c.is_whitespace() || c == '"' || c == '\'');
    if !needs_quotes {
        return word.to_string();
    }
    let quote = if word.contains('"') { '\'' } else { '"' };
    format!("{quote}{word}{quote}")
}

#[derive(Clone)]
pub struct SkillTool {
    pub source: Arc<dyn SkillSource>,
}

impl Tool for SkillTool {
    const NAME: &'static str = "skill";
    type Args = SkillArgs;
    type Output = String;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "Load a skill: instructions for a kind of task, named in the skill list of the system \
         prompt. Takes the skill's `name` and returns its text, which you then follow. \
         Pass optional `args` to give the skill arguments."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "The skill's name, from the skill list." },
                "args": {
                    "type": "string",
                    "description": "Arguments for the skill, as a single string; shell-style \
                                    quoting groups words. Omit when the skill takes none.",
                },
            },
            "required": ["name"],
            "additionalProperties": false,
        })
    }

    async fn call(
        &self,
        context: &mut ToolContext,
        args: SkillArgs,
    ) -> Result<String, ToolExecutionError> {
        let agent = context
            .get::<AgentId>()
            .ok()
            .flatten()
            .map_or_else(|| ROOT_AGENT.to_string(), |id| id.0);
        self.source
            .load(&args.name, &args.args, &agent)
            .await
            .map_err(ToolExecutionError::invalid_args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(value: Value) -> Result<SkillArgs, serde_json::Error> {
        serde_json::from_value(value)
    }

    fn args_of(value: Value) -> String {
        parse(value).unwrap().args
    }

    #[test]
    fn the_reported_call_parses_with_its_arguments() {
        let parsed = parse(json!({"name": "x", "arguments": "tasks/p.md"})).unwrap();
        assert_eq!(parsed.name, "x");
        assert_eq!(parsed.args, "tasks/p.md");
    }

    #[test]
    fn args_and_each_alias_are_accepted() {
        for field in ["args", "arguments", "argument", "input", "params"] {
            assert_eq!(
                args_of(json!({"name": "x", field: "a b"})),
                "a b",
                "{field}"
            );
        }
    }

    #[test]
    fn omitted_empty_and_null_arguments_are_empty() {
        assert_eq!(args_of(json!({"name": "x"})), "");
        assert_eq!(args_of(json!({"name": "x", "args": ""})), "");
        assert_eq!(args_of(json!({"name": "x", "args": null})), "");
    }

    #[test]
    fn an_array_becomes_a_shell_quoted_string() {
        assert_eq!(
            args_of(json!({"name": "x", "args": ["a", "b c", "", "it's", "say \"hi\""]})),
            "a \"b c\" \"\" \"it's\" 'say \"hi\"'"
        );
    }

    #[test]
    fn a_non_string_array_element_is_quoted_as_one_word() {
        assert_eq!(
            args_of(json!({"name": "x", "args": ["a", {"k": "a b"}]})),
            "a '{\"k\":\"a b\"}'"
        );
    }

    struct Recorder(std::sync::Mutex<Vec<(String, String, String)>>);

    impl SkillSource for Recorder {
        fn load<'a>(
            &'a self,
            name: &'a str,
            arguments: &'a str,
            agent: &'a str,
        ) -> BoxFuture<'a, Result<String, String>> {
            self.0
                .lock()
                .unwrap()
                .push((name.into(), arguments.into(), agent.into()));
            Box::pin(async { Ok("body".to_string()) })
        }
    }

    #[tokio::test]
    async fn the_tool_passes_arguments_and_agent_to_the_source() {
        let source = Arc::new(Recorder(Default::default()));
        let tool = SkillTool {
            source: source.clone(),
        };
        let args = parse(json!({"name": "x", "arguments": "a \"b c\""})).unwrap();
        let out = tool.call(&mut ToolContext::new(), args).await.unwrap();
        assert_eq!(out, "body");
        assert_eq!(
            *source.0.lock().unwrap(),
            vec![("x".into(), "a \"b c\"".into(), ROOT_AGENT.to_string())]
        );
    }

    #[test]
    fn other_values_become_json_text() {
        assert_eq!(args_of(json!({"name": "x", "args": {"k": 1}})), "{\"k\":1}");
        assert_eq!(args_of(json!({"name": "x", "args": 42})), "42");
        assert_eq!(args_of(json!({"name": "x", "args": true})), "true");
    }

    #[test]
    fn unknown_fields_are_rejected() {
        assert!(parse(json!({"name": "x", "bogus": 1})).is_err());
        assert!(parse(json!({"skill": "x"})).is_err());
    }

    #[test]
    fn two_spellings_of_the_arguments_are_rejected() {
        let err = serde_json::from_str::<SkillArgs>(r#"{"name":"x","args":"a","arguments":"b"}"#)
            .unwrap_err();
        assert!(err.to_string().contains("duplicate field"), "{err}");
    }

    #[test]
    fn a_missing_name_is_an_error() {
        assert!(parse(json!({"args": "a"})).is_err());
    }
}
