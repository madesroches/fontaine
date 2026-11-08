//! Filing a bug report: the local bundle file, then either `gh issue create` or a prefilled
//! issue URL for the user to open. The destination is fixed; no setting changes it.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::owner_only;

pub const REPO: &str = "madesroches/fontaine";
pub const LABELS: [&str; 1] = ["bug"];
/// Bytes of a prefilled URL, after form-encoding; GitHub rejects URLs near 8 KB.
pub const URL_BUDGET: usize = 8000;
const BUNDLE_DIR: &str = "bug-reports";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Submit {
    Gh,
    Url,
}

impl Submit {
    pub fn as_str(self) -> &'static str {
        match self {
            Submit::Gh => "gh",
            Submit::Url => "url",
        }
    }
}

#[derive(Debug)]
pub enum SubmitError {
    Bundle { path: PathBuf, source: io::Error },
    Spawn { program: String, source: io::Error },
    Gh { stderr: String },
}

impl std::fmt::Display for SubmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SubmitError::Bundle { path, .. } => write!(f, "cannot write {}", path.display()),
            SubmitError::Spawn { program, .. } => write!(f, "cannot run {program}"),
            SubmitError::Gh { stderr } => write!(f, "gh issue create failed: {stderr}"),
        }
    }
}

impl std::error::Error for SubmitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SubmitError::Bundle { source, .. } | SubmitError::Spawn { source, .. } => Some(source),
            SubmitError::Gh { .. } => None,
        }
    }
}

/// Writes `text` to `<data_dir>/bug-reports/<session>-<timestamp>[-n].md`, owner-only, never over
/// an existing file.
pub fn write_bundle(data_dir: &Path, session: &str, text: &str) -> Result<PathBuf, SubmitError> {
    let dir = data_dir.join(BUNDLE_DIR);
    let fail = |path: &Path, source| SubmitError::Bundle {
        path: path.to_path_buf(),
        source,
    };
    std::fs::create_dir_all(&dir).map_err(|e| fail(&dir, e))?;
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let session = session.get(..8).unwrap_or(session);
    for attempt in 0u32.. {
        let name = if attempt == 0 {
            format!("{session}-{stamp}.md")
        } else {
            format!("{session}-{stamp}-{attempt}.md")
        };
        let path = dir.join(name);
        match owner_only::create_new(&path) {
            Ok(mut file) => {
                let written = file.write_all(text.as_bytes()).and_then(|()| file.flush());
                if let Err(e) = written {
                    drop(file);
                    // A half-written bundle would be mistaken for a whole one.
                    let _ = std::fs::remove_file(&path);
                    return Err(fail(&path, e));
                }
                return Ok(path);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(fail(&path, e)),
        }
    }
    unreachable!("the loop only returns")
}

/// The argv after `gh` that files the issue, reading the body from stdin.
pub fn gh_issue_args(title: &str) -> Vec<String> {
    let mut args: Vec<String> = ["issue", "create", "--repo", REPO]
        .into_iter()
        .map(String::from)
        .collect();
    for label in LABELS {
        args.push("--label".into());
        args.push(label.into());
    }
    args.extend([
        "--title".into(),
        title.into(),
        "--body-file".into(),
        "-".into(),
    ]);
    args
}

/// Whether `gh` is installed and logged in.
pub fn gh_ready(program: &str) -> bool {
    Command::new(program)
        .args(["auth", "status"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Files the issue with `gh` and returns the URL it prints. Blocking: call it off the UI thread.
pub fn file_with_gh(program: &str, title: &str, body: &str) -> Result<String, SubmitError> {
    let spawn_error = |source| SubmitError::Spawn {
        program: program.to_string(),
        source,
    };
    let mut child = Command::new(program)
        .args(gh_issue_args(title))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(spawn_error)?;
    let mut stdin = child.stdin.take().expect("stdin was piped");
    // The body is written from a thread so a child that fills its stdout first cannot deadlock us.
    let body = body.to_string();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(body.as_bytes());
    });
    let output = child.wait_with_output().map_err(spawn_error)?;
    let _ = writer.join();
    if !output.status.success() {
        return Err(SubmitError::Gh {
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with("http"))
        .unwrap_or(stdout.trim())
        .trim()
        .to_string())
}

/// The prefilled new-issue URL, at most [`URL_BUDGET`] bytes once encoded. The body is cut to fit
/// with a pointer to the bundle file by name; pass a body without the transcript.
pub fn prefilled_url(title: &str, body: &str, bundle: &Path) -> String {
    let url_with = |body: &str| {
        let mut url = url::Url::parse(&format!("https://github.com/{REPO}/issues/new"))
            .expect("the repository URL is valid");
        url.query_pairs_mut()
            .append_pair("title", title)
            .append_pair("labels", &LABELS.join(","))
            .append_pair("body", body);
        String::from(url)
    };
    let whole = url_with(body);
    if whole.len() <= URL_BUDGET {
        return whole;
    }
    let name = bundle
        .file_name()
        .map_or_else(|| "the bundle file".into(), |n| n.to_string_lossy());
    let note = format!("\n\n_Cut short; the whole report is in the local bundle file {name}._\n");
    let ends: Vec<usize> = body
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(body.len()))
        .collect();
    // The longest prefix whose encoded URL fits; encoding only grows with the prefix.
    let (mut low, mut high) = (0, ends.len() - 1);
    while low < high {
        let mid = (low + high).div_ceil(2);
        if url_with(&format!("{}{note}", &body[..ends[mid]])).len() <= URL_BUDGET {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    url_with(&format!("{}{note}", &body[..ends[low]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundles_are_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let a = write_bundle(dir.path(), "session1234", "one").unwrap();
        let b = write_bundle(dir.path(), "session1234", "two").unwrap();
        assert_ne!(a, b);
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "one");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "two");
        assert!(a.starts_with(dir.path().join("bug-reports")));
    }

    #[cfg(unix)]
    #[test]
    fn bundles_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = write_bundle(dir.path(), "s", "x").unwrap();
        let mode = std::fs::metadata(path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0);
    }

    #[test]
    fn gh_argv_is_fixed_and_has_no_shell() {
        let args = gh_issue_args("a title; rm -rf /");
        assert_eq!(
            args,
            [
                "issue",
                "create",
                "--repo",
                "madesroches/fontaine",
                "--label",
                "bug",
                "--title",
                "a title; rm -rf /",
                "--body-file",
                "-"
            ]
        );
    }

    #[test]
    fn the_url_is_encoded_and_cut() {
        let body = format!("a&b=c {}", "x".repeat(URL_BUDGET));
        let url = prefilled_url("t & u", &body, Path::new("/d/bundle.md"));
        assert!(url.starts_with("https://github.com/madesroches/fontaine/issues/new?"));
        assert!(url.contains("title=t+%26+u"));
        assert!(url.contains("labels=bug"));
        assert!(url.contains("a%26b%3Dc"));
        assert!(url.contains("bundle.md"));
        assert!(!url.contains("/d/"));
        assert!(url.len() <= URL_BUDGET);
        let short = prefilled_url("t", "small", Path::new("/b.md"));
        assert!(!short.contains("Cut+short"));
    }

    #[test]
    fn a_heavily_encoded_body_still_fits_the_budget() {
        let body = "line one\n\"quoted\" & <tag>; 日本語 é 🙂\n".repeat(400);
        let url = prefilled_url("t", &body, Path::new("/d/bundle.md"));
        assert!(url.len() <= URL_BUDGET, "{}", url.len());
        assert!(url.len() > URL_BUDGET - 200, "{}", url.len());
        let (_, query) = url.split_once('?').unwrap();
        let decoded = url::form_urlencoded::parse(query.as_bytes())
            .find(|(k, _)| k == "body")
            .unwrap()
            .1
            .into_owned();
        assert!(decoded.ends_with("bundle.md._\n"));
        assert!(decoded.starts_with("line one"));
    }

    #[cfg(unix)]
    fn fake_gh(dir: &Path, script: &str) -> String {
        let path = dir.join("gh");
        crate::test_support::write_executable(&path, script);
        path.display().to_string()
    }

    #[cfg(unix)]
    #[test]
    fn a_fake_gh_files_the_issue_and_gets_the_body_on_stdin() {
        let dir = tempfile::tempdir().unwrap();
        let seen = dir.path().join("seen");
        let gh = fake_gh(
            dir.path(),
            &format!(
                "if [ \"$1\" = auth ]; then exit 0; fi\ncat > {}\necho https://github.com/madesroches/fontaine/issues/9",
                seen.display()
            ),
        );
        assert!(gh_ready(&gh));
        let url = file_with_gh(&gh, "title", "the body").unwrap();
        assert_eq!(url, "https://github.com/madesroches/fontaine/issues/9");
        assert_eq!(std::fs::read_to_string(seen).unwrap(), "the body");
    }

    #[cfg(unix)]
    #[test]
    fn a_failing_gh_reports_its_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let gh = fake_gh(dir.path(), "echo nope >&2\nexit 1");
        assert!(!gh_ready(&gh));
        let err = file_with_gh(&gh, "t", "b").unwrap_err();
        assert!(err.to_string().contains("nope"));
    }

    #[test]
    fn a_missing_gh_is_not_ready() {
        assert!(!gh_ready("/nonexistent/gh-for-fontaine-tests"));
    }
}
