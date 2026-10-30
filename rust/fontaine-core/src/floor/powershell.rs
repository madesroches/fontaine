//! The critical-path check for `powershell` commands. No PowerShell lexer is in the tree, so
//! this is a small token scan whose soundness rule mirrors the POSIX one: anything it cannot
//! positively read as safe while a deleter is mentioned is critical.

use std::path::{Path, PathBuf};

use fontaine_tools::scope::fold;

use super::{CriticalEnv, is_critical, is_critical_text, same_path, under};

/// Commands, aliases and executables that delete files, compared case-insensitively as the last
/// path component without `.exe`.
const DELETERS: [&str; 7] = ["remove-item", "ri", "rm", "rmdir", "rd", "del", "erase"];

/// Entry points that run a string or another interpreter, hiding the command from this scan.
const NESTED: [&str; 7] = [
    "pwsh",
    "powershell",
    "iex",
    "invoke-expression",
    "cmd",
    "invoke-command",
    "icm",
];

/// `-c`, `-Command`, `-EncodedCommand` and their short forms, as a nested interpreter takes them.
const NESTED_OPTIONS: [&str; 5] = ["c", "command", "enc", "ec", "encodedcommand"];

/// Parameters of `Remove-Item` that take a target.
const TARGET_PARAMS: [&str; 2] = ["path", "literalpath"];
/// Parameters that take a pattern, which is never a target.
const PATTERN_PARAMS: [&str; 3] = ["include", "exclude", "filter"];
/// Parameters that take no value.
const SWITCHES: [&str; 4] = ["recurse", "force", "whatif", "confirm"];

/// Characters that make a command more than one simple command, or that hide what a token is.
const STRUCTURE: [char; 10] = [';', '|', '&', '(', ')', '{', '}', '@', '\n', '\r'];

#[derive(Debug, PartialEq, Eq)]
enum Token {
    /// `plain_head` is the byte length of `text` before its first quote, so `-Path:'x'` can be
    /// told from a quoted operand that merely looks like an option.
    Word {
        text: String,
        quoted: bool,
        plain_head: usize,
    },
    Sep(char),
}

/// Why a PowerShell command is critical: it deletes the file system root, a top-level
/// directory, the home directory, a workspace root or a parent of one, or it cannot be shown
/// not to. Targets on non-FileSystem providers (`HKLM:`, `Env:`) are left to the guardian.
pub fn critical_powershell(
    command: &str,
    cwd: Option<&str>,
    env: &CriticalEnv<'_>,
) -> Option<String> {
    let base = match cwd {
        Some(cwd) => env.scope.resolved(cwd),
        None => env.scope.primary().to_path_buf(),
    };
    scan(command, &base, env)
}

fn scan(command: &str, base: &Path, env: &CriticalEnv<'_>) -> Option<String> {
    if calls_dotnet_delete(command) {
        return Some("a .NET delete cannot be resolved".to_string());
    }
    if calls_string_eval(command) {
        return Some("a string is run as code this check cannot read".to_string());
    }
    let tokens = tokenize(command);
    for (at, token) in tokens.iter().enumerate() {
        if is_call_operator(&tokens, at) {
            return Some("the command name cannot be resolved".to_string());
        }
        let Token::Word { text, quoted, .. } = token else {
            continue;
        };
        let name = command_name(text);
        if NESTED.contains(&name.as_str()) {
            return Some(format!("`{text}` runs a command this check cannot read"));
        }
        if !quoted && text.starts_with('-') && NESTED_OPTIONS.contains(&option_name(text).as_str())
        {
            return Some(format!("`{text}` runs a command this check cannot read"));
        }
        if *quoted && mentions_deleter(text) {
            return Some("a string mentions a deleter".to_string());
        }
        if !quoted && text.starts_with('-') && is_com_object_option(&option_name(text)) {
            return Some("a COM object can delete files this check cannot see".to_string());
        }
        if matches!(name.as_str(), "foreach-object" | "foreach" | "%")
            && names_delete_member(&tokens[at + 1..])
        {
            return Some("a piped member call deletes".to_string());
        }
    }
    if let Some(Token::Sep('&')) = tokens.last() {
        return Some("the command name cannot be resolved".to_string());
    }
    let mut mentions = false;
    for (at, token) in tokens.iter().enumerate() {
        let Token::Word { text, .. } = token else {
            continue;
        };
        if DELETERS.contains(&command_name(text).as_str()) {
            mentions = true;
            if let Some(found) = check_operands(&tokens[at + 1..], base, env) {
                return Some(found);
            }
        }
    }
    if mentions && command.contains(STRUCTURE) {
        return Some("a deleter inside a command that is not one simple command".to_string());
    }
    None
}

/// Whether `tokens[at]` is a call or dot-source operator whose command is not a literal word.
fn is_call_operator(tokens: &[Token], at: usize) -> bool {
    let operator = match &tokens[at] {
        Token::Sep('&') => true,
        Token::Word { text, quoted, .. } => text == "." && !quoted,
        Token::Sep(_) => false,
    };
    if !operator {
        return false;
    }
    match tokens.get(at + 1) {
        // `&&` is a chain; its second `&` is judged on its own.
        Some(Token::Sep('&')) => false,
        Some(Token::Sep(_)) => true,
        Some(Token::Word { text, .. }) => is_non_literal(text),
        None => false,
    }
}

/// The text of a command name or program path as a name: its last component, lowercase, without
/// `.exe`, so `Microsoft.PowerShell.Management\Remove-Item` and `C:\Tools\RM.EXE` read as the
/// command.
fn command_name(text: &str) -> String {
    let last = text.rsplit(['\\', '/']).next().unwrap_or(text);
    let lower = last.to_ascii_lowercase();
    lower.strip_suffix(".exe").unwrap_or(&lower).to_string()
}

fn option_name(text: &str) -> String {
    let body = text.trim_start_matches('-');
    body.split(':').next().unwrap_or(body).to_ascii_lowercase()
}

/// Whether the words of a quoted string include a deleter, as a script block built from a
/// string would run one.
fn mentions_deleter(text: &str) -> bool {
    tokenize(text).iter().any(|token| match token {
        Token::Word { text, .. } => DELETERS.contains(&command_name(text).as_str()),
        Token::Sep(_) => false,
    })
}

/// The raw text lowercased with quotes and backticks removed, so `::'Delete'(` and `` Del`ete ``
/// read as the member they name.
fn member_text(command: &str) -> String {
    command
        .chars()
        .filter(|c| *c != '`' && !is_single_quote(*c) && !is_double_quote(*c))
        .collect::<String>()
        .to_ascii_lowercase()
}

/// A `::Delete` or `.Delete…(` call, which `[IO.File]::Delete(...)`, `[IO.Directory]:: Delete(...)`,
/// `$directory.Delete()`, `$directory.Delete.Invoke()` and `$fso.DeleteFolder(...)` all are.
/// Scanned in the raw text, whatever the quoting.
fn calls_dotnet_delete(command: &str) -> bool {
    let lower = member_text(command);
    lower
        .match_indices("::")
        .any(|(at, found)| lower[at + found.len()..].trim_start().starts_with("delete"))
        || lower.match_indices(".delete").any(|(at, found)| {
            let rest = lower[at + found.len()..]
                .trim_start_matches(|c: char| c.is_alphanumeric() || c == '_')
                .trim_start();
            rest.starts_with('(') || rest.starts_with(".invoke")
        })
}

/// Calls that run a string as code, which hide what they run from this scan whatever the string.
fn calls_string_eval(command: &str) -> bool {
    let lower = member_text(command);
    lower.match_indices("::").any(|(at, found)| {
        lower[..at].trim_end().ends_with("scriptblock]")
            && lower[at + found.len()..].trim_start().starts_with("create")
    }) || [".invokescript", ".newscriptblock"].iter().any(|member| {
        lower
            .match_indices(member)
            .any(|(at, found)| lower[at + found.len()..].trim_start().starts_with('('))
    })
}

/// `-ComObject` or an abbreviation of it: `Scripting.FileSystemObject` deletes through methods
/// this scan does not follow.
fn is_com_object_option(name: &str) -> bool {
    name.len() >= 3 && "comobject".starts_with(name)
}

/// Whether the words after `ForEach-Object` name a `Delete…` member to call on each piped
/// object, positionally or as `-MemberName`.
fn names_delete_member(rest: &[Token]) -> bool {
    rest.iter()
        .take_while(|token| matches!(token, Token::Word { .. } | Token::Sep(',')))
        .any(|token| match token {
            Token::Word { text, .. } => {
                let lower = text.to_ascii_lowercase();
                let value = lower.split_once(':').map_or(lower.as_str(), |(_, v)| v);
                value.starts_with("delete")
            }
            Token::Sep(_) => false,
        })
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
            });
        }
        *self = Pending::default();
    }
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
            word.started = true;
            match chars.next() {
                // A backtick before a line break continues the line.
                Some('\n' | '\r') | None => {}
                Some(escaped) => word.text.push(escaped),
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

/// What a token's text cannot be resolved to without running PowerShell.
fn is_non_literal(text: &str) -> bool {
    text.contains(['$', '(', ')', '@', '{', '}', '%'])
}

fn check_operands(rest: &[Token], base: &Path, env: &CriticalEnv<'_>) -> Option<String> {
    let mut operands = 0;
    let mut at = 0;
    while at < rest.len() {
        let (text, quoted, plain_head) = match &rest[at] {
            Token::Word {
                text,
                quoted,
                plain_head,
            } => (text.as_str(), *quoted, *plain_head),
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
            operands += 1;
            if let Some(found) = judge(text, base, env) {
                return Some(found);
            }
            continue;
        }
        let (name, inline) = match text[1..].split_once(':') {
            Some((name, value)) => (name, Some(value)),
            None => (&text[1..], None),
        };
        let Some(known) = known_parameter(name) else {
            return Some(format!(
                "option `{text}` is not one this check can read past"
            ));
        };
        if SWITCHES.contains(&known) {
            continue;
        }
        let mut values: Vec<&str> = Vec::new();
        match inline {
            Some(value) if !value.is_empty() => values.push(value),
            _ => match rest.get(at) {
                Some(Token::Word { text, .. }) => {
                    values.push(text);
                    at += 1;
                }
                _ => return Some(format!("the value of `{text}` cannot be resolved")),
            },
        }
        // A comma list continues the value.
        while let (Some(Token::Sep(',')), Some(Token::Word { text, .. })) =
            (rest.get(at), rest.get(at + 1))
        {
            values.push(text);
            at += 2;
        }
        if TARGET_PARAMS.contains(&known) {
            for value in values {
                operands += 1;
                if let Some(found) = judge(value, base, env) {
                    return Some(found);
                }
            }
        }
    }
    if operands == 0 {
        return Some("a deleter with no target word; its targets are unknown".to_string());
    }
    None
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

/// Why `operand` is critical, if it is.
fn judge(operand: &str, base: &Path, env: &CriticalEnv<'_>) -> Option<String> {
    let critical = |why: &str| Some(format!("{why} `{operand}`"));
    let home = env.home.map(Path::to_string_lossy);
    let expanded = match expand_home(operand, home.as_deref()) {
        Expanded::Text(text) => text,
        Expanded::NoHome => return critical("the home directory is unknown for"),
    };
    let text = expanded.as_str();
    if is_non_literal(text) {
        return critical("target cannot be resolved:");
    }
    let text = strip_filesystem_provider(text);
    if let Some(letters) = provider_letters(text)
        && letters >= 2
    {
        return None;
    }
    if text.contains(['*', '?', '[']) {
        return wildcard_critical(text, base, env).then(|| format!("target `{operand}`"));
    }
    is_critical_text(text, base, env).then(|| format!("target `{operand}`"))
}

enum Expanded {
    Text(String),
    NoHome,
}

/// `$env:USERPROFILE`, `$HOME`, `~` and `%USERPROFILE%` at the start of `operand`, replaced
/// by the home directory.
fn expand_home(operand: &str, home: Option<&str>) -> Expanded {
    for prefix in ["$env:userprofile", "$home", "~", "%userprofile%"] {
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

/// A pattern is critical when it matches everything in the directory, or the directory it
/// searches is not strictly inside a workspace root: a pattern like `..\pro*` or `C:\Users\m*`
/// can match a critical place itself, whatever its literal prefix.
fn wildcard_critical(text: &str, base: &Path, env: &CriticalEnv<'_>) -> bool {
    if text.chars().all(|c| matches!(c, '*' | '?' | '.')) {
        return true;
    }
    let at = text.find(['*', '?', '[']).unwrap_or(text.len());
    let head = &text[..at];
    let (dir, explicit) = match head.rfind(['\\', '/']) {
        Some(end) => (&head[..=end], true),
        // `C:*` searches the current directory of a drive.
        None if head.contains(':') => return true,
        None => ("", false),
    };
    let joined = if dir.is_empty() {
        base.to_path_buf()
    } else if Path::new(dir).is_absolute() || dir.starts_with(['/', '\\']) {
        PathBuf::from(dir)
    } else {
        base.join(dir)
    };
    let lexical = fold(&joined);
    // A symlink to a critical place is as critical as the place.
    let resolved = std::fs::canonicalize(&lexical).ok();
    let roots = env.scope.roots();
    let inside = |path: &Path| {
        let strictly = roots
            .iter()
            .any(|root| under(path, root).is_some_and(|rest| rest.components().next().is_some()));
        let at_root = !explicit && roots.iter().any(|root| same_path(path, root));
        (strictly && !is_critical(path, env)) || at_root
    };
    !([Some(lexical), resolved]
        .into_iter()
        .flatten()
        .all(|path| inside(&path)))
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

    fn critical(command: &str) -> bool {
        let scope = Scope::new(root(), []);
        let env = CriticalEnv {
            scope: &scope,
            home: Some(Path::new(home())),
        };
        critical_powershell(command, None, &env).is_some()
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
    fn critical_targets() {
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
            "Remove-Item %USERPROFILE%".to_string(),
            format!("Remove-Item $env:USERPROFILE{sep}.."),
            format!("Remove-Item {top_dir}{sep}*"),
            format!("Remove-Item {root_dir}*"),
            "Remove-Item *".to_string(),
            "Remove-Item *.*".to_string(),
            "Remove-Item .".to_string(),
            "Remove-Item ..".to_string(),
            "Remove-Item".to_string(),
            "Remove-Item -Recurse".to_string(),
            "Remove-Item -Path".to_string(),
            "Get-Help rm".to_string(),
        ] {
            assert!(critical(&command), "{command}");
        }
    }

    #[test]
    fn unresolvable_targets_are_critical() {
        for command in [
            "rm -r (Get-Location).Path",
            "Remove-Item $x",
            "Remove-Item $env:TEMP\\x",
            "Remove-Item $PWD",
            "Remove-Item %TEMP%\\x",
            "Remove-Item @files",
            "Remove-Item -Path (Join-Path a b)",
            "Get-ChildItem | Remove-Item",
            "Get-ChildItem | % { Remove-Item $_ }",
            "Get-ChildItem | rm -Recurse",
            "Remove-Item a.txt -ErrorAction SilentlyContinue",
            "Remove-Item a.txt -Credential $c",
            "Remove-Item a.txt -Stream x",
            "Remove-Item a.txt -R",
            "Remove-Item a.txt -Unknown",
            "Remove-Item -Path",
        ] {
            assert!(critical(command), "{command}");
        }
    }

    #[test]
    fn a_deleter_anywhere_in_a_compound_command_is_critical() {
        for command in [
            "echo hi; rm a.txt",
            "echo hi;rm -r C:\\",
            "echo hi;rm a.txt",
            "(rm a.txt)",
            "$(rm a.txt)",
            "a|rm x",
            "{rm a.txt}",
            "if ($x) { rm a.txt }",
            "& { rm a.txt }",
            "rm a.txt && echo done",
            "rm a.txt || echo failed",
            "rm a.txt & echo background",
            "rm a.txt\nGet-Date",
            "rm a.txt\r\nGet-Date",
            "Remove-Item a.txt `\n -Force",
            "foreach ($f in 1..3) { Remove-Item a.txt }",
            "Remove-Item @('a.txt')",
        ] {
            assert!(critical(command), "{command}");
        }
    }

    #[test]
    fn nested_interpreters_and_string_evaluation_are_critical() {
        for command in [
            "powershell -Command \"rm -r C:\\\"",
            "pwsh -c 'rm -r C:\\'",
            "pwsh.exe -NoProfile -File x.ps1",
            "powershell.exe -enc AAAA",
            "pwsh -EncodedCommand AAAA",
            "iex \"rm C:\\\"",
            "Invoke-Expression $text",
            "Get-Date | iex",
            "cmd /c \"rd /s /q C:\\\"",
            "cmd.exe /c del x",
            "cmd /d /c rd x",
            "CMD /C rd x",
            "Invoke-Command -ScriptBlock { 1 }",
            "icm { 1 }",
            "Get-Date -c",
            "Get-Date -Command x",
            "Get-Date -EncodedCommand x",
            "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe -c 1",
            "[scriptblock]::Create(\"rm x\").Invoke()",
            "Start-Job -ScriptBlock ([scriptblock]::Create('Remove-Item x'))",
            "[scriptblock]::Create($s).Invoke()",
            "$ExecutionContext.InvokeCommand.InvokeScript($s)",
            "$ExecutionContext.InvokeCommand.NewScriptBlock($s)",
            "# don't\nrm C:\\",
            "<# it's #> Remove-Item C:\\",
        ] {
            assert!(critical(command), "{command}");
        }
    }

    #[test]
    fn dotnet_deletes_are_critical() {
        for command in [
            "[IO.Directory]::Delete('x')",
            "[System.IO.Directory]::Delete('x', $true)",
            "[IO.File]::Delete('x')",
            "[IO.Directory]:: Delete('x')",
            "[IO.Directory]::\tDELETE ('x')",
            "[Directory]::Delete('x')",
            "$obj.Delete()",
            "$obj.delete ()",
            "(Get-Item x).Delete()",
            "$fso.DeleteFile('x')",
            "[IO.Directory]::'Delete'('x')",
            "[IO.Directory]::\"Delete\"('x')",
            "$o.Delete.Invoke()",
            "$o.'Delete'()",
            "(New-Object -ComObject Scripting.FileSystemObject).DeleteFolder('x')",
            "New-Object -Com Scripting.FileSystemObject",
            "Get-Item $HOME | ForEach-Object Delete",
            "Get-Item x | % Delete",
            "Get-Item x | % -MemberName Delete -ArgumentList $true",
            "Get-Item x | foreach -Member:Delete",
        ] {
            assert!(critical(command), "{command}");
        }
    }

    #[test]
    fn a_non_literal_command_name_is_critical() {
        for command in [
            "& $tool C:\\",
            "& (Get-Command rm) x",
            ". $script",
            "& $x",
            "a &",
        ] {
            assert!(critical(command), "{command}");
        }
    }

    #[test]
    fn module_qualified_and_exe_names_are_deleters() {
        let (_, top_dir) = top();
        for command in [
            format!("Microsoft.PowerShell.Management\\Remove-Item {top_dir}"),
            format!("rm.exe {top_dir}"),
            format!("RM.EXE {top_dir}"),
            format!("C:\\Tools\\rm.exe {top_dir}"),
            format!("& 'rm' {top_dir}"),
            format!("& rm {top_dir}"),
            format!("“Remove-Item” {top_dir}"),
            format!("Remove-Item ‘{top_dir}’"),
            format!("Remove-Item “{top_dir}”"),
        ] {
            assert!(critical(&command), "{command}");
        }
    }

    #[test]
    fn the_filesystem_provider_prefix_is_read_through() {
        let (_, top_dir) = top();
        for command in [
            format!("Remove-Item FileSystem::{top_dir}"),
            format!("Remove-Item filesystem::{top_dir}"),
            format!("Remove-Item Microsoft.PowerShell.Core\\FileSystem::{top_dir}"),
        ] {
            assert!(critical(&command), "{command}");
        }
        assert!(!critical("Remove-Item FileSystem::build"));
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
            "Remove-Item build\\*.o",
            "Remove-Item *.tmp",
            "Remove-Item a.txt -Include *.tmp",
            "Remove-Item build -Include *.tmp -Recurse",
            "Remove-Item -Path build -Filter *.o",
            "Remove-Item -Exclude keep.txt build",
            "Remove-Item HKLM:\\Software\\X",
            "Remove-Item Env:FOO",
            "Remove-Item Cert:\\CurrentUser\\My\\ABC",
            "Remove-Item $env:USERPROFILE\\proj\\build",
            "Remove-Item ~/proj/build",
            "Get-ChildItem -Recurse",
            "Write-Host hello",
            "Get-Process | Format-Table",
            "Get-Process | Where-Object CPU",
            "Write-Host 'cleaning up'",
            "Get-Date",
            "$x = 1",
            "Set-Location sub",
            "git status",
            "git commit -m 'fix the build'",
            "git rm src/a.rs",
            "Format-Table",
            "[Console]::WriteLine('hi')",
        ] {
            assert!(!critical(command), "{command}");
        }
    }

    #[test]
    fn a_wildcard_that_can_match_a_critical_place_is_critical() {
        let sep = if cfg!(windows) { '\\' } else { '/' };
        let proj = root().display().to_string();
        let parent = Path::new(&proj).parent().unwrap().display().to_string();
        let name = Path::new(&proj)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let home = home();
        let home_stem = &home[..home.len() - 1];
        for command in [
            format!(
                "Remove-Item -Recurse -Force ..{sep}{}*",
                &name[..name.len() - 1]
            ),
            format!("Remove-Item -Recurse {home_stem}*"),
            format!("Remove-Item -Recurse {parent}{sep}{}*", &name[..2]),
            format!("Remove-Item {proj}{sep}*.o"),
            format!("Remove-Item {proj}{sep}..{sep}*"),
            format!("Remove-Item {proj}{sep}b*{sep}x"),
        ] {
            assert!(critical(&command), "{command}");
        }
        assert!(!critical(&format!("Remove-Item {proj}{sep}build{sep}*.o")));
    }

    #[test]
    fn deleter_names_inside_other_tokens_do_not_match() {
        for command in [
            "Get-Process",
            "Write-Host 'Format-Table'",
            "Get-Command remove-itemx",
            "echo trim",
            "Get-ChildItem prm",
            "Get-ChildItem .\\arm.txt",
        ] {
            assert!(!critical(command), "{command}");
        }
    }

    #[test]
    fn the_working_directory_is_the_base_of_relative_targets() {
        let dir = tempfile::tempdir().expect("temp dir");
        let base = std::fs::canonicalize(dir.path()).expect("canonical");
        std::fs::create_dir(base.join("sub")).expect("subdirectory");
        let scope = Scope::new(base, []);
        let env = CriticalEnv {
            scope: &scope,
            home: Some(Path::new(home())),
        };
        let (root_dir, _) = top();
        assert!(critical_powershell("Remove-Item sub", Some(root_dir), &env).is_some());
        assert!(critical_powershell("Remove-Item ..", Some("sub"), &env).is_some());
        assert!(critical_powershell("Remove-Item sub", None, &env).is_none());
        assert!(critical_powershell("Remove-Item x", Some("sub"), &env).is_none());
    }

    #[test]
    fn a_missing_home_makes_home_targets_critical() {
        let scope = Scope::new(root(), []);
        let env = CriticalEnv {
            scope: &scope,
            home: None,
        };
        assert!(critical_powershell("Remove-Item ~/x", None, &env).is_some());
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
            r"Remove-Item C:\Win*",
            r"Remove-Item C:\Users\me\*",
            r"Remove-Item FileSystem::C:\Windows",
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
        };
        assert!(critical_powershell("Remove-Item link", None, &env).is_some());
        assert!(critical_powershell("Remove-Item other", None, &env).is_none());
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
        };
        assert!(critical_powershell("Remove-Item link", None, &env).is_some());
        assert!(critical_powershell("Remove-Item other", None, &env).is_none());
    }
}
