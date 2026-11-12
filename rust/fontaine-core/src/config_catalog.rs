//! The keys the TUI's settings panel lists: how each renders its effective value, whether the
//! panel may change it, and when a saved change takes effect. One table, in the style of the
//! scope table, with a test that fails when a settings key has no row.

use std::fmt::Display;

use toml_edit::Value;

use crate::config::{Config, HookEvent, Slot, ThemeName};
use crate::effort::Effort;

/// Live state that diverges from [`Config`] once a session switches theme or model.
pub struct Running<'a> {
    pub theme: &'a str,
    pub main_model: &'a str,
    pub guardian_model: Option<&'a str>,
    /// The session's level, which `--effort` and `/effort` move off `[model] effort`.
    pub effort: Option<Effort>,
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

pub(crate) fn rank(slot: Slot) -> u8 {
    match slot {
        Slot::Managed => 0,
        Slot::Cli => 1,
        Slot::Local => 2,
        Slot::ProjectLocal => 3,
        Slot::Project => 4,
        Slot::User => 5,
        Slot::Plugin => 6,
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

/// A shell command line as its program and argument count: arguments can carry inline secrets.
fn command_summary(command: Option<&str>) -> String {
    let Some(command) = command else {
        return "unset".to_string();
    };
    let mut words = command.split_whitespace();
    let program = words.next().unwrap_or("");
    format!("{program} (+{} args)", words.count())
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

fn editor_mode_choices() -> Vec<ChoiceValue> {
    vec![
        ("emacs", Some("emacs".into())),
        ("vim", Some("vim".into())),
        ("unset", None),
    ]
}

fn notify_choices() -> Vec<ChoiceValue> {
    vec![
        ("bell", Some("bell".into())),
        ("osc9", Some("osc9".into())),
        ("osc777", Some("osc777".into())),
        ("off", Some("off".into())),
        ("unset", None),
    ]
}

fn composition_field(
    config: &Config,
    field: impl Fn(&crate::config::GuardianComposition) -> String,
) -> String {
    config
        .guardian
        .composition
        .as_ref()
        .map_or_else(|| "unset".to_string(), field)
}

/// `name = value, ...` for a map's effective value, or `unset`.
fn composition_map(
    config: &Config,
    entries: impl Fn(&crate::config::GuardianComposition) -> Vec<String>,
) -> String {
    composition_field(config, |c| {
        let entries = entries(c);
        if entries.is_empty() {
            "none".to_string()
        } else {
            entries.join(", ")
        }
    })
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
        "tui.show_reasoning",
        |c, _| c.tui.show_reasoning.to_string(),
        Edit::Choice(bool_choices),
        NextLaunch
    ),
    row!(
        "tui.editor_mode",
        |c, _| lowercase_debug(c.tui.editor_mode),
        Edit::Choice(editor_mode_choices),
        NextLaunch
    ),
    row!(
        "tui.notify",
        |c, _| c.tui.notify.as_str().to_string(),
        Edit::Choice(notify_choices),
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
        "memory.auto",
        |c, _| c.memory.auto.to_string(),
        Edit::Choice(bool_choices),
        NextLaunch
    ),
    row!(
        "shell.max_timeout_secs",
        |c, _| c.shell.max_timeout_secs.to_string(),
        Edit::PositiveInt,
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
    row!("model.effort", Scalar, |_, r| unset_or(
        r.effort,
        "unset (the model's own default)"
    )),
    row!("model.max_output_tokens", Scalar, |c, _| unset_or(
        c.max_output_tokens,
        "unset (the model's output limit, else 32000)"
    )),
    row!("accounts", Map, |c, _| names(c.accounts.keys())),
    row!("models.roles", Map, |c, _| names(c.roles.keys())),
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
    row!("guardian.scanner_command", Scalar, |c, _| {
        command_summary(c.guardian.scanner_command.as_deref())
    }),
    row!("guardian.scanner_model_path", Scalar, |c, _| opt(c
        .guardian
        .scanner_model_path
        .as_deref())),
    row!("guardian.scanner_hf_token_env", Scalar, |c, _| c
        .guardian
        .scanner_hf_token_env
        .clone()),
    row!("guardian.composition.preset", Scalar, |c, _| {
        composition_field(c, |s| s.preset.clone().unwrap_or_else(|| "unset".into()))
    }),
    row!("guardian.composition.script", Scalar, |c, _| {
        composition_field(c, |s| {
            s.script
                .as_deref()
                .map_or_else(|| "unset".into(), |_| "set".into())
        })
    }),
    row!("guardian.composition.model", Scalar, |c, _| {
        composition_field(c, |s| s.model.clone().unwrap_or_else(|| "unset".into()))
    }),
    row!("guardian.composition.timeout_ms", Scalar, |c, _| {
        composition_field(c, |s| s.timeout_ms.to_string())
    }),
    row!("guardian.composition.shadow", Scalar, |c, _| {
        composition_field(c, |s| s.shadow.to_string())
    }),
    row!("guardian.composition.params", Map, |c, _| {
        composition_map(c, |s| {
            s.params.keys().map(|k| format!("{k} = <hidden>")).collect()
        })
    }),
    row!("guardian.composition.reads", Map, |c, _| {
        composition_map(c, |s| {
            s.reads
                .iter()
                .map(|(name, read)| {
                    format!(
                        "{name} = {} {}/{}",
                        s.read_model(read).unwrap_or("unset"),
                        read.prompt.as_str(),
                        read.question.as_str()
                    )
                })
                .collect()
        })
    }),
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
    row!("permissions.auto_tracked_edits", Scalar, |c, _| c
        .permissions
        .auto_tracked_edits
        .to_string()),
    row!("permissions.auto_scratch_writes", Scalar, |c, _| c
        .permissions
        .auto_scratch_writes
        .to_string()),
    row!("hooks.pre_tool_use", List, |c, _| entries(
        c.hooks.get(HookEvent::PreToolUse).len()
    )),
    row!("hooks.post_tool_use", List, |c, _| entries(
        c.hooks.get(HookEvent::PostToolUse).len()
    )),
    row!("hooks.post_tool_use_failure", List, |c, _| entries(
        c.hooks.get(HookEvent::PostToolUseFailure).len()
    )),
    row!("hooks.permission_denied", List, |c, _| entries(
        c.hooks.get(HookEvent::PermissionDenied).len()
    )),
    row!("hooks.task_completed", List, |c, _| entries(
        c.hooks.get(HookEvent::TaskCompleted).len()
    )),
    row!("hooks.task_created", List, |c, _| entries(
        c.hooks.get(HookEvent::TaskCreated).len()
    )),
    row!("hooks.teammate_idle", List, |c, _| entries(
        c.hooks.get(HookEvent::TeammateIdle).len()
    )),
    row!("hooks.session_start", List, |c, _| entries(
        c.hooks.get(HookEvent::SessionStart).len()
    )),
    row!("hooks.session_end", List, |c, _| entries(
        c.hooks.get(HookEvent::SessionEnd).len()
    )),
    row!("hooks.user_prompt_submit", List, |c, _| entries(
        c.hooks.get(HookEvent::UserPromptSubmit).len()
    )),
    row!("hooks.stop", List, |c, _| entries(
        c.hooks.get(HookEvent::Stop).len()
    )),
    row!("hooks.stop_failure", List, |c, _| entries(
        c.hooks.get(HookEvent::StopFailure).len()
    )),
    row!("hooks.subagent_start", List, |c, _| entries(
        c.hooks.get(HookEvent::SubagentStart).len()
    )),
    row!("hooks.subagent_stop", List, |c, _| entries(
        c.hooks.get(HookEvent::SubagentStop).len()
    )),
    row!("hooks.pre_compact", List, |c, _| entries(
        c.hooks.get(HookEvent::PreCompact).len()
    )),
    row!("hooks.post_compact", List, |c, _| entries(
        c.hooks.get(HookEvent::PostCompact).len()
    )),
    row!("hooks.notification", List, |c, _| entries(
        c.hooks.get(HookEvent::Notification).len()
    )),
    row!("mcp.servers", Map, |c, _| names(c.mcp_servers.keys())),
    row!("lsp.servers", Map, |c, _| names(
        c.lsp_table().servers.iter().map(|(name, _)| name)
    )),
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
    row!("auto_mode.setup_reminder", Scalar, |c, _| c
        .auto_mode
        .setup_reminder
        .to_string()),
    row!("agents.max_parallel", Scalar, |c, _| c
        .agents
        .max_parallel
        .to_string()),
    row!("agents.subagent_token_budget", Scalar, |c, _| opt(c
        .agents
        .subagent_token_budget)),
    row!("agents.subagent_cost_budget_usd", Scalar, |c, _| opt(c
        .agents
        .subagent_cost_budget_usd)),
    row!("agents.session_token_budget", Scalar, |c, _| opt(c
        .agents
        .session_token_budget)),
    row!("agents.session_cost_budget_usd", Scalar, |c, _| opt(c
        .agents
        .session_cost_budget_usd)),
    row!("agents.teams", Scalar, |c, _| c.agents.teams.to_string()),
    row!("agents.types", Map, |c, _| names(c.agents.types.keys())),
    row!("goal.max_turns", Scalar, |c, _| c
        .goal
        .max_turns
        .to_string()),
    row!("goal.max_cost_usd", Scalar, |c, _| opt(c.goal.max_cost_usd)),
    row!("headless.max_usage_wait_secs", Scalar, |c, _| c
        .headless
        .max_usage_wait_secs
        .to_string()),
    row!(
        "headless.on_usage_limit",
        Scalar,
        |c, _| match c.headless.on_usage_limit {
            crate::config::OnUsageLimit::Wait => "wait".to_string(),
            crate::config::OnUsageLimit::Exit => "exit".to_string(),
        }
    ),
    row!("afk.temperature_ceiling", Scalar, |c, _| c
        .afk
        .ceiling()
        .to_string()),
    row!("checkpoints.enabled", Scalar, |c, _| c
        .checkpoints
        .enabled
        .to_string()),
    row!("checkpoints.max_file_bytes", Scalar, |c, _| c
        .checkpoints
        .max_file_bytes
        .to_string()),
    row!("checkpoints.max_session_bytes", Scalar, |c, _| c
        .checkpoints
        .max_session_bytes
        .to_string()),
    row!("plugins.auto_update", Scalar, |c, _| c
        .plugins
        .auto_update
        .to_string()),
    row!("shell_preclassifier.allow_extra", List, |c, _| entries(
        c.shell_preclassifier_allow_extra.len()
    )),
    row!("statusline.command", Scalar, |c, _| {
        command_summary(c.statusline.command.as_deref())
    }),
    row!("statusline.refresh_interval_secs", Scalar, |c, _| opt(c
        .statusline
        .refresh_interval_secs)),
    row!("feedback.drafts", Scalar, |c, _| c
        .feedback
        .drafts
        .as_str()
        .to_string()),
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
    row!("websearch.backend", Scalar, |c, _| {
        c.websearch
            .backend
            .map_or_else(|| "unset".to_string(), lowercase_debug)
    }),
    row!("websearch.base_url", Scalar, |c, _| {
        c.websearch
            .base_url
            .as_deref()
            .map_or_else(|| "unset".to_string(), redacted_url)
    }),
    row!("websearch.command", Scalar, |c, _| {
        c.websearch.command.as_ref().map_or_else(
            || "unset".to_string(),
            |argv| {
                // Arguments can carry inline secrets (docker `-e NAME=value`).
                let program = argv.first().map_or("", String::as_str);
                format!("{program} (+{} args)", argv.len().saturating_sub(1))
            },
        )
    }),
    row!("websearch.startup_timeout_secs", Scalar, |c, _| opt(c
        .websearch
        .startup_timeout_secs)),
    row!("websearch.native", Scalar, |c, _| lowercase_debug(
        c.websearch.native
    )),
    row!("websearch.api_key_env", Scalar, |c, _| opt(c
        .websearch
        .api_key_env
        .as_deref())),
    row!("websearch.max_results", Scalar, |c, _| c
        .websearch
        .max_results
        .to_string()),
    row!("websearch.timeout_secs", Scalar, |c, _| c
        .websearch
        .timeout_secs
        .to_string()),
    row!("env", Map, |c, _| names(c.env.keys())),
    row!("keybindings", Map, |c, _| entries(c.keybindings.len())),
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
            effort: None,
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
        assert_eq!(effective("tui.show_reasoning", &c), "true");
        assert_eq!(effective("schedule.enabled", &c), "true");
        assert_eq!(effective("agents.max_parallel", &c), "4");
        assert_eq!(effective("agents.subagent_token_budget", &c), "unset");
        assert_eq!(effective("agents.subagent_cost_budget_usd", &c), "unset");
        assert_eq!(
            effective("model.effort", &c),
            "unset (the model's own default)"
        );
        assert_eq!(
            effective("model.max_output_tokens", &c),
            "unset (the model's output limit, else 32000)"
        );
        assert_eq!(effective("guardian.profile", &c), "full");
        assert_eq!(effective("guardian.scanner", &c), "off");
        assert_eq!(effective("guardian.composition.model", &c), "unset");
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
    fn a_set_subagent_token_budget_renders_as_a_number() {
        let mut c = defaults();
        c.agents.subagent_token_budget = Some(200_000);
        assert_eq!(effective("agents.subagent_token_budget", &c), "200000");

        c.agents.subagent_cost_budget_usd = Some(2.5);
        assert_eq!(effective("agents.subagent_cost_budget_usd", &c), "2.5");
    }

    #[test]
    fn the_searxng_command_shows_the_program_but_not_its_arguments() {
        let mut c = defaults();
        assert_eq!(effective("websearch.command", &c), "unset");
        c.websearch.command = Some(
            ["docker", "run", "-e", "SEARXNG_SECRET=hunter2"]
                .map(String::from)
                .to_vec(),
        );
        let shown = effective("websearch.command", &c);
        assert_eq!(shown, "docker (+3 args)");
        assert!(!shown.contains("hunter2"));
    }

    #[test]
    fn command_like_rows_never_show_inline_secrets() {
        let mut c = defaults();
        c.statusline.command = Some("status --token=tok-statusline".into());
        c.guardian.scanner_command = Some("scan --key tok-scanner".into());
        assert_eq!(effective("statusline.command", &c), "status (+1 args)");
        assert_eq!(effective("guardian.scanner_command", &c), "scan (+2 args)");
        c.guardian.composition = Some(crate::config::GuardianComposition {
            preset: None,
            script: Some("/s/tok-script.rn".into()),
            script_source: None,
            model: None,
            shadow: false,
            timeout_ms: 1000,
            params: [("token".to_string(), serde_json::json!("tok-param"))].into(),
            reads: Default::default(),
        });
        let shown = [
            effective("statusline.command", &c),
            effective("guardian.scanner_command", &c),
            effective("guardian.composition.script", &c),
            effective("guardian.composition.params", &c),
        ]
        .join("\n");
        assert!(!shown.contains("tok-"), "{shown}");
        assert!(shown.contains("token"), "{shown}");
    }

    #[test]
    fn running_state_overrides_the_config_for_live_rows() {
        let c = defaults();
        let running = Running {
            theme: "light",
            main_model: "a/b",
            guardian_model: Some("c/d"),
            effort: Some(Effort::High),
        };
        let show = |path: &str| (find(path).unwrap().effective)(&c, &running);
        assert_eq!(show("tui.theme"), "light");
        assert_eq!(show("model.model"), "a/b");
        assert_eq!(show("guardian.model"), "c/d");
        assert_eq!(show("model.effort"), "high");
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
                "tui.show_reasoning",
                "tui.editor_mode",
                "tui.notify",
                "context.autocompact_window",
                "schedule.enabled",
                "memory.auto",
                "shell.max_timeout_secs",
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
