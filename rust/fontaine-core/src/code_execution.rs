//! Programs that run whatever code their arguments hand them. Allow rules over them are
//! suspended in `auto`, and a call to one is destructive: the program's own name says nothing
//! about what it will do.

use fontaine_tools::preclassify::program_name;

/// Entries whose bare form, or form with more arguments (`<entry> *`), runs arbitrary code.
/// Multi-word entries name a subcommand that does.
pub const PROGRAMS: [&str; 36] = [
    "sh",
    "bash",
    "zsh",
    "python",
    "node",
    "ruby",
    "perl",
    "deno",
    "bun",
    "cargo run",
    "npm run",
    "npm exec",
    "bunx",
    "npx",
    "pnpm",
    "yarn",
    "make",
    "eval",
    "exec",
    "xargs",
    "env",
    "systemd-run",
    "parallel",
    "sudo",
    "awk",
    "gawk",
    "mawk",
    "nawk",
    "php",
    "lua",
    "luajit",
    "tclsh",
    "wish",
    "pwsh",
    "powershell",
    "cmd",
];

/// Argv launchers: the options that take a separate value, and how many operands precede the
/// wrapped program. Each one runs any program it is given, so a rule on one is as broad as a
/// shell, while a call is judged by the program it wraps.
pub const LAUNCHERS: [(&str, &[&str], usize); 24] = [
    ("timeout", &["-s", "--signal", "-k", "--kill-after"], 1),
    ("nice", &["-n", "--adjustment"], 0),
    ("nohup", &[], 0),
    ("setsid", &[], 0),
    ("stdbuf", &["-i", "-o", "-e"], 0),
    ("chroot", &["--userspec", "--groups"], 1),
    ("command", &[], 0),
    ("doas", &["-u", "-C"], 0),
    ("busybox", &[], 0),
    ("ionice", &["-c", "-n", "-p", "-P", "-u"], 0),
    ("watch", &["-n", "--interval"], 0),
    (
        "flock",
        &["-w", "--timeout", "-E", "--conflict-exit-code"],
        1,
    ),
    (
        "strace",
        &[
            "-e", "-o", "-p", "-s", "-u", "-E", "-P", "-a", "-O", "-S", "-b", "-I", "-X",
        ],
        0,
    ),
    (
        "ltrace",
        &["-e", "-l", "-o", "-p", "-s", "-u", "-n", "-A", "-a", "-F"],
        0,
    ),
    ("taskset", &[], 1),
    (
        "fakeroot",
        &["-l", "--lib", "-f", "--faked", "-i", "-s", "-b"],
        0,
    ),
    (
        "unshare",
        &[
            "-S",
            "-G",
            "-R",
            "-w",
            "--setuid",
            "--setgid",
            "--root",
            "--wd",
            "--propagation",
            "--map-user",
            "--map-group",
        ],
        0,
    ),
    ("chrt", &[], 1),
    (
        "nsenter",
        &["-t", "--target", "-S", "-G", "-r", "-w", "--root", "--wd"],
        0,
    ),
    ("firejail", &[], 0),
    ("caffeinate", &["-t", "-w"], 0),
    ("arch", &[], 0),
    ("prlimit", &["-p", "--pid"], 0),
    ("time", &["-f", "--format", "-o", "--output"], 0),
];

/// A program name without a trailing version (`python3.12` is `python`), for the lookups in
/// this module only: other tables key on the exact name (`pip3`).
pub fn trim_version(name: &str) -> &str {
    let trimmed = name.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    if trimmed.is_empty() { name } else { trimmed }
}

/// The program a command word names, normalized the way the shell resolves it: directory and
/// `.exe` dropped, version suffix trimmed.
pub fn normalized_program(word: &str) -> String {
    trim_version(&program_name(word)).to_string()
}

fn entries() -> impl Iterator<Item = &'static str> {
    PROGRAMS
        .into_iter()
        .chain(LAUNCHERS.iter().map(|(name, ..)| *name))
}

/// Whether `argv` (program word first) is exactly an entry.
pub fn is_exactly_entry(argv: &[String]) -> bool {
    let Some((first, rest)) = argv.split_first() else {
        return false;
    };
    let program = normalized_program(first);
    entries().any(|entry| {
        let mut words = entry.split(' ');
        words.next() == Some(program.as_str()) && words.eq(rest.iter().map(String::as_str))
    })
}

/// Whether a ` *` rule over `argv` can match something an entry covers. Either direction
/// overlaps: `python -c *` runs inside `python`, and `cargo *` reaches `cargo run`.
pub fn wildcard_overlaps_entry(argv: &[String]) -> bool {
    let Some((first, rest)) = argv.split_first() else {
        return false;
    };
    let program = normalized_program(first);
    entries().any(|entry| {
        let mut words = entry.split(' ');
        if words.next() != Some(program.as_str()) {
            return false;
        }
        let entry_rest: Vec<&str> = words.collect();
        let n = entry_rest.len().min(rest.len());
        entry_rest[..n].iter().zip(rest).all(|(x, y)| *x == y)
    })
}

/// Whether a call to `program` (already stripped of directory and `.exe`) with `args` starts
/// with an entry: `cargo run --release` starts with `cargo run`, `shred` does not start with
/// `sh`. Launchers are the caller's to unwrap first.
pub fn starts_with_entry(program: &str, args: &[String]) -> bool {
    let program = trim_version(program);
    entries().any(|entry| {
        let mut words = entry.split(' ');
        words.next() == Some(program)
            && words
                .enumerate()
                .all(|(i, word)| args.get(i).is_some_and(|a| a == word))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn versions_and_paths_normalize() {
        for (word, want) in [
            ("python3.12", "python"),
            ("/usr/bin/python3.11", "python"),
            ("ruby3.2", "ruby"),
            ("python3", "python"),
            ("PowerShell.EXE", "powershell"),
            ("bash", "bash"),
        ] {
            assert_eq!(normalized_program(word), want);
        }
    }

    #[test]
    fn lookups() {
        assert!(is_exactly_entry(&argv(&["/bin/python3"])));
        assert!(!is_exactly_entry(&argv(&["python", "x.py"])));
        assert!(wildcard_overlaps_entry(&argv(&["timeout"])));
        assert!(!wildcard_overlaps_entry(&argv(&["cargo", "test"])));
        assert!(starts_with_entry("cargo", &argv(&["run", "--release"])));
        assert!(!starts_with_entry("shred", &[]));
    }
}
