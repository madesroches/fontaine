//! Whether the provider's own server-side search is offered to the model this turn. It runs
//! inside the model's response, so neither the guardian nor the result scanner sees a query or a
//! result; the request-time gate here is the only check.

use serde_json::Value;

use crate::config::{Config, NativeSearch, PermissionMode};
use crate::gate::{Rules, SessionState};
use crate::models::Models;
use crate::permission_rules::SourcedRule;

const TOOL: &str = "websearch";

fn names_tool(rules: &[SourcedRule]) -> bool {
    rules.iter().any(|r| r.rule.tool == TOOL)
}

/// Whether the live mode and rules let a search run without a decision of Fontaine's. A `deny`
/// or `ask` rule for `websearch` always withholds it. Where a non-allowed call would ask or be
/// denied (`manual`, `acceptEdits`, `dontAsk`) it needs an `allow` rule.
pub(crate) fn permitted(mode: PermissionMode, rules: &Rules) -> bool {
    if names_tool(&rules.deny) || names_tool(&rules.ask) {
        return false;
    }
    match mode {
        PermissionMode::Auto | PermissionMode::Plan => true,
        PermissionMode::Manual | PermissionMode::AcceptEdits | PermissionMode::DontAsk => {
            names_tool(&rules.allow)
        }
    }
}

/// The provider's search tool entry when the main model's account supports it and
/// `[websearch] native` allows it.
fn provider_entry(config: &Config, models: &Models) -> Option<Value> {
    if config.websearch.native == NativeSearch::Off {
        return None;
    }
    let provider = models.main.kind.provider();
    let entry = provider.native_web_search()?;
    if config.websearch.native == NativeSearch::On {
        return Some(entry);
    }
    let account = config.accounts.get(&models.main.reference.account)?;
    let base = account.base_url.as_deref().filter(|url| !url.is_empty());
    let official = provider.default_endpoint()?;
    base.is_none_or(|url| url.trim_end_matches('/') == official)
        .then_some(entry)
}

/// The tool entry the request carries now, or `None` when the client-side backend (if any)
/// serves searches.
pub(crate) fn offered(config: &Config, models: &Models, state: &SessionState) -> Option<Value> {
    let entry = provider_entry(config, models)?;
    permitted(state.mode(), &state.rules()).then_some(entry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permission_rules::{RuleSource, parse_rule};

    fn rule(text: &str) -> SourcedRule {
        SourcedRule::new(parse_rule(text).unwrap(), RuleSource::Session)
    }

    fn rules(deny: &[&str], ask: &[&str], allow: &[&str]) -> Rules {
        let list = |texts: &[&str]| texts.iter().map(|t| rule(t)).collect();
        Rules {
            deny: list(deny),
            ask: list(ask),
            allow: list(allow),
        }
    }

    #[test]
    fn auto_and_plan_offer_it_without_a_rule() {
        for mode in [PermissionMode::Auto, PermissionMode::Plan] {
            assert!(permitted(mode, &Rules::default()), "{mode:?}");
        }
    }

    #[test]
    fn modes_that_would_ask_or_deny_need_an_allow_rule() {
        for mode in [
            PermissionMode::Manual,
            PermissionMode::AcceptEdits,
            PermissionMode::DontAsk,
        ] {
            assert!(!permitted(mode, &Rules::default()), "{mode:?}");
            assert!(
                permitted(mode, &rules(&[], &[], &["websearch"])),
                "{mode:?}"
            );
        }
    }

    #[test]
    fn a_deny_or_ask_rule_withholds_it_in_every_mode() {
        for mode in [
            PermissionMode::Auto,
            PermissionMode::Plan,
            PermissionMode::Manual,
        ] {
            assert!(!permitted(
                mode,
                &rules(&["websearch"], &[], &["websearch"])
            ));
            assert!(!permitted(
                mode,
                &rules(&[], &["websearch"], &["websearch"])
            ));
        }
    }

    #[test]
    fn rules_for_other_tools_do_not_count() {
        assert!(!permitted(
            PermissionMode::Manual,
            &rules(&[], &[], &["webfetch(domain:example.com)"])
        ));
        assert!(permitted(
            PermissionMode::Auto,
            &rules(&["webfetch"], &["shell"], &[])
        ));
    }
}
