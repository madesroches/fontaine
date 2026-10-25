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
}
