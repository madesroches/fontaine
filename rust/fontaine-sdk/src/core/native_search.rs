//! Whether the provider's own server-side search is offered to the model this turn. It runs
//! inside the model's response, so neither the guardian nor the result scanner sees a query or a
//! result; the request-time gate here is the only check.

use serde_json::Value;

use crate::core::config::{Config, NativeSearch, PermissionMode};
use crate::core::gate::{Rules, SessionState};
use crate::core::models::Models;
use crate::core::permission_rules::{SourcedRule, parse_rule};

const TOOL: &str = "websearch";

fn names_tool(rules: &[SourcedRule]) -> bool {
    rules.iter().any(|r| r.rule.tool == TOOL)
}

/// Whether the live mode and rules let a search run without a decision of Fontaine's. A `deny`
/// or `ask` rule for `websearch` always withholds it (so does a covering hook, see `offered`). Where a non-allowed call would ask or be
/// denied (`manual`, `acceptEdits`, `dontAsk`) it needs an `allow` rule. While the loop breaker
/// has paused `auto`, the guardian-judged modes withhold it too: the session is back with a
/// human, and a search query is an outbound channel nothing judges.
pub(crate) fn permitted(mode: PermissionMode, rules: &Rules, paused: bool) -> bool {
    if names_tool(&rules.deny) || names_tool(&rules.ask) {
        return false;
    }
    match mode {
        PermissionMode::Auto | PermissionMode::Afk => !paused,
        PermissionMode::Manual | PermissionMode::AcceptEdits | PermissionMode::DontAsk => {
            names_tool(&rules.allow)
        }
    }
}

/// Whether a `pre_tool_use` hook could cover `websearch`. The provider runs its search inside
/// the model's response, so a hook would never see it; a hook that cannot be read counts as
/// covering, as it runs for every call.
fn hook_covers(config: &Config) -> bool {
    config
        .hooks
        .get(crate::core::config::HookEvent::PreToolUse)
        .iter()
        .any(|hook| {
            hook.matcher
                .as_deref()
                .is_none_or(|text| parse_rule(text).map_or(true, |rule| rule.tool == TOOL))
        })
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
    let Some(base) = account.base_url.as_deref().filter(|url| !url.is_empty()) else {
        return Some(entry);
    };
    let official = provider.default_endpoint()?;
    (base.trim_end_matches('/') == official).then_some(entry)
}

/// Whether the main model's account has a provider search at all, whatever the mode and rules
/// say this turn.
pub(crate) fn provider_available(config: &Config, models: &Models) -> bool {
    provider_entry(config, models).is_some()
}

/// The tool entry the request carries now, or `None` when the client-side backend (if any)
/// serves searches.
pub(crate) fn offered(config: &Config, models: &Models, state: &SessionState) -> Option<Value> {
    let entry = provider_entry(config, models)?;
    if hook_covers(config) {
        return None;
    }
    permitted(state.mode(), &state.rules(), state.auto_paused()).then_some(entry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::permission_rules::{RuleSource, parse_rule};

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

    fn provider_available_for(kind: &str, base_url: Option<&str>, native: &str) -> bool {
        use crate::core::config::{Account, AccountKind};
        use crate::core::models::{GuardianChoice, ModelHandle, ModelRef};
        let mut config = Config::defaults(std::path::PathBuf::new());
        config.accounts.insert(
            "acct".to_string(),
            Account::new(AccountKind::named(kind), base_url.map(str::to_owned)),
        );
        config.websearch.native = match native {
            "off" => NativeSearch::Off,
            "on" => NativeSearch::On,
            _ => NativeSearch::Auto,
        };
        let handle = ModelHandle::new(
            ModelRef::parse("acct/m").unwrap(),
            AccountKind::named(kind),
            rig_core::test_utils::MockCompletionModel::from_stream_turns(Vec::<
                Vec<rig_core::test_utils::MockStreamEvent>,
            >::new())
            .into(),
        );
        let models = Models {
            main_chain: crate::core::model_roles::ModelChain::single(handle.reference.clone()),
            main: handle,
            main_listing: crate::core::models::Listing::default(),
            guardian: None,
            guardian_choice: GuardianChoice::MainFallback,
            notices: Vec::new(),
        };
        provider_available(&config, &models)
    }

    #[test]
    fn claude_subscription_has_no_endpoint_and_still_passes_auto() {
        assert!(provider_available_for("claude-subscription", None, "auto"));
        assert!(provider_available_for(
            "claude-subscription",
            Some(""),
            "auto"
        ));
        assert!(provider_available_for("claude-subscription", None, "on"));
        assert!(!provider_available_for("claude-subscription", None, "off"));
    }

    #[test]
    fn auto_still_compares_a_set_base_url_with_the_official_endpoint() {
        assert!(provider_available_for("anthropic", None, "auto"));
        assert!(provider_available_for(
            "anthropic",
            Some("https://api.anthropic.com/"),
            "auto"
        ));
        assert!(!provider_available_for(
            "anthropic",
            Some("https://gateway.example.com"),
            "auto"
        ));
        assert!(!provider_available_for("openai", None, "auto"));
    }

    fn config_with_hook(matcher: Option<&str>) -> Config {
        let mut config = Config::defaults(std::path::PathBuf::new());
        config
            .hooks
            .list_mut(crate::core::config::HookEvent::PreToolUse)
            .push(crate::core::config::Hook::command(matcher, "true"));
        config
    }

    #[test]
    fn a_hook_that_could_cover_websearch_withholds_it() {
        assert!(!hook_covers(&Config::defaults(std::path::PathBuf::new())));
        for covering in [Some("websearch"), Some("websearch(x)"), None, Some("((")] {
            assert!(hook_covers(&config_with_hook(covering)), "{covering:?}");
        }
        for other in ["shell", "webfetch(domain:example.com)"] {
            assert!(!hook_covers(&config_with_hook(Some(other))), "{other}");
        }
    }

    #[test]
    fn auto_and_afk_offer_it_without_a_rule() {
        for mode in [PermissionMode::Auto, PermissionMode::Afk] {
            assert!(permitted(mode, &Rules::default(), false), "{mode:?}");
        }
    }

    #[test]
    fn auto_and_afk_withhold_it_while_paused() {
        for mode in [PermissionMode::Auto, PermissionMode::Afk] {
            assert!(!permitted(mode, &Rules::default(), true), "{mode:?}");
            assert!(
                !permitted(mode, &rules(&[], &[], &["websearch"]), true),
                "{mode:?}"
            );
        }
    }

    #[test]
    fn a_paused_manual_mode_still_follows_its_allow_rule() {
        assert!(permitted(
            PermissionMode::Manual,
            &rules(&[], &[], &["websearch"]),
            true
        ));
        assert!(!permitted(PermissionMode::Manual, &Rules::default(), true));
    }

    #[test]
    fn modes_that_would_ask_or_deny_need_an_allow_rule() {
        for mode in [
            PermissionMode::Manual,
            PermissionMode::AcceptEdits,
            PermissionMode::DontAsk,
        ] {
            assert!(!permitted(mode, &Rules::default(), false), "{mode:?}");
            assert!(
                permitted(mode, &rules(&[], &[], &["websearch"]), false),
                "{mode:?}"
            );
        }
    }

    #[test]
    fn a_deny_or_ask_rule_withholds_it_in_every_mode() {
        for mode in [
            PermissionMode::Auto,
            PermissionMode::Afk,
            PermissionMode::Manual,
        ] {
            assert!(!permitted(
                mode,
                &rules(&["websearch"], &[], &["websearch"]),
                false
            ));
            assert!(!permitted(
                mode,
                &rules(&[], &["websearch"], &["websearch"]),
                false
            ));
        }
    }

    #[test]
    fn rules_for_other_tools_do_not_count() {
        assert!(!permitted(
            PermissionMode::Manual,
            &rules(&[], &[], &["webfetch(domain:example.com)"]),
            false
        ));
        assert!(permitted(
            PermissionMode::Auto,
            &rules(&["webfetch"], &["shell"], &[]),
            false
        ));
    }
}
