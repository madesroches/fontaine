//! Whether a call is of the class the guardian's confidence must back up. It is derived from the
//! call itself, structurally, on the shell parser's output and the scope validator, and never
//! from the guardian's own label: a verdict labelled `allow` cannot make a destructive call
//! look routine.

use std::path::Path;

use fontaine_tools::descriptor::{Destructive, find, is_shell_class};
use fontaine_tools::names::is_mcp;
use fontaine_tools::preclassify::{SimpleCommand, analyze, program_name};
use fontaine_tools::scope::Scope;
use serde_json::Value;

use crate::code_execution::{LAUNCHERS, starts_with_entry, trim_version};
use crate::ledger::{Existence, LedgerView};

/// Programs that remove, overwrite or re-point files whatever their arguments.
pub const REMOVAL_OVERWRITE_WORDS: [&str; 14] = [
    "rm", "rmdir", "mv", "cp", "dd", "truncate", "shred", "unlink", "ln", "chmod", "chown",
    "install", "tee", "xargs",
];

/// Programs that talk to the network whatever their arguments.
pub const NETWORK_CLIENT_WORDS: [&str; 12] = [
    "curl", "wget", "ssh", "scp", "sftp", "rsync", "nc", "ncat", "netcat", "socat", "telnet", "ftp",
];

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

/// Installers that take a bare `install` subcommand, so they need no entry in the table above.
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
    match find(tool).map(|d| d.destructive) {
        Some(Destructive::Never) => false,
        Some(Destructive::FileWrite) => file_write(args, facts),
        Some(Destructive::Shell) => shell(tool, args, facts),
        // A tool that runs at all can do anything: a spawned agent, a message, the network.
        Some(Destructive::Always) | None => true,
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

fn shell(tool: &str, args: &Value, facts: &Facts<'_>) -> bool {
    let Some(command) = args.get("command").and_then(Value::as_str) else {
        return true;
    };
    if facts.scope.locate(tool, args).is_err() {
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
        let name = arg.split_once('=').map_or(arg.as_str(), |(name, _)| name);
        if long.contains(&name) {
            return true;
        }
        arg.starts_with('-')
            && !arg.starts_with("--")
            && arg.chars().skip(1).any(|c| short.contains(&c))
    })
}

fn git_destructive(args: &[String]) -> bool {
    // Config injection (`-c alias.x=!cmd`, `core.pager`, `core.fsmonitor`) and an alternate
    // exec path run arbitrary programs under any subcommand.
    let before_subcommand = git_subcommand(args).unwrap_or(args.len());
    if args[..before_subcommand].iter().any(|a| {
        a == "-c"
            || a.starts_with("--config-env")
            || a.starts_with("--exec-path")
            || (a.starts_with("-c") && !a.starts_with("--") && a.len() > 2 && a.contains('='))
    }) {
        return true;
    }
    let Some(at) = git_subcommand(args) else {
        return false;
    };
    let rest = &args[at + 1..];
    match args[at].as_str() {
        "reset" => rest.iter().any(|a| a == "--hard"),
        "clean" | "restore" | "rebase" | "filter-branch" | "rm" => true,
        // A branch name and a pathspec look alike, so any operand is treated as a possible
        // discard of working-tree changes unless the call is creating a branch.
        "checkout" => {
            let creates = rest
                .iter()
                .any(|a| matches!(a.as_str(), "-b" | "-B" | "--orphan"));
            rest.iter().any(|a| a == "--" || a == "-f")
                || rest
                    .iter()
                    .any(|a| a == "--force" || a.starts_with("--force="))
                || (!creates && rest.iter().any(|a| !a.starts_with('-')))
        }
        "switch" => rest
            .iter()
            .any(|a| a == "-f" || a == "--force" || a == "--discard-changes"),
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

fn destructive_command(command: &SimpleCommand) -> bool {
    // A command word that expands (`$cmd`, `$(pick)`) cannot be classified.
    let Some(first) = command.words.first() else {
        return false;
    };
    let Some(first) = first.literal.as_deref() else {
        return true;
    };
    let args: Vec<String> = match words(command) {
        Some(all) => all[1..].to_vec(),
        // Expanding arguments of a program are still judged by what the program is.
        None => command.words[1..]
            .iter()
            .map(|w| w.literal.clone().unwrap_or_else(|| w.raw.clone()))
            .collect(),
    };
    destructive_argv(&program_name(first), &args, 0)
}

/// The program a launcher runs, as an index into `args`; `None` when it cannot be located.
fn wrapped_program(options_with_value: &[&str], operands: usize, args: &[String]) -> Option<usize> {
    let mut at = 0;
    while let Some(arg) = args.get(at) {
        if arg == "--" {
            at += 1;
            break;
        }
        if !arg.starts_with('-') || arg == "-" {
            break;
        }
        at += if options_with_value.contains(&arg.as_str()) {
            2
        } else {
            1
        };
    }
    let at = at + operands;
    (at < args.len()).then_some(at)
}

fn destructive_argv(program: &str, args: &[String], depth: usize) -> bool {
    if let Some((_, options, operands)) = LAUNCHERS.iter().find(|(name, ..)| *name == program) {
        // Nesting past a few levels is not a command anyone means to run.
        if depth >= 8 {
            return true;
        }
        let Some(at) = wrapped_program(options, *operands, args) else {
            return true;
        };
        let wrapped = &args[at];
        if wrapped.contains(['$', '`']) {
            return true;
        }
        return destructive_argv(&program_name(wrapped), &args[at + 1..], depth + 1);
    }

    if REMOVAL_OVERWRITE_WORDS.contains(&program) || NETWORK_CLIENT_WORDS.contains(&program) {
        return true;
    }
    match program {
        "sed" if has_flag(args, &["--in-place"], &['i']) => return true,
        "perl" if has_flag(args, &[], &['i']) => return true,
        "find"
            if args
                .iter()
                .any(|a| FIND_DESTRUCTIVE_FLAGS.contains(&a.as_str())) =>
        {
            return true;
        }
        "git" if git_destructive(args) || git_network(args) => return true,
        _ => {}
    }
    installer(program, args) || starts_with_entry(trim_version(program), args)
}

/// The absolute paths a call touches, as the scope validator resolves them. A file tool names
/// its path; a shell command's redirect targets and literal words that look like paths are
/// resolved against the call's working directory.
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
        "read" | "write" | "edit" | "notebook_edit" | "lsp" => {
            out.extend(text("path").map(resolve))
        }
        "ls" | "grep" => out.push(
            text("path").map_or_else(|| scope.primary().to_string_lossy().into_owned(), resolve),
        ),
        "glob" => {
            let (base, _) =
                fontaine_tools::scope::glob_target(text("path"), text("pattern").unwrap_or(""));
            out.push(resolve(&base));
        }
        tool if is_shell_class(tool) => {
            let Some(command) = text("command") else {
                return out;
            };
            let base = match text("cwd") {
                Some(cwd) => scope.resolved(cwd),
                None => scope.primary().to_path_buf(),
            };
            let join = |literal: &str| {
                if Path::new(literal).is_absolute() {
                    scope.resolved(literal)
                } else {
                    scope.resolved(&base.join(literal).to_string_lossy())
                }
            };
            let analysis = analyze(command);
            // First, so the targets survive the cap: a redirect target is a file by construction.
            for literal in analysis
                .output_targets
                .iter()
                .filter_map(|w| w.literal.as_deref())
            {
                if !literal.is_empty() {
                    out.push(join(literal).to_string_lossy().into_owned());
                }
            }
            for simple in &analysis.commands {
                for word in simple.words.iter().skip(1) {
                    let Some(literal) = word.literal.as_deref() else {
                        continue;
                    };
                    if literal.is_empty() || literal.starts_with('-') {
                        continue;
                    }
                    let joined = join(literal);
                    let pathlike = literal.contains(['/', '\\']) || literal.starts_with('.');
                    if pathlike || joined.exists() {
                        out.push(joined.to_string_lossy().into_owned());
                    }
                }
            }
        }
        _ => {}
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|p| seen.insert(p.clone()));
    out.truncate(LIMIT);
    out
}
