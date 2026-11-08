//! Whether the provider's own server-side search is offered to the model this turn. It runs
//! inside the model's response, so neither the guardian nor the result scanner sees a query or a
//! result; the request-time gate here is the only check.

use serde_json::Value;

use crate::config::{Account, Config, NativeSearch, PermissionMode};
use crate::gate::{Rules, SessionState};
use crate::models::Models;
use crate::permission_rules::{SourcedRule, parse_rule};
use crate::providers::Provider;

const TOOL: &str = "websearch";

fn names_tool(rules: &[SourcedRule]) -> bool {
    rules.iter().any(|r| r.rule.tool == TOOL)
}

/// Whether the live mode and rules let a search run without a decision of Fontaine's. A `deny`
/// or `ask` rule for `websearch` always withholds it (so does a covering hook, see `offered`). Where a non-allowed call would ask or be
/// denied (`manual`, `acceptEdits`, `dontAsk`) it needs an `allow` rule.
pub(crate) fn permitted(mode: PermissionMode, rules: &Rules) -> bool {
    if names_tool(&rules.deny) || names_tool(&rules.ask) {
        return false;
    }
    match mode {
        PermissionMode::Auto | PermissionMode::Afk | PermissionMode::Plan => true,
        PermissionMode::Manual | PermissionMode::AcceptEdits | PermissionMode::DontAsk => {
            names_tool(&rules.allow)
        }
    }
}

/// Whether a `pre_tool_use` hook could cover `websearch`. The provider runs its search inside
/// the model's response, so a hook would never see it; a hook that cannot be read counts as
/// covering, as it runs for every call.
fn hook_covers(config: &Config) -> bool {
    config.hooks.pre_tool_use.iter().any(|hook| {
        hook.matcher
            .as_deref()
            .is_none_or(|text| parse_rule(text).map_or(true, |rule| rule.tool == TOOL))
    })
}

/// Why the main model's account offers no provider search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Absence {
    /// The account kind has no server-side search Fontaine can offer (its `name()`).
    Unsupported(&'static str),
    /// `[websearch] native = "off"`.
    Off,
    /// `native = "auto"` and the account's `base_url` is not the kind's own endpoint.
    Unverified,
}

impl std::fmt::Display for Absence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Absence::Unsupported(kind) => {
                write!(f, "`{kind}` accounts have no built-in search Fontaine can offer")
            }
            Absence::Off => f.write_str("`[websearch] native = \"off\"` turns off the provider's search"),
            Absence::Unverified => f.write_str(
                "the account's `base_url` is not the provider's own endpoint, so its search needs `[websearch] native = \"on\"`",
            ),
        }
    }
}

/// `Unsupported` precedes `Off` so a kind with no search is never told that `native = "off"`
/// turns off a search it never had.
fn entry_for(
    native: NativeSearch,
    provider: &dyn Provider,
    account: Option<&Account>,
) -> Result<Value, Absence> {
    let entry = provider
        .native_web_search()
        .ok_or(Absence::Unsupported(provider.name()))?;
    match native {
        NativeSearch::Off => return Err(Absence::Off),
        NativeSearch::On => return Ok(entry),
        _ => {}
    }
    let account = account.ok_or(Absence::Unverified)?;
    let base = account.base_url.as_deref().filter(|url| !url.is_empty());
    let official = provider.default_endpoint().ok_or(Absence::Unverified)?;
    if base.is_none_or(|url| url.trim_end_matches('/') == official) {
        Ok(entry)
    } else {
        Err(Absence::Unverified)
    }
}

/// The provider's search tool entry when the main model's account supports it and
/// `[websearch] native` allows it.
fn provider_entry(config: &Config, models: &Models) -> Result<Value, Absence> {
    entry_for(
        config.websearch.native,
        models.main.kind.provider(),
        config.accounts.get(&models.main.reference.account),
    )
}

/// Why the main model's account has no provider search, whatever the mode and rules say this
/// turn; `None` when it has one.
pub(crate) fn absence(config: &Config, models: &Models) -> Option<Absence> {
    provider_entry(config, models).err()
}

/// The tool entry the request carries now, or `None` when the client-side backend (if any)
/// serves searches.
pub(crate) fn offered(config: &Config, models: &Models, state: &SessionState) -> Option<Value> {
    let entry = provider_entry(config, models).ok()?;
    if hook_covers(config) {
        return None;
    }
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

    fn config_with_hook(matcher: Option<&str>) -> Config {
        let mut config = Config::defaults(std::path::PathBuf::new());
        config.hooks.pre_tool_use.push(crate::config::Hook {
            matcher: matcher.map(str::to_string),
            command: "true".to_string(),
            timeout_secs: None,
            origin: Default::default(),
            plugin: None,
        });
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

    fn kind(name: &str) -> &'static dyn Provider {
        crate::config::AccountKind::named(name).provider()
    }

    fn account(name: &str, base_url: Option<&str>) -> Account {
        Account::new(
            crate::config::AccountKind::named(name),
            base_url.map(str::to_string),
        )
    }

    #[test]
    fn claude_subscription_gets_no_entry_and_the_reason_names_it() {
        for native in [NativeSearch::Auto, NativeSearch::On] {
            assert_eq!(
                entry_for(native, kind("claude-subscription"), None),
                Err(Absence::Unsupported("claude-subscription"))
            );
        }
    }

    #[test]
    fn anthropic_entry_follows_mode_and_endpoint() {
        let p = kind("anthropic");
        let official = p.default_endpoint().unwrap();
        assert_eq!(
            entry_for(NativeSearch::Off, p, Some(&account("anthropic", None))),
            Err(Absence::Off)
        );
        for base in [
            None,
            Some(""),
            Some(official),
            Some(format!("{official}/").as_str()),
        ] {
            let a = account("anthropic", base);
            assert!(
                entry_for(NativeSearch::Auto, p, Some(&a)).is_ok(),
                "{base:?}"
            );
        }
        let gateway = account("anthropic", Some("https://gateway.example.com"));
        assert_eq!(
            entry_for(NativeSearch::Auto, p, Some(&gateway)),
            Err(Absence::Unverified)
        );
        assert!(entry_for(NativeSearch::On, p, Some(&gateway)).is_ok());
    }

    #[test]
    fn a_kind_without_search_is_unsupported_even_when_off() {
        assert_eq!(
            entry_for(NativeSearch::Off, kind("openai-compatible"), None),
            Err(Absence::Unsupported("openai-compatible"))
        );
    }

    #[test]
    fn a_missing_account_is_unverified_only_where_the_endpoint_decides() {
        let p = kind("anthropic");
        assert_eq!(entry_for(NativeSearch::Off, p, None), Err(Absence::Off));
        assert!(entry_for(NativeSearch::On, p, None).is_ok());
        assert_eq!(
            entry_for(NativeSearch::Auto, p, None),
            Err(Absence::Unverified)
        );
        assert_eq!(
            entry_for(NativeSearch::Auto, kind("openai-compatible"), None),
            Err(Absence::Unsupported("openai-compatible"))
        );
    }

    #[test]
    fn each_absence_says_its_key_phrase() {
        assert!(
            Absence::Unsupported("x")
                .to_string()
                .contains("`x` accounts")
        );
        assert!(Absence::Off.to_string().contains("native = \"off\""));
        assert!(Absence::Unverified.to_string().contains("native = \"on\""));
    }
}
