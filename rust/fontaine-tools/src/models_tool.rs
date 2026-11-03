//! The `models` tool: reports the models each configured account offers, so the agent need not
//! read the config directory to learn them. Its only argument names an account, and the report
//! it gets back carries no endpoint, key or environment value.

use std::sync::Arc;

use futures::future::BoxFuture;
use rig_core::tool::{Tool, ToolContext, ToolExecutionError};
use serde::Deserialize;
use serde_json::json;

/// Where the report comes from. `fontaine-core` implements it over the session's accounts.
pub trait ModelSource: Send + Sync {
    /// The report for `account`, or for every account when `None`, as text; or why it cannot be
    /// made.
    fn report<'a>(&'a self, account: Option<&'a str>) -> BoxFuture<'a, Result<String, String>>;
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelsArgs {
    #[serde(default)]
    pub account: Option<String>,
}

#[derive(Clone)]
pub struct ModelsTool {
    pub source: Arc<dyn ModelSource>,
}

impl Tool for ModelsTool {
    const NAME: &'static str = "models";
    type Args = ModelsArgs;
    type Output = String;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "List the models each configured account offers, marking the session's main and guardian \
         models and the benchmark-validated guardian models. Takes an optional `account` to list \
         just that one. Use it to pick a model id for an `<account>/<model>` reference."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "account": {
                    "type": "string",
                    "description": "List only this account; every configured account when omitted.",
                },
            },
            "additionalProperties": false,
        })
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: ModelsArgs,
    ) -> Result<String, ToolExecutionError> {
        self.source
            .report(args.account.as_deref())
            .await
            .map_err(ToolExecutionError::invalid_args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_reject_unknown_fields() {
        let bad = serde_json::from_value::<ModelsArgs>(json!({"account": "a", "url": "x"}));
        assert!(bad.is_err());
        let none: ModelsArgs = serde_json::from_value(json!({})).unwrap();
        assert_eq!(none.account, None);
    }
}
