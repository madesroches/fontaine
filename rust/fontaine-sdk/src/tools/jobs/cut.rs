//! Cutting a job's output to fit a tool result, and the status lines that end it. rig keeps its
//! own cutter and status texts private, so these reproduce them: the same first lines, error and
//! failure lines from between, and last lines, and the same wording, so a command that ends in
//! time reads as it did before jobs existed. `split_status` keeps recognising the lines.

use std::borrow::Cow;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use rig_core::redact;
use rig_core::tool::{ExitStatus, KeptEnd, OutputCut};
use rig_tools::process::STOP_GRACE;
use rig_tools::{OutputLimit, Spill, clip};

use crate::tools::error_chain::chain;

/// The most bytes of cut shell output a session saves to files. Scratch is typically memory-backed
/// and nothing else bounds the number of files, so past this the output is only cut.
pub const SPILL_BUDGET: u64 = 128 * 1024 * 1024;

/// Where a session keeps the shell output it cut, and how much it may still keep.
pub struct ShellSpill {
    dir: Spill,
    budget: u64,
    written: AtomicU64,
    warned: AtomicBool,
    warn: Arc<dyn Fn(&str) + Send + Sync>,
}

impl std::fmt::Debug for ShellSpill {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellSpill")
            .field("dir", &self.dir)
            .field("budget", &self.budget)
            .field("written", &self.written)
            .finish()
    }
}

impl ShellSpill {
    pub fn new(dir: Spill, warn: Arc<dyn Fn(&str) + Send + Sync>) -> Self {
        Self::with_budget(dir, SPILL_BUDGET, warn)
    }

    pub fn with_budget(dir: Spill, budget: u64, warn: Arc<dyn Fn(&str) + Send + Sync>) -> Self {
        ShellSpill {
            dir,
            budget,
            written: AtomicU64::new(0),
            warned: AtomicBool::new(false),
            warn,
        }
    }

    /// Reports that output could not be saved, once per session: the next calls fall back to the
    /// plain cut quietly.
    pub fn report(&self, error: &io::Error) {
        if !self.warned.swap(true, Ordering::SeqCst) {
            (self.warn)(&format!(
                "shell output is not saved to a file; long output is only cut: {}",
                chain(error)
            ));
        }
    }

    fn save(&self, lines: &[(u64, String)]) -> io::Result<PathBuf> {
        let bytes: u64 = lines.iter().map(|(_, line)| line.len() as u64).sum();
        let before = self.written.fetch_add(bytes, Ordering::SeqCst);
        if before.saturating_add(bytes) > self.budget {
            self.written.fetch_sub(bytes, Ordering::SeqCst);
            return Err(io::Error::other(format!(
                "the session's {} MiB limit on saved shell output is used up",
                self.budget / 1024 / 1024
            )));
        }
        let saved = self.write(lines);
        if saved.is_err() {
            self.written.fetch_sub(bytes, Ordering::SeqCst);
        }
        saved
    }

    fn write(&self, lines: &[(u64, String)]) -> io::Result<PathBuf> {
        let (path, mut file) = self.dir.create("shell")?;
        let written = lines
            .iter()
            .try_for_each(|(_, line)| file.write_all(line.as_bytes()))
            .and_then(|()| file.flush());
        match written {
            Ok(()) => Ok(path),
            Err(error) => {
                // A half-written file would read as the whole output.
                let _ = std::fs::remove_file(&path);
                Err(error)
            }
        }
    }
}

/// How many lines after a line that matters are kept with it.
const CONTEXT: usize = 3;
/// The longest a line kept from the cut middle is shown.
const MAX_PICKED: usize = 300;
/// The longest line a stored line may be; a longer one is broken into lines of this length.
pub(super) const MAX_HELD: usize = 1024 * 1024;

/// What a status line adds when the command printed nothing at all.
const SILENT: &str = ". It printed nothing: a program whose output is not a terminal may hold \
                      it back until it ends, so make it print as it goes (such as \
                      `python3 -u` or `stdbuf -oL`) to see how far it got";

fn matters(line: &str) -> bool {
    let start = line.trim_start();
    start.starts_with("error")
        || start.starts_with("Error")
        || [
            "FAILED",
            "FAIL:",
            "failed",
            "panicked",
            "Traceback",
            "Error:",
        ]
        .iter()
        .any(|word| line.contains(word))
}

/// `line` without the terminal escape sequences in it, such as colours or a link's markup: the
/// text between them stays.
pub(super) fn plain(line: &str) -> Cow<'_, str> {
    if !line.contains('\x1b') {
        return Cow::Borrowed(line);
    }
    let mut text = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            text.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                let _ = chars.by_ref().find(|c| ('\x40'..='\x7e').contains(c));
            }
            Some(']' | 'P' | 'X' | '^' | '_') => {
                while let Some(c) = chars.next() {
                    if c == '\x07' || (c == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            Some(' '..='/') => {
                let _ = chars.by_ref().find(|c| !(' '..='/').contains(c));
            }
            _ => {}
        }
    }
    Cow::Owned(text)
}

/// Strips escape sequences and redacts secrets from one line. A private key block spans lines:
/// `in_key` carries whether the previous line was inside one, and the lines through its end line
/// come back empty.
pub(super) fn sanitize(piece: &str, in_key: &mut bool) -> String {
    let piece = plain(piece);
    if *in_key {
        *in_key = !piece.contains("-----END ");
        return String::new();
    }
    let mut redacted = redact::text(&piece);
    if let Cow::Owned(line) = &mut redacted {
        *in_key = redact::find(&piece).any(|secret| {
            secret.kind == redact::Kind::PrivateKey
                && piece
                    .get(secret.range)
                    .is_some_and(|key| !key.contains("-----END "))
        });
        // A key redacted to the line's end took its newline.
        if piece.ends_with('\n') && !line.ends_with('\n') {
            line.push('\n');
        }
    }
    redacted.into_owned()
}

/// `lines`, numbered, cut to `limit`: when they do not fit, the first lines (half of the limit),
/// the lines that matter from the middle with a few after each (a quarter) and the last lines
/// (the rest). `earlier` lines were dropped before the first of them and are counted as cut.
///
/// When they do not fit and `spill` is set, `lines` are first saved to a file and the text starts
/// with a line naming it: the summarizer keeps only the start of a result, so a trailing pointer
/// would never reach it. A failed save leaves the plain cut and comes back as the error.
pub fn cut(
    lines: &[(u64, String)],
    earlier: u64,
    limit: OutputLimit,
    spill: Option<&ShellSpill>,
) -> (String, Option<OutputCut>, Option<io::Error>) {
    let total_bytes: usize = lines.iter().map(|(_, line)| line.len()).sum();
    let mut text = String::new();
    if lines.len() <= limit.lines && total_bytes <= limit.bytes {
        if earlier > 0 {
            text.push_str(&format!("[{earlier} earlier lines dropped]\n"));
        }
        for (_, line) in lines {
            text.push_str(line);
        }
        return (text, None, None);
    }

    let mut saved_at = None;
    let mut failed = None;
    if let Some(spill) = spill {
        match spill.save(lines) {
            Ok(path) => {
                let dropped = if earlier > 0 {
                    format!(" (the first {earlier} lines were dropped before this file)")
                } else {
                    String::new()
                };
                text.push_str(&format!(
                    "[output cut; saved for this session: {}{dropped}. Use read with \
                     offset/limit, or grep that file.]\n",
                    path.display()
                ));
                saved_at = Some(path);
            }
            Err(error) => failed = Some(error),
        }
    }
    if earlier > 0 {
        text.push_str(&format!("[{earlier} earlier lines dropped]\n"));
    }

    let mut head_end = 0;
    let mut head_bytes = 0;
    while head_end < lines.len() && head_end < limit.lines / 2 {
        let len = lines[head_end].1.len();
        if head_bytes + len > limit.bytes / 2 {
            break;
        }
        head_bytes += len;
        head_end += 1;
    }
    let pick_lines = limit.lines / 4;
    let pick_bytes = limit.bytes / 4;
    let tail_lines = limit.lines.saturating_sub(head_end + pick_lines);
    let tail_budget = limit.bytes.saturating_sub(head_bytes + pick_bytes);
    let mut tail_start = lines.len();
    let mut tail_bytes = 0;
    while tail_start > head_end && lines.len() - tail_start < tail_lines {
        let len = lines[tail_start - 1].1.len();
        if tail_bytes + len > tail_budget {
            break;
        }
        tail_bytes += len;
        tail_start -= 1;
    }

    let mut picked: Vec<(usize, bool)> = Vec::new();
    let mut context = 0;
    for (at, (_, line)) in lines
        .iter()
        .enumerate()
        .skip(head_end)
        .take(tail_start - head_end)
    {
        let starts = matters(line);
        if starts {
            context = CONTEXT + 1;
        }
        if context > 0 {
            context -= 1;
            picked.push((at, starts));
        }
    }
    let size = |at: usize| clip(lines[at].1.trim_end_matches('\n'), MAX_PICKED).len() + 1;
    let all: Vec<usize> = picked.iter().map(|(at, _)| *at).collect();
    let fits =
        all.len() <= pick_lines && all.iter().map(|at| size(*at)).sum::<usize>() <= pick_bytes;
    // Over the room, the first picked lines and the last ones stay.
    let (keep, missed) = if fits {
        (all, false)
    } else {
        let mut front: Vec<usize> = Vec::new();
        let (mut room, mut bytes) = (pick_lines / 2, pick_bytes / 2);
        for at in &all {
            if room == 0 || size(*at) > bytes {
                break;
            }
            room -= 1;
            bytes -= size(*at);
            front.push(*at);
        }
        let mut back: Vec<usize> = Vec::new();
        let (mut room, mut bytes) = (pick_lines - pick_lines / 2, pick_bytes / 2);
        for at in all.iter().rev() {
            if room == 0 || size(*at) > bytes || front.contains(at) {
                break;
            }
            room -= 1;
            bytes -= size(*at);
            back.push(*at);
        }
        back.reverse();
        front.extend(back);
        (front, true)
    };

    let errors = if missed {
        "the first and last lines with errors or failures"
    } else {
        "lines with errors or failures"
    };
    let kept_lines = head_end + keep.len() + (lines.len() - tail_start);
    let total = lines.len() as u64 + earlier;
    let cut_lines = total - kept_lines as u64;
    text.push_str(&format!(
        "[{cut_lines} of {total} lines cut, but for {errors}]\n"
    ));
    let mut next = lines.first().map_or(0, |(seq, _)| *seq);
    let kept = (0..head_end)
        .chain(keep.iter().copied())
        .chain(tail_start..lines.len());
    for at in kept {
        let (seq, line) = &lines[at];
        if *seq > next {
            text.push_str(&format!(
                "[… {} lines cut (lines {}-{seq}) …]\n",
                seq - next,
                next + 1
            ));
        }
        if keep.contains(&at) {
            text.push_str(clip(line.trim_end_matches('\n'), MAX_PICKED));
            text.push('\n');
        } else {
            text.push_str(line);
        }
        next = seq + 1;
    }
    let fact = OutputCut {
        kept: KeptEnd::Ends,
        kept_lines,
        total_lines: total as usize,
        spill: saved_at.map(|path| path.to_string_lossy().into_owned()),
    };
    (text, Some(fact), failed)
}

/// What this tool did to stop a command, as the status lines say it.
pub fn stopped(forced: bool, remote: Option<&str>) -> String {
    let grace = STOP_GRACE.as_secs();
    let how = if !cfg!(unix) {
        "it was killed".to_owned()
    } else if forced {
        format!("it got SIGTERM, then SIGKILL {grace}s later as it still ran")
    } else {
        "it got SIGTERM and ended".to_owned()
    };
    match remote {
        Some(host) => format!(
            "{how}. That was the transport on this machine; whether the command on {host} \
             stopped too is the transport's doing, so check there before running it again"
        ),
        None => how,
    }
}

/// The status line of a command that ended by a signal this tool did not send.
fn killed(status: std::process::ExitStatus) -> String {
    #[cfg(unix)]
    if let Some(signal) = std::os::unix::process::ExitStatusExt::signal(&status) {
        let (name, why) = sent(signal);
        return format!(
            "[killed by {name} (signal {signal}), which this tool did not send: {why}. It was \
             the process this call started here, `sh` or the last program it ran; when that \
             program was a client of another machine, such as a remote shell, the command \
             there may still be running or may have finished, so check there before running \
             it again]"
        );
    }
    #[cfg(not(unix))]
    let _ = status;
    "[killed by a signal]".to_owned()
}

#[cfg(unix)]
fn sent(signal: i32) -> (&'static str, &'static str) {
    match signal {
        libc::SIGHUP => ("SIGHUP", "its terminal or session went away"),
        libc::SIGINT => ("SIGINT", "something on this machine interrupted it"),
        libc::SIGQUIT => ("SIGQUIT", "something on this machine asked it to quit"),
        libc::SIGILL => ("SIGILL", "it crashed"),
        libc::SIGTRAP => ("SIGTRAP", "it crashed, or a debugger stopped it"),
        libc::SIGABRT => ("SIGABRT", "it aborted itself, as on a failed assertion"),
        libc::SIGBUS => ("SIGBUS", "it crashed"),
        libc::SIGFPE => ("SIGFPE", "it crashed on an arithmetic error"),
        libc::SIGKILL => (
            "SIGKILL",
            "something on this machine killed it, such as the out-of-memory killer or a `kill`",
        ),
        libc::SIGSEGV => ("SIGSEGV", "it crashed"),
        libc::SIGPIPE => ("SIGPIPE", "it wrote to a pipe that nothing read any more"),
        libc::SIGTERM => ("SIGTERM", "something on this machine asked it to stop"),
        _ => ("a signal", "something on this machine sent it"),
    }
}

/// The status line of a command that exited on its own, and what it means as a fact.
pub fn exit_line(status: std::process::ExitStatus) -> (String, ExitStatus) {
    match status.code() {
        Some(code) => (format!("[exit code {code}]"), ExitStatus::Exited(code)),
        None => (killed(status), ExitStatus::Signaled),
    }
}

pub fn timed_out_line(
    secs: u64,
    max_timeout: u64,
    stopped: &str,
    silent: bool,
) -> (String, ExitStatus) {
    let mut line = format!("[timed out after {secs}s and stopped: {stopped}");
    if silent {
        line.push_str(SILENT);
    }
    if secs < max_timeout {
        line.push_str(&format!(
            ". Pass timeout_secs up to {max_timeout} to wait longer"
        ));
    }
    line.push(']');
    (line, ExitStatus::TimedOut)
}

pub fn quiet_line(secs: u64, stopped: &str, silent: bool) -> (String, ExitStatus) {
    let mut line =
        format!("[timed out after {secs}s of printing nothing, so it was stopped: {stopped}");
    if silent {
        line.push_str(SILENT);
    }
    line.push_str(
        ". A command that is stuck, such as one waiting for input or looping forever, needs \
         fixing, not a longer wait; only one known to be quiet for long, such as a build, \
         needs timeout_secs]",
    );
    (line, ExitStatus::TimedOut)
}

pub fn runaway_line(megabytes: u64, stopped: &str) -> (String, ExitStatus) {
    (
        format!("[stopped after printing more than {megabytes} MB: {stopped}]"),
        ExitStatus::Killed,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbered(texts: &[&str]) -> Vec<(u64, String)> {
        texts
            .iter()
            .enumerate()
            .map(|(at, text)| (at as u64, format!("{text}\n")))
            .collect()
    }

    fn seq(count: usize) -> Vec<(u64, String)> {
        (0..count).map(|n| (n as u64, format!("{n}\n"))).collect()
    }

    const SMALL: OutputLimit = OutputLimit {
        lines: 20,
        bytes: 10_000,
    };

    type Warnings = Arc<std::sync::Mutex<Vec<String>>>;

    fn spill_in(dir: &std::path::Path, budget: u64) -> (ShellSpill, Warnings) {
        let warnings = Warnings::default();
        let sink = warnings.clone();
        let spill = ShellSpill::with_budget(
            Spill(dir.join("shell-output")),
            budget,
            Arc::new(move |message| sink.lock().unwrap().push(message.to_owned())),
        );
        (spill, warnings)
    }

    #[test]
    fn a_cut_output_is_saved_and_the_first_line_names_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let (spill, _) = spill_in(dir.path(), SPILL_BUDGET);
        let lines = seq(100);
        let (text, fact, failed) = cut(&lines, 0, SMALL, Some(&spill));
        assert!(failed.is_none());
        let first = text.lines().next().unwrap();
        assert!(
            first.starts_with("[output cut; saved for this session: "),
            "{text}"
        );
        let path = fact.expect("cut").spill.expect("the fact names the file");
        assert!(first.contains(&path), "{first}");
        let saved = std::fs::read_to_string(&path).unwrap();
        let whole: String = lines.iter().map(|(_, line)| line.as_str()).collect();
        assert_eq!(saved, whole);
        assert!(!first.contains("dropped"), "{first}");
    }

    #[test]
    fn output_that_fits_is_not_saved() {
        let dir = tempfile::tempdir().unwrap();
        let (spill, _) = spill_in(dir.path(), SPILL_BUDGET);
        let (text, fact, failed) = cut(&seq(3), 0, SMALL, Some(&spill));
        assert_eq!(text, "0\n1\n2\n");
        assert!(fact.is_none() && failed.is_none());
        assert!(!dir.path().join("shell-output").exists());
    }

    #[test]
    fn without_a_spill_the_text_is_the_plain_cut() {
        let (text, fact, failed) = cut(&seq(100), 0, SMALL, None);
        assert!(
            text.starts_with("[") && text.contains("of 100 lines cut"),
            "{text}"
        );
        assert!(!text.contains("saved for this session"));
        assert_eq!(fact.expect("cut").spill, None);
        assert!(failed.is_none());
    }

    #[test]
    fn a_failed_write_falls_back_to_the_plain_cut() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("blocked");
        std::fs::write(&blocker, "a file where the directory should be").unwrap();
        let (spill, _) = spill_in(&blocker, SPILL_BUDGET);
        let (text, fact, failed) = cut(&seq(100), 0, SMALL, Some(&spill));
        assert!(failed.is_some());
        assert!(
            text.starts_with("[") && text.contains("of 100 lines cut"),
            "{text}"
        );
        assert!(!text.contains("saved for this session"));
        assert_eq!(fact.expect("cut").spill, None);
    }

    #[test]
    fn dropped_lines_make_the_pointer_say_the_file_is_partial() {
        let dir = tempfile::tempdir().unwrap();
        let (spill, _) = spill_in(dir.path(), SPILL_BUDGET);
        let lines: Vec<(u64, String)> = (7..107).map(|n| (n, format!("{n}\n"))).collect();
        let (text, _, _) = cut(&lines, 7, SMALL, Some(&spill));
        let first = text.lines().next().unwrap();
        assert!(
            first.contains("the first 7 lines were dropped before this file"),
            "{first}"
        );
        assert!(text.contains("[7 earlier lines dropped]"), "{text}");
    }

    #[test]
    fn a_spill_past_the_session_budget_falls_back_and_warns_once() {
        let dir = tempfile::tempdir().unwrap();
        let (spill, warnings) = spill_in(dir.path(), 100);
        for _ in 0..2 {
            let (text, fact, failed) = cut(&seq(100), 0, SMALL, Some(&spill));
            assert!(!text.contains("saved for this session"), "{text}");
            assert_eq!(fact.expect("cut").spill, None);
            spill.report(&failed.expect("the budget refused the save"));
        }
        let warnings = warnings.lock().unwrap();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("not saved"), "{warnings:?}");
    }

    #[test]
    fn a_secret_in_the_output_is_not_in_the_saved_file() {
        let dir = tempfile::tempdir().unwrap();
        let (spill, _) = spill_in(dir.path(), SPILL_BUDGET);
        let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        let mut in_key = false;
        let mut lines = seq(100);
        lines[50].1 = sanitize(&format!("token {secret}\n"), &mut in_key);
        let (_, fact, _) = cut(&lines, 0, SMALL, Some(&spill));
        let saved = std::fs::read_to_string(fact.unwrap().spill.unwrap()).unwrap();
        assert!(!saved.contains(secret), "{saved}");
        assert!(saved.contains("token "), "{saved}");
    }

    #[test]
    fn output_within_the_limit_is_kept_whole() {
        let lines = numbered(&["one", "two"]);
        let (text, fact, _) = cut(&lines, 0, OutputLimit::default(), None);
        assert_eq!(text, "one\ntwo\n");
        assert_eq!(fact, None);
    }

    #[test]
    fn a_long_output_keeps_its_first_and_last_lines() {
        let lines = seq(100);
        let limit = OutputLimit {
            lines: 20,
            bytes: 10_000,
        };
        let (text, fact, _) = cut(&lines, 0, limit, None);
        assert!(
            text.starts_with("[") && text.contains("of 100 lines cut"),
            "{text}"
        );
        assert!(text.contains("\n0\n") && text.contains("\n9\n"), "{text}");
        assert!(text.ends_with("99\n"), "{text}");
        assert!(!text.contains("\n50\n"), "{text}");
        let fact = fact.expect("the output was cut");
        assert_eq!(fact.total_lines, 100);
        assert_eq!(fact.kept, KeptEnd::Ends);
        assert!(fact.kept_lines < 100);
    }

    #[test]
    fn an_error_from_the_middle_is_kept_with_its_context() {
        let mut texts: Vec<String> = (0..200).map(|n| format!("line {n}")).collect();
        texts[100] = "error: it broke".to_owned();
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        let lines = numbered(&refs);
        let limit = OutputLimit {
            lines: 40,
            bytes: 100_000,
        };
        let (text, _, _) = cut(&lines, 0, limit, None);
        assert!(text.contains("error: it broke"), "{text}");
        assert!(text.contains("line 102"), "context after the error: {text}");
        assert!(text.contains("(lines"), "the gaps are marked: {text}");
    }

    #[test]
    fn the_bytes_limit_cuts_too() {
        let lines: Vec<(u64, String)> = (0..10).map(|n| (n, "x".repeat(100) + "\n")).collect();
        let limit = OutputLimit {
            lines: 1000,
            bytes: 400,
        };
        let (text, fact, _) = cut(&lines, 0, limit, None);
        assert!(fact.is_some());
        assert!(text.len() < 700, "{}", text.len());
    }

    #[test]
    fn dropped_earlier_lines_are_counted() {
        let lines: Vec<(u64, String)> = (5..8).map(|n| (n, format!("{n}\n"))).collect();
        let (text, fact, _) = cut(&lines, 5, OutputLimit::default(), None);
        assert!(text.starts_with("[5 earlier lines dropped]\n"), "{text}");
        assert_eq!(fact, None);
    }

    #[test]
    fn escape_sequences_and_secrets_are_removed() {
        let mut in_key = false;
        assert_eq!(sanitize("\x1b[31mred\x1b[0m\n", &mut in_key), "red\n");
        let begin = sanitize("-----BEGIN PRIVATE KEY-----\n", &mut in_key);
        assert!(
            !begin.contains("BEGIN PRIVATE KEY") || begin.contains("REDACTED"),
            "{begin}"
        );
        if in_key {
            assert_eq!(sanitize("MIIBVQIBADANBg\n", &mut in_key), "");
            assert_eq!(sanitize("-----END PRIVATE KEY-----\n", &mut in_key), "");
            assert!(!in_key);
        }
    }

    #[test]
    fn status_lines_are_the_ones_split_status_knows() {
        let (line, status) = timed_out_line(5, 600, "it got SIGTERM and ended", false);
        assert_eq!(status, ExitStatus::TimedOut);
        let text = format!("out\n{line}");
        assert_eq!(rig_tools::split_status(&text).1, Some(line.as_str()));
        let (line, _) = quiet_line(60, "it got SIGTERM and ended", true);
        assert!(line.contains("It printed nothing"));
        assert!(rig_tools::split_status(&line).1.is_some());
        let (line, status) = runaway_line(64, "it was killed");
        assert_eq!(status, ExitStatus::Killed);
        assert!(rig_tools::split_status(&line).1.is_some());
    }
}
