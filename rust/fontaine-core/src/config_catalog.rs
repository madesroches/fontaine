//! The keys the TUI's settings panel lists: how each renders its effective value, whether the
//! panel may change it, and when a saved change takes effect. One table, in the style of the
//! scope table, with a test that fails when a settings key has no row.

use std::fmt::Display;

use toml_edit::Value;

use crate::config::{Config, Slot, ThemeName};

/// Live state that diverges from [`Config`] once a session switches theme or model.
pub struct Running<'a> {
    pub theme: &'a str,
    pub main_model: &'a str,
    pub guardian_model: Option<&'a str>,
}

/// Which model setting a picker row stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelSlot {
    Main,
    Guardian,
}

/// One cycle entry of a [`Edit::Choice`]: the label shown, and the value it writes; `None`
/// removes the key.
pub type ChoiceValue = (&'static str, Option<Value>);

pub enum Edit {
    /// Cycles through fixed pairs; a `None` value resets (removes) the key.
    Choice(fn() -> Vec<ChoiceValue>),
    /// A positive integer typed into a field; empty clears the key.
    PositiveInt,
    /// Opens an existing picker instead (`/model`, `/guardian-model`).
    Picker(ModelSlot),
}

/// Whether a saved change applies to the running session or only from the next launch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Applies {
    Live,
    NextLaunch,
}

/// How a row's source label is read from [`Config::sources`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// The highest layer that set the key wins.
    Scalar,
    /// Every layer that set the key contributes.
    List,
    /// A named-entry section; every layer that defined an entry contributes.
    Map,
}

pub struct KeyRow {
    /// Dotted path, as in the scope table.
    pub path: &'static str,
    /// The running value as the panel shows it, defaults included.
    pub effective: fn(&Config, &Running) -> String,
    /// How the panel may change it; `None` is read-only.
    pub edit: Option<Edit>,
    pub applies: Applies,
    pub shape: Shape,
}

impl KeyRow {
    /// The layers that supplied this row, highest precedence first; empty when the built-in
    /// default applies.
    pub fn slots(&self, config: &Config) -> Vec<Slot> {
        let mut slots = match self.shape {
            Shape::Scalar => config
                .sources
                .get(self.path)
                .and_then(|s| s.first())
                .map(|s| vec![*s])
                .unwrap_or_default(),
            Shape::List => config.sources.get(self.path).cloned().unwrap_or_default(),
            Shape::Map => {
                let prefix = format!("{}.", self.path);
                config
                    .sources
                    .iter()
                    .filter(|(k, _)| k.starts_with(&prefix))
                    .flat_map(|(_, v)| v.iter().copied())
                    .collect()
            }
        };
        slots.sort_by_key(|s| rank(*s));
        slots.dedup();
        slots
    }

    /// [`KeyRow::slots`] as the panel prints it.
    pub fn source_label(&self, config: &Config) -> String {
        let slots = self.slots(config);
        if slots.is_empty() {
            return "default".to_string();
        }
        slots
            .iter()
            .map(|s| s.label())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// The highest layer that set `path`, if any.
pub fn winning_slot(config: &Config, path: &str) -> Option<Slot> {
    config
        .sources
        .get(path)
        .and_then(|slots| slots.first().copied())
}

fn rank(slot: Slot) -> u8 {
    match slot {
        Slot::Managed => 0,
        Slot::Cli => 1,
        Slot::ProjectLocal => 2,
        Slot::Project => 3,
        Slot::User => 4,
    }
}

pub fn find(path: &str) -> Option<&'static KeyRow> {
    KEYS.iter().find(|r| r.path == path)
}

fn unset_or<T: Display>(value: Option<T>, unset: &str) -> String {
    value.map_or_else(|| unset.to_string(), |v| v.to_string())
}

fn opt<T: Display>(value: Option<T>) -> String {
    unset_or(value, "unset")
}

fn lowercase_debug(value: impl std::fmt::Debug) -> String {
    format!("{value:?}").to_lowercase()
}

fn entries(n: usize) -> String {
    match n {
        1 => "1 entry".to_string(),
        n => format!("{n} entries"),
    }
}

fn names<'a>(names: impl Iterator<Item = &'a String>) -> String {
    let names: Vec<&str> = names.map(String::as_str).collect();
    if names.is_empty() {
        "none".to_string()
    } else {
        names.join(", ")
    }
}

// A proxy or endpoint URL may carry credentials in its userinfo or query.
fn redacted_url(url: &str) -> String {
    let url = url.split(['?', '#']).next().unwrap_or(url);
    let (scheme, rest) = url.split_once("://").map_or(("", url), |(s, r)| (s, r));
    let end = rest.find('/').unwrap_or(rest.len());
    let (authority, path) = rest.split_at(end);
    match authority.rsplit_once('@') {
        Some((_, host)) if scheme.is_empty() => format!("***@{host}{path}"),
        Some((_, host)) => format!("{scheme}://***@{host}{path}"),
        None => url.to_string(),
    }
}

fn bool_choices() -> Vec<ChoiceValue> {
    vec![("true", Some(true.into())), ("false", Some(false.into()))]
}

fn bool_or_unset_choices() -> Vec<ChoiceValue> {
    let mut choices = bool_choices();
    choices.push(("unset", None));
    choices
}

fn theme_choices() -> Vec<ChoiceValue> {
    ThemeName::ALL
        .into_iter()
        .map(|t| (t.as_str(), Some(Value::from(t.as_str()))))
        .collect()
}

fn screen_field(
    config: &Config,
    field: impl Fn(&crate::config::GuardianScreen) -> String,
) -> String {
    config
        .guardian
        .screen
        .as_ref()
        .map_or_else(|| "unset".to_string(), field)
}

macro_rules! row {
    ($path:literal, $shape:ident, $effective:expr) => {
        KeyRow {
            path: $path,
            effective: $effective,
            edit: None,
            applies: Applies::NextLaunch,
            shape: Shape::$shape,
        }
    };
    ($path:literal, $effective:expr, $edit:expr, $applies:ident) => {
        KeyRow {
            path: $path,
            effective: $effective,
            edit: Some($edit),
            applies: Applies::$applies,
            shape: Shape::Scalar,
        }
    };
}

pub const KEYS: &[KeyRow] = &[
    row!(
        "tui.theme",
        |_, r| r.theme.to_string(),
        Edit::Choice(theme_choices),
        Live
    ),
    row!(
        "tui.screen_reader",
        |c, _| opt(c.tui.screen_reader),
        Edit::Choice(bool_or_unset_choices),
        NextLaunch
    ),
    row!(
        "context.autocompact_window",
        |c, _| unset_or(
            c.context.autocompact_window,
            "unset (the model's own threshold)"
        ),
        Edit::PositiveInt,
        NextLaunch
    ),
    row!(
        "schedule.enabled",
        |c, _| c.schedule.enabled.to_string(),
        Edit::Choice(bool_choices),
        NextLaunch
    ),
    row!(
        "model.model",
        |_, r| r.main_model.to_string(),
        Edit::Picker(ModelSlot::Main),
        Live
    ),
    row!(
        "guardian.model",
        |_, r| unset_or(r.guardian_model, "unset (best validated, else main model)"),
        Edit::Picker(ModelSlot::Guardian),
        Live
    ),
    row!("accounts", Map, |c, _| names(c.accounts.keys())),
    row!("network.https_proxy", Scalar, |c, _| {
        c.network
            .https_proxy
            .as_deref()
            .map_or_else(|| "unset".to_string(), redacted_url)
    }),
    row!("network.http_proxy", Scalar, |c, _| {
        c.network
            .http_proxy
            .as_deref()
            .map_or_else(|| "unset".to_string(), redacted_url)
    }),
    row!("network.no_proxy", Scalar, |c, _| opt(c
        .network
        .no_proxy
        .as_deref())),
    row!("network.ca_bundle", Scalar, |c, _| opt(c
        .network
        .ca_bundle
        .as_ref()
        .map(|p| p.display()))),
    row!("network.client_cert", Scalar, |c, _| opt(c
        .network
        .client_cert
        .as_ref()
        .map(|p| p.display()))),
    row!("network.client_key", Scalar, |c, _| opt(c
        .network
        .client_key
        .as_ref()
        .map(|p| p.display()))),
    row!("guardian.profile", Scalar, |c, _| lowercase_debug(
        c.guardian.profile
    )),
    row!("guardian.timeout_secs", Scalar, |c, _| c
        .guardian
        .timeout_secs
        .to_string()),
    row!("guardian.confidence_threshold", Scalar, |c, _| c
        .guardian
        .confidence_threshold
        .to_string()),
    row!("guardian.cache", Scalar, |c, _| c
        .guardian
        .cache
        .to_string()),
    row!("guardian.scanner", Scalar, |c, _| lowercase_debug(
        c.guardian.scanner
    )),
    row!("guardian.scanner_command", Scalar, |c, _| opt(c
        .guardian
        .scanner_command
        .as_deref())),
    row!("guardian.scanner_model_path", Scalar, |c, _| opt(c
        .guardian
        .scanner_model_path
        .as_deref())),
    row!("guardian.scanner_hf_token_env", Scalar, |c, _| c
        .guardian
        .scanner_hf_token_env
        .clone()),
    row!("guardian.screen.model", Scalar, |c, _| screen_field(
        c,
        |s| { s.model.clone() }
    )),
    row!("guardian.screen.question", Scalar, |c, _| screen_field(
        c,
        |s| s.question.as_str().to_string()
    )),
    row!("guardian.screen.stop_threshold", Scalar, |c, _| {
        screen_field(c, |s| opt(s.stop_threshold))
    }),
    row!("guardian.screen.allow_threshold", Scalar, |c, _| {
        screen_field(c, |s| opt(s.allow_threshold))
    }),
    row!("guardian.screen.steering_veto", Scalar, |c, _| {
        screen_field(c, |s| s.steering_veto.to_string())
    }),
    row!("guardian.screen.timeout_ms", Scalar, |c, _| screen_field(
        c,
        |s| s.timeout_ms.to_string()
    )),
    row!("guardian.screen.shadow", Scalar, |c, _| screen_field(
        c,
        |s| { s.shadow.to_string() }
    )),
    row!("permissions.default_mode", Scalar, |c, _| unset_or(
        c.permissions.default_mode.map(|m| m.as_str()),
        "unset (interactive: auto, headless: manual)"
    )),
    row!("permissions.deny", List, |c, _| entries(
        c.permissions.deny.len()
    )),
    row!("permissions.ask", List, |c, _| entries(
        c.permissions.ask.len()
    )),
    row!("permissions.allow", List, |c, _| entries(
        c.permissions.allow.len()
    )),
    row!("permissions.additional_directories", List, |c, _| entries(
        c.permissions.additional_directories.len()
    )),
    row!("hooks.pre_tool_use", List, |c, _| entries(
        c.hooks.pre_tool_use.len()
    )),
    row!("hooks.post_tool_use", List, |c, _| entries(
        c.hooks.post_tool_use.len()
    )),
    row!("hooks.permission_denied", List, |c, _| entries(
        c.hooks.permission_denied.len()
    )),
    row!("mcp.servers", Map, |c, _| names(c.mcp_servers.keys())),
    row!("auto_mode.environment", List, |c, _| entries(
        c.auto_mode.environment.len()
    )),
    row!("auto_mode.allow", List, |c, _| entries(
        c.auto_mode.allow.len()
    )),
    row!("auto_mode.soft_deny", List, |c, _| entries(
        c.auto_mode.soft_deny.len()
    )),
    row!("auto_mode.hard_deny", List, |c, _| entries(
        c.auto_mode.hard_deny.len()
    )),
    row!("agents.max_parallel", Scalar, |c, _| c
        .agents
        .max_parallel
        .to_string()),
    row!("agents.subagent_token_budget", Scalar, |c, _| c
        .agents
        .subagent_token_budget
        .to_string()),
    row!("agents.subagent_cost_budget_usd", Scalar, |c, _| c
        .agents
        .subagent_cost_budget_usd
        .to_string()),
    row!("agents.session_token_budget", Scalar, |c, _| opt(c
        .agents
        .session_token_budget)),
    row!("agents.session_cost_budget_usd", Scalar, |c, _| opt(c
        .agents
        .session_cost_budget_usd)),
    row!("agents.types", Map, |c, _| names(c.agents.types.keys())),
    row!("goal.max_turns", Scalar, |c, _| c
        .goal
        .max_turns
        .to_string()),
    row!("goal.max_cost_usd", Scalar, |c, _| opt(c.goal.max_cost_usd)),
    row!("shell_preclassifier.allow_extra", List, |c, _| entries(
        c.shell_preclassifier_allow_extra.len()
    )),
    row!("telemetry.enabled", Scalar, |c, _| c
        .telemetry
        .enabled
        .to_string()),
    row!("telemetry.backend", Scalar, |c, _| lowercase_debug(
        c.telemetry.backend
    )),
    row!("telemetry.endpoint", Scalar, |c, _| {
        c.telemetry
            .endpoint
            .as_deref()
            .map_or_else(|| "unset".to_string(), redacted_url)
    }),
    row!("telemetry.api_key_env", Scalar, |c, _| opt(c
        .telemetry
        .api_key_env
        .as_deref())),
    row!("telemetry.traces", Scalar, |c, _| c
        .telemetry
        .traces
        .to_string()),
    row!("telemetry.log_user_prompts", Scalar, |c, _| c
        .telemetry
        .log_user_prompts
        .to_string()),
    row!("telemetry.log_assistant_responses", Scalar, |c, _| c
        .telemetry
        .log_assistant_responses
        .to_string()),
    row!("telemetry.log_tool_details", Scalar, |c, _| c
        .telemetry
        .log_tool_details
        .to_string()),
    row!("telemetry.log_tool_content", Scalar, |c, _| c
        .telemetry
        .log_tool_content
        .to_string()),
    row!("telemetry.content_max_bytes", Scalar, |c, _| c
        .telemetry
        .content_max_bytes
        .to_string()),
    row!("telemetry.resource_attributes", Map, |c, _| names(
        c.telemetry.resource_attributes.keys()
    )),
    row!("env", Map, |c, _| names(c.env.keys())),
];

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::config_scope::schema::{is_prefix, schema_nodes};

    #[test]
    fn urls_lose_credentials_with_or_without_a_scheme() {
        assert_eq!(redacted_url("user:pw@proxy:8080"), "***@proxy:8080");
        assert_eq!(
            redacted_url("http://user:pw@proxy:8080/x?t=1"),
            "http://***@proxy:8080/x"
        );
        assert_eq!(redacted_url("proxy:8080"), "proxy:8080");
    }

    fn defaults() -> Config {
        Config::defaults(PathBuf::from("/work"))
    }

    fn running() -> Running<'static> {
        Running {
            theme: "dark-ansi",
            main_model: "work/opus",
            guardian_model: None,
        }
    }

    fn effective(path: &str, config: &Config) -> String {
        (find(path)
            .unwrap_or_else(|| panic!("no row {path}"))
            .effective)(config, &running())
    }

    #[test]
    fn every_settings_leaf_has_a_row() {
        let missing: Vec<_> = schema_nodes()
            .into_iter()
            .filter(|(_, leaf)| *leaf)
            .map(|(p, _)| p)
            .filter(|p| !KEYS.iter().any(|r| is_prefix(r.path, p)))
            .collect();
        assert!(
            missing.is_empty(),
            "settings keys with no catalog row: {missing:?}"
        );
    }

    #[test]
    fn every_row_matches_a_settings_node() {
        let nodes = schema_nodes();
        let dead: Vec<_> = KEYS
            .iter()
            .map(|r| r.path)
            .filter(|p| !nodes.iter().any(|(n, _)| n == p))
            .collect();
        assert!(dead.is_empty(), "rows matching no settings key: {dead:?}");
    }

    #[test]
    fn no_row_is_listed_twice() {
        for (i, a) in KEYS.iter().enumerate() {
            assert!(
                KEYS[i + 1..].iter().all(|b| b.path != a.path),
                "{} appears twice",
                a.path
            );
        }
    }

    #[test]
    fn defaults_render_as_documented() {
        let c = defaults();
        assert_eq!(effective("tui.theme", &c), "dark-ansi");
        assert_eq!(effective("tui.screen_reader", &c), "unset");
        assert_eq!(effective("schedule.enabled", &c), "true");
        assert_eq!(effective("agents.max_parallel", &c), "4");
        assert_eq!(effective("guardian.profile", &c), "full");
        assert_eq!(effective("guardian.scanner", &c), "off");
        assert_eq!(effective("guardian.screen.model", &c), "unset");
        assert_eq!(effective("permissions.deny", &c), "0 entries");
        assert_eq!(effective("accounts", &c), "none");
        assert_eq!(
            effective("permissions.default_mode", &c),
            "unset (interactive: auto, headless: manual)"
        );
        assert_eq!(
            effective("guardian.model", &c),
            "unset (best validated, else main model)"
        );
    }

    #[test]
    fn running_state_overrides_the_config_for_live_rows() {
        let c = defaults();
        let running = Running {
            theme: "light",
            main_model: "a/b",
            guardian_model: Some("c/d"),
        };
        let show = |path: &str| (find(path).unwrap().effective)(&c, &running);
        assert_eq!(show("tui.theme"), "light");
        assert_eq!(show("model.model"), "a/b");
        assert_eq!(show("guardian.model"), "c/d");
    }

    #[test]
    fn the_env_row_names_variables_and_never_their_values() {
        let w = tempfile::tempdir().unwrap();
        let dir = w.path().join("config");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.toml"),
            "[env]\nAPI_TOKEN = \"hunter2-secret\"\n",
        )
        .unwrap();
        let config = crate::config::load(&crate::config::LoadParams {
            launch_dir: w.path(),
            config_dir: &dir,
            data_dir: w.path(),
            settings_file: None,
            cli_rules: Default::default(),
            env: &|_| None,
        })
        .unwrap();
        let shown = effective("env", &config);
        assert_eq!(shown, "API_TOKEN");
        assert!(!shown.contains("hunter2"));
    }

    #[test]
    fn proxy_credentials_are_not_shown() {
        let mut c = defaults();
        c.network.https_proxy = Some("http://user:pw@proxy:8080/x?token=1".to_string());
        assert_eq!(
            effective("network.https_proxy", &c),
            "http://***@proxy:8080/x"
        );
    }

    #[test]
    fn editable_rows_are_exactly_the_preference_set() {
        let editable: Vec<_> = KEYS
            .iter()
            .filter(|r| r.edit.is_some())
            .map(|r| r.path)
            .collect();
        assert_eq!(
            editable,
            [
                "tui.theme",
                "tui.screen_reader",
                "context.autocompact_window",
                "schedule.enabled",
                "model.model",
                "guardian.model",
            ]
        );
    }

    #[test]
    fn choices_carry_typed_values_and_unset_removes() {
        let Some(Edit::Choice(choices)) = &find("tui.screen_reader").unwrap().edit else {
            panic!("screen_reader is a choice");
        };
        let choices = choices();
        assert_eq!(choices[0].1.as_ref().and_then(Value::as_bool), Some(true));
        let last = choices.last().unwrap();
        assert!(last.0 == "unset" && last.1.is_none());
        let Some(Edit::Choice(themes)) = &find("tui.theme").unwrap().edit else {
            panic!("theme is a choice");
        };
        assert_eq!(themes().len(), ThemeName::ALL.len());
    }

    #[test]
    fn source_labels_follow_the_row_shape() {
        let mut c = defaults();
        c.sources
            .insert("permissions.deny".into(), vec![Slot::Project, Slot::User]);
        c.sources
            .insert("tui.theme".into(), vec![Slot::ProjectLocal, Slot::User]);
        c.sources
            .insert("accounts.work.kind".into(), vec![Slot::User]);
        c.sources
            .insert("accounts.local.kind".into(), vec![Slot::Cli]);
        assert_eq!(
            find("permissions.deny").unwrap().source_label(&c),
            "project, user"
        );
        assert_eq!(find("tui.theme").unwrap().source_label(&c), "project-local");
        assert_eq!(find("accounts").unwrap().source_label(&c), "cli, user");
        assert_eq!(
            find("schedule.enabled").unwrap().source_label(&c),
            "default"
        );
        assert_eq!(winning_slot(&c, "tui.theme"), Some(Slot::ProjectLocal));
    }
}
