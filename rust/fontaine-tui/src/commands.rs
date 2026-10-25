//! The built-in slash commands: one table that both the parser and the `/` menu read, so a
//! command cannot be runnable without being listed, or listed without being runnable.

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Exit,
    AddDir(String),
    Denied,
    Agents,
    /// `/todos`: shows or hides the expanded task list.
    Todos,
    /// `/clear` (`/reset`, `/new`); a name for the old conversation is refused.
    Clear(String),
    /// `/compact [focus]`.
    Compact(String),
    /// `/context [all]`.
    Context(String),
    OutputStyle(String),
    Model(String),
    GuardianModel(String),
    /// `/login [account]`.
    Login(String),
    Help,
    Status,
    /// `/theme [name]`.
    Theme(String),
    Permissions,
    /// `/mcp [argument]`; only the empty form does anything.
    Mcp(String),
    Hooks,
    Skills,
    /// `/loop [interval] <prompt>`, `/loop list` or `/loop stop [id]`.
    Loop(String),
    /// `/plan [description]`.
    Plan(String),
    /// `/copy [n]`.
    Copy(String),
    /// A name no built-in command has; it may be a skill.
    Other {
        name: String,
        argument: String,
    },
}

pub struct Builtin {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub hint: Option<&'static str>,
    pub description: &'static str,
    /// Builds the command from the trimmed argument, which commands without one ignore.
    pub command: fn(String) -> Command,
}

impl Builtin {
    fn answers_to(&self, name: &str) -> bool {
        self.name == name || self.aliases.contains(&name)
    }
}

/// Sorted by name, which is the order the menu shows.
pub const BUILTINS: &[Builtin] = &[
    Builtin {
        name: "add-dir",
        aliases: &[],
        hint: Some("<path>"),
        description: "add a workspace root",
        command: Command::AddDir,
    },
    Builtin {
        name: "agents",
        aliases: &["tasks"],
        hint: None,
        description: "the background agents panel",
        command: |_| Command::Agents,
    },
    Builtin {
        name: "clear",
        aliases: &["reset", "new"],
        hint: None,
        description: "start over with an empty context",
        command: Command::Clear,
    },
    Builtin {
        name: "compact",
        aliases: &[],
        hint: Some("[focus]"),
        description: "summarize the conversation to free context",
        command: Command::Compact,
    },
    Builtin {
        name: "context",
        aliases: &[],
        hint: Some("[all]"),
        description: "show what fills the context window",
        command: Command::Context,
    },
    Builtin {
        name: "copy",
        aliases: &[],
        hint: Some("[n]"),
        description: "copy the latest assistant reply to the clipboard",
        command: Command::Copy,
    },
    Builtin {
        name: "denied",
        aliases: &[],
        hint: None,
        description: "calls the guardian denied",
        command: |_| Command::Denied,
    },
    Builtin {
        name: "exit",
        aliases: &["quit"],
        hint: None,
        description: "leave Fontaine",
        command: |_| Command::Exit,
    },
    Builtin {
        name: "guardian-model",
        aliases: &[],
        hint: Some("[account/model]"),
        description: "choose the guardian model",
        command: Command::GuardianModel,
    },
    Builtin {
        name: "help",
        aliases: &[],
        hint: None,
        description: "list the commands and skills",
        command: |_| Command::Help,
    },
    Builtin {
        name: "hooks",
        aliases: &[],
        hint: None,
        description: "the configured hooks",
        command: |_| Command::Hooks,
    },
    Builtin {
        name: "login",
        aliases: &[],
        hint: Some("[account]"),
        description: "manage providers, add accounts and sign in",
        command: Command::Login,
    },
    Builtin {
        name: "loop",
        aliases: &[],
        hint: Some("[interval] <prompt> | list | stop [id]"),
        description: "run a prompt again and again while the session is idle",
        command: Command::Loop,
    },
    Builtin {
        name: "mcp",
        aliases: &[],
        hint: None,
        description: "the MCP servers and their state",
        command: Command::Mcp,
    },
    Builtin {
        name: "model",
        aliases: &[],
        hint: Some("[account/model]"),
        description: "choose the model",
        command: Command::Model,
    },
    Builtin {
        name: "output-style",
        aliases: &[],
        hint: Some("[name|none]"),
        description: "show or change the output style",
        command: Command::OutputStyle,
    },
    Builtin {
        name: "permissions",
        aliases: &[],
        hint: None,
        description: "the permission mode and rules in force",
        command: |_| Command::Permissions,
    },
    Builtin {
        name: "plan",
        aliases: &[],
        hint: Some("[description]"),
        description: "enter plan mode, optionally with a task",
        command: Command::Plan,
    },
    Builtin {
        name: "skills",
        aliases: &[],
        hint: None,
        description: "the loaded skills",
        command: |_| Command::Skills,
    },
    Builtin {
        name: "status",
        aliases: &[],
        hint: None,
        description: "version, session, model, mode and guardian",
        command: |_| Command::Status,
    },
    Builtin {
        name: "theme",
        aliases: &[],
        hint: Some("[<name>]"),
        description: "choose the colour theme",
        command: Command::Theme,
    },
    Builtin {
        name: "todos",
        aliases: &[],
        hint: None,
        description: "expand or collapse the task list",
        command: |_| Command::Todos,
    },
];

/// Whether a built-in command answers to `name`.
pub fn is_builtin(name: &str) -> bool {
    BUILTINS.iter().any(|b| b.answers_to(name))
}

/// Reads a slash command from a submitted message; `None` when it is an ordinary message.
pub fn parse_command(text: &str) -> Option<Command> {
    let text = text.trim();
    let rest = text.strip_prefix('/')?;
    let (name, argument) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let argument = argument.trim().to_string();
    Some(match BUILTINS.iter().find(|b| b.answers_to(name)) {
        Some(builtin) => (builtin.command)(argument),
        None => Command::Other {
            name: name.to_string(),
            argument,
        },
    })
}

/// One line of the `/` menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuEntry {
    pub name: String,
    pub hint: Option<String>,
    pub description: String,
    /// Other names the command answers to; always empty for skills.
    pub aliases: Vec<String>,
}

/// The built-in commands, as the `/` menu lists them. Aliases get no row of their own.
pub fn builtin_menu() -> Vec<MenuEntry> {
    BUILTINS
        .iter()
        .map(|b| MenuEntry {
            name: b.name.to_string(),
            hint: b.hint.map(str::to_string),
            description: b.description.to_string(),
            aliases: b.aliases.iter().map(|a| a.to_string()).collect(),
        })
        .collect()
}

impl MenuEntry {
    /// Whether the command cannot run bare: its hint opens with `<`, as skills' verbatim
    /// argument hints do too.
    pub fn requires_argument(&self) -> bool {
        self.hint.as_deref().is_some_and(|h| h.starts_with('<'))
    }

    /// The names the entry answers to, lowercased.
    fn names(&self) -> impl Iterator<Item = String> + '_ {
        std::iter::once(&self.name)
            .chain(&self.aliases)
            .map(|n| n.to_lowercase())
    }

    /// The alias that made the entry match `typed`, when its own name did not.
    pub fn matched_alias(&self, typed: &str) -> Option<&str> {
        let typed = typed.to_lowercase();
        let name = self.name.to_lowercase();
        if name.starts_with(&typed) {
            return None;
        }
        // A name that merely contains the text loses to an alias that starts with it, as in
        // match_menu's ranking.
        let alias_prefix = self
            .aliases
            .iter()
            .any(|a| a.to_lowercase().starts_with(&typed));
        if name.contains(&typed) && !alias_prefix {
            return None;
        }
        let by = |hit: fn(&str, &str) -> bool| {
            self.aliases
                .iter()
                .find(|a| hit(&a.to_lowercase(), &typed))
                .map(String::as_str)
        };
        by(|a, t| a.starts_with(t)).or_else(|| by(|a, t| a.contains(t)))
    }
}

/// The entries `typed` (the text after the `/`) could be: names or aliases starting with it,
/// an exact one first, then those merely containing it. Case-insensitive; the menu's order
/// otherwise.
pub fn match_menu<'a>(menu: &'a [MenuEntry], typed: &str) -> Vec<&'a MenuEntry> {
    let typed = typed.to_lowercase();
    let mut exact = Vec::new();
    let mut prefix = Vec::new();
    let mut contains = Vec::new();
    for entry in menu {
        let names: Vec<String> = entry.names().collect();
        if names.contains(&typed) {
            exact.push(entry);
        } else if names.iter().any(|n| n.starts_with(&typed)) {
            prefix.push(entry);
        } else if names.iter().any(|n| n.contains(&typed)) {
            contains.push(entry);
        }
    }
    exact.extend(prefix);
    exact.extend(contains);
    exact
}

/// What `/loop` was asked to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoopRequest {
    List,
    /// Cancel the task with this id, or every task and the loop when none is given.
    Stop(Option<String>),
    /// Run `prompt` on a wall-clock aligned cadence, starting with one iteration now.
    Every(Interval),
    /// Run `prompt` now and let the model pace the later iterations with `schedule_wakeup`.
    SelfPaced(String),
}

/// A cadence `/loop` can schedule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Interval {
    pub prompt: String,
    /// The cron expression of the cadence chosen.
    pub cron: String,
    /// The interval as the user wrote it, when it had to be rounded to an even cadence.
    pub rounded_from: Option<String>,
}

/// Minutes between runs that divide an hour and read cleanly.
const MINUTE_STEPS: [u64; 7] = [1, 2, 5, 10, 15, 20, 30];
/// Hours between runs that divide a day.
const HOUR_STEPS: [u64; 7] = [1, 2, 3, 4, 6, 8, 12];
const MINUTES_PER_DAY: u64 = 24 * 60;
const MAX_DAYS: u64 = 31;

/// The cadences `/loop` accepts, in minutes: minute steps that divide an hour, whole hours that
/// divide a day, and 1-31 days.
fn cadences() -> Vec<u64> {
    let hours = HOUR_STEPS.iter().map(|h| h * 60);
    let days = (1..=MAX_DAYS).map(|d| d * MINUTES_PER_DAY);
    let mut all: Vec<u64> = MINUTE_STEPS.into_iter().chain(hours).chain(days).collect();
    all.sort_unstable();
    all
}

/// The accepted cadence nearest to `minutes`; the shorter one when two are equally near.
fn nearest_cadence(minutes: u64) -> u64 {
    cadences()
        .into_iter()
        .min_by_key(|c| (c.abs_diff(minutes), *c))
        .expect("the cadence table is not empty")
}

fn cadence_cron(minutes: u64) -> String {
    if minutes < 60 {
        match minutes {
            1 => "* * * * *".to_string(),
            n => format!("*/{n} * * * *"),
        }
    } else if minutes < MINUTES_PER_DAY {
        match minutes / 60 {
            1 => "0 * * * *".to_string(),
            n => format!("0 */{n} * * *"),
        }
    } else {
        match minutes / MINUTES_PER_DAY {
            1 => "0 0 * * *".to_string(),
            n => format!("0 0 */{n} * *"),
        }
    }
}

fn unit_seconds(unit: &str) -> Option<u64> {
    Some(match unit.to_lowercase().as_str() {
        "s" | "sec" | "secs" | "second" | "seconds" => 1,
        "m" | "min" | "mins" | "minute" | "minutes" => 60,
        "h" | "hr" | "hrs" | "hour" | "hours" => 3_600,
        "d" | "day" | "days" => 86_400,
        _ => return None,
    })
}

/// `5m`, `2h`, `30s`, `5min`: digits then a unit.
fn compact_interval(token: &str) -> Option<(u64, u64, String)> {
    let split = token.find(|c: char| !c.is_ascii_digit())?;
    let (digits, unit) = token.split_at(split);
    if digits.is_empty() {
        return None;
    }
    let seconds = unit_seconds(unit)?;
    Some((digits.parse().ok()?, seconds, token.to_string()))
}

/// A trailing `every 2 hours` or `every hour`: the text before it and the interval.
fn trailing_interval(text: &str) -> Option<(&str, u64, u64, String)> {
    fn last(t: &str) -> Option<(&str, &str)> {
        let t = t.trim_end();
        match t.rsplit_once(char::is_whitespace) {
            Some((rest, word)) => Some((rest.trim_end(), word)),
            None => (!t.is_empty()).then_some(("", t)),
        }
    }
    let (rest, unit_word) = last(text)?;
    let seconds = unit_seconds(unit_word)?;
    let (before, word) = last(rest)?;
    if word.eq_ignore_ascii_case("every") {
        return Some((before, 1, seconds, format!("every {unit_word}")));
    }
    let count: u64 = word.parse().ok()?;
    let (before, every) = last(before)?;
    every
        .eq_ignore_ascii_case("every")
        .then(|| (before, count, seconds, format!("every {count} {unit_word}")))
}

fn interval(
    prompt: &str,
    count: u64,
    seconds: u64,
    written: String,
) -> Result<LoopRequest, String> {
    if prompt.trim().is_empty() {
        return Err("/loop needs a prompt to run, such as `/loop 5m check the build`".to_string());
    }
    if count == 0 {
        return Err(format!(
            "the interval `{written}` for /loop must be at least 1"
        ));
    }
    let minutes = count
        .saturating_mul(seconds)
        .div_ceil(60)
        .min(MAX_DAYS * MINUTES_PER_DAY);
    let chosen = nearest_cadence(minutes);
    let exact = count.saturating_mul(seconds) == chosen * 60;
    Ok(LoopRequest::Every(Interval {
        prompt: prompt.trim().to_string(),
        cron: cadence_cron(chosen),
        rounded_from: (!exact).then_some(written),
    }))
}

/// Reads the argument of `/loop`.
pub fn parse_loop(argument: &str) -> Result<LoopRequest, String> {
    let argument = argument.trim();
    if argument.is_empty() {
        return Err(
            "usage: /loop [interval] <prompt>, /loop list, /loop stop [id]; for example `/loop 5m check the build`"
                .to_string(),
        );
    }
    if argument == "list" {
        return Ok(LoopRequest::List);
    }
    let mut words = argument.split_whitespace();
    if words.next() == Some("stop") {
        let id = words.next();
        if words.next().is_none() {
            return Ok(LoopRequest::Stop(id.map(str::to_string)));
        }
    }
    let (first, rest) = argument
        .split_once(char::is_whitespace)
        .unwrap_or((argument, ""));
    if let Some((count, seconds, written)) = compact_interval(first) {
        return interval(rest, count, seconds, written);
    }
    if let Some((before, count, seconds, written)) = trailing_interval(argument) {
        return interval(before, count, seconds, written);
    }
    Ok(LoopRequest::SelfPaced(argument.to_string()))
}

/// Which reply `/copy` is asked for: 1 is the latest.
pub fn reply_number(argument: &str) -> Result<usize, String> {
    if argument.is_empty() {
        return Ok(1);
    }
    match argument.parse::<usize>() {
        Ok(n) if n > 0 => Ok(n),
        _ => Err(format!(
            "/copy takes a positive number, such as `/copy 2`, not `{argument}`"
        )),
    }
}

/// Whether `/context` is asked for the per-item breakdown.
pub fn context_all(argument: &str) -> Result<bool, String> {
    match argument {
        "" => Ok(false),
        "all" => Ok(true),
        _ => Err(format!(
            "/context takes no argument or `all`, not `{argument}`"
        )),
    }
}

/// The menu rows for user-invocable skills (name, hint, description), and the names of those
/// left out because a built-in command answers to them.
pub fn skill_menu(skills: Vec<(String, Option<String>, String)>) -> (Vec<MenuEntry>, Vec<String>) {
    let mut rows = Vec::new();
    let mut hidden = Vec::new();
    for (name, hint, description) in skills {
        if is_builtin(&name) {
            hidden.push(name);
        } else {
            rows.push(MenuEntry {
                name,
                hint,
                description,
                aliases: Vec::new(),
            });
        }
    }
    (rows, hidden)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_parse() {
        assert_eq!(parse_command("hello"), None);
        assert_eq!(parse_command("/exit"), Some(Command::Exit));
        assert_eq!(parse_command("/quit"), Some(Command::Exit));
        assert_eq!(
            parse_command("/add-dir  /tmp/x "),
            Some(Command::AddDir("/tmp/x".into()))
        );
        assert_eq!(
            parse_command("/nope  with args "),
            Some(Command::Other {
                name: "nope".into(),
                argument: "with args".into()
            })
        );
        assert_eq!(parse_command("/agents"), Some(Command::Agents));
        assert_eq!(parse_command("/tasks"), Some(Command::Agents));
        assert_eq!(parse_command("/todos"), Some(Command::Todos));
        assert_eq!(
            parse_command("/output-style terse"),
            Some(Command::OutputStyle("terse".into()))
        );
        assert_eq!(parse_command("/denied"), Some(Command::Denied));
        assert_eq!(parse_command("/denied foo"), Some(Command::Denied));
        assert_eq!(
            parse_command("/model  work/claude-sonnet-4-6 "),
            Some(Command::Model("work/claude-sonnet-4-6".into()))
        );
        assert_eq!(parse_command("/model"), Some(Command::Model(String::new())));
        assert_eq!(
            parse_command("/guardian-model a/b"),
            Some(Command::GuardianModel("a/b".into()))
        );
        assert_eq!(
            parse_command("/login go"),
            Some(Command::Login("go".into()))
        );
        assert_eq!(parse_command("/login"), Some(Command::Login(String::new())));
        assert_eq!(parse_command("/help"), Some(Command::Help));
        assert_eq!(parse_command("/status"), Some(Command::Status));
        assert_eq!(parse_command("/permissions"), Some(Command::Permissions));
        assert_eq!(parse_command("/hooks"), Some(Command::Hooks));
        assert_eq!(parse_command("/skills"), Some(Command::Skills));
        assert_eq!(parse_command("/mcp x"), Some(Command::Mcp("x".into())));
        assert_eq!(parse_command("/mcp"), Some(Command::Mcp(String::new())));
        assert_eq!(
            parse_command("/plan  fix the bug "),
            Some(Command::Plan("fix the bug".into()))
        );
        assert_eq!(parse_command("/plan"), Some(Command::Plan(String::new())));
        assert_eq!(parse_command("/copy 2"), Some(Command::Copy("2".into())));
        assert_eq!(
            parse_command("/loop 5m check the build"),
            Some(Command::Loop("5m check the build".into()))
        );
        assert_eq!(parse_command("/loop"), Some(Command::Loop(String::new())));
        assert_eq!(parse_command("/theme"), Some(Command::Theme(String::new())));
        assert_eq!(
            parse_command("/theme  dark-ansi "),
            Some(Command::Theme("dark-ansi".into()))
        );
        for name in ["clear", "reset", "new"] {
            assert_eq!(
                parse_command(&format!("/{name}")),
                Some(Command::Clear(String::new()))
            );
        }
        assert_eq!(
            parse_command("/clear old work"),
            Some(Command::Clear("old work".into()))
        );
        assert_eq!(
            parse_command("/compact keep the API notes"),
            Some(Command::Compact("keep the API notes".into()))
        );
        assert_eq!(
            parse_command("/compact"),
            Some(Command::Compact(String::new()))
        );
        assert_eq!(
            parse_command("/context all"),
            Some(Command::Context("all".into()))
        );
    }

    #[test]
    fn context_takes_nothing_or_all() {
        assert_eq!(context_all(""), Ok(false));
        assert_eq!(context_all("all"), Ok(true));
        let err = context_all("bogus").unwrap_err();
        assert!(err.contains("all") && err.contains("bogus"), "{err}");
    }

    #[test]
    fn copy_takes_a_positive_number_or_none() {
        assert_eq!(reply_number(""), Ok(1));
        assert_eq!(reply_number("3"), Ok(3));
        for bad in ["0", "-1", "x", "1.5"] {
            assert!(reply_number(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn no_name_or_alias_is_used_twice() {
        let mut seen = std::collections::HashSet::new();
        for builtin in BUILTINS {
            for name in std::iter::once(&builtin.name).chain(builtin.aliases) {
                assert!(seen.insert(*name), "`{name}` is used twice");
            }
        }
    }

    #[test]
    fn the_table_is_sorted_by_name() {
        let names: Vec<&str> = BUILTINS.iter().map(|b| b.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
    }

    #[test]
    fn the_menu_has_a_row_per_entry_and_none_for_aliases() {
        let menu = builtin_menu();
        assert_eq!(menu.len(), BUILTINS.len());
        assert!(menu.iter().all(|m| m.name != "quit" && m.name != "tasks"));
    }

    #[test]
    fn a_skill_named_like_a_builtin_or_its_alias_is_left_out_of_the_menu() {
        let skill = |name: &str| (name.to_string(), None, "d".to_string());
        let (rows, hidden) = skill_menu(vec![skill("status"), skill("quit"), skill("review")]);
        assert_eq!(hidden, vec!["status".to_string(), "quit".to_string()]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "review");
        assert_eq!(parse_command("/status"), Some(Command::Status));
    }

    fn entry(name: &str, aliases: &[&str], hint: Option<&str>) -> MenuEntry {
        MenuEntry {
            name: name.into(),
            hint: hint.map(str::to_string),
            description: "d".into(),
            aliases: aliases.iter().map(|a| a.to_string()).collect(),
        }
    }

    fn names<'a>(found: &[&'a MenuEntry]) -> Vec<&'a str> {
        found.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn prefix_matches_come_before_substring_matches() {
        let menu = vec![
            entry("add-dir", &[], None),
            entry("model", &[], None),
            entry("guardian-model", &[], None),
            entry("mcp", &[], None),
        ];
        assert_eq!(names(&match_menu(&menu, "mo")), ["model", "guardian-model"]);
    }

    #[test]
    fn aliases_match_and_the_matched_one_is_reported() {
        let menu = builtin_menu();
        let found = match_menu(&menu, "res");
        assert_eq!(names(&found), ["clear"]);
        assert_eq!(found[0].matched_alias("res"), Some("reset"));
        assert_eq!(found[0].matched_alias("cle"), None);
        assert_eq!(names(&match_menu(&menu, "tas")), ["agents"]);
        let agents = match_menu(&menu, "t");
        let agents = agents
            .iter()
            .find(|e| e.name == "agents")
            .expect("agents matches t");
        assert_eq!(agents.matched_alias("t"), Some("tasks"));
    }

    #[test]
    fn matching_ignores_case_and_an_empty_query_matches_everything() {
        let menu = builtin_menu();
        assert_eq!(names(&match_menu(&menu, "MODEL"))[0], "model");
        assert_eq!(match_menu(&menu, "").len(), menu.len());
    }

    #[test]
    fn an_exact_name_or_alias_ranks_first_in_the_prefix_group() {
        let menu = vec![
            entry("context", &[], None),
            entry("copy", &[], None),
            entry("co", &[], None),
            entry("zed", &["cop"], None),
        ];
        assert_eq!(
            names(&match_menu(&menu, "co")),
            ["co", "context", "copy", "zed"]
        );
        assert_eq!(names(&match_menu(&menu, "cop"))[0], "zed");
    }

    #[test]
    fn an_entry_matching_by_name_and_alias_appears_once_and_skills_match_by_name() {
        let menu = vec![entry("clear", &["clean"], None), entry("review", &[], None)];
        assert_eq!(names(&match_menu(&menu, "cle")), ["clear"]);
        assert_eq!(names(&match_menu(&menu, "view")), ["review"]);
    }

    #[test]
    fn a_hint_opening_with_an_angle_bracket_requires_an_argument() {
        assert!(entry("add-dir", &[], Some("<path>")).requires_argument());
        assert!(!entry("plan", &[], Some("[description]")).requires_argument());
        assert!(!entry("help", &[], None).requires_argument());
    }

    fn every(request: Result<LoopRequest, String>) -> Interval {
        match request {
            Ok(LoopRequest::Every(interval)) => interval,
            other => panic!("expected an interval, got {other:?}"),
        }
    }

    #[test]
    fn a_leading_interval_sets_the_cadence_and_leaves_the_prompt() {
        let i = every(parse_loop("5m check the build"));
        assert_eq!(i.cron, "*/5 * * * *");
        assert_eq!(i.prompt, "check the build");
        assert_eq!(i.rounded_from, None);
        for (text, cron) in [
            ("1m x", "* * * * *"),
            ("30m x", "*/30 * * * *"),
            ("1h x", "0 * * * *"),
            ("2h x", "0 */2 * * *"),
            ("12h x", "0 */12 * * *"),
            ("1d x", "0 0 * * *"),
            ("24h x", "0 0 * * *"),
            ("3d x", "0 0 */3 * *"),
            ("31d x", "0 0 */31 * *"),
        ] {
            let i = every(parse_loop(text));
            assert_eq!(i.cron, cron, "{text}");
            assert_eq!(i.rounded_from, None, "{text}");
        }
    }

    #[test]
    fn seconds_round_up_to_a_whole_minute() {
        for text in ["1s x", "30s x", "60s x"] {
            let i = every(parse_loop(text));
            assert_eq!(i.cron, "* * * * *", "{text}");
        }
        let i = every(parse_loop("61s x"));
        assert_eq!(i.cron, "*/2 * * * *");
        // 30 seconds is not a minute, but it becomes the minute that is scheduled.
        assert!(every(parse_loop("30s x")).rounded_from.is_some());
        assert_eq!(every(parse_loop("60s x")).rounded_from, None);
    }

    #[test]
    fn an_interval_that_does_not_divide_evenly_goes_to_the_nearest_cadence_ties_down() {
        let rounded = |text: &str| every(parse_loop(text));
        let seven = rounded("7m x");
        assert_eq!(seven.cron, "*/5 * * * *");
        assert_eq!(seven.rounded_from.as_deref(), Some("7m"));
        let ninety = rounded("90m x");
        assert_eq!(ninety.cron, "0 * * * *");
        assert_eq!(ninety.rounded_from.as_deref(), Some("90m"));
        assert_eq!(rounded("45m x").cron, "*/30 * * * *", "a tie rounds down");
        assert_eq!(rounded("100m x").cron, "0 */2 * * *");
        assert_eq!(rounded("5h x").cron, "0 */4 * * *", "a tie rounds down");
        assert_eq!(rounded("10h x").cron, "0 */8 * * *");
        assert_eq!(rounded("36h x").cron, "0 0 * * *", "a tie rounds down");
        assert_eq!(rounded("90d x").cron, "0 0 */31 * *");
    }

    #[test]
    fn a_spelled_out_leading_unit_is_the_interval() {
        let i = every(parse_loop("5min check"));
        assert_eq!(
            (i.prompt.as_str(), i.cron.as_str()),
            ("check", "*/5 * * * *")
        );
        assert_eq!(every(parse_loop("2hours x")).cron, "0 */2 * * *");
    }

    #[test]
    fn a_trailing_every_clause_is_the_interval() {
        let i = every(parse_loop("check the build every 2 hours"));
        assert_eq!(i.prompt, "check the build");
        assert_eq!(i.cron, "0 */2 * * *");
        let i = every(parse_loop("poll the queue every hour"));
        assert_eq!(i.prompt, "poll the queue");
        assert_eq!(i.cron, "0 * * * *");
        let i = every(parse_loop("watch the logs   every 10 minutes"));
        assert_eq!(i.prompt, "watch the logs");
        assert_eq!(i.cron, "*/10 * * * *");
        assert_eq!(every(parse_loop("x every 1 day")).cron, "0 0 * * *");
    }

    #[test]
    fn text_that_only_looks_like_an_interval_is_a_prompt() {
        for text in [
            "check the build",
            "5 things to check",
            "5mm check",
            "summarize every file",
            "every 2 weeks do it",
        ] {
            assert_eq!(
                parse_loop(text),
                Ok(LoopRequest::SelfPaced(text.to_string())),
                "{text}"
            );
        }
    }

    #[test]
    fn bad_forms_are_usage_errors() {
        for bad in ["", "   ", "5m", "5m   ", "0m check", "every 2 hours"] {
            let error = parse_loop(bad).unwrap_err();
            assert!(error.contains("/loop"), "{bad}: {error}");
        }
    }

    #[test]
    fn list_and_stop_are_subcommands() {
        assert_eq!(parse_loop("list"), Ok(LoopRequest::List));
        assert_eq!(parse_loop("stop"), Ok(LoopRequest::Stop(None)));
        assert_eq!(
            parse_loop("stop ab12cd34"),
            Ok(LoopRequest::Stop(Some("ab12cd34".into())))
        );
        // Longer text is a prompt, not a subcommand.
        assert_eq!(
            parse_loop("stop the build when it is green"),
            Ok(LoopRequest::SelfPaced(
                "stop the build when it is green".into()
            ))
        );
        assert_eq!(
            parse_loop("list the failing tests"),
            Ok(LoopRequest::SelfPaced("list the failing tests".into()))
        );
    }
}
