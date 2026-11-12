//! OSC 8 hyperlinks for the transcript. Row builders tag the text that refers to something
//! (a URL, an issue, a file) by putting an id in the span's `underline_color`; once a frame is
//! drawn into a buffer, `Links::apply` turns each tagged cell into the escape sequence and clears
//! the tag, so the backend never sees one. Rendering stays free of session context: what a
//! reference points at is decided only at that last step.

use std::collections::HashMap;
use std::num::NonZeroU16;
use std::ops::{BitOr, Range};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use fontaine_tools::lock::lock_recovering;
use ratatui::buffer::{Buffer, CellDiffOption, CellWidth};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

/// What a piece of text refers to, before the session's context resolves it.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Ref {
    /// An http(s) URL, or a markdown link target.
    Url(String),
    /// `owner/repo#N`, or a bare `#N` when `repo` is `None`.
    Issue {
        repo: Option<(String, String)>,
        number: u64,
    },
    /// A file as written, with any `:line[:col]` stripped.
    Path(String),
}

impl Ref {
    /// Whether the target is known without the session's directory or remote, so the text is
    /// coloured as a link even where hyperlinks are off.
    fn context_free(&self) -> bool {
        match self {
            Ref::Url(_) => true,
            Ref::Issue { repo, .. } => repo.is_some(),
            Ref::Path(_) => false,
        }
    }
}

const MAX_ID: usize = 0xFF_FFFF;

#[derive(Default)]
struct Interner {
    ids: HashMap<Ref, u32>,
    refs: Vec<Arc<Ref>>,
}

impl Interner {
    fn intern(&mut self, r: Ref) -> Option<u32> {
        if let Some(id) = self.ids.get(&r) {
            return Some(*id);
        }
        if self.refs.len() >= MAX_ID {
            return None;
        }
        self.refs.push(Arc::new(r.clone()));
        let id = u32::try_from(self.refs.len()).expect("the interner holds at most 0xFFFFFF refs");
        self.ids.insert(r, id);
        Some(id)
    }

    fn get(&self, id: u32) -> Option<Arc<Ref>> {
        let index = usize::try_from(id).ok()?.checked_sub(1)?;
        self.refs.get(index).cloned()
    }
}

/// Process-global so rendering needs no context. It grows with the distinct tokens a session
/// shows.
static REFS: LazyLock<Mutex<Interner>> = LazyLock::new(Mutex::default);

/// The ref behind a tag.
#[cfg(test)]
pub(crate) fn ref_of(id: u32) -> Ref {
    (*lock_recovering(&REFS).get(id).expect("a known id")).clone()
}

/// The interned id a tag colour carries.
pub(crate) fn id_of(color: Color) -> Option<u32> {
    let Color::Rgb(r, g, b) = color else {
        return None;
    };
    Some(u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b))
}

/// The ref a span style is tagged with.
#[cfg(test)]
pub(crate) fn ref_of_style(style: Style) -> Option<Ref> {
    Some(ref_of(id_of(style.underline_color?)?))
}

/// `style` with `r` attached. When the interner is full the style is returned unchanged and the
/// text simply is not a link.
pub(crate) fn tag(style: Style, r: Ref) -> Style {
    // Display state: a poisoned interner still maps ids to the refs it holds.
    let id = lock_recovering(&REFS).intern(r);
    match id {
        Some(id) => style.underline_color(Color::Rgb((id >> 16) as u8, (id >> 8) as u8, id as u8)),
        None => style,
    }
}

/// `style` for the text of `r`: context-free refs get the link style now, the others only
/// when they resolve.
pub(crate) fn styled(style: Style, link: Style, r: Ref) -> Style {
    let style = if r.context_free() {
        style.patch(link)
    } else {
        style
    };
    tag(style, r)
}

/// Which references `scan` looks for.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Kinds(u8);

impl Kinds {
    pub(crate) const URL: Kinds = Kinds(1);
    pub(crate) const ISSUE: Kinds = Kinds(2);
    pub(crate) const PATH: Kinds = Kinds(4);
    /// Restricts URLs to ones that are a whole whitespace-delimited token, as in code, where
    /// `x=https://a` is not a link.
    pub(crate) const WHOLE: Kinds = Kinds(8);
    /// Assistant prose.
    pub(crate) const PROSE: Kinds = Kinds(7);
    /// Tool and shell output: issue references there are rarely issues.
    pub(crate) const OUTPUT: Kinds = Kinds(5);

    fn has(self, other: Kinds) -> bool {
        self.0 & other.0 != 0
    }
}

impl BitOr for Kinds {
    type Output = Kinds;

    fn bitor(self, rhs: Kinds) -> Kinds {
        Kinds(self.0 | rhs.0)
    }
}

/// The references in `text` with their byte ranges, in order and not overlapping. The earliest
/// match wins; at the same start a URL beats an issue reference, which beats a path.
pub(crate) fn scan(text: &str, kinds: Kinds) -> Vec<(Range<usize>, Ref)> {
    let mut found: Vec<(Range<usize>, u8, Ref)> = Vec::new();
    if kinds.has(Kinds::URL) {
        urls(text, kinds.has(Kinds::WHOLE), &mut found);
    }
    if kinds.has(Kinds::ISSUE) {
        issues(text, &mut found);
    }
    if kinds.has(Kinds::PATH) {
        paths(text, &mut found);
    }
    found.sort_by_key(|(range, rank, _)| (range.start, *rank));
    let mut out = Vec::new();
    let mut end = 0;
    for (range, _, r) in found {
        if range.start >= end {
            end = range.end;
            out.push((range, r));
        }
    }
    out
}

/// A URL runs to the next whitespace, minus trailing sentence punctuation and closing brackets
/// or quotes that have no opener inside it.
fn urls(text: &str, whole: bool, out: &mut Vec<(Range<usize>, u8, Ref)>) {
    let mut at = 0;
    while let Some(found) = text[at..].find("http") {
        let start = at + found;
        at = start + 4;
        let rest = &text[start..];
        let scheme = if rest.starts_with("https://") {
            8
        } else if rest.starts_with("http://") {
            7
        } else {
            continue;
        };
        let before = text[..start].chars().next_back();
        if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        if whole && before.is_some_and(|c| !c.is_whitespace()) {
            continue;
        }
        let run = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let len = trim_url(&rest[..run]).len();
        if len <= scheme {
            continue;
        }
        out.push((
            start..start + len,
            0,
            Ref::Url(text[start..start + len].to_string()),
        ));
        at = start + len;
    }
}

pub(crate) fn trim_url(url: &str) -> &str {
    let mut url = url;
    while let Some(last) = url.chars().next_back() {
        let count = |c: char| url.chars().filter(|x| *x == c).count();
        let unbalanced = match last {
            '.' | ',' | ';' | ':' | '!' | '?' => true,
            ')' => count(')') > count('('),
            ']' => count(']') > count('['),
            '\'' | '"' => count(last) % 2 == 1,
            _ => false,
        };
        if !unbalanced {
            break;
        }
        url = &url[..url.len() - last.len_utf8()];
    }
    url
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_slug(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-')
}

/// Whether `c` before a reference makes it part of a longer word, an entity or a path.
fn blocks_start(c: char) -> bool {
    is_word(c) || matches!(c, '/' | '&' | '#')
}

/// `#N` and `owner/repo#N`: one to nine digits after a `#` at a word start, not followed by a
/// word character.
fn issues(text: &str, out: &mut Vec<(Range<usize>, u8, Ref)>) {
    for (hash, _) in text.match_indices('#') {
        let digits = text.as_bytes()[hash + 1..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        if digits == 0 || digits > 9 {
            continue;
        }
        let end = hash + 1 + digits;
        if text[end..].chars().next().is_some_and(is_word) {
            continue;
        }
        let Ok(number) = text[hash + 1..end].parse::<u64>() else {
            continue;
        };
        if number == 0 {
            continue;
        }
        let before = text[..hash].chars().next_back();
        if !before.is_some_and(blocks_start) {
            out.push((hash..end, 1, Ref::Issue { repo: None, number }));
            continue;
        }
        let head = &text.as_bytes()[..hash];
        let repo_len = head.iter().rev().take_while(|b| is_slug(**b)).count();
        let slash = hash - repo_len;
        if repo_len == 0 || slash == 0 || head[slash - 1] != b'/' {
            continue;
        }
        let owner_len = head[..slash - 1]
            .iter()
            .rev()
            .take_while(|b| is_slug(**b))
            .count();
        let owner = slash - 1 - owner_len;
        if owner_len == 0 || text[..owner].chars().next_back().is_some_and(blocks_start) {
            continue;
        }
        out.push((
            owner..end,
            1,
            Ref::Issue {
                repo: Some((
                    text[owner..slash - 1].to_string(),
                    text[slash..hash].to_string(),
                )),
                number,
            },
        ));
    }
}

/// Drops one leading wrapper (a bracket or quote) with its closer, and trailing punctuation.
/// Returns the bytes dropped in front and what is left.
fn trim_token(token: &str) -> (usize, &str) {
    let mut s = token;
    let mut lead = 0;
    let mut closer = None;
    if let Some(c) = s.chars().next() {
        closer = match c {
            '(' => Some(')'),
            '[' => Some(']'),
            '<' => Some('>'),
            '{' => Some('}'),
            '\'' | '"' | '`' => Some(c),
            _ => None,
        };
        if closer.is_some() {
            lead = c.len_utf8();
            s = &s[lead..];
        }
    }
    loop {
        let trimmed = trim_url(s);
        if trimmed.len() < s.len() {
            s = trimmed;
        } else if let Some(c) = closer.filter(|c| s.ends_with(*c)) {
            s = &s[..s.len() - c.len_utf8()];
            closer = None;
        } else {
            break;
        }
    }
    (lead, s)
}

/// The path of a `path[:line[:col]]` token, and whether it carried a line.
fn split_line(token: &str) -> (&str, bool) {
    let mut s = token;
    let mut line = false;
    for _ in 0..2 {
        match s.rsplit_once(':') {
            Some((head, tail))
                if !head.is_empty()
                    && !tail.is_empty()
                    && tail.bytes().all(|b| b.is_ascii_digit()) =>
            {
                s = head;
                line = true;
            }
            _ => break,
        }
    }
    (s, line)
}

/// Whitespace-delimited tokens that name a file: they hold a `/`, or end `:line` with a `.` in
/// the file name. Whether the file exists is for `resolve` to say.
fn paths(text: &str, out: &mut Vec<(Range<usize>, u8, Ref)>) {
    for token in text.split_whitespace() {
        let offset = token.as_ptr() as usize - text.as_ptr() as usize;
        let (lead, core) = trim_token(token);
        let (path, line) = split_line(core);
        if path.is_empty() || path.contains("://") {
            continue;
        }
        let separated = path.contains('/') || (cfg!(windows) && path.contains('\\'));
        let named = line
            && path
                .rsplit(['/', '\\'])
                .next()
                .is_some_and(|name| name.contains('.'));
        if !(separated || named) {
            continue;
        }
        let start = offset + lead;
        out.push((start..start + core.len(), 2, Ref::Path(path.to_string())));
    }
}

/// Where a resolved reference points, and whether the text was already styled as a link.
struct Resolved {
    target: Option<String>,
    context_free: bool,
}

/// How long a path's result, hit or miss, is trusted before the disk is asked again.
const PATH_RECHECK: Duration = Duration::from_secs(1);

/// The session context hyperlinks resolve against, and whether to emit them at all.
#[derive(Default)]
pub(crate) struct Links {
    /// Off until the terminal is known to take OSC 8, so every render path stays as plain as
    /// before.
    pub(crate) enabled: bool,
    github: Option<(String, String)>,
    cwd: PathBuf,
    resolved: Mutex<HashMap<u32, Option<String>>>,
    /// Path results with when they were read: a file created, deleted or made executable later
    /// changes its link.
    checked: Mutex<HashMap<u32, (Instant, Option<String>)>>,
}

impl Links {
    /// Reads the `origin` remote of `cwd` when the directory changed.
    pub(crate) fn set_context(&mut self, cwd: &Path) {
        if self.cwd == cwd {
            return;
        }
        let github = fontaine_core::ledger::git_origin(cwd)
            .and_then(|url| fontaine_core::project_env::discover::github_slug(&url))
            .and_then(|slug| {
                slug.split_once('/')
                    .map(|(owner, repo)| (owner.to_string(), repo.to_string()))
            });
        self.configure(cwd, github);
    }

    pub(crate) fn configure(&mut self, cwd: &Path, github: Option<(String, String)>) {
        self.cwd = cwd.to_path_buf();
        self.github = github;
        // Display state: a cleared cache only costs a re-resolve.
        lock_recovering(&self.resolved).clear();
        lock_recovering(&self.checked).clear();
    }

    /// The URL the interned ref `id` opens, or `None` when it points at nothing.
    pub(crate) fn resolve(&self, id: u32) -> Option<String> {
        // Display state: a stale entry only shows or hides one link.
        if let Some(known) = lock_recovering(&self.resolved).get(&id) {
            return known.clone();
        }
        if let Some((at, known)) = lock_recovering(&self.checked).get(&id)
            && at.elapsed() < PATH_RECHECK
        {
            return known.clone();
        }
        let r = lock_recovering(&REFS).get(id)?;
        let target = self.target(&r);
        if matches!(*r, Ref::Path(_)) {
            lock_recovering(&self.checked).insert(id, (Instant::now(), target.clone()));
        } else {
            lock_recovering(&self.resolved).insert(id, target.clone());
        }
        target
    }

    fn target(&self, r: &Ref) -> Option<String> {
        match r {
            Ref::Url(url) => {
                let url = url::Url::parse(url).ok()?;
                (matches!(url.scheme(), "http" | "https") && url.has_host()).then(|| url.into())
            }
            Ref::Issue { repo, number } => {
                let (owner, name) = repo.as_ref().or(self.github.as_ref())?;
                Some(format!("https://github.com/{owner}/{name}/issues/{number}"))
            }
            Ref::Path(path) => self.file_url(path),
        }
    }

    /// The path joined to the primary root, judged lexically before any filesystem call: the text
    /// comes from the model or a tool, and resolving a UNC or device path would reach the network.
    fn confined(&self, path: &str) -> Option<PathBuf> {
        if path.starts_with("//") || path.starts_with("\\\\") {
            return None;
        }
        let path = Path::new(path);
        let plain = path.components().all(|c| {
            !matches!(
                c,
                std::path::Component::Prefix(_) | std::path::Component::ParentDir
            )
        });
        let full = self.cwd.join(path);
        (plain && full.starts_with(&self.cwd)).then_some(full)
    }

    /// A `file://` URL for an existing regular file that a click cannot run.
    fn file_url(&self, path: &str) -> Option<String> {
        let full = self.confined(path)?;
        // A symlink is judged by its own name and by its target, and the URL names the target.
        let real = std::fs::canonicalize(&full).ok()?;
        // A symlink inside the root must not lead out to a network mount.
        if !real.starts_with(std::fs::canonicalize(&self.cwd).ok()?) {
            return None;
        }
        let meta = std::fs::metadata(&real).ok()?;
        if !meta.is_file() || launchable(&full, &meta) || launchable(&real, &meta) {
            return None;
        }
        let mut url = url::Url::from_file_path(&real).ok()?;
        if let Some(host) = hostname() {
            // A rejected name leaves the host empty, which is still a valid local file URL.
            let _ = url.set_host(Some(host));
        }
        Some(url.into())
    }

    /// Turns every tagged cell of `area` into what it stands for: the link style for a
    /// context-dependent ref that resolved, and, when enabled, the OSC 8 escapes around the cell.
    pub(crate) fn apply(&self, buf: &mut Buffer, area: Rect, link: Style) {
        let mut seen: HashMap<u32, Option<Resolved>> = HashMap::new();
        let area = area.intersection(buf.area);
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                let cell = &mut buf[(x, y)];
                let Some(id) = id_of(cell.underline_color) else {
                    continue;
                };
                // The tag is internal: clear it even when nothing resolves, or the backend emits it as SGR 58.
                cell.underline_color = Color::Reset;
                let Some(resolved) = seen.entry(id).or_insert_with(|| self.lookup(id)) else {
                    continue;
                };
                let Some(target) = &resolved.target else {
                    continue;
                };
                if !resolved.context_free {
                    cell.set_style(link);
                }
                if !self.enabled || !target.bytes().all(|b| (0x20..=0x7E).contains(&b)) {
                    continue;
                }
                let Some(width) = NonZeroU16::new(cell.cell_width()) else {
                    continue;
                };
                let symbol = format!(
                    "\x1b]8;id={id:x};{target}\x1b\\{}\x1b]8;;\x1b\\",
                    cell.symbol()
                );
                cell.set_symbol(&symbol);
                cell.set_diff_option(CellDiffOption::ForcedWidth(width));
            }
        }
    }

    fn lookup(&self, id: u32) -> Option<Resolved> {
        // Display state: the intern table only grows, so a stale read shows at worst no link.
        let r = lock_recovering(&REFS).get(id)?;
        // A path is only ever resolved to be linked, so with links off it stays unresolved.
        if !self.enabled && matches!(*r, Ref::Path(_)) {
            return None;
        }
        Some(Resolved {
            target: self.resolve(id),
            context_free: r.context_free(),
        })
    }
}

/// Files a click would launch rather than open: the OS opener runs them.
fn launchable(path: &Path, meta: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 != 0 {
            return true;
        }
    }
    #[cfg(not(unix))]
    let _ = meta;
    const LAUNCHERS: &[&str] = &[
        "desktop",
        "command",
        "tool",
        "terminal",
        "app",
        "workflow",
        "scpt",
        "pkg",
        "dmg",
        "appimage",
        "exe",
        "com",
        "bat",
        "cmd",
        "ps1",
        "vbs",
        "vbe",
        "js",
        "jse",
        "wsf",
        "wsh",
        "msi",
        "msc",
        "scr",
        "cpl",
        "lnk",
        "url",
        "reg",
        "hta",
        "jar",
        "py",
        "pyw",
        "pif",
        "application",
        "appref-ms",
        "jnlp",
        "chm",
        "xll",
        "msp",
        "appinstaller",
        "msix",
        "msixbundle",
        "appx",
        "appxbundle",
        "fileloc",
        "inetloc",
        "webloc",
        "mpkg",
        "scf",
    ];
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    let ext = ext.to_ascii_lowercase();
    if LAUNCHERS.contains(&ext.as_str()) {
        return true;
    }
    cfg!(windows)
        && std::env::var("PATHEXT").is_ok_and(|exts| {
            exts.split(';')
                .any(|e| e.trim_start_matches('.').eq_ignore_ascii_case(&ext))
        })
}

/// The local hostname, which OSC 8 asks for in a `file://` URL so a terminal on another machine
/// does not open a same-named local file.
fn hostname() -> Option<&'static str> {
    static HOST: LazyLock<Option<String>> = LazyLock::new(read_hostname);
    HOST.as_deref()
}

#[cfg(unix)]
fn read_hostname() -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer is valid for `buf.len()` bytes, which is what gethostname is told.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if rc != 0 {
        return None;
    }
    let end = buf.iter().position(|b| *b == 0)?;
    String::from_utf8(buf[..end].to_vec())
        .ok()
        .filter(|host| !host.is_empty())
}

#[cfg(not(unix))]
fn read_hostname() -> Option<String> {
    None
}

/// Whether the terminal is worth sending OSC 8 to. Terminals that do not implement it ignore
/// the sequence, so the rules only name the places where it would show.
pub(crate) fn supported(env: impl Fn(&str) -> Option<String>, windows: bool) -> bool {
    let set = |key: &str| env(key).is_some_and(|v| !v.is_empty());
    if let Some(force) = env("FORCE_HYPERLINK").filter(|v| !v.is_empty()) {
        return force != "0";
    }
    if set("TMUX") {
        return true;
    }
    if windows {
        return set("WT_SESSION") || set("TERM_PROGRAM");
    }
    match env("TERM") {
        None => false,
        Some(term) => {
            !(term.is_empty() || term == "dumb" || term == "linux" || term.starts_with("screen"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Cell;
    use ratatui::style::Modifier;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_string())
        }
    }

    fn found(text: &str, kinds: Kinds) -> Vec<(String, Ref)> {
        scan(text, kinds)
            .into_iter()
            .map(|(range, r)| (text[range].to_string(), r))
            .collect()
    }

    fn texts(text: &str, kinds: Kinds) -> Vec<String> {
        found(text, kinds).into_iter().map(|(t, _)| t).collect()
    }

    fn issue(number: u64) -> Ref {
        Ref::Issue { repo: None, number }
    }

    fn id_of(r: Ref) -> u32 {
        let color = tag(Style::default(), r).underline_color;
        super::id_of(color.expect("tagged")).expect("an rgb tag")
    }

    fn links_in(dir: &Path, github: Option<(&str, &str)>) -> Links {
        let mut links = Links {
            enabled: true,
            ..Links::default()
        };
        links.configure(dir, github.map(|(o, r)| (o.to_string(), r.to_string())));
        links
    }

    #[test]
    fn a_path_missing_at_first_resolves_once_the_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        let links = links_in(dir.path(), None);
        let id = id_of(Ref::Path("later.txt".into()));
        assert_eq!(links.resolve(id), None);
        std::fs::write(dir.path().join("later.txt"), "x").unwrap();
        assert_eq!(links.resolve(id), None, "a result is trusted briefly");
        lock_recovering(&links.checked).clear();
        assert!(links.resolve(id).is_some());
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_path_stops_linking_once_it_becomes_executable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("deploy.sh");
        std::fs::write(&file, "x").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        let links = links_in(dir.path(), None);
        let id = id_of(Ref::Path("deploy.sh".into()));
        assert!(links.resolve(id).is_some());
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        lock_recovering(&links.checked).clear();
        assert_eq!(links.resolve(id), None);
    }

    #[test]
    fn paths_outside_the_root_are_never_resolved() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("o.txt"), "x").unwrap();
        std::fs::write(dir.path().join("in.txt"), "x").unwrap();
        let links = links_in(dir.path(), None);
        let abs_outside = outside.path().join("o.txt").to_string_lossy().into_owned();
        for p in [
            "\\\\host\\share\\a.txt",
            "//host/x",
            "../outside.txt",
            "sub/../in.txt",
            abs_outside.as_str(),
        ] {
            assert_eq!(links.confined(p), None, "{p}");
            assert_eq!(links.file_url(p), None, "{p}");
        }
        let inside = dir.path().join("in.txt").to_string_lossy().into_owned();
        assert!(links.confined(&inside).is_some());
        assert!(links.file_url(&inside).is_some());
        assert!(links.file_url("in.txt").is_some());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_out_of_the_root_is_not_linked() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("o.txt"), "x").unwrap();
        std::os::unix::fs::symlink(outside.path().join("o.txt"), dir.path().join("l.txt")).unwrap();
        assert_eq!(links_in(dir.path(), None).file_url("l.txt"), None);
    }

    #[test]
    fn a_path_stays_unresolved_when_links_are_off() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("in.txt"), "x").unwrap();
        let mut links = links_in(dir.path(), None);
        links.enabled = false;
        let id = id_of(Ref::Path("in.txt".into()));
        assert!(links.lookup(id).is_none());
        links.enabled = true;
        assert!(links.lookup(id).is_some());
    }

    #[test]
    fn issue_zero_is_not_a_reference() {
        assert!(found("see #0 and #000", Kinds::ISSUE).is_empty());
    }

    #[test]
    fn urls_keep_their_trimming_rules() {
        assert_eq!(texts("go to https://a.io.", Kinds::URL), ["https://a.io"]);
        assert_eq!(
            texts("(see https://a.io/x).", Kinds::URL),
            ["https://a.io/x"]
        );
        assert_eq!(
            texts("http://a.io and https://b.io", Kinds::URL),
            ["http://a.io", "https://b.io"]
        );
        for text in ["http://", "https://.", "a.io/x", "xhttps://a.io", "http:/a"] {
            assert!(found(text, Kinds::URL).is_empty(), "{text}");
        }
    }

    #[test]
    fn a_whole_token_url_excludes_one_after_other_text() {
        assert_eq!(
            texts("curl https://a", Kinds::URL | Kinds::WHOLE),
            ["https://a"]
        );
        assert!(found("x=https://a", Kinds::URL | Kinds::WHOLE).is_empty());
        assert_eq!(texts("x=https://a", Kinds::URL), ["https://a"]);
    }

    #[test]
    fn a_bare_issue_needs_a_word_start() {
        assert_eq!(
            found("#82 is open", Kinds::ISSUE),
            [("#82".into(), issue(82))]
        );
        assert_eq!(texts("fixed (#82).", Kinds::ISSUE), ["#82"]);
        assert_eq!(texts("see #82, #83", Kinds::ISSUE), ["#82", "#83"]);
        for text in [
            "a#82",
            "&#82;",
            "x/#82",
            "##82",
            "#",
            "#x",
            "#82x",
            "#1234567890",
        ] {
            assert!(found(text, Kinds::ISSUE).is_empty(), "{text}");
        }
    }

    #[test]
    fn an_owner_repo_issue_is_one_reference() {
        let got = found("see owner/repo#7 now", Kinds::PROSE);
        assert_eq!(
            got,
            [(
                "owner/repo#7".into(),
                Ref::Issue {
                    repo: Some(("owner".into(), "repo".into())),
                    number: 7
                }
            )]
        );
        assert!(found("xowner/repo#7y", Kinds::ISSUE).is_empty());
    }

    #[test]
    fn an_issue_inside_a_url_is_not_matched_separately() {
        let got = found("https://github.com/a/b/issues/1#82 ok", Kinds::PROSE);
        assert_eq!(got.len(), 1);
        assert!(matches!(got[0].1, Ref::Url(_)));
    }

    #[test]
    fn gh_dash_numbers_are_not_references() {
        assert!(found("GH-82", Kinds::PROSE).is_empty());
    }

    #[test]
    fn a_path_scans_without_its_wrapper_and_keeps_the_line_in_the_text() {
        let path = |p: &str| Ref::Path(p.to_string());
        assert_eq!(
            found("(src/main.rs:12)", Kinds::PATH),
            [("src/main.rs:12".into(), path("src/main.rs"))]
        );
        assert_eq!(
            found("at src/main.rs:12:5.", Kinds::PATH),
            [("src/main.rs:12:5".into(), path("src/main.rs"))]
        );
        assert_eq!(found("a/b", Kinds::PATH), [("a/b".into(), path("a/b"))]);
        assert_eq!(
            found("`src/x.rs` and \"src/y.rs\"", Kinds::PATH),
            [
                ("src/x.rs".into(), path("src/x.rs")),
                ("src/y.rs".into(), path("src/y.rs"))
            ]
        );
        assert_eq!(
            found("file.rs:3", Kinds::PATH),
            [("file.rs:3".into(), path("file.rs"))]
        );
    }

    #[test]
    fn what_is_not_a_path_is_left_alone() {
        for text in ["foo.rs", "foo", "https://x/y", "see 12:30", "a:12"] {
            assert!(found(text, Kinds::PATH).is_empty(), "{text}");
        }
    }

    #[test]
    fn only_the_requested_kinds_are_found() {
        assert!(found("#82 src/a.rs", Kinds::URL).is_empty());
        assert_eq!(texts("#82 src/a.rs", Kinds::OUTPUT), ["src/a.rs"]);
    }

    #[test]
    fn a_bare_issue_resolves_only_against_a_github_origin() {
        let dir = tempfile::tempdir().unwrap();
        let id = id_of(issue(82));
        assert_eq!(links_in(dir.path(), None).resolve(id), None);
        assert_eq!(
            links_in(dir.path(), Some(("o", "r")))
                .resolve(id)
                .as_deref(),
            Some("https://github.com/o/r/issues/82")
        );
    }

    #[test]
    fn an_owner_repo_issue_needs_no_origin() {
        let dir = tempfile::tempdir().unwrap();
        let id = id_of(Ref::Issue {
            repo: Some(("a".into(), "b".into())),
            number: 7,
        });
        assert_eq!(
            links_in(dir.path(), None).resolve(id).as_deref(),
            Some("https://github.com/a/b/issues/7")
        );
    }

    #[test]
    fn urls_resolve_percent_encoded_and_other_schemes_do_not_resolve() {
        let dir = tempfile::tempdir().unwrap();
        let links = links_in(dir.path(), None);
        let id = id_of(Ref::Url("https://de.wikipedia.org/wiki/Müller".into()));
        assert_eq!(
            links.resolve(id).as_deref(),
            Some("https://de.wikipedia.org/wiki/M%C3%BCller")
        );
        for bad in ["javascript:alert(1)", "file:///etc/passwd", "https://"] {
            assert_eq!(links.resolve(id_of(Ref::Url(bad.into()))), None, "{bad}");
        }
    }

    #[test]
    fn a_path_resolves_to_a_file_url_for_an_existing_file_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a b.txt"), "x").unwrap();
        let links = links_in(dir.path(), None);
        let target = links
            .resolve(id_of(Ref::Path("a b.txt".into())))
            .expect("an existing file resolves");
        assert!(target.starts_with("file://"), "{target}");
        assert!(target.ends_with("/a%20b.txt"), "{target}");
        #[cfg(unix)]
        {
            let host = hostname().expect("the test machine has a hostname");
            assert!(
                target
                    .to_ascii_lowercase()
                    .starts_with(&format!("file://{}/", host.to_ascii_lowercase())),
                "{target}"
            );
        }
        assert_eq!(links.resolve(id_of(Ref::Path("missing.txt".into()))), None);
        assert_eq!(links.resolve(id_of(Ref::Path(".".into()))), None);
    }

    #[test]
    fn a_file_a_click_would_launch_stays_plain() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["x.desktop", "x.bat", "X.EXE", "x.pif", "x.fileloc"] {
            std::fs::write(dir.path().join(name), "x").unwrap();
        }
        let links = links_in(dir.path(), None);
        for name in ["x.desktop", "x.bat", "X.EXE", "x.pif", "x.fileloc"] {
            assert_eq!(links.resolve(id_of(Ref::Path(name.into()))), None, "{name}");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let script = dir.path().join("run.txt");
            std::fs::write(&script, "x").unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert_eq!(links.resolve(id_of(Ref::Path("run.txt".into()))), None);
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_judged_by_their_target() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("x.terminal"), "x").unwrap();
        std::fs::write(dir.path().join("ok.rs"), "x").unwrap();
        std::os::unix::fs::symlink("x.terminal", dir.path().join("notes.txt")).unwrap();
        std::os::unix::fs::symlink("ok.rs", dir.path().join("link.txt")).unwrap();
        let links = links_in(dir.path(), None);
        assert_eq!(links.resolve(id_of(Ref::Path("notes.txt".into()))), None);
        let real = std::fs::canonicalize(dir.path().join("ok.rs")).unwrap();
        let url = links.resolve(id_of(Ref::Path("link.txt".into()))).unwrap();
        assert!(url.ends_with(real.to_str().unwrap()), "{url}");
    }

    #[test]
    fn force_hyperlink_overrides_everything() {
        assert!(supported(
            env(&[("FORCE_HYPERLINK", "1"), ("TERM", "dumb")]),
            false
        ));
        assert!(!supported(
            env(&[("FORCE_HYPERLINK", "0"), ("TMUX", "x")]),
            false
        ));
        assert!(supported(env(&[("FORCE_HYPERLINK", "yes")]), true));
    }

    #[test]
    fn tmux_and_ordinary_terms_are_on_and_dumb_ones_off() {
        assert!(supported(
            env(&[("TMUX", "/tmp/t"), ("TERM", "screen-256color")]),
            false
        ));
        assert!(!supported(env(&[("TERM", "screen")]), false));
        assert!(!supported(env(&[("TERM", "screen-256color")]), false));
        assert!(!supported(env(&[("TERM", "dumb")]), false));
        assert!(!supported(env(&[("TERM", "linux")]), false));
        assert!(!supported(env(&[]), false));
        assert!(supported(env(&[("TERM", "xterm-256color")]), false));
    }

    #[test]
    fn windows_needs_a_terminal_that_says_what_it_is() {
        assert!(supported(env(&[("WT_SESSION", "1")]), true));
        assert!(supported(
            env(&[("TERM_PROGRAM", "vscode"), ("TERM", "dumb")]),
            true
        ));
        assert!(!supported(env(&[("TERM", "xterm")]), true));
    }

    fn row(text: &str, style: Style) -> Buffer {
        let mut buf = Buffer::empty(Rect::new(0, 0, text.chars().count() as u16, 1));
        buf.set_string(0, 0, text, style);
        buf
    }

    fn symbols(buf: &Buffer) -> Vec<String> {
        buf.content.iter().map(|c| c.symbol().to_string()).collect()
    }

    fn stripped(symbol: &str) -> String {
        match symbol.strip_prefix("\x1b]8;") {
            Some(rest) => {
                let rest = rest.split_once("\x1b\\").unwrap().1;
                rest.strip_suffix("\x1b]8;;\x1b\\").unwrap().to_string()
            }
            None => symbol.to_string(),
        }
    }

    #[test]
    fn a_tagged_row_gets_osc8_around_each_cell_and_keeps_its_text() {
        let dir = tempfile::tempdir().unwrap();
        let links = links_in(dir.path(), None);
        let r = Ref::Url("https://a.io/x".into());
        let id = id_of(r.clone());
        let mut buf = row("ab", tag(Style::default(), r));
        let plain = row("ab", Style::default());
        let area = buf.area;
        links.apply(&mut buf, area, Style::default());
        let got = symbols(&buf);
        assert_eq!(
            got[0],
            format!("\x1b]8;id={id:x};https://a.io/x\x1b\\a\x1b]8;;\x1b\\")
        );
        assert_eq!(
            got.iter().map(|s| stripped(s)).collect::<Vec<_>>(),
            symbols(&plain)
        );
        for cell in &buf.content {
            assert_eq!(cell.underline_color, Color::Reset);
            assert_eq!(cell.cell_width(), 1);
        }
    }

    #[test]
    fn with_links_off_no_internal_tag_survives_apply() {
        let dir = tempfile::tempdir().unwrap();
        let mut links = links_in(dir.path(), None);
        links.enabled = false;
        let mut buf = row("ab", tag(Style::default(), Ref::Path("a.txt".into())));
        buf[(1, 0)].underline_color = Color::Rgb(0xff, 0xff, 0xfe);
        let area = buf.area;
        links.apply(&mut buf, area, Style::default());
        for cell in &buf.content {
            assert_eq!(cell.underline_color, Color::Reset);
            assert!(!cell.symbol().contains("\x1b]8"));
        }
    }

    #[test]
    fn a_wide_symbol_keeps_its_width() {
        let dir = tempfile::tempdir().unwrap();
        let links = links_in(dir.path(), None);
        let mut buf = Buffer::empty(Rect::new(0, 0, 3, 1));
        buf.set_string(
            0,
            0,
            "世a",
            tag(Style::default(), Ref::Url("https://a.io".into())),
        );
        let area = buf.area;
        links.apply(&mut buf, area, Style::default());
        assert_eq!(buf[(0, 0)].cell_width(), 2);
        assert!(buf[(0, 0)].symbol().contains('世'));
        assert_eq!(buf[(1, 0)].symbol(), " ");
    }

    #[test]
    fn disabled_links_clear_the_tag_and_keep_context_free_styling() {
        let dir = tempfile::tempdir().unwrap();
        let mut links = links_in(dir.path(), None);
        links.enabled = false;
        let link = Style::default()
            .fg(Color::Blue)
            .add_modifier(Modifier::UNDERLINED);
        let r = Ref::Url("https://a.io/x".into());
        let mut buf = row("ab", styled(Style::default(), link, r));
        let area = buf.area;
        links.apply(&mut buf, area, link);
        assert_eq!(symbols(&buf), ["a", "b"]);
        for cell in &buf.content {
            assert_eq!(cell.underline_color, Color::Reset);
            assert_eq!(cell.fg, Color::Blue);
        }
    }

    #[test]
    fn an_unresolved_reference_gets_no_link_style_and_no_link() {
        let dir = tempfile::tempdir().unwrap();
        let links = links_in(dir.path(), None);
        let link = Style::default()
            .fg(Color::Blue)
            .add_modifier(Modifier::UNDERLINED);
        let mut buf = row("#82", styled(Style::default(), link, issue(82)));
        let area = buf.area;
        links.apply(&mut buf, area, link);
        for cell in &buf.content {
            assert_eq!(cell.underline_color, Color::Reset);
            assert_eq!(cell.fg, Color::Reset);
            assert!(!cell.modifier.contains(Modifier::UNDERLINED));
        }
        assert_eq!(symbols(&buf), ["#", "8", "2"]);
    }

    #[test]
    fn a_resolved_context_dependent_reference_gets_the_link_style() {
        let dir = tempfile::tempdir().unwrap();
        let links = links_in(dir.path(), Some(("o", "r")));
        let link = Style::default()
            .fg(Color::Blue)
            .add_modifier(Modifier::UNDERLINED);
        let mut buf = row("#82", styled(Style::default(), link, issue(82)));
        let area = buf.area;
        links.apply(&mut buf, area, link);
        for cell in &buf.content {
            assert_eq!(cell.fg, Color::Blue);
            assert!(cell.modifier.contains(Modifier::UNDERLINED));
            assert!(cell.symbol().contains("github.com/o/r/issues/82"));
        }
    }

    #[test]
    fn a_target_with_a_control_byte_is_never_linked() {
        let dir = tempfile::tempdir().unwrap();
        let mut links = links_in(dir.path(), None);
        // A path whose file name holds an escape resolves to a percent-encoded URL, so the guard
        // is exercised directly through a hand-made resolution.
        let id = id_of(Ref::Url("https://a.io/x".into()));
        lock_recovering(&links.resolved).insert(id, Some("https://a.io/\x1b]8;;evil".into()));
        links.enabled = true;
        let mut buf = row(
            "a",
            tag(Style::default(), Ref::Url("https://a.io/x".into())),
        );
        let area = buf.area;
        links.apply(&mut buf, area, Style::default());
        assert_eq!(buf[(0, 0)].symbol(), "a");
        assert_eq!(buf[(0, 0)].underline_color, Color::Reset);
        assert_eq!(buf[(0, 0)], {
            let mut c = Cell::new("a");
            c.underline_color = Color::Reset;
            c
        });
    }

    #[test]
    fn a_redrawn_cell_inside_a_link_still_carries_the_target() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        let links = links_in(dir.path(), None);
        let r = Ref::Path("a.txt".into());
        let link = Style::default().fg(Color::Blue);
        let frame = |text: &str| {
            let mut buf = row(text, tag(Style::default(), r.clone()));
            let area = buf.area;
            links.apply(&mut buf, area, link);
            buf
        };
        let (prev, next) = (frame("a.txt:1"), frame("a.txt:2"));
        let diff = prev.diff(&next);
        assert_eq!(diff.len(), 1);
        let (x, _, cell) = diff[0];
        assert_eq!(x, 6);
        assert!(cell.symbol().starts_with("\x1b]8;id="), "{}", cell.symbol());
        assert!(cell.symbol().contains("file://"), "{}", cell.symbol());
        assert_eq!(stripped(cell.symbol()), "2");
    }
}
