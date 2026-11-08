//! One report of the models each configured account offers, behind both `fontaine models` and
//! the `models` tool. It is built from listings and model references only, so no endpoint, key
//! or environment value can reach its output.

use std::fmt::Write;

use fontaine_guardian::models::ModelEntry;
use serde_json::{Value, json};

use crate::models::{AccountListing, ModelRef};
use crate::providers::RefusedModel;

/// What a session uses a listed model for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Main,
    Guardian,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Main => "main",
            Role::Guardian => "guardian",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelRow {
    /// The id as the account lists it, `name:tag` for Ollama.
    pub id: String,
    pub roles: Vec<Role>,
    /// The shipped guardian ranking names this model for the account's kind.
    pub validated_guardian: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountReport {
    pub account: String,
    pub kind: &'static str,
    /// The models, or the error chain of the listing that failed.
    pub models: Result<Vec<ModelRow>, String>,
    /// The rows the account listed but refuses, with why.
    pub unavailable: Vec<RefusedModel>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelReport {
    pub accounts: Vec<AccountReport>,
}

impl ModelReport {
    /// `main` and `guardian` mark the rows they name, by the provider's own matching so an Ollama
    /// bare name marks its `:latest` row; `validated` flags the rows the guardian ranking lists.
    pub fn build(
        listings: &[AccountListing],
        main: Option<&ModelRef>,
        guardian: Option<&ModelRef>,
        validated: &[ModelEntry],
    ) -> ModelReport {
        let accounts = listings
            .iter()
            .map(|listing| AccountReport {
                account: listing.account.clone(),
                kind: listing.kind.as_str(),
                models: listing.models.as_ref().map_err(Clone::clone).map(|ids| {
                    ids.iter()
                        .map(|id| row(listing, id, main, guardian, validated))
                        .collect()
                }),
                unavailable: listing.refused.clone(),
            })
            .collect();
        ModelReport { accounts }
    }

    /// Whether the report has an account and every one of them failed to list.
    pub fn all_failed(&self) -> bool {
        !self.accounts.is_empty() && self.accounts.iter().all(|a| a.models.is_err())
    }

    pub fn render_text(&self) -> String {
        if self.accounts.is_empty() {
            return "no accounts to list\n".to_string();
        }
        let mut out = String::new();
        for account in &self.accounts {
            match &account.models {
                Err(error) => {
                    let _ = writeln!(
                        out,
                        "{} ({}): could not list models: {error}",
                        account.account, account.kind
                    );
                }
                Ok(rows) => {
                    let _ = writeln!(out, "{} ({})", account.account, account.kind);
                    if rows.is_empty() && account.unavailable.is_empty() {
                        out.push_str("  (no models)\n");
                    }
                    for row in rows {
                        let _ = write!(out, "  {}", row.id);
                        for role in &row.roles {
                            let _ = write!(out, " [{}]", role.as_str());
                        }
                        if row.validated_guardian {
                            out.push_str(" [validated guardian]");
                        }
                        out.push('\n');
                    }
                    for refused in &account.unavailable {
                        let _ = writeln!(out, "  {} (unavailable: {})", refused.id, refused.reason);
                    }
                }
            }
        }
        out
    }

    pub fn to_json(&self) -> Value {
        let accounts: Vec<Value> = self
            .accounts
            .iter()
            .map(|account| match &account.models {
                Ok(rows) => {
                    let mut value = json!({
                        "account": account.account,
                        "kind": account.kind,
                        "models": rows.iter().map(|row| json!({
                            "id": row.id,
                            "roles": row.roles.iter().map(|r| r.as_str()).collect::<Vec<_>>(),
                            "validated_guardian": row.validated_guardian,
                        })).collect::<Vec<_>>(),
                    });
                    if !account.unavailable.is_empty() {
                        value["unavailable"] = account
                            .unavailable
                            .iter()
                            .map(|r| json!({"id": r.id, "reason": r.reason}))
                            .collect();
                    }
                    value
                }
                Err(error) => json!({
                    "account": account.account,
                    "kind": account.kind,
                    "error": error,
                }),
            })
            .collect();
        json!({ "accounts": accounts })
    }
}

fn row(
    listing: &AccountListing,
    id: &str,
    main: Option<&ModelRef>,
    guardian: Option<&ModelRef>,
    validated: &[ModelEntry],
) -> ModelRow {
    let provider = listing.kind.provider();
    let ids = [id.to_string()];
    let marks = |reference: Option<&ModelRef>| {
        reference.is_some_and(|r| r.account == listing.account && provider.lists(&ids, &r.model))
    };
    let roles = [(Role::Main, main), (Role::Guardian, guardian)]
        .into_iter()
        .filter(|(_, reference)| marks(*reference))
        .map(|(role, _)| role)
        .collect();
    ModelRow {
        id: id.to_string(),
        roles,
        validated_guardian: validated
            .iter()
            .any(|e| e.kind == listing.kind.as_str() && provider.lists(&ids, &e.model)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AccountKind;

    fn entry(kind: &str, model: &str) -> ModelEntry {
        ModelEntry {
            kind: kind.into(),
            model: model.into(),
            catch_rate: 1.0,
            false_deny_rate: 0.0,
            p95_ms: 1,
            scored_on: "2026-01-01".into(),
            dataset_version: "v1".into(),
            transport: "tool".into(),
        }
    }

    fn listing(account: &str, kind: &str, models: Result<Vec<&str>, &str>) -> AccountListing {
        AccountListing {
            account: account.into(),
            kind: AccountKind::named(kind),
            models: models
                .map(|ids| ids.into_iter().map(String::from).collect())
                .map_err(String::from),
            refused: Vec::new(),
        }
    }

    fn reference(text: &str) -> ModelRef {
        ModelRef::parse(text).unwrap()
    }

    #[test]
    fn roles_and_the_validated_flag_mark_the_named_rows() {
        let listings = [
            listing("a", "anthropic", Ok(vec!["m1", "m2"])),
            listing("b", "anthropic", Ok(vec!["m1"])),
        ];
        let report = ModelReport::build(
            &listings,
            Some(&reference("a/m1")),
            Some(&reference("b/m1")),
            &[entry("anthropic", "m2")],
        );
        let a = report.accounts[0].models.as_ref().unwrap();
        assert_eq!(a[0].roles, [Role::Main]);
        assert!(a[1].roles.is_empty() && a[1].validated_guardian);
        let b = report.accounts[1].models.as_ref().unwrap();
        assert_eq!(b[0].roles, [Role::Guardian]);
        assert!(!b[0].validated_guardian);
    }

    #[test]
    fn a_validated_entry_of_another_kind_does_not_flag_the_row() {
        let listings = [listing("a", "anthropic", Ok(vec!["m1"]))];
        let report = ModelReport::build(&listings, None, None, &[entry("openai", "m1")]);
        assert!(!report.accounts[0].models.as_ref().unwrap()[0].validated_guardian);
    }

    #[test]
    fn ollama_ids_keep_their_tags_and_a_bare_name_marks_latest() {
        let listings = [listing("o", "ollama", Ok(vec!["llama3:latest", "qwen:7b"]))];
        let report = ModelReport::build(&listings, Some(&reference("o/llama3")), None, &[]);
        let rows = report.accounts[0].models.as_ref().unwrap();
        assert_eq!(rows[0].id, "llama3:latest");
        assert_eq!(rows[0].roles, [Role::Main]);
        assert_eq!(rows[1].id, "qwen:7b");
        assert!(rows[1].roles.is_empty());
    }

    #[test]
    fn a_failed_account_keeps_an_error_row() {
        let listings = [
            listing("a", "anthropic", Err("boom")),
            listing("b", "anthropic", Ok(vec!["m"])),
        ];
        let report = ModelReport::build(&listings, None, None, &[]);
        assert_eq!(report.accounts[0].models, Err("boom".to_string()));
        assert!(!report.all_failed());
        assert!(ModelReport::build(&listings[..1], None, None, &[]).all_failed());
    }

    #[test]
    fn unavailable_rows_follow_the_models_and_are_omitted_when_empty() {
        let mut with = listing("a", "claude-subscription", Ok(vec!["haiku"]));
        with.refused = vec![RefusedModel {
            id: "future".into(),
            reason: "update Fontaine".into(),
        }];
        let report = ModelReport::build(&[with], None, None, &[]);
        assert_eq!(
            report.render_text(),
            "a (claude-subscription)\n  haiku\n  future (unavailable: update Fontaine)\n"
        );
        assert_eq!(
            report.to_json(),
            json!({"accounts": [{
                "account": "a", "kind": "claude-subscription",
                "models": [{"id": "haiku", "roles": [], "validated_guardian": false}],
                "unavailable": [{"id": "future", "reason": "update Fontaine"}],
            }]})
        );
        let plain =
            ModelReport::build(&[listing("a", "anthropic", Ok(vec!["m"]))], None, None, &[]);
        assert!(plain.to_json()["accounts"][0].get("unavailable").is_none());
    }

    #[test]
    fn text_and_json_are_pinned() {
        let listings = [
            listing("a", "anthropic", Ok(vec!["m1", "m2"])),
            listing("z", "openai", Err("down")),
        ];
        let report = ModelReport::build(
            &listings,
            Some(&reference("a/m1")),
            Some(&reference("a/m1")),
            &[entry("anthropic", "m1")],
        );
        assert_eq!(
            report.render_text(),
            "a (anthropic)\n  m1 [main] [guardian] [validated guardian]\n  m2\n\
             z (openai): could not list models: down\n"
        );
        assert_eq!(
            report.to_json(),
            json!({"accounts": [
                {"account": "a", "kind": "anthropic", "models": [
                    {"id": "m1", "roles": ["main", "guardian"], "validated_guardian": true},
                    {"id": "m2", "roles": [], "validated_guardian": false},
                ]},
                {"account": "z", "kind": "openai", "error": "down"},
            ]})
        );
    }
}
