//! The per-key scope policy: one row per settings key, and a walker that applies it to the raw
//! TOML of a file. A key with no row is refused from project scopes, so a new settings field
//! fails closed until someone decides where it may be set.

use crate::config::Column;

type ScopeFailure = (String, String);

/// What one non-user column may do with a key. The user file may set everything.
#[derive(Clone, Copy)]
pub(crate) enum Cell {
    No,
    Yes,
    /// Allowed only for values the predicate accepts.
    If(fn(&toml::Value) -> bool),
}

/// A row matching a node stops the descent, so it covers the whole subtree (`env` covers every
/// environment variable). Map keys add a segment, so a row cannot name a node below a map; array
/// elements add none.
pub(crate) struct Rule {
    pub path: &'static str,
    pub project: Cell,
    pub local: Cell,
    pub cli: Cell,
    pub reason: &'static str,
}

impl Rule {
    const fn any(path: &'static str) -> Rule {
        Rule {
            path,
            project: Cell::Yes,
            local: Cell::Yes,
            cli: Cell::Yes,
            reason: "",
        }
    }

    /// User file, project-local and CLI; shared project settings may not.
    const fn user_trust(path: &'static str, reason: &'static str) -> Rule {
        Rule {
            path,
            project: Cell::No,
            local: Cell::Yes,
            cli: Cell::Yes,
            reason,
        }
    }

    /// User file and CLI only.
    const fn user_or_cli(path: &'static str, reason: &'static str) -> Rule {
        Rule {
            path,
            project: Cell::No,
            local: Cell::No,
            cli: Cell::Yes,
            reason,
        }
    }

    const fn user_only(path: &'static str, reason: &'static str) -> Rule {
        Rule {
            path,
            project: Cell::No,
            local: Cell::No,
            cli: Cell::No,
            reason,
        }
    }

    const fn shadow_true(path: &'static str, reason: &'static str) -> Rule {
        Rule {
            path,
            project: Cell::If(is_shadow_true),
            local: Cell::If(is_shadow_true),
            cli: Cell::If(is_shadow_true),
            reason,
        }
    }

    /// Value-gated in the project scopes, unrestricted from the CLI.
    const fn if_project_local(
        path: &'static str,
        pred: fn(&toml::Value) -> bool,
        reason: &'static str,
    ) -> Rule {
        Rule {
            path,
            project: Cell::If(pred),
            local: Cell::If(pred),
            cli: Cell::Yes,
            reason,
        }
    }

    fn cell(&self, column: Column) -> Cell {
        match column {
            Column::Project => self.project,
            Column::ProjectLocal => self.local,
            Column::Cli => self.cli,
            Column::User => Cell::Yes,
        }
    }
}

const TIGHTEN: &str = "shared project settings may only tighten";
const USER_FILE: &str =
    "endpoint and credential configuration comes only from the user file or --settings";
const NETWORK: &str =
    "proxy and trust-root configuration comes only from the user file or --settings";
const WEBSEARCH: &str = "the search backend, its key and the command that starts it come only from the user file or --settings";
const TUI: &str = "terminal preferences come only from the user file or --settings";
const ENV: &str = "environment overrides come only from user-trust scopes";
const SCANNER: &str = "scanner settings come only from the user file";
const SCREEN: &str = "the screen stage is configured only in the user file; other scopes may only set `shadow = true`";
const DEFAULT_MODE: &str = "only `manual` and `dontAsk` are accepted here";
const SCHEDULE: &str = "scheduled prompts run without the user typing them, so only the user file or --settings turns them on or off";
const MEMORY: &str = "auto memory persists text into later sessions, so only the user file or --settings turns it on";
const GOAL: &str =
    "goal turns run unattended, so only the user file or --settings sets how long they may run";
const TELEMETRY: &str = "only `enabled = false` may be set here";

fn is_tightening_mode(v: &toml::Value) -> bool {
    matches!(v.as_str(), Some("manual" | "dontAsk"))
}

fn is_false(v: &toml::Value) -> bool {
    v.as_bool() == Some(false)
}

fn is_shadow_true(v: &toml::Value) -> bool {
    v.as_bool() == Some(true)
}

pub(crate) const RULES: &[Rule] = &[
    Rule::user_or_cli("tui", TUI),
    Rule::user_or_cli("schedule.enabled", SCHEDULE),
    Rule::user_or_cli("memory", MEMORY),
    Rule::user_or_cli("goal", GOAL),
    Rule::user_or_cli("accounts", USER_FILE),
    Rule::user_or_cli("network", NETWORK),
    Rule::user_or_cli("websearch", WEBSEARCH),
    Rule::user_trust("model", TIGHTEN),
    Rule::user_trust("guardian.model", TIGHTEN),
    Rule::user_trust("guardian.profile", TIGHTEN),
    Rule::user_trust("guardian.timeout_secs", TIGHTEN),
    Rule::user_trust("guardian.confidence_threshold", TIGHTEN),
    Rule::user_trust("guardian.cache", TIGHTEN),
    Rule::user_only("guardian.scanner", SCANNER),
    Rule::user_only("guardian.scanner_command", SCANNER),
    Rule::user_only("guardian.scanner_model_path", SCANNER),
    Rule::user_only("guardian.scanner_hf_token_env", SCANNER),
    Rule::user_only("guardian.screen.model", SCREEN),
    Rule::user_only("guardian.screen.question", SCREEN),
    Rule::user_only("guardian.screen.stop_threshold", SCREEN),
    Rule::user_only("guardian.screen.allow_threshold", SCREEN),
    Rule::user_only("guardian.screen.steering_veto", SCREEN),
    Rule::user_only("guardian.screen.timeout_ms", SCREEN),
    Rule::shadow_true("guardian.screen.shadow", SCREEN),
    Rule::if_project_local("permissions.default_mode", is_tightening_mode, DEFAULT_MODE),
    Rule::any("permissions.deny"),
    Rule::any("permissions.ask"),
    Rule::user_trust("permissions.allow", TIGHTEN),
    Rule::user_trust("permissions.additional_directories", TIGHTEN),
    Rule::any("hooks.pre_tool_use"),
    Rule::any("hooks.post_tool_use"),
    Rule::any("hooks.permission_denied"),
    Rule::any("mcp.servers"),
    Rule::user_trust("auto_mode.environment", TIGHTEN),
    Rule::user_trust("auto_mode.allow", TIGHTEN),
    Rule::any("auto_mode.soft_deny"),
    Rule::any("auto_mode.hard_deny"),
    Rule::user_trust("agents", TIGHTEN),
    Rule::user_trust("context", TIGHTEN),
    Rule::user_trust("shell_preclassifier", TIGHTEN),
    Rule::if_project_local("telemetry.enabled", is_false, TELEMETRY),
    Rule::user_or_cli("telemetry.backend", TELEMETRY),
    Rule::user_or_cli("telemetry.endpoint", TELEMETRY),
    Rule::user_or_cli("telemetry.api_key_env", TELEMETRY),
    Rule::user_or_cli("telemetry.traces", TELEMETRY),
    Rule::user_or_cli("telemetry.log_user_prompts", TELEMETRY),
    Rule::user_or_cli("telemetry.log_assistant_responses", TELEMETRY),
    Rule::user_or_cli("telemetry.log_tool_details", TELEMETRY),
    Rule::user_or_cli("telemetry.log_tool_content", TELEMETRY),
    Rule::user_or_cli("telemetry.content_max_bytes", TELEMETRY),
    Rule::user_or_cli("telemetry.resource_attributes", TELEMETRY),
    Rule::user_trust("env", ENV),
];

pub(crate) fn check_scope(table: &toml::Table, column: Column) -> Result<(), ScopeFailure> {
    check_rules(RULES, table, column)
}

fn check_rules(rules: &[Rule], table: &toml::Table, column: Column) -> Result<(), ScopeFailure> {
    if column == Column::User {
        return Ok(());
    }
    walk(rules, table, "", column)
}

fn walk(
    rules: &[Rule],
    table: &toml::Table,
    prefix: &str,
    column: Column,
) -> Result<(), ScopeFailure> {
    for (key, value) in table {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        visit(rules, value, &path, column)?;
    }
    Ok(())
}

fn visit(
    rules: &[Rule],
    value: &toml::Value,
    path: &str,
    column: Column,
) -> Result<(), ScopeFailure> {
    if let Some(rule) = rules.iter().find(|r| r.path == path) {
        let allowed = match rule.cell(column) {
            Cell::Yes => true,
            Cell::No => false,
            Cell::If(pred) => pred(value),
        };
        return if allowed {
            Ok(())
        } else {
            Err((rule.path.to_string(), rule.reason.to_string()))
        };
    }
    match value {
        toml::Value::Table(t) => walk(rules, t, path, column),
        toml::Value::Array(items) if !items.is_empty() => items
            .iter()
            .try_for_each(|item| visit(rules, item, path, column)),
        _ if column == Column::Cli => Ok(()),
        _ => Err((
            path.to_string(),
            "no scope policy is declared for this key, so only the user file or --settings may set it"
                .to_string(),
        )),
    }
}

/// The `Settings` schema as dotted paths, shared by the tests that pin a table to the settings
/// keys.
#[cfg(test)]
pub(crate) mod schema {
    use serde_json::Value;

    use crate::config::Settings;

    pub(crate) fn is_prefix(prefix: &str, path: &str) -> bool {
        path == prefix
            || path
                .strip_prefix(prefix)
                .is_some_and(|r| r.starts_with('.'))
    }

    /// Every node of the `Settings` schema as a dotted path, with whether it is a leaf. Map keys
    /// are `*` and array elements add no segment.
    pub(crate) fn schema_nodes() -> Vec<(String, bool)> {
        let schema = schemars::schema_for!(Settings);
        let root = schema.as_value().clone();
        let mut out = Vec::new();
        collect(&root, &root, "", &mut out);
        out
    }

    fn resolve<'a>(root: &'a Value, node: &'a Value) -> &'a Value {
        if let Some(r) = node.get("$ref").and_then(Value::as_str) {
            let name = r.strip_prefix("#/$defs/").expect("local $defs reference");
            return resolve(root, &root["$defs"][name]);
        }
        if let Some(any) = node.get("anyOf").and_then(Value::as_array) {
            let live: Vec<_> = any
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) != Some("null"))
                .collect();
            if let [only] = live.as_slice() {
                return resolve(root, only);
            }
        }
        node
    }

    fn collect(root: &Value, node: &Value, path: &str, out: &mut Vec<(String, bool)>) {
        let node = resolve(root, node);
        let mut children: Vec<(String, &Value)> = Vec::new();
        if let Some(props) = node.get("properties").and_then(Value::as_object) {
            for (k, v) in props {
                let child = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                children.push((child, v));
            }
        }
        if let Some(extra) = node.get("additionalProperties").filter(|v| v.is_object()) {
            children.push((format!("{path}.*"), extra));
        }
        if let Some(items) = node.get("items").filter(|v| v.is_object()) {
            children.push((path.to_string(), items));
        }
        let is_leaf = children.is_empty();
        if !path.is_empty() && !out.iter().any(|(p, _)| p == path) {
            out.push((path.to_string(), is_leaf));
        }
        for (child, v) in children {
            collect(root, v, &child, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::schema::{is_prefix, schema_nodes};
    use super::*;
    use crate::config::{ConfigError, parse_settings};

    const PROJECT_COLUMNS: [Column; 3] = [Column::Project, Column::ProjectLocal, Column::Cli];

    #[test]
    fn every_settings_leaf_has_a_row() {
        let missing: Vec<_> = schema_nodes()
            .into_iter()
            .filter(|(_, leaf)| *leaf)
            .map(|(p, _)| p)
            .filter(|p| !RULES.iter().any(|r| is_prefix(r.path, p)))
            .collect();
        assert!(
            missing.is_empty(),
            "settings keys with no scope policy row: {missing:?}"
        );
    }

    #[test]
    fn every_row_matches_a_settings_node() {
        let nodes = schema_nodes();
        let dead: Vec<_> = RULES
            .iter()
            .map(|r| r.path)
            .filter(|p| !nodes.iter().any(|(n, _)| n == p))
            .collect();
        assert!(dead.is_empty(), "rows matching no settings key: {dead:?}");
    }

    #[test]
    fn no_two_rows_overlap() {
        for a in RULES {
            for b in RULES {
                assert!(
                    std::ptr::eq(a, b) || !is_prefix(a.path, b.path),
                    "`{}` covers `{}`",
                    a.path,
                    b.path
                );
            }
        }
    }

    /// One valid snippet per row, and for value-gated rows a second one the gate refuses.
    const SNIPPETS: &[(&str, &str, Option<&str>)] = &[
        ("tui", "[tui]\ntheme = \"dark\"\n", None),
        ("accounts", "[accounts.x]\nkind = \"ollama\"\n", None),
        ("network", "[network]\nhttps_proxy = \"http://p:1\"\n", None),
        ("websearch", "[websearch]\nnative = \"off\"\n", None),
        ("model", "[model]\nmodel = \"a/b\"\n", None),
        ("guardian.model", "[guardian]\nmodel = \"a/b\"\n", None),
        (
            "guardian.profile",
            "[guardian]\nprofile = \"minimal\"\n",
            None,
        ),
        (
            "guardian.timeout_secs",
            "[guardian]\ntimeout_secs = 5\n",
            None,
        ),
        (
            "guardian.confidence_threshold",
            "[guardian]\nconfidence_threshold = 0.1\n",
            None,
        ),
        ("guardian.cache", "[guardian]\ncache = false\n", None),
        ("guardian.scanner", "[guardian]\nscanner = \"llm\"\n", None),
        (
            "guardian.scanner_command",
            "[guardian]\nscanner_command = \"/bin/scan\"\n",
            None,
        ),
        (
            "guardian.scanner_model_path",
            "[guardian]\nscanner_model_path = \"/m\"\n",
            None,
        ),
        (
            "guardian.scanner_hf_token_env",
            "[guardian]\nscanner_hf_token_env = \"HF\"\n",
            None,
        ),
        (
            "guardian.screen.model",
            "[guardian.screen]\nmodel = \"a/b\"\n",
            None,
        ),
        (
            "guardian.screen.question",
            "[guardian.screen]\nquestion = \"outside_request\"\n",
            None,
        ),
        (
            "guardian.screen.stop_threshold",
            "[guardian.screen]\nstop_threshold = 0.9\n",
            None,
        ),
        (
            "guardian.screen.allow_threshold",
            "[guardian.screen]\nallow_threshold = 0.1\n",
            None,
        ),
        (
            "guardian.screen.steering_veto",
            "[guardian.screen]\nsteering_veto = 0.5\n",
            None,
        ),
        (
            "guardian.screen.timeout_ms",
            "[guardian.screen]\ntimeout_ms = 500\n",
            None,
        ),
        (
            "guardian.screen.shadow",
            "[guardian.screen]\nshadow = true\n",
            Some("[guardian.screen]\nshadow = false\n"),
        ),
        (
            "permissions.default_mode",
            "[permissions]\ndefault_mode = \"dontAsk\"\n",
            Some("[permissions]\ndefault_mode = \"plan\"\n"),
        ),
        (
            "permissions.deny",
            "[permissions]\ndeny = [\"shell(rm)\"]\n",
            None,
        ),
        (
            "permissions.ask",
            "[permissions]\nask = [\"webfetch\"]\n",
            None,
        ),
        (
            "permissions.allow",
            "[permissions]\nallow = [\"shell(ls)\"]\n",
            None,
        ),
        (
            "permissions.additional_directories",
            "[permissions]\nadditional_directories = [\"/tmp\"]\n",
            None,
        ),
        (
            "hooks.pre_tool_use",
            "[[hooks.pre_tool_use]]\ncommand = \"true\"\n",
            None,
        ),
        (
            "hooks.post_tool_use",
            "[[hooks.post_tool_use]]\ncommand = \"true\"\n",
            None,
        ),
        (
            "hooks.permission_denied",
            "[[hooks.permission_denied]]\ncommand = \"true\"\n",
            None,
        ),
        ("mcp.servers", "[mcp.servers.s]\ncommand = \"srv\"\n", None),
        (
            "auto_mode.environment",
            "[auto_mode]\nenvironment = [\"x\"]\n",
            None,
        ),
        ("auto_mode.allow", "[auto_mode]\nallow = [\"x\"]\n", None),
        (
            "auto_mode.soft_deny",
            "[auto_mode]\nsoft_deny = [\"x\"]\n",
            None,
        ),
        (
            "auto_mode.hard_deny",
            "[auto_mode]\nhard_deny = [\"x\"]\n",
            None,
        ),
        ("agents", "[agents]\nmax_parallel = 2\n", None),
        ("schedule.enabled", "[schedule]\nenabled = false\n", None),
        ("memory", "[memory]\nauto = true\n", None),
        ("goal", "[goal]\nmax_turns = 10\n", None),
        ("context", "[context]\nautocompact_window = 150000\n", None),
        (
            "shell_preclassifier",
            "[shell_preclassifier]\nallow_extra = [\"tree\"]\n",
            None,
        ),
        (
            "telemetry.enabled",
            "[telemetry]\nenabled = false\n",
            Some("[telemetry]\nenabled = true\n"),
        ),
        (
            "telemetry.backend",
            "[telemetry]\nbackend = \"otlp\"\n",
            None,
        ),
        (
            "telemetry.endpoint",
            "[telemetry]\nendpoint = \"https://t.example.com\"\n",
            None,
        ),
        (
            "telemetry.api_key_env",
            "[telemetry]\napi_key_env = \"K\"\n",
            None,
        ),
        ("telemetry.traces", "[telemetry]\ntraces = true\n", None),
        (
            "telemetry.log_user_prompts",
            "[telemetry]\nlog_user_prompts = true\n",
            None,
        ),
        (
            "telemetry.log_assistant_responses",
            "[telemetry]\nlog_assistant_responses = true\n",
            None,
        ),
        (
            "telemetry.log_tool_details",
            "[telemetry]\nlog_tool_details = true\n",
            None,
        ),
        (
            "telemetry.log_tool_content",
            "[telemetry]\nlog_tool_content = true\n",
            None,
        ),
        (
            "telemetry.content_max_bytes",
            "[telemetry]\ncontent_max_bytes = 100\n",
            None,
        ),
        (
            "telemetry.resource_attributes",
            "[telemetry]\nresource_attributes = { a = \"b\" }\n",
            None,
        ),
        ("env", "[env]\nHTTPS_PROXY = \"http://p\"\n", None),
    ];

    #[test]
    fn every_row_has_a_snippet_and_no_snippet_lacks_a_row() {
        for rule in RULES {
            assert!(
                SNIPPETS.iter().any(|(p, ..)| *p == rule.path),
                "no snippet for row `{}`",
                rule.path
            );
        }
        for (path, ..) in SNIPPETS {
            assert!(
                RULES.iter().any(|r| r.path == *path),
                "snippet for unknown row `{path}`"
            );
        }
    }

    fn outcome(text: &str, column: Column) -> Result<(), ConfigError> {
        parse_settings(text, Path::new("settings.toml"), column).map(|_| ())
    }

    fn assert_scope(text: &str, column: Column, what: &str) {
        match outcome(text, column) {
            Err(ConfigError::Scope { .. }) => {}
            other => panic!("{what}: expected a scope error, got {other:?}"),
        }
    }

    fn assert_not_scope(text: &str, column: Column, what: &str) {
        let result = outcome(text, column);
        assert!(
            !matches!(
                result,
                Err(ConfigError::Scope { .. } | ConfigError::Parse { .. })
            ),
            "{what}: unexpected {result:?}"
        );
    }

    #[test]
    fn the_user_column_accepts_every_snippet() {
        for (path, accept, reject) in SNIPPETS {
            for text in std::iter::once(accept).chain(reject) {
                assert_not_scope(text, Column::User, &format!("user: {path}"));
            }
        }
    }

    #[test]
    fn each_column_follows_its_cells() {
        for rule in RULES {
            let (_, accept, reject) = SNIPPETS
                .iter()
                .find(|(p, ..)| *p == rule.path)
                .expect("checked by the snippet coverage test");
            for column in PROJECT_COLUMNS {
                let what = format!("{}: {}", column.label(), rule.path);
                match rule.cell(column) {
                    Cell::No => assert_scope(accept, column, &what),
                    Cell::Yes => {
                        for text in std::iter::once(accept).chain(reject) {
                            assert_not_scope(text, column, &what);
                        }
                    }
                    Cell::If(_) => {
                        assert_not_scope(accept, column, &what);
                        assert_scope(
                            reject.expect("a gated row has a refused snippet"),
                            column,
                            &what,
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn predicates_accept_and_refuse_their_values() {
        let s = |v: &str| toml::Value::String(v.to_string());
        let b = toml::Value::Boolean;
        assert!(is_tightening_mode(&s("manual")));
        assert!(is_tightening_mode(&s("dontAsk")));
        assert!(!is_tightening_mode(&s("plan")));
        assert!(!is_tightening_mode(&s("auto")));
        assert!(is_false(&b(false)));
        assert!(!is_false(&b(true)));
        assert!(is_shadow_true(&b(true)));
        assert!(!is_shadow_true(&b(false)));
    }

    #[test]
    fn a_key_with_no_row_is_refused_from_project_scopes_only() {
        let rules = [Rule::any("a.known")];
        let table: toml::Table = toml::from_str("[a]\nknown = 1\nunlisted = 2\n").unwrap();
        for column in [Column::Project, Column::ProjectLocal] {
            let (key, _) = check_rules(&rules, &table, column).unwrap_err();
            assert_eq!(key, "a.unlisted");
        }
        check_rules(&rules, &table, Column::Cli).unwrap();
        check_rules(&rules, &table, Column::User).unwrap();
    }

    #[test]
    fn empty_interior_tables_are_accepted() {
        let table: toml::Table =
            toml::from_str("[guardian]\n[permissions]\n[telemetry]\n[auto_mode]\n[hooks]\n")
                .unwrap();
        for column in PROJECT_COLUMNS {
            check_scope(&table, column).unwrap();
        }
    }
}
