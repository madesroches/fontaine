//! PowerShell support for the `powershell` tool on Windows: finding an interpreter, and handing
//! it a command so PowerShell parses exactly the text that was judged.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Overrides discovery with the path of a PowerShell executable.
pub const OVERRIDE_VAR: &str = "FONTAINE_POWERSHELL_PATH";

/// What runs before every command. Progress records would otherwise reach the output, and the
/// console would encode it in the OEM codepage instead of UTF-8.
const PREAMBLE: &str = "$ProgressPreference='SilentlyContinue'; \
                        [Console]::OutputEncoding=[Text.UTF8Encoding]::new($false); \
                        $OutputEncoding=[Console]::OutputEncoding";

/// The interpreter the `powershell` tool runs commands with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PowerShellSettings {
    pub path: PathBuf,
}

/// Where discovery looks, abstracted so it can be tested off Windows.
pub struct Probe<'a> {
    pub env: &'a dyn Fn(&str) -> Option<OsString>,
    pub is_file: &'a dyn Fn(&Path) -> bool,
}

/// The override when set (a path that is not a file means no PowerShell, not a fallback),
/// otherwise `pwsh.exe` then `powershell.exe` on PATH, otherwise Windows PowerShell where
/// Windows installs it. `None` when there is none: the tool does not exist then.
pub fn discover(probe: &Probe<'_>) -> Option<PowerShellSettings> {
    if let Some(value) = (probe.env)(OVERRIDE_VAR).filter(|v| !v.is_empty()) {
        let path = PathBuf::from(value);
        return (probe.is_file)(&path).then_some(PowerShellSettings { path });
    }
    let dirs: Vec<PathBuf> = (probe.env)("PATH")
        // A relative or empty entry means the current directory, which a command could plant.
        .map(|path| {
            std::env::split_paths(&path)
                .filter(|dir| dir.has_root())
                .collect()
        })
        .unwrap_or_default();
    let system = (probe.env)("SystemRoot").map(|root| {
        PathBuf::from(root)
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
    });
    ["pwsh.exe", "powershell.exe"]
        .into_iter()
        .flat_map(|name| dirs.iter().map(move |dir| dir.join(name)))
        .chain(system.map(|dir| dir.join("powershell.exe")))
        .find(|candidate| (probe.is_file)(candidate))
        .map(|path| PowerShellSettings { path })
}

/// The arguments that run `command` under the preamble. The script travels as UTF-16LE base64
/// so the Windows command line cannot re-parse it.
pub fn launch_args(command: &str) -> Vec<OsString> {
    let (usings, rest) = split_usings(command);
    let break_after = if usings.is_empty() || usings.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    let script = format!("{usings}{break_after}{PREAMBLE}\n{rest}");
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    [
        "-NoProfile",
        "-NonInteractive",
        "-OutputFormat",
        "Text",
        "-EncodedCommand",
    ]
    .into_iter()
    .map(OsString::from)
    .chain([OsString::from(base64(&bytes))])
    .collect()
}

/// The leading `using` statements of `command` (with the blank lines before them) and the rest.
/// PowerShell rejects a `using` that follows any other statement, so they must precede the preamble.
fn split_usings(command: &str) -> (&str, &str) {
    let mut end = 0;
    let mut kept = 0;
    let mut in_block_comment = false;
    for line in command.split_inclusive('\n') {
        if in_block_comment || line.trim_start().starts_with("<#") {
            end += line.len();
            in_block_comment = !line.contains("#>");
            continue;
        }
        let trimmed = line.trim_start().to_ascii_lowercase();
        let is_using = ["namespace", "module", "assembly"].iter().any(|kind| {
            trimmed.strip_prefix("using").is_some_and(|after| {
                after.starts_with(char::is_whitespace) && after.trim_start().starts_with(kind)
            })
        });
        end += line.len();
        if is_using {
            kept = end;
        } else if !line.trim().is_empty() && !line.trim_start().starts_with('#') {
            break;
        }
    }
    command.split_at(kept)
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let group = chunk.iter().enumerate().fold(0u32, |acc, (at, byte)| {
            acc | (u32::from(*byte) << (16 - 8 * at))
        });
        for at in 0..4 {
            if at <= chunk.len() {
                out.push(char::from(
                    ALPHABET[((group >> (18 - 6 * at)) & 63) as usize],
                ));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn unbase64(text: &str) -> Vec<u8> {
        let value = |c: char| -> u32 {
            match c {
                'A'..='Z' => c as u32 - 'A' as u32,
                'a'..='z' => c as u32 - 'a' as u32 + 26,
                '0'..='9' => c as u32 - '0' as u32 + 52,
                '+' => 62,
                '/' => 63,
                _ => panic!("not base64: {c}"),
            }
        };
        let mut out = Vec::new();
        for chunk in text.as_bytes().chunks(4) {
            let letters: Vec<char> = chunk
                .iter()
                .map(|b| char::from(*b))
                .filter(|c| *c != '=')
                .collect();
            let group = letters
                .iter()
                .enumerate()
                .fold(0u32, |acc, (at, c)| acc | (value(*c) << (18 - 6 * at)));
            for at in 0..letters.len() - 1 {
                out.push((group >> (16 - 8 * at)) as u8);
            }
        }
        out
    }

    fn decoded_script(command: &str) -> String {
        let args = launch_args(command);
        assert_eq!(args[args.len() - 2], "-EncodedCommand");
        let bytes = unbase64(
            args.last()
                .and_then(|a| a.to_str())
                .expect("utf-8 argument"),
        );
        let units: Vec<u16> = bytes
            .chunks(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        String::from_utf16(&units).expect("valid UTF-16")
    }

    #[test]
    fn base64_matches_the_standard_vectors() {
        for (plain, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(plain.as_bytes()), encoded);
        }
    }

    #[test]
    fn the_command_reaches_powershell_exactly_as_written() {
        for command in [
            r#"Write-Host "a 'b' \"c\"""#,
            r"Get-ChildItem C:\Users\me\\share",
            "Write-Host é€ü 😀",
            "line one\r\nline two\nline three",
            "",
        ] {
            let script = decoded_script(command);
            assert_eq!(script, format!("{PREAMBLE}\n{command}"));
            assert!(script.ends_with(command));
        }
    }

    #[test]
    fn leading_using_statements_move_ahead_of_the_preamble() {
        let script =
            decoded_script("using namespace System.IO\n\nUSING module Foo\r\n[File]::Exists('x')");
        assert_eq!(
            script,
            format!(
                "using namespace System.IO\n\nUSING module Foo\r\n{PREAMBLE}\n[File]::Exists('x')"
            )
        );
        let script = decoded_script("using namespace System.IO");
        assert_eq!(script, format!("using namespace System.IO\n{PREAMBLE}\n"));
        // Only a leading statement is moved; text that merely mentions `using` stays put.
        for command in [
            "Write-Host 1\nusing namespace System.IO",
            "Write-Host using namespace",
            "usingnamespace x",
        ] {
            assert_eq!(decoded_script(command), format!("{PREAMBLE}\n{command}"));
        }
    }

    #[test]
    fn the_arguments_skip_profiles_prompts_and_clixml() {
        let args = launch_args("exit 3");
        let words: Vec<&str> = args.iter().filter_map(|a| a.to_str()).take(4).collect();
        assert_eq!(
            words,
            ["-NoProfile", "-NonInteractive", "-OutputFormat", "Text"]
        );
    }

    fn found(files: &[&str], env: &[(&str, &str)]) -> Option<PathBuf> {
        let files: HashSet<PathBuf> = files.iter().map(PathBuf::from).collect();
        let env: Vec<(String, OsString)> = env
            .iter()
            .map(|(k, v)| ((*k).to_string(), OsString::from(v)))
            .collect();
        let lookup = |name: &str| env.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
        let is_file = |path: &Path| files.contains(path);
        discover(&Probe {
            env: &lookup,
            is_file: &is_file,
        })
        .map(|s| s.path)
    }

    fn path_of(dirs: &[&Path]) -> OsString {
        std::env::join_paths(dirs).expect("joinable")
    }

    #[test]
    fn pwsh_on_path_wins_over_windows_powershell() {
        let (a, b) = (PathBuf::from("/a"), PathBuf::from("/b"));
        let path = path_of(&[&a, &b]);
        let path = path.to_str().expect("utf-8");
        let all = [
            a.join("powershell.exe").to_str().unwrap().to_string(),
            b.join("pwsh.exe").to_str().unwrap().to_string(),
        ];
        let files: Vec<&str> = all.iter().map(String::as_str).collect();
        assert_eq!(found(&files, &[("PATH", path)]), Some(b.join("pwsh.exe")));
    }

    #[test]
    fn windows_powershell_is_found_under_system_root_off_path() {
        let exe = PathBuf::from("/win")
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe");
        let file = exe.to_str().unwrap().to_string();
        assert_eq!(
            found(&[file.as_str()], &[("PATH", ""), ("SystemRoot", "/win")]),
            Some(exe)
        );
    }

    #[test]
    fn the_override_wins_and_a_bad_one_means_no_powershell() {
        let pwsh = PathBuf::from("/a").join("pwsh.exe");
        let pwsh_text = pwsh.to_str().unwrap().to_string();
        let own = PathBuf::from("/mine").join("pwsh.exe");
        let own_text = own.to_str().unwrap().to_string();
        let path = path_of(&[Path::new("/a")]);
        let path = path.to_str().unwrap();
        let files = [pwsh_text.as_str(), own_text.as_str()];
        assert_eq!(
            found(&files, &[("PATH", path), (OVERRIDE_VAR, own_text.as_str())]),
            Some(own)
        );
        assert_eq!(
            found(
                &files,
                &[("PATH", path), (OVERRIDE_VAR, "/nowhere/pwsh.exe")]
            ),
            None
        );
    }

    #[test]
    fn relative_and_empty_path_entries_are_skipped() {
        let a = PathBuf::from("/a");
        let path = std::env::join_paths([Path::new("."), Path::new(""), Path::new("rel"), &a])
            .expect("joinable");
        let path = path.to_str().expect("utf-8");
        let planted = [
            Path::new(".").join("pwsh.exe"),
            Path::new("rel").join("pwsh.exe"),
            PathBuf::from("pwsh.exe"),
        ];
        let theirs = a.join("powershell.exe");
        let mut files: Vec<&str> = planted.iter().map(|p| p.to_str().unwrap()).collect();
        files.push(theirs.to_str().unwrap());
        assert_eq!(found(&files, &[("PATH", path)]), Some(theirs));
    }

    #[test]
    fn a_comment_before_the_using_block_does_not_hide_it() {
        let script =
            decoded_script("#requires -Version 5\n# note\nusing namespace System.IO\nGet-Date");
        assert_eq!(
            script,
            format!(
                "#requires -Version 5\n# note\nusing namespace System.IO\n{PREAMBLE}\nGet-Date"
            )
        );
    }

    #[test]
    fn a_block_comment_before_the_using_block_does_not_hide_it() {
        let script = decoded_script("<#\n  notes\n#>\nusing namespace System.IO\nGet-Date");
        assert_eq!(
            script,
            format!("<#\n  notes\n#>\nusing namespace System.IO\n{PREAMBLE}\nGet-Date")
        );
    }

    #[test]
    fn nothing_found_is_none() {
        assert_eq!(found(&[], &[("PATH", "/x")]), None);
    }
}
