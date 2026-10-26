//! Reading one instruction file at a time: confinement, the `read` deny rules, dedup, and the
//! `@path` imports a file pulls in right after itself.

use std::path::{Path, PathBuf};

use serde_json::json;
use sha2::{Digest, Sha256};

use super::rules::{self, PathRule};
use super::{InstructionFile, Origin, Seen, Sources, Tier, markdown};
use crate::floor::under;
use crate::permission_rules::{CallContext, Posture, matches};
use fontaine_tools::error_chain::chain;

/// Imports nest at most this deep.
const MAX_HOPS: usize = 4;

pub(crate) struct Loader<'a> {
    sources: &'a Sources,
    seen: Seen,
    pub(crate) files: Vec<InstructionFile>,
    pub(crate) warnings: Vec<String>,
    pub(crate) path_rules: Vec<PathRule>,
}

/// A file that passed the checks, with what it said.
struct Admitted {
    canonical: PathBuf,
    raw: String,
}

impl<'a> Loader<'a> {
    pub(crate) fn new(sources: &'a Sources, seen: Seen) -> Loader<'a> {
        Loader {
            sources,
            seen,
            files: Vec::new(),
            warnings: Vec::new(),
            path_rules: Vec::new(),
        }
    }

    pub(crate) fn finish(self) -> (Vec<InstructionFile>, Vec<String>, Vec<PathRule>, Seen) {
        (self.files, self.warnings, self.path_rules, self.seen)
    }

    pub(crate) fn user_tier(&mut self) {
        let sources = self.sources;
        if let Some(home) = &sources.home {
            let claude = home.join(".claude");
            self.file(&claude.join("CLAUDE.md"), Tier::User, Origin::User, 0);
            self.rules_dir(&claude.join("rules"), Tier::User, Origin::User, None);
        }
        self.file(
            &sources.config_dir.join("FONTAINE.md"),
            Tier::User,
            Origin::User,
            0,
        );
        self.rules_dir(
            &sources.config_dir.join("rules"),
            Tier::User,
            Origin::User,
            None,
        );
    }

    /// One directory of the walk: its instruction files in order, then its rules, its local
    /// files and, when `agents` is set, its `AGENTS.md`.
    pub(crate) fn walk_dir(&mut self, dir: &Path, agents: bool) {
        let tier = self.sources.dir_tier(dir);
        for name in [
            "CLAUDE.md",
            ".claude/CLAUDE.md",
            "FONTAINE.md",
            ".fontaine/FONTAINE.md",
        ] {
            self.file(&dir.join(name), tier, Origin::Walk, 0);
        }
        for rules in [".claude/rules", ".fontaine/rules"] {
            self.rules_dir(&dir.join(rules), tier, Origin::Rule, Some(dir));
        }
        for name in ["CLAUDE.local.md", "FONTAINE.local.md"] {
            self.file(&dir.join(name), tier, Origin::Local, 0);
        }
        if agents {
            for name in ["AGENTS.md", ".claude/AGENTS.md"] {
                self.file(&dir.join(name), tier, Origin::Agents, 0);
            }
        }
    }

    /// Loads the rules without `paths` and registers the others. `base` is where path patterns
    /// are relative to; user rules use the primary root.
    fn rules_dir(&mut self, dir: &Path, tier: Tier, origin: Origin, base: Option<&Path>) {
        let base = base.unwrap_or(&self.sources.primary_root).to_path_buf();
        for file in rules::rule_files(dir) {
            let Some(admitted) = self.admit(&file, tier, &origin, 0) else {
                continue;
            };
            let parsed = rules::parse(&admitted.raw);
            let Some(patterns) = parsed.paths else {
                self.accept(&file, admitted.canonical, parsed.text, tier, &origin, 0);
                continue;
            };
            let (globs, problems) = rules::compile(&patterns);
            for problem in problems {
                self.warnings
                    .push(format!("rule {}: {problem}", file.display()));
            }
            if let Some(globs) = globs {
                self.path_rules.push(PathRule {
                    base: base.clone(),
                    file,
                    globs,
                    tier,
                    text: parsed.text,
                });
            }
        }
    }

    /// Loads a rule that a path brought into view.
    pub(crate) fn take_rule(&mut self, rule: &PathRule) {
        let Ok(canonical) = std::fs::canonicalize(&rule.file) else {
            return;
        };
        if self.seen.paths.contains(&canonical) {
            return;
        }
        self.accept(
            &rule.file,
            canonical,
            rule.text.clone(),
            rule.tier,
            &Origin::Rule,
            0,
        );
    }

    /// Loads `path` if it exists and passes the checks, then its imports.
    pub(crate) fn file(&mut self, path: &Path, tier: Tier, origin: Origin, hop: usize) {
        let Some(admitted) = self.admit(path, tier, &origin, hop) else {
            return;
        };
        let text = markdown::strip_comments(&admitted.raw);
        self.accept(path, admitted.canonical, text, tier, &origin, hop);
    }

    fn skip(&mut self, path: &Path, origin: &Origin, reason: &str) {
        let via = match origin {
            Origin::Import { from } => format!(" (imported by {})", from.display()),
            _ => String::new(),
        };
        self.warnings.push(format!(
            "instruction file {}{via} was skipped: {reason}",
            path.display()
        ));
    }

    /// Checks a candidate and reads it. A file that does not exist, or that was already loaded,
    /// is passed over silently; every other refusal leaves a warning.
    fn admit(&mut self, path: &Path, tier: Tier, origin: &Origin, hop: usize) -> Option<Admitted> {
        if !std::fs::metadata(path).ok()?.is_file() {
            return None;
        }
        let canonical = match std::fs::canonicalize(path) {
            Ok(canonical) => canonical,
            Err(e) => {
                self.skip(
                    path,
                    origin,
                    &format!("it cannot be resolved: {}", chain(&e)),
                );
                return None;
            }
        };
        if self.seen.paths.contains(&canonical) {
            return None;
        }
        if hop > MAX_HOPS {
            self.skip(path, origin, "imports nest more than four levels deep");
            return None;
        }
        if tier == Tier::Repo && !self.sources.confined(&canonical) {
            self.skip(
                path,
                origin,
                "it resolves outside the workspace roots and the enclosing git work tree",
            );
            return None;
        }
        if self.denied(path) || self.denied(&canonical) {
            self.skip(path, origin, "a `read` deny rule covers it");
            return None;
        }
        match std::fs::read(&canonical) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(raw) => Some(Admitted { canonical, raw }),
                Err(_) => {
                    self.skip(path, origin, "it is not UTF-8 text");
                    None
                }
            },
            Err(e) => {
                self.skip(path, origin, &format!("it cannot be read: {}", chain(&e)));
                None
            }
        }
    }

    fn denied(&self, path: &Path) -> bool {
        let args = json!({ "path": path.to_string_lossy() });
        let call = CallContext {
            tool: "read",
            args: &args,
            scope: &self.sources.scope,
            home: self.sources.home.as_deref(),
        };
        self.sources
            .deny
            .iter()
            .any(|rule| matches(rule, &call, Posture::Deny))
    }

    /// Records the file, drops it if blank or a repeat of text already loaded, and loads its
    /// imports right after it.
    fn accept(
        &mut self,
        found: &Path,
        canonical: PathBuf,
        text: String,
        tier: Tier,
        origin: &Origin,
        hop: usize,
    ) {
        self.seen.paths.insert(canonical);
        let effective = text.trim();
        if effective.is_empty() {
            return;
        }
        let digest: [u8; 32] = Sha256::digest(effective.as_bytes()).into();
        if !self.seen.digests.insert(digest) {
            return;
        }
        let tokens = markdown::imports(&text);
        self.files.push(InstructionFile {
            path: found.to_path_buf(),
            text,
            origin: origin.clone(),
        });
        let import_tier = if tier == Tier::User {
            Tier::User
        } else {
            Tier::Repo
        };
        for token in tokens {
            self.import(found, &token, import_tier, hop + 1);
        }
    }

    fn import(&mut self, from: &Path, token: &str, tier: Tier, hop: usize) {
        let target = if let Some(rest) = token.strip_prefix("~/") {
            match &self.sources.home {
                Some(home) => home.join(rest),
                None => return,
            }
        } else if Path::new(token).is_absolute() {
            PathBuf::from(token)
        } else {
            from.parent().unwrap_or(Path::new(".")).join(token)
        };
        let origin = Origin::Import {
            from: from.to_path_buf(),
        };
        self.file(&target, tier, origin, hop);
    }
}

impl Sources {
    pub(crate) fn confined(&self, canonical: &Path) -> bool {
        self.confinement
            .iter()
            .any(|root| under(canonical, root).is_some())
    }

    /// Whether a walk directory lies inside the confinement set.
    pub(crate) fn dir_tier(&self, dir: &Path) -> Tier {
        if self.confined(dir) {
            Tier::Repo
        } else {
            Tier::Above
        }
    }
}
