//! The change an `edit`, `write` or `notebook_edit` call would make, as the approval prompt shows it. Display
//! only: nothing here reaches a decision, the guardian, a hook or the ledger.

use fontaine_tools::scope::Scope;
use rig_core::redact;
use rig_core::tool::{FileChange, HunkLine};
use rig_tools::Replacement;
use rig_tools::edit::apply;
use rig_tools::fs::file_change;
use serde_json::Value;

/// Files larger than this get no preview, far below what the tools themselves read.
pub const PREVIEW_MAX_BYTES: u64 = 1024 * 1024;

/// Most diff rows a preview carries; the rest is counted in `hidden`.
pub const PREVIEW_MAX_LINES: usize = 200;

const TAB_WIDTH: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffKind {
    Context,
    Added,
    Removed,
    /// Where lines between two hunks are left out.
    Gap,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffKind,
    pub old_no: Option<usize>,
    pub new_no: Option<usize>,
    /// One line, safe to paint: no control characters, credential-shaped text redacted.
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffPreview {
    pub path: String,
    pub added: usize,
    pub removed: usize,
    pub lines: Vec<DiffLine>,
    /// Changed lines left out of `lines` by the cap.
    pub hidden: usize,
}

/// The diff `tool` would make with `args`, or `None` when there is nothing to show: another
/// tool, a path outside the roots, a file that cannot be read as UTF-8 within the limit, or
/// edits the tool itself would refuse.
pub fn preview(tool: &str, args: &Value, scope: &Scope) -> Option<DiffPreview> {
    let path = args.get("path")?.as_str()?;
    if !matches!(tool, "edit" | "write" | "notebook_edit") {
        return None;
    }
    let (_, resolved) = scope.find(path).ok()?;
    let before = read_text(&resolved);
    let change = match tool {
        "edit" => {
            let edits: Vec<Replacement> =
                serde_json::from_value(args.get("edits")?.clone()).ok()?;
            let applied = apply(path, &before.ok()??, &edits).ok()?;
            applied.change
        }
        "notebook_edit" => {
            let cell = fontaine_tools::notebook::cell_change(&before.ok()??, args)?;
            file_change(
                path,
                Some(cell.before.as_deref().unwrap_or("")),
                Some(cell.after.as_deref().unwrap_or("")),
            )
        }
        _ => {
            let content = args.get("content")?.as_str()?;
            if content.len() as u64 > PREVIEW_MAX_BYTES {
                return None;
            }
            file_change(path, before.ok()?.as_deref(), Some(content))
        }
    };
    from_change(path, &change)
}

/// `Ok(None)` for a file that does not exist; `Err` for one that cannot be shown.
fn read_text(path: &std::path::Path) -> Result<Option<String>, ()> {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(()),
    };
    if !meta.is_file() || meta.len() > PREVIEW_MAX_BYTES {
        return Err(());
    }
    let bytes = std::fs::read(path).map_err(|_| ())?;
    String::from_utf8(bytes).map(Some).map_err(|_| ())
}

fn from_change(path: &str, change: &FileChange) -> Option<DiffPreview> {
    if change.hunks.is_empty() {
        return None;
    }
    let mut lines = Vec::new();
    let mut hidden = 0;
    for (i, hunk) in change.hunks.iter().enumerate() {
        let mut rows = Vec::new();
        if i > 0 {
            rows.push(DiffLine {
                kind: DiffKind::Gap,
                old_no: None,
                new_no: None,
                text: "⋯".to_string(),
            });
        }
        let (mut old, mut new) = (hunk.old.start, hunk.new.start);
        for line in &hunk.lines {
            let text = sanitize(line.text());
            rows.push(match line {
                HunkLine::Same(_) => {
                    let row = DiffLine {
                        kind: DiffKind::Context,
                        old_no: Some(old),
                        new_no: Some(new),
                        text,
                    };
                    old += 1;
                    new += 1;
                    row
                }
                HunkLine::Removed(_) => {
                    old += 1;
                    DiffLine {
                        kind: DiffKind::Removed,
                        old_no: Some(old - 1),
                        new_no: None,
                        text,
                    }
                }
                HunkLine::Added(_) => {
                    new += 1;
                    DiffLine {
                        kind: DiffKind::Added,
                        old_no: None,
                        new_no: Some(new - 1),
                        text,
                    }
                }
            });
        }
        for row in rows {
            if lines.len() < PREVIEW_MAX_LINES {
                lines.push(row);
            } else if matches!(row.kind, DiffKind::Added | DiffKind::Removed) {
                hidden += 1;
            }
        }
    }
    Some(DiffPreview {
        path: sanitize(path),
        added: change.added,
        removed: change.removed,
        lines,
        hidden,
    })
}

/// Multi-line text made safe to paint, line by line: of a line redrawn with `\r` (a progress
/// meter) only the last drawing is kept, as a terminal would show it; the rest is as `sanitize`.
/// Line breaks are kept.
pub fn display_text(raw: &str) -> String {
    raw.split('\n')
        .map(|line| {
            let line = line.trim_end_matches('\r');
            sanitize(line.rsplit('\r').next().unwrap_or(line))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// One line with its ending dropped, tabs expanded and every other control character blanked, so
/// file content cannot drive the terminal; credential-shaped text is redacted.
fn sanitize(raw: &str) -> String {
    let raw = raw.trim_end_matches(['\n', '\r']);
    let mut out = String::with_capacity(raw.len());
    let mut column = 0;
    for c in raw.chars() {
        match c {
            '\t' => {
                let fill = TAB_WIDTH - column % TAB_WIDTH;
                out.extend(std::iter::repeat_n(' ', fill));
                column += fill;
            }
            c if c.is_control() => {
                out.push(' ');
                column += 1;
            }
            c => {
                out.push(c);
                column += 1;
            }
        }
    }
    redact::text(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn setup() -> (tempfile::TempDir, Scope) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        (dir, Scope::new(root, []))
    }

    fn put(dir: &tempfile::TempDir, name: &str, content: &[u8]) {
        std::fs::write(dir.path().join(name), content).unwrap();
    }

    fn kinds(p: &DiffPreview, kind: DiffKind) -> Vec<(Option<usize>, Option<usize>, &str)> {
        p.lines
            .iter()
            .filter(|l| l.kind == kind)
            .map(|l| (l.old_no, l.new_no, l.text.as_str()))
            .collect()
    }

    fn edit(path: &str, pairs: &[(&str, &str)]) -> Value {
        let edits: Vec<Value> = pairs
            .iter()
            .map(|(o, n)| json!({"old_text": o, "new_text": n}))
            .collect();
        json!({"path": path, "edits": edits})
    }

    #[test]
    fn single_edit_has_numbers_and_counts() {
        let (dir, scope) = setup();
        put(&dir, "a.txt", b"one\ntwo\nthree\nfour\n");
        let p = preview("edit", &edit("a.txt", &[("two", "2")]), &scope).unwrap();
        assert_eq!((p.added, p.removed, p.hidden), (1, 1, 0));
        assert_eq!(kinds(&p, DiffKind::Removed), [(Some(2), None, "two")]);
        assert_eq!(kinds(&p, DiffKind::Added), [(None, Some(2), "2")]);
        assert_eq!(p.lines[0].text, "one");
        assert_eq!((p.lines[0].old_no, p.lines[0].new_no), (Some(1), Some(1)));
    }

    #[test]
    fn distant_edits_are_separated_by_a_gap() {
        let (dir, scope) = setup();
        let text: String = (1..=30).map(|n| format!("l{n}\n")).collect();
        put(&dir, "a.txt", text.as_bytes());
        let args = edit("a.txt", &[("l2\n", "L2\n"), ("l28\n", "L28\n")]);
        let p = preview("edit", &args, &scope).unwrap();
        assert_eq!((p.added, p.removed), (2, 2));
        assert_eq!(
            p.lines.iter().filter(|l| l.kind == DiffKind::Gap).count(),
            1
        );
        assert_eq!(kinds(&p, DiffKind::Added)[1], (None, Some(28), "L28"));
    }

    #[test]
    fn replace_all_shows_every_match() {
        let (dir, scope) = setup();
        put(&dir, "a.txt", b"x\nfoo\ny\nfoo\n");
        let args = json!({"path": "a.txt", "edits": [
            {"old_text": "foo", "new_text": "bar", "replace_all": true}]});
        let p = preview("edit", &args, &scope).unwrap();
        assert_eq!((p.added, p.removed), (2, 2));
    }

    #[test]
    fn write_over_an_existing_file() {
        let (dir, scope) = setup();
        put(&dir, "a.txt", b"a\nb\n");
        let args = json!({"path": "a.txt", "content": "a\nc\n"});
        let p = preview("write", &args, &scope).unwrap();
        assert_eq!(kinds(&p, DiffKind::Removed), [(Some(2), None, "b")]);
        assert_eq!(kinds(&p, DiffKind::Added), [(None, Some(2), "c")]);
    }

    #[test]
    fn write_creating_a_file_is_all_added() {
        let (_dir, scope) = setup();
        let args = json!({"path": "new.txt", "content": "a\nb\n"});
        let p = preview("write", &args, &scope).unwrap();
        assert_eq!((p.added, p.removed), (2, 0));
        assert!(p.lines.iter().all(|l| l.kind == DiffKind::Added));
    }

    #[test]
    fn nothing_to_show_gives_none() {
        let (dir, scope) = setup();
        put(&dir, "a.txt", b"one\n");
        put(&dir, "bin.dat", b"a\xff");
        let mut big = vec![b'a'; PREVIEW_MAX_BYTES as usize];
        big.push(b'Z');
        put(&dir, "big.txt", &big);
        let ok = edit("a.txt", &[("one", "1")]);
        assert!(preview("read", &ok, &scope).is_none());
        assert!(preview("edit", &edit("missing.txt", &[("a", "b")]), &scope).is_none());
        assert!(preview("edit", &edit("/etc/hostname", &[("a", "b")]), &scope).is_none());
        assert!(preview("edit", &edit("bin.dat", &[("a", "b")]), &scope).is_none());
        assert!(preview("edit", &edit("big.txt", &[("Z", "Y")]), &scope).is_none());
        assert!(preview("edit", &edit("a.txt", &[("absent", "b")]), &scope).is_none());
        assert!(preview("edit", &edit("a.txt", &[("one", "one")]), &scope).is_none());
        assert!(preview("edit", &json!({"path": "a.txt"}), &scope).is_none());
        let outside = json!({"path": "/etc/hostname", "content": "x"});
        assert!(preview("write", &outside, &scope).is_none());
        let huge = "x".repeat(PREVIEW_MAX_BYTES as usize + 1);
        assert!(preview("write", &json!({"path": "a.txt", "content": huge}), &scope).is_none());
    }

    #[test]
    fn escapes_are_blanked_and_secrets_redacted() {
        let (dir, scope) = setup();
        let key = "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJ";
        put(
            &dir,
            "a.txt",
            format!("\x1b[31mred\x07\nkey {key}\n\tx\n").as_bytes(),
        );
        let p = preview("edit", &edit("a.txt", &[("\tx", "\ty")]), &scope).unwrap();
        for line in &p.lines {
            assert!(!line.text.chars().any(char::is_control), "{:?}", line.text);
            assert!(!line.text.contains("abcdefghijklmnopqrstuvwxyz0123456789"));
        }
        assert_eq!(p.lines[0].text, " [31mred ");
        assert_eq!(p.lines.last().unwrap().text, "    y");
    }

    #[test]
    fn display_text_keeps_lines_and_makes_each_safe() {
        let key = "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJ";
        let raw = format!("a\tb\n\x1b[2Jx\x07\n10%\r50%\r100%\r\nkey {key}\n");
        let out = display_text(&raw);
        let lines: Vec<&str> = out.split('\n').collect();
        assert_eq!(lines[0], "a   b");
        assert_eq!(lines[1], " [2Jx ");
        assert_eq!(lines[2], "100%");
        assert!(!lines[3].contains("abcdefghijklmnopqrstuvwxyz0123456789"));
        assert_eq!(lines[4], "");
        assert!(!out.chars().any(|c| c.is_control() && c != '\n'));
    }

    #[test]
    fn crlf_cr_and_bom_files_preview() {
        let (dir, scope) = setup();
        put(&dir, "a.txt", b"\xef\xbb\xbfone\r\ntwo\r\nthree\r\n");
        let p = preview("edit", &edit("a.txt", &[("two", "2")]), &scope).unwrap();
        assert_eq!(kinds(&p, DiffKind::Added), [(None, Some(2), "2")]);
        put(&dir, "b.txt", b"x\ry\n");
        let args = json!({"path": "b.txt", "content": "x\rz\n"});
        let p = preview("write", &args, &scope).unwrap();
        assert!(p.lines.iter().all(|l| !l.text.contains('\r')));
    }

    #[test]
    fn long_diffs_are_capped_and_the_rest_counted() {
        let (_dir, scope) = setup();
        let content: String = (0..300).map(|n| format!("line {n}\n")).collect();
        let args = json!({"path": "new.txt", "content": content});
        let p = preview("write", &args, &scope).unwrap();
        assert_eq!(p.lines.len(), PREVIEW_MAX_LINES);
        assert_eq!(p.added, 300);
        assert_eq!(p.hidden, 100);
    }

    #[test]
    fn notebook_edit_previews_the_cell_source_change() {
        let (dir, scope) = setup();
        let nb = r#"{"cells":[{"cell_type":"code","id":"a","metadata":{},"outputs":[],"execution_count":null,"source":["x = 1\n","y = 2"]}],"metadata":{},"nbformat":4,"nbformat_minor":5}"#;
        put(&dir, "n.ipynb", nb.as_bytes());
        let args = json!({"path": "n.ipynb", "cell_id": "a", "new_source": "x = 1\ny = 3"});
        let p = preview("notebook_edit", &args, &scope).unwrap();
        assert_eq!(kinds(&p, DiffKind::Removed), [(Some(2), None, "y = 2")]);
        assert_eq!(kinds(&p, DiffKind::Added), [(None, Some(2), "y = 3")]);
        let missing = json!({"path": "n.ipynb", "cell_id": "ghost", "new_source": "x"});
        assert!(preview("notebook_edit", &missing, &scope).is_none());
        assert!(preview("notebook_edit", &json!({"path": "a.txt"}), &scope).is_none());
    }
}
