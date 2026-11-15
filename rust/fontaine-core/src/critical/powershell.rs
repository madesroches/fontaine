//! The `Remove-Item` counterpart of the shell walk: the literal operands of a deleter that name a
//! critical place. No PowerShell lexer is in the tree, so this is a small token scan; anything it
//! cannot read as a literal path adds nothing and is left to the guardian.

use std::path::Path;

use super::{CriticalEnv, critical_place};

/// Commands, aliases and executables that delete files, compared case-insensitively as the last
/// path component without `.exe`.
const DELETERS: [&str; 7] = ["remove-item", "ri", "rm", "rmdir", "rd", "del", "erase"];

/// Parameters of `Remove-Item` that take a target.
const TARGET_PARAMS: [&str; 2] = ["path", "literalpath"];
/// Parameters that take a pattern, which is never a target.
const PATTERN_PARAMS: [&str; 3] = ["include", "exclude", "filter"];
/// Parameters that take no value.
const SWITCHES: [&str; 4] = ["recurse", "force", "whatif", "confirm"];

/// Commands and aliases that change the working directory.
const DIRECTORY_CHANGERS: [&str; 8] = [
    "set-location",
    "sl",
    "cd",
    "chdir",
    "push-location",
    "pushd",
    "pop-location",
    "popd",
];

#[derive(Debug, PartialEq, Eq)]
enum Token {
    /// `plain_head` is the byte length of `text` before its first quote, so `-Path:'x'` can be
    /// told from a quoted operand that merely looks like an option.
    Word {
        text: String,
        quoted: bool,
        /// Some part is single-quoted, where PowerShell expands no variable.
        single: bool,
        plain_head: usize,
    },
    Sep(char),
}

/// The critical places the deleters of a PowerShell command name by literal operand, each as the
/// resolved path and its kind. Targets on non-FileSystem providers (`HKLM:`, `Env:`) are left to
/// the guardian.
pub fn critical_powershell(command: &str, base: &Path, env: &CriticalEnv<'_>) -> Vec<String> {
    let tokens = tokenize(command);
    let mut found = Vec::new();
    // A directory change earlier in the command leaves relative operands without a known base.
    let mut moved = false;
    for (at, token) in tokens.iter().enumerate() {
        let Token::Word { text, .. } = token else {
            continue;
        };
        // A deleter's name anywhere else is an argument (`echo rm C:\`).
        let at_command = at == 0 || matches!(tokens[at - 1], Token::Sep(c) if c != ',');
        if !at_command {
            continue;
        }
        let name = command_name(text);
        if DELETERS.contains(&name.as_str()) {
            collect_operands(&tokens[at + 1..], base, moved, env, &mut found);
        } else if DIRECTORY_CHANGERS.contains(&name.as_str()) {
            moved = true;
        }
    }
    found
}

/// The text of a command name or program path as a name: its last component, lowercase, without
/// `.exe`, so `Microsoft.PowerShell.Management\Remove-Item` and `C:\Tools\RM.EXE` read as the
/// command.
fn command_name(text: &str) -> String {
    let last = text.rsplit(['\\', '/']).next().unwrap_or(text);
    let lower = last.to_ascii_lowercase();
    lower.strip_suffix(".exe").unwrap_or(&lower).to_string()
}

/// What a token's text cannot be resolved to without running PowerShell.
fn is_non_literal(text: &str) -> bool {
    text.contains(['$', '(', ')', '@', '{', '}', '%'])
}

fn collect_operands(
    rest: &[Token],
    base: &Path,
    moved: bool,
    env: &CriticalEnv<'_>,
    found: &mut Vec<String>,
) {
    let mut at = 0;
    while at < rest.len() {
        let (text, quoted, single, plain_head) = match &rest[at] {
            Token::Word {
                text,
                quoted,
                single,
                plain_head,
            } => (text.as_str(), *quoted, *single, *plain_head),
            Token::Sep(',') => {
                at += 1;
                continue;
            }
            Token::Sep(_) => break,
        };
        at += 1;
        // `-Path:'x'` is an option whose value is quoted; `'-Path'` is an operand.
        let is_option = (!quoted || text[..plain_head].contains(':'))
            && text.len() > 1
            && text.starts_with('-')
            && text[1..].starts_with(|c: char| c.is_ascii_alphabetic());
        if !is_option {
            judge(text, !single, base, moved, env, found);
            continue;
        }
        let (name, inline) = match text[1..].split_once(':') {
            Some((name, value)) => (name, Some(value)),
            None => (&text[1..], None),
        };
        let Some(known) = known_parameter(name) else {
            continue;
        };
        if SWITCHES.contains(&known) {
            continue;
        }
        // Each value with whether its variables expand.
        let mut values: Vec<(&str, bool)> = Vec::new();
        match inline {
            Some(value) if !value.is_empty() => values.push((value, !single)),
            _ => {
                if let Some(Token::Word { text, single, .. }) = rest.get(at) {
                    values.push((text, !single));
                    at += 1;
                }
            }
        }
        // A comma list continues the value.
        while let (Some(Token::Sep(',')), Some(Token::Word { text, single, .. })) =
            (rest.get(at), rest.get(at + 1))
        {
            values.push((text, !single));
            at += 2;
        }
        if TARGET_PARAMS.contains(&known) {
            for (value, expands) in values {
                judge(value, expands, base, moved, env, found);
            }
        }
    }
}

/// The parameter `name` abbreviates, when it names exactly one of the known ones.
fn known_parameter(name: &str) -> Option<&'static str> {
    let name = name.to_ascii_lowercase();
    if name.len() < 2 {
        return None;
    }
    let mut hits = TARGET_PARAMS
        .iter()
        .chain(&PATTERN_PARAMS)
        .chain(&SWITCHES)
        .filter(|param| param.starts_with(name.as_str()));
    let first = hits.next()?;
    hits.next().is_none().then_some(*first)
}

/// Adds `operand` to `found` when it is a literal path that names a critical place. A wildcard,
/// an expansion and a home-relative path with no known home add nothing. `expands` is whether
/// PowerShell would expand variables in the word.
fn judge(
    operand: &str,
    expands: bool,
    base: &Path,
    moved: bool,
    env: &CriticalEnv<'_>,
    found: &mut Vec<String>,
) {
    let home = env.home.map(Path::to_string_lossy);
    let Expanded::Text(expanded) = expand_home(operand, expands, home.as_deref()) else {
        return;
    };
    let text = expanded.as_str();
    if is_non_literal(text) {
        return;
    }
    let text = strip_filesystem_provider(text);
    if let Some(letters) = provider_letters(text)
        && letters >= 2
    {
        return;
    }
    if text.contains(['*', '?', '[']) {
        return;
    }
    if moved && !(Path::new(text).is_absolute() || text.starts_with(['/', '\\'])) {
        return;
    }
    if let Some(place) = critical_place(text, base, env)
        && !found.contains(&place)
    {
        found.push(place);
    }
}

enum Expanded {
    Text(String),
    NoHome,
}

/// `$env:USERPROFILE`, `$HOME` and `~` at the start of `operand`, replaced by the home directory.
/// Variables are left alone when `variables` is false (a single-quoted word); `~` is the
/// provider's, so quotes do not stop it.
fn expand_home(operand: &str, variables: bool, home: Option<&str>) -> Expanded {
    for prefix in ["$env:userprofile", "$home", "~"] {
        if !variables && prefix.starts_with('$') {
            continue;
        }
        let Some(head) = operand.get(..prefix.len()) else {
            continue;
        };
        if !head.eq_ignore_ascii_case(prefix) {
            continue;
        }
        let rest = &operand[prefix.len()..];
        if !(rest.is_empty() || rest.starts_with(['\\', '/'])) {
            continue;
        }
        return match home {
            Some(home) => Expanded::Text(format!("{}{rest}", home.trim_end_matches(['\\', '/']))),
            None => Expanded::NoHome,
        };
    }
    Expanded::Text(operand.to_string())
}

/// The path after a `FileSystem::` provider prefix, with or without its module.
fn strip_filesystem_provider(text: &str) -> &str {
    for prefix in ["microsoft.powershell.core\\filesystem::", "filesystem::"] {
        if let Some(head) = text.get(..prefix.len())
            && head.eq_ignore_ascii_case(prefix)
        {
            return &text[prefix.len()..];
        }
    }
    text
}

/// The number of letters before the first `:`, when only letters precede it: one is a drive
/// (`C:`), two or more a provider (`HKLM:`, `Env:`).
fn provider_letters(text: &str) -> Option<usize> {
    let (head, _) = text.split_once(':')?;
    (!head.is_empty() && head.chars().all(|c| c.is_ascii_alphabetic())).then_some(head.len())
}

/// Splits on whitespace and `; | & ( ) { } ,`, honoring single and double quotes (PowerShell
/// treats typographic quotes as quotes too) and backtick escapes.
fn tokenize(text: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut word = Pending::default();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '#' && !word.started {
            while chars.next_if(|next| !matches!(next, '\n' | '\r')).is_some() {}
        } else if c == '<' && !word.started && chars.next_if_eq(&'#').is_some() {
            let mut previous = ' ';
            for inner in chars.by_ref() {
                if previous == '#' && inner == '>' {
                    break;
                }
                previous = inner;
            }
        } else if is_single_quote(c) {
            word.open_quote();
            word.single = true;
            while let Some(inner) = chars.next() {
                if !is_single_quote(inner) || chars.next_if(|next| is_single_quote(*next)).is_some()
                {
                    // A doubled quote is a literal one.
                    word.text.push(inner);
                } else {
                    break;
                }
            }
        } else if is_double_quote(c) {
            word.open_quote();
            while let Some(inner) = chars.next() {
                if inner == '`' {
                    if let Some(escaped) = chars.next() {
                        word.text.push(escaped);
                    }
                } else if !is_double_quote(inner)
                    || chars.next_if(|next| is_double_quote(*next)).is_some()
                {
                    // A doubled quote is a literal one.
                    word.text.push(inner);
                } else {
                    break;
                }
            }
        } else if c == '`' {
            match chars.next() {
                // A backtick before a line break continues the line; it must not leave an empty
                // word behind, which would hide a non-literal word after a call operator.
                Some('\n' | '\r') | None => word.flush(&mut tokens),
                Some(escaped) => {
                    word.started = true;
                    word.text.push(escaped);
                }
            }
        } else if c.is_whitespace() {
            word.flush(&mut tokens);
        } else if matches!(c, ';' | '|' | '&' | '(' | ')' | '{' | '}' | ',') {
            word.flush(&mut tokens);
            tokens.push(Token::Sep(c));
        } else {
            word.started = true;
            word.text.push(c);
        }
    }
    word.flush(&mut tokens);
    tokens
}

fn is_single_quote(c: char) -> bool {
    matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}')
}

fn is_double_quote(c: char) -> bool {
    matches!(c, '"' | '\u{201C}' | '\u{201D}' | '\u{201E}')
}

/// The word being read.
#[derive(Default)]
struct Pending {
    text: String,
    started: bool,
    quoted: bool,
    single: bool,
    plain_head: Option<usize>,
}

impl Pending {
    fn open_quote(&mut self) {
        self.started = true;
        self.quoted = true;
        self.plain_head.get_or_insert(self.text.len());
    }

    fn flush(&mut self, tokens: &mut Vec<Token>) {
        if self.started {
            let text = std::mem::take(&mut self.text);
            tokens.push(Token::Word {
                plain_head: self.plain_head.unwrap_or(text.len()),
                text,
                quoted: self.quoted,
                single: self.single,
            });
        }
        *self = Pending::default();
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use fontaine_tools::scope::Scope;

    use super::*;

    fn root() -> PathBuf {
        PathBuf::from(if cfg!(windows) {
            r"C:\Users\me\proj"
        } else {
            "/home/me/proj"
        })
    }

    fn home() -> &'static str {
        if cfg!(windows) {
            r"C:\Users\me"
        } else {
            "/home/me"
        }
    }

    fn found(command: &str) -> Vec<String> {
        let scope = Scope::new(root(), []);
        let env = CriticalEnv {
            scope: &scope,
            home: Some(Path::new(home())),
            scratch: None,
        };
        critical_powershell(command, &root(), &env)
    }

    fn critical(command: &str) -> bool {
        !found(command).is_empty()
    }

    /// A drive root and a top-level directory, written for the platform.
    fn top() -> (&'static str, &'static str) {
        if cfg!(windows) {
            (r"C:\", r"C:\Windows")
        } else {
            ("/", "/usr")
        }
    }

    #[test]
    fn critical_targets_are_recognized() {
        let (root_dir, top_dir) = top();
        let sep = if cfg!(windows) { '\\' } else { '/' };
        let proj = root().display().to_string();
        let parent = Path::new(&proj).parent().unwrap().display().to_string();
        for command in [
            format!("Remove-Item -Recurse -Force {root_dir}"),
            format!("ri {top_dir}"),
            format!("REMOVE-ITEM {top_dir}"),
            format!("Remove-Item -Path {top_dir}"),
            format!("Remove-Item -LiteralPath {top_dir}"),
            format!("Remove-Item -Pa {top_dir}"),
            format!("Remove-Item -Lit {top_dir}"),
            format!("Remove-Item -Path:{top_dir}"),
            format!("Remove-Item -Path:{top_dir} -Recurse"),
            format!("Remove-Item -Path:'{top_dir}' -Recurse"),
            format!("Remove-Item -LiteralPath:\"{top_dir}\" -Recurse"),
            format!("rm -r {proj}"),
            format!("rm -r {parent}"),
            format!("rm {proj}{sep}"),
            format!("Remove-Item a.txt,{top_dir}"),
            format!("Remove-Item 'a.txt', '{top_dir}'"),
            format!("Remove-Item -Path a.txt,{top_dir}"),
            "del $env:USERPROFILE".to_string(),
            "del $env:userprofile".to_string(),
            "Remove-Item ~ -Recurse".to_string(),
            "Remove-Item $HOME".to_string(),
            "Remove-Item \"$HOME\"".to_string(),
            format!("Remove-Item $env:USERPROFILE{sep}.."),
            "Remove-Item .".to_string(),
            "Remove-Item ..".to_string(),
            format!("echo hi; rm {top_dir}"),
            format!("rm a.txt && rm {top_dir}"),
            format!("Microsoft.PowerShell.Management\\Remove-Item {top_dir}"),
            format!("rm.exe {top_dir}"),
            format!("RM.EXE {top_dir}"),
            format!("C:\\Tools\\rm.exe {top_dir}"),
            format!("& 'rm' {top_dir}"),
            format!("& rm {top_dir}"),
            format!("“Remove-Item” {top_dir}"),
            format!("Remove-Item ‘{top_dir}’"),
            format!("Remove-Item “{top_dir}”"),
            format!("Remove-Item FileSystem::{top_dir}"),
            format!("Remove-Item filesystem::{top_dir}"),
            format!("Remove-Item Microsoft.PowerShell.Core\\FileSystem::{top_dir}"),
        ] {
            assert!(critical(&command), "{command}");
        }
    }

    #[test]
    fn a_target_names_its_resolved_path_and_kind() {
        let (_, top_dir) = top();
        let kind = found(&format!("Remove-Item {top_dir}"));
        assert_eq!(kind.len(), 1, "{kind:?}");
        assert!(kind[0].ends_with("(top-level directory)"), "{kind:?}");
        assert_eq!(
            found("Remove-Item ~"),
            [format!("{} (home directory)", home())]
        );
    }

    #[test]
    fn what_cannot_be_read_as_a_literal_target_adds_nothing() {
        for command in [
            "rm -r (Get-Location).Path",
            "Remove-Item $x",
            "Remove-Item $env:TEMP\\x",
            "Remove-Item $PWD",
            "Remove-Item %TEMP%\\x",
            "Remove-Item %USERPROFILE%",
            "Remove-Item -Recurse '$HOME'",
            "Remove-Item -Path:'$env:USERPROFILE'",
            "Remove-Item @files",
            "Remove-Item -Path (Join-Path a b)",
            "Get-ChildItem | Remove-Item",
            "Get-ChildItem | % { Remove-Item $_ }",
            "Remove-Item",
            "Remove-Item -Recurse",
            "Remove-Item -Path",
            "Remove-Item *",
            "Remove-Item *.*",
            "Remove-Item ../*",
            "Remove-Item ..\\pro*",
            "Remove-Item a.txt -Unknown",
            "& $tool C:\\",
            ". $script",
            "iex \"rm C:\\\"",
            "powershell -Command \"rm -r C:\\\"",
            "[IO.Directory]::Delete('x')",
            "Get-Help rm",
            "cd sub; Remove-Item ..",
            "Set-Location sub; Remove-Item ..",
            "Push-Location sub; rm -r .",
            "Get-ChildItem | rm -Recurse",
        ] {
            assert_eq!(found(command), Vec::<String>::new(), "{command}");
        }
    }

    #[test]
    fn a_directory_change_keeps_only_absolute_and_home_operands() {
        let (root_dir, _) = top();
        assert!(critical(&format!("cd sub; Remove-Item {root_dir}")));
        assert!(critical(&format!("Set-Location sub; rm {root_dir}")));
        assert!(critical("cd sub; Remove-Item ~"));
    }

    #[test]
    fn workspace_deletes_and_other_commands_are_not_critical() {
        for command in [
            "Remove-Item .\\build\\out.txt",
            "Remove-Item ./build/out.txt",
            "Remove-Item -Recurse target",
            "Remove-Item -Recurse -Force target",
            "Remove-Item -Path target -Recurse",
            "Remove-Item -LiteralPath 'target dir'",
            "Remove-Item target -WhatIf",
            "Remove-Item -Rec target",
            "Remove-Item -Recurse:$true target",
            "rm a.txt, b.txt",
            "del 'a b.txt'",
            "Remove-Item build\\*",
            "Remove-Item a.txt -Include *.tmp",
            "Remove-Item build -Include *.tmp -Recurse",
            "Remove-Item -Path build -Filter *.o",
            "Remove-Item -Exclude keep.txt build",
            "Remove-Item HKLM:\\Software\\X",
            "Remove-Item Env:FOO",
            "Remove-Item Cert:\\CurrentUser\\My\\ABC",
            "Remove-Item $env:USERPROFILE\\proj\\build",
            "Remove-Item ~/proj/build",
            "Remove-Item FileSystem::build",
            "Get-ChildItem -Recurse",
            "Write-Host 'cleaning up'",
            "Get-Process",
            "Write-Host 'Format-Table'",
            "Get-Command remove-itemx",
            "echo trim",
            "Get-ChildItem .\\arm.txt",
            "git rm src/a.rs",
            "git rm -r --cached C:\\",
            "echo rm C:\\",
        ] {
            assert_eq!(found(command), Vec::<String>::new(), "{command}");
        }
    }

    #[test]
    fn the_working_directory_is_the_base_of_relative_targets() {
        let dir = tempfile::tempdir().expect("temp dir");
        let base = std::fs::canonicalize(dir.path()).expect("canonical");
        std::fs::create_dir(base.join("sub")).expect("subdirectory");
        let scope = Scope::new(base.clone(), []);
        let env = CriticalEnv {
            scope: &scope,
            home: Some(Path::new(home())),
            scratch: None,
        };
        let (root_dir, _) = top();
        assert!(!critical_powershell("Remove-Item sub", Path::new(root_dir), &env).is_empty());
        assert!(!critical_powershell("Remove-Item ..", &base.join("sub"), &env).is_empty());
        assert!(critical_powershell("Remove-Item sub", &base, &env).is_empty());
        assert!(critical_powershell("Remove-Item x", &base.join("sub"), &env).is_empty());
    }

    #[test]
    fn the_scratch_directory_is_not_critical_but_its_parent_is() {
        let dir = tempfile::tempdir().expect("temp dir");
        let base = std::fs::canonicalize(dir.path()).expect("canonical");
        let scratch = base.join("scratch").join("session");
        std::fs::create_dir_all(&scratch).expect("scratch directory");
        let scope = Scope::new(base.join("proj"), [scratch.clone()]);
        let env = CriticalEnv {
            scope: &scope,
            home: Some(Path::new(home())),
            scratch: Some(&scratch),
        };
        let remove = |target: &Path| {
            critical_powershell(&format!("Remove-Item '{}'", target.display()), &base, &env)
        };
        assert!(remove(&scratch).is_empty());
        assert!(!remove(scratch.parent().expect("parent")).is_empty());
        assert!(!remove(&base.join("proj")).is_empty());
    }

    #[test]
    fn a_missing_home_adds_nothing() {
        let scope = Scope::new(root(), []);
        let env = CriticalEnv {
            scope: &scope,
            home: None,
            scratch: None,
        };
        assert!(critical_powershell("Remove-Item ~/x", &root(), &env).is_empty());
        assert!(critical_powershell("Remove-Item ~", &root(), &env).is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn windows_forms_of_critical_targets() {
        for command in [
            r"Remove-Item C:\",
            r"Remove-Item C:",
            r"Remove-Item c:\windows",
            r"Remove-Item C:\Users",
            r"Remove-Item C:\Users\me",
            r"Remove-Item C:\Users\ME\PROJ",
            r"Remove-Item C:/Users/me",
            r"Remove-Item \\server\share",
            r"Remove-Item .\..",
            r"Remove-Item C:\Users\me\proj\..",
        ] {
            assert!(critical(command), "{command}");
        }
        for command in [
            r"Remove-Item C:\Users\me\proj\build",
            r"Remove-Item C:\Users\me\proj\build\*",
            r"Remove-Item C:\Users\me\other",
            r"Remove-Item .\build",
        ] {
            assert!(!critical(command), "{command}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_link_to_a_critical_place_is_as_critical_as_the_place() {
        let dir = tempfile::tempdir().expect("temp dir");
        let base = std::fs::canonicalize(dir.path()).expect("canonical");
        let link = base.join("link");
        std::os::unix::fs::symlink("/usr", &link).expect("symlink");
        let scope = Scope::new(base.clone(), []);
        let env = CriticalEnv {
            scope: &scope,
            home: Some(Path::new("/home/me")),
            scratch: None,
        };
        assert!(!critical_powershell("Remove-Item link", &base, &env).is_empty());
        assert!(critical_powershell("Remove-Item other", &base, &env).is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn a_junction_to_a_critical_place_is_as_critical_as_the_place() {
        let dir = tempfile::tempdir().expect("temp dir");
        let base = std::fs::canonicalize(dir.path()).expect("canonical");
        let link = base.join("link");
        let made = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(r"C:\Windows")
            .output()
            .expect("cmd runs");
        assert!(made.status.success(), "{made:?}");
        let scope = Scope::new(base.clone(), []);
        let env = CriticalEnv {
            scope: &scope,
            home: Some(Path::new(r"C:\Users\me")),
            scratch: None,
        };
        assert!(!critical_powershell("Remove-Item link", &base, &env).is_empty());
        assert!(critical_powershell("Remove-Item other", &base, &env).is_empty());
    }
}
