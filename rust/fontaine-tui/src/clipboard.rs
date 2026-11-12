//! The system clipboard from the terminal UI: text is put on it, images are read from it.

use std::ffi::{OsStr, OsString};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use ratatui::crossterm::clipboard::CopyToClipboard;
use rig_core::message::ImageMediaType;
use rig_tools::process::{Launcher, kill_group};

/// How long a `tmux load-buffer` may take before it is given up on.
const TMUX_TIMEOUT: Duration = Duration::from_secs(2);

/// How long all the probes of one image read may take together. `powershell.exe` can need a few
/// seconds to start through a cold WSL interop, and the UI waits for the read.
const READ_DEADLINE: Duration = Duration::from_secs(15);

/// Prints the Windows clipboard's image as base64 PNG, and nothing when there is none.
/// `powershell.exe` (never `pwsh`) because `Get-Clipboard -Format Image` needs an STA apartment,
/// which only Windows PowerShell defaults to.
const WINDOWS_CLIPBOARD_SCRIPT: &str = "Add-Type -AssemblyName System.Drawing
$image = Get-Clipboard -Format Image
if ($null -eq $image) { exit 0 }
$stream = New-Object System.IO.MemoryStream
$image.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png)
[Console]::Out.Write([Convert]::ToBase64String($stream.ToArray()))";

/// Copies `text` the way Claude Code does: inside tmux, through `tmux load-buffer -w -` first,
/// then with an OSC 52 escape to the terminal. tmux's default `set-clipboard external` drops an
/// application's OSC 52 but forwards a buffer loaded with `-w` to the outer terminal.
pub fn copy(text: &str) -> std::io::Result<()> {
    if std::env::var_os("TMUX").is_some_and(|v| !v.is_empty()) {
        // Without `-w` the text still lands in a tmux buffer, so prefix-] pastes it.
        if !tmux_load_buffer(&["load-buffer", "-w", "-"], text) {
            tmux_load_buffer(&["load-buffer", "-"], text);
        }
    }
    ratatui::crossterm::execute!(std::io::stdout(), CopyToClipboard::to_clipboard_from(text))
}

/// Runs `tmux <args>` with `text` on its stdin; whether it exited successfully in time. A tmux
/// that is missing, fails or hangs only means the OSC 52 escape is the one route left.
fn tmux_load_buffer(args: &[&str], text: &str) -> bool {
    let Ok(mut child) = Command::new("tmux")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let written = child
        .stdin
        .take()
        .is_some_and(|mut stdin| stdin.write_all(text.as_bytes()).is_ok());
    let deadline = Instant::now() + TMUX_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return written && status.success(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

/// How one clipboard probe ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProbeOutcome {
    /// The program could not be started.
    Missing,
    /// It timed out, could not be read, or (powershell) exited non-zero.
    Failed,
    /// It ran, and what it printed is not an image.
    NoImage,
    /// It printed an image.
    Image,
}

impl ProbeOutcome {
    fn label(self) -> &'static str {
        match self {
            Self::Missing => "not installed",
            Self::Failed => "failed",
            Self::NoImage => "no image",
            Self::Image => "image",
        }
    }
}

/// One program tried against the clipboard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Probe {
    program: &'static str,
    outcome: ProbeOutcome,
}

/// The result of reading the clipboard image.
#[derive(Debug)]
pub(crate) struct ImageRead {
    /// Where the image was saved (sniffed type, `pasted-<millis>.<ext>`), if any.
    pub path: Option<PathBuf>,
    /// Every probe tried, in order.
    pub probes: Vec<Probe>,
}

/// The one-line notice for a read that found no image, naming each probe and what it found.
pub(crate) fn no_image_notice(read: &ImageRead) -> String {
    if read.probes.is_empty() {
        return "no image on the clipboard".to_string();
    }
    let tried: Vec<String> = read
        .probes
        .iter()
        .map(|probe| format!("{}: {}", probe.program, probe.outcome.label()))
        .collect();
    format!("no image on the clipboard (tried {})", tried.join(", "))
}

/// What a probe prints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Output {
    /// The image's own bytes.
    Raw,
    /// The image's bytes as base64 text.
    Base64,
}

struct Row {
    program: &'static str,
    args: Vec<OsString>,
    output: Output,
}

impl Row {
    fn raw(program: &'static str, args: &[&str]) -> Self {
        Self {
            program,
            args: args.iter().map(OsString::from).collect(),
            output: Output::Raw,
        }
    }

    fn windows_clipboard() -> Self {
        Self {
            program: "powershell.exe",
            args: fontaine_tools::powershell::launch_args(WINDOWS_CLIPBOARD_SCRIPT),
            output: Output::Base64,
        }
    }
}

/// Whether this is WSL: its interop variables are set, or the kernel says it is Microsoft's.
fn on_wsl(env: &dyn Fn(&str) -> Option<OsString>, osrelease: &str) -> bool {
    if env("WSL_INTEROP").is_some() || env("WSL_DISTRO_NAME").is_some() {
        return true;
    }
    let release = osrelease.to_ascii_lowercase();
    release.contains("microsoft") || release.contains("wsl")
}

/// The programs to try, in order.
fn probe_table(env: &dyn Fn(&str) -> Option<OsString>, osrelease: &str) -> Vec<Row> {
    let mut rows = Vec::new();
    if env("WAYLAND_DISPLAY").is_some() {
        rows.push(Row::raw(
            "wl-paste",
            &["--no-newline", "--type", "image/png"],
        ));
    }
    if env("DISPLAY").is_some() {
        rows.push(Row::raw(
            "xclip",
            &["-selection", "clipboard", "-target", "image/png", "-out"],
        ));
    }
    if cfg!(target_os = "macos") {
        rows.push(Row::raw("pngpaste", &["-"]));
    }
    // On native Windows the same row serves; WSL detection only matters elsewhere.
    if cfg!(target_os = "windows") || on_wsl(env, osrelease) {
        rows.push(Row::windows_clipboard());
    }
    rows
}

enum Run {
    Missing,
    Failed,
    /// Ran to completion but exited non-zero.
    NonZero,
    Output(Vec<u8>),
}

/// Runs `program` and collects its stdout as bytes, so an image survives intact. A probe that
/// is still running at `deadline` is killed with its process group.
fn run_probe(launcher: &Launcher, program: &OsStr, args: &[OsString], deadline: Instant) -> Run {
    if Instant::now() >= deadline {
        return Run::Failed;
    }
    let Ok(mut child) = launcher
        .command(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return Run::Missing;
    };
    let mut stdout = child.stdout.take().expect("stdout was configured as piped");
    let (sender, receiver) = mpsc::channel();
    // Detached, not joined: a process outside the killed group can hold the pipe open past the
    // deadline, and a join would wait for it.
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let read = stdout.read_to_end(&mut bytes).map(|_| bytes);
        let _ = sender.send(read);
    });
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                kill_group(&mut child);
                let _ = child.wait();
                return Run::Failed;
            }
        }
    };
    if !status.success() {
        return Run::NonZero;
    }
    match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(Ok(bytes)) => Run::Output(bytes),
        _ => Run::Failed,
    }
}

/// The image a probe printed, if it printed one.
fn image_of(output: Output, printed: &[u8]) -> Option<(Vec<u8>, ImageMediaType)> {
    let bytes = match output {
        Output::Raw => printed.to_vec(),
        Output::Base64 => {
            let text = printed.trim_ascii();
            if text.is_empty() {
                return None;
            }
            base64::engine::general_purpose::STANDARD
                .decode(text)
                .ok()?
        }
    };
    let kind = ImageMediaType::sniff(&bytes)?;
    Some((bytes, kind))
}

fn save(into: &Path, bytes: &[u8], kind: ImageMediaType) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(into)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    let path = into.join(format!("pasted-{stamp}.{}", kind.extension()));
    std::fs::write(&path, bytes)?;
    Ok(path)
}

/// Saves the clipboard's image in the directory `into`, named by the time and the image's type.
/// Blocking: call it off a thread that must not wait.
///
/// # Errors
///
/// [`Unsupported`](ErrorKind::Unsupported) when this machine has no clipboard program to try,
/// or the file could not be saved. No image is `Ok` with no path.
pub(crate) fn read_image(launcher: &Launcher, into: &Path) -> std::io::Result<ImageRead> {
    // An unreadable os-release only means WSL is not detected from the kernel.
    let osrelease = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    read_image_with(
        launcher,
        into,
        &|name| std::env::var_os(name),
        &osrelease,
        READ_DEADLINE,
    )
}

/// [`read_image`] with the environment, the kernel's os-release text and the time all probes
/// share passed in.
pub(crate) fn read_image_with(
    launcher: &Launcher,
    into: &Path,
    env: &dyn Fn(&str) -> Option<OsString>,
    osrelease: &str,
    deadline: Duration,
) -> std::io::Result<ImageRead> {
    let rows = probe_table(env, osrelease);
    if rows.is_empty() {
        return Err(std::io::Error::new(
            ErrorKind::Unsupported,
            "no clipboard here: it needs wl-paste and wl-copy, xclip, or pbpaste and pbcopy",
        ));
    }
    let deadline = Instant::now() + deadline;
    let mut probes = Vec::new();
    for row in rows {
        let outcome = match run_probe(launcher, OsStr::new(row.program), &row.args, deadline) {
            Run::Missing => ProbeOutcome::Missing,
            Run::Failed => ProbeOutcome::Failed,
            // wl-paste, xclip and pngpaste exit non-zero when the clipboard holds no image; the
            // powershell script exits 0 for that, so its non-zero exit is a real failure.
            Run::NonZero if row.output == Output::Raw => ProbeOutcome::NoImage,
            Run::NonZero => ProbeOutcome::Failed,
            Run::Output(printed) => match image_of(row.output, &printed) {
                None => ProbeOutcome::NoImage,
                Some((bytes, kind)) => {
                    let path = save(into, &bytes, kind)?;
                    probes.push(Probe {
                        program: row.program,
                        outcome: ProbeOutcome::Image,
                    });
                    return Ok(ImageRead {
                        path: Some(path),
                        probes,
                    });
                }
            },
        };
        probes.push(Probe {
            program: row.program,
            outcome,
        });
    }
    Ok(ImageRead { path: None, probes })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &'static [&'static str]) -> impl Fn(&str) -> Option<OsString> {
        move |name| pairs.contains(&name).then(|| OsString::from("1"))
    }

    fn programs(rows: &[Row]) -> Vec<&'static str> {
        rows.iter().map(|row| row.program).collect()
    }

    fn png() -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.extend_from_slice(b"not really a picture");
        bytes
    }

    fn b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_table_follows_the_environment() {
        let table = |vars: &'static [&'static str], osrelease: &str| {
            programs(&probe_table(&env_of(vars), osrelease))
        };
        assert_eq!(table(&["WAYLAND_DISPLAY"], ""), ["wl-paste"]);
        assert_eq!(table(&["DISPLAY"], ""), ["xclip"]);
        assert_eq!(
            table(&["WAYLAND_DISPLAY", "DISPLAY"], ""),
            ["wl-paste", "xclip"]
        );
        assert_eq!(
            table(&["WAYLAND_DISPLAY", "DISPLAY", "WSL_INTEROP"], ""),
            ["wl-paste", "xclip", "powershell.exe"]
        );
        assert_eq!(
            table(&["DISPLAY"], "5.15 microsoft-standard-WSL2"),
            ["xclip", "powershell.exe"]
        );
        assert_eq!(table(&["WSL_DISTRO_NAME"], ""), ["powershell.exe"]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_empty_table_is_unsupported() {
        let dir = tempfile::tempdir().unwrap();
        let error = read_image_with(
            &Launcher::default(),
            dir.path(),
            &env_of(&[]),
            "6.1.0-generic",
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Unsupported);
        assert_eq!(
            error.to_string(),
            "no clipboard here: it needs wl-paste and wl-copy, xclip, or pbpaste and pbcopy"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_adds_pngpaste() {
        assert_eq!(programs(&probe_table(&env_of(&[]), "")), ["pngpaste"]);
    }

    #[cfg(windows)]
    #[test]
    fn native_windows_adds_powershell_once() {
        assert_eq!(
            programs(&probe_table(&env_of(&["WSL_INTEROP"]), "microsoft")),
            ["powershell.exe"]
        );
    }

    #[test]
    fn on_wsl_truth_table() {
        let cases: [(&'static [&'static str], &str, bool); 8] = [
            (&[], "", false),
            (&[], "6.1.0-generic", false),
            (&["WSL_INTEROP"], "", true),
            (&["WSL_DISTRO_NAME"], "", true),
            (&[], "5.15.153.1-microsoft-standard-WSL2", true),
            (&[], "4.4.0-19041-Microsoft", true),
            (&[], "5.10.16.3-wsl", true),
            (&["DISPLAY"], "", false),
        ];
        for (vars, osrelease, expected) in cases {
            assert_eq!(
                on_wsl(&env_of(vars), osrelease),
                expected,
                "{vars:?} {osrelease:?}"
            );
        }
    }

    #[test]
    fn the_windows_probe_uses_windows_powershell_and_exits_quietly_without_an_image() {
        let row = Row::windows_clipboard();
        assert_eq!(row.program, "powershell.exe");
        assert_eq!(row.output, Output::Base64);
        assert!(row.args.iter().any(|arg| arg == "-EncodedCommand"));
        assert!(WINDOWS_CLIPBOARD_SCRIPT.contains("Get-Clipboard -Format Image"));
        assert!(WINDOWS_CLIPBOARD_SCRIPT.contains("if ($null -eq $image) { exit 0 }"));
        assert!(WINDOWS_CLIPBOARD_SCRIPT.contains("ToBase64String"));
    }

    #[test]
    fn base64_output_decodes_to_the_image() {
        let bytes = png();
        let (decoded, kind) = image_of(Output::Base64, b64(&bytes).as_bytes()).unwrap();
        assert_eq!(decoded, bytes);
        assert_eq!(kind.extension(), "png");
        let padded = format!("\r\n  {}\r\n", b64(&bytes));
        assert_eq!(
            image_of(Output::Base64, padded.as_bytes()).unwrap().0,
            bytes
        );
    }

    #[test]
    fn empty_bad_or_non_image_output_is_no_image() {
        assert!(image_of(Output::Base64, b"").is_none());
        assert!(image_of(Output::Base64, b" \r\n\t").is_none());
        assert!(image_of(Output::Base64, b"!!! not base64 !!!").is_none());
        assert!(image_of(Output::Base64, b64(b"plain text").as_bytes()).is_none());
        assert!(image_of(Output::Raw, b"plain text").is_none());
        assert!(image_of(Output::Raw, b"").is_none());
    }

    #[test]
    fn a_decoded_image_is_saved_under_its_type() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = png();
        let (decoded, kind) = image_of(Output::Base64, b64(&bytes).as_bytes()).unwrap();
        let path = save(&dir.path().join("scratch"), &decoded, kind).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with("pasted-") && name.ends_with(".png"),
            "{name}"
        );
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }

    fn read_of(outcomes: &[(&'static str, ProbeOutcome)]) -> ImageRead {
        ImageRead {
            path: None,
            probes: outcomes
                .iter()
                .map(|&(program, outcome)| Probe { program, outcome })
                .collect(),
        }
    }

    #[test]
    fn the_notice_names_every_probe_and_what_it_found() {
        use ProbeOutcome::{Failed, Missing, NoImage};
        assert_eq!(
            no_image_notice(&read_of(&[("wl-paste", Missing), ("xclip", Missing)])),
            "no image on the clipboard (tried wl-paste: not installed, xclip: not installed)"
        );
        assert_eq!(
            no_image_notice(&read_of(&[
                ("wl-paste", Missing),
                ("xclip", NoImage),
                ("powershell.exe", NoImage)
            ])),
            "no image on the clipboard (tried wl-paste: not installed, xclip: no image, powershell.exe: no image)"
        );
        assert_eq!(
            no_image_notice(&read_of(&[("xclip", NoImage), ("powershell.exe", Failed)])),
            "no image on the clipboard (tried xclip: no image, powershell.exe: failed)"
        );
    }

    #[cfg(unix)]
    fn write_executable(path: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(
            path,
            format!("#!/bin/sh\n[ -z \"$FONTAINE_TEST_PROBE\" ] || exit 0\n{body}\n"),
        )
        .unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        // A forked child of a parallel test can still hold the file open for writing, which
        // makes exec fail with ETXTBSY until it execs or exits.
        for _ in 0..500 {
            match Command::new(path).env("FONTAINE_TEST_PROBE", "1").status() {
                Ok(status) => {
                    assert!(
                        status.success(),
                        "probe of {} failed: {status}",
                        path.display()
                    );
                    return;
                }
                Err(e) if e.kind() == ErrorKind::ExecutableFileBusy => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => panic!("probe of {} failed: {e}", path.display()),
            }
        }
        panic!("{} stayed busy", path.display());
    }

    #[cfg(unix)]
    #[test]
    fn the_runner_keeps_non_utf8_bytes_intact() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("emit");
        write_executable(&fake, r"printf '\377\376\000\200PNG\r\n'");
        let run = run_probe(
            &Launcher::default(),
            fake.as_os_str(),
            &[],
            Instant::now() + Duration::from_secs(10),
        );
        let Run::Output(bytes) = run else {
            panic!("the probe did not succeed");
        };
        assert_eq!(bytes, b"\xff\xfe\x00\x80PNG\r\n");
    }

    #[cfg(unix)]
    #[test]
    fn the_runner_kills_a_hung_child_at_the_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("hang");
        write_executable(&fake, "exec sleep 30");
        let started = Instant::now();
        let run = run_probe(
            &Launcher::default(),
            fake.as_os_str(),
            &[],
            Instant::now() + Duration::from_millis(300),
        );
        assert!(matches!(run, Run::Failed));
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[cfg(unix)]
    #[test]
    fn the_runner_reports_a_missing_program_and_a_failing_one() {
        let dir = tempfile::tempdir().unwrap();
        let fail = dir.path().join("fail");
        write_executable(&fail, "exit 1");
        let deadline = Instant::now() + Duration::from_secs(10);
        let launcher = Launcher::default();
        assert!(matches!(
            run_probe(
                &launcher,
                dir.path().join("absent").as_os_str(),
                &[],
                deadline
            ),
            Run::Missing
        ));
        assert!(matches!(
            run_probe(&launcher, fail.as_os_str(), &[], deadline),
            Run::NonZero
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn wslg_without_an_image_target_falls_back_to_the_windows_clipboard() {
        let bin = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let bytes = png();
        write_executable(&bin.path().join("wl-paste"), "exit 1");
        write_executable(&bin.path().join("xclip"), "exit 1");
        write_executable(
            &bin.path().join("powershell.exe"),
            &format!("printf '%s' '{}'", b64(&bytes)),
        );
        let launcher = Launcher::default().with_env("PATH", bin.path());
        let read = read_image_with(
            &launcher,
            scratch.path(),
            &env_of(&["WAYLAND_DISPLAY", "DISPLAY", "WSL_INTEROP"]),
            "",
            Duration::from_secs(30),
        )
        .unwrap();
        let path = read.path.expect("the Windows clipboard's image is saved");
        assert_eq!(path.extension().unwrap(), "png");
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        assert_eq!(
            read.probes,
            [
                Probe {
                    program: "wl-paste",
                    outcome: ProbeOutcome::NoImage
                },
                Probe {
                    program: "xclip",
                    outcome: ProbeOutcome::NoImage
                },
                Probe {
                    program: "powershell.exe",
                    outcome: ProbeOutcome::Image
                },
            ]
        );
    }
}
