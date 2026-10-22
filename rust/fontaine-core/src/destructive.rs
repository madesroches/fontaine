//! Whether a call is of the class the guardian's confidence must back up. It is derived from the
//! call itself, structurally, on the shell parser's output and the scope validator, and never
//! from the guardian's own label: a verdict labelled `allow` cannot make a destructive call
//! look routine.

use std::path::Path;

use fontaine_tools::names::is_mcp;
use fontaine_tools::preclassify::{SimpleCommand, analyze, program_name};
use fontaine_tools::scope::Scope;
use serde_json::Value;

use crate::ledger::{Existence, LedgerView};
use crate::permission_rules::BROAD_CODE_EXECUTION;

/// Programs that remove, overwrite or re-point files whatever their arguments.
pub const REMOVAL_OVERWRITE_WORDS: [&str; 14] = [
    "rm", "rmdir", "mv", "cp", "dd", "truncate", "shred", "unlink", "ln", "chmod", "chown",
    "install", "tee", "xargs",
];

/// Programs that talk to the network whatever their arguments.
pub const NETWORK_CLIENT_WORDS: [&str; 10] = [
    "curl", "wget", "ssh", "scp", "sftp", "rsync", "nc", "ncat", "netcat", "socat",
];

/// More network clients, kept apart so the table above stays a plain array.
pub const NETWORK_CLIENT_WORDS_EXTRA: [&str; 2] = ["telnet", "ftp"];

/// `git` subcommands that reach a remote.
pub const GIT_NETWORK_SUBCOMMANDS: [&str; 5] = ["push", "fetch", "pull", "clone", "remote"];

/// Package managers and the subcommands that install from a registry.
pub const INSTALLERS: [(&str, &[&str]); 11] = [
    ("pip", &["install"]),
    ("pip3", &["install"]),
    ("npm", &["install", "add", "i"]),
    ("pnpm", &["install", "add", "i"]),
    ("yarn", &["install", "add"]),
    ("cargo", &["install"]),
    ("gem", &["install"]),
    ("go", &["install"]),
    ("apt", &["install"]),
    ("apt-get", &["install"]),
    ("brew", &["install"]),
];

/// Installers from the table above that need no subcommand beyond `install`.
const MORE_INSTALLERS: [&str; 2] = ["dnf", "yum"];

const FIND_DESTRUCTIVE_FLAGS: [&str; 5] = ["-delete", "-exec", "-execdir", "-ok", "-okdir"];

/// What the derivation reads besides the call.
pub struct Facts<'a> {
    pub scope: &'a Scope,
    pub view: &'a LedgerView,
}

/// Whether `tool` called with `args` is destructive.
pub fn derive(tool: &str, args: &Value, facts: &Facts<'_>) -> bool {
    if is_mcp(tool) {
        return true;
    }
    match tool {
        "read" | "ls" | "glob" | "grep" | "skill" => false,
        "webfetch" => true,
        "write" | "edit" => file_write(args, facts),
        "shell" => shell(args, facts),
        // Any other tool that runs at all can do anything: a spawned agent, a message.
        _ => true,
    }
}

fn file_write(args: &Value, facts: &Facts<'_>) -> bool {
    let Some(path) = args.get("path").and_then(Value::as_str) else {
        return true;
    };
    let Ok((_, resolved)) = facts.scope.find(path) else {
        return true;
    };
    match facts.view.existence(&resolved.to_string_lossy()) {
        Existence::CreatedThisSession => false,
        // A write to a path that is not there is a creation, not an overwrite.
        Existence::PreExisting | Existence::Unindexed => resolved.exists(),
    }
}

fn shell(args: &Value, facts: &Facts<'_>) -> bool {
    let Some(command) = args.get("command").and_then(Value::as_str) else {
        return true;
    };
    if facts.scope.locate("shell", args).is_err() {
        return true;
    }
    let analysis = analyze(command);
    if !analysis.parsed || analysis.unanalyzed || analysis.nested_chain || analysis.output_redirect
    {
        return true;
    }
    analysis.commands.iter().any(destructive_command)
}

fn words(command: &SimpleCommand) -> Option<Vec<String>> {
    command.words.iter().map(|w| w.literal.clone()).collect()
}

/// The first word of `args` that is not an option, skipping the options of `git` that take a
/// value.
fn git_subcommand(args: &[String]) -> Option<usize> {
    let mut at = 0;
    while at < args.len() {
        let arg = args[at].as_str();
        if matches!(
            arg,
            "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace"
        ) {
            at += 2;
        } else if arg.starts_with('-') {
            at += 1;
        } else {
            return Some(at);
        }
    }
    None
}

fn has_flag(args: &[String], long: &[&str], short: &[char]) -> bool {
    args.iter().any(|arg| {
        if long.contains(&arg.as_str()) {
            return true;
        }
        arg.starts_with('-')
            && !arg.starts_with("--")
            && arg.chars().skip(1).any(|c| short.contains(&c))
    })
}

fn git_destructive(args: &[String]) -> bool {
    let Some(at) = git_subcommand(args) else {
        return false;
    };
    let rest = &args[at + 1..];
    match args[at].as_str() {
        "reset" => rest.iter().any(|a| a == "--hard"),
        "clean" | "restore" | "rebase" | "filter-branch" => true,
        "checkout" => rest.iter().any(|a| a == "--"),
        "branch" => rest.iter().any(|a| a == "-D"),
        "push" => rest
            .iter()
            .any(|a| a == "-f" || a.starts_with("--force") || (a.starts_with('+') && a.len() > 1)),
        "stash" => rest.first().is_some_and(|a| a == "drop" || a == "clear"),
        _ => false,
    }
}

fn git_network(args: &[String]) -> bool {
    git_subcommand(args).is_some_and(|at| GIT_NETWORK_SUBCOMMANDS.contains(&args[at].as_str()))
}

fn installer(program: &str, args: &[String]) -> bool {
    let first = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .map(String::as_str);
    if MORE_INSTALLERS.contains(&program) {
        return first == Some("install");
    }
    INSTALLERS
        .iter()
        .find(|(name, _)| *name == program)
        .is_some_and(|(_, subs)| first.is_some_and(|f| subs.contains(&f)))
}

/// Whether the leading words of the command start with an entry of the broad code-execution
/// list: `cargo run --release` starts with `cargo run`, `shred` does not start with `sh`.
fn broad_code_execution(program: &str, args: &[String]) -> bool {
    BROAD_CODE_EXECUTION.iter().any(|entry| {
        let mut entry_words = entry.split(' ');
        let head = entry_words.next().unwrap_or("");
        head == program
            && entry_words
                .enumerate()
                .all(|(i, word)| args.get(i).is_some_and(|a| a == word))
    })
}

fn destructive_command(command: &SimpleCommand) -> bool {
    // A command word that expands (`$cmd`, `$(pick)`) cannot be classified.
    let Some(first) = command.words.first() else {
        return false;
    };
    let Some(first) = first.literal.as_deref() else {
        return true;
    };
    let program = program_name(first).into_owned();
    let args: Vec<String> = match words(command) {
        Some(all) => all[1..].to_vec(),
        // Expanding arguments of a program are still judged by what the program is.
        None => command.words[1..]
            .iter()
            .map(|w| w.literal.clone().unwrap_or_else(|| w.raw.clone()))
            .collect(),
    };
    let program = program.as_str();

    if REMOVAL_OVERWRITE_WORDS.contains(&program)
        || NETWORK_CLIENT_WORDS.contains(&program)
        || NETWORK_CLIENT_WORDS_EXTRA.contains(&program)
    {
        return true;
    }
    match program {
        "sed" if has_flag(&args, &["--in-place"], &['i']) => return true,
        "perl" if has_flag(&args, &[], &['i']) => return true,
        "find"
            if args
                .iter()
                .any(|a| FIND_DESTRUCTIVE_FLAGS.contains(&a.as_str())) =>
        {
            return true;
        }
        "git" if git_destructive(&args) || git_network(&args) => return true,
        _ => {}
    }
    installer(program, &args) || broad_code_execution(program, &args)
}

/// The absolute paths a call touches, as the scope validator resolves them. A file tool names
/// its path; a shell command's literal words that look like paths are resolved against the
/// call's working directory.
pub fn touched_paths(tool: &str, args: &Value, scope: &Scope) -> Vec<String> {
    const LIMIT: usize = 16;
    let text = |key: &str| args.get(key).and_then(Value::as_str);
    let resolve = |path: &str| {
        scope
            .find(path)
            .map_or_else(|_| scope.resolved(path), |(_, resolved)| resolved)
            .to_string_lossy()
            .into_owned()
    };
    let mut out: Vec<String> = Vec::new();
    match tool {
        "read" | "write" | "edit" => out.extend(text("path").map(resolve)),
        "ls" | "grep" => out.push(
            text("path").map_or_else(|| scope.primary().to_string_lossy().into_owned(), resolve),
        ),
        "glob" => {
            let (base, _) =
                fontaine_tools::scope::glob_target(text("path"), text("pattern").unwrap_or(""));
            out.push(resolve(&base));
        }
        "shell" => {
            let Some(command) = text("command") else {
                return out;
            };
            let base = match text("cwd") {
                Some(cwd) => scope.resolved(cwd),
                None => scope.primary().to_path_buf(),
            };
            for simple in &analyze(command).commands {
                for word in simple.words.iter().skip(1) {
                    let Some(literal) = word.literal.as_deref() else {
                        continue;
                    };
                    if literal.is_empty() || literal.starts_with('-') {
                        continue;
                    }
                    let joined = if Path::new(literal).is_absolute() {
                        scope.resolved(literal)
                    } else {
                        scope.resolved(&base.join(literal).to_string_lossy())
                    };
                    let pathlike = literal.contains(['/', '\\']) || literal.starts_with('.');
                    if pathlike || joined.exists() {
                        out.push(joined.to_string_lossy().into_owned());
                    }
                }
            }
        }
        _ => {}
    }
    out.dedup();
    let mut seen = std::collections::HashSet::new();
    out.retain(|p| seen.insert(p.clone()));
    out.truncate(LIMIT);
    out
}
