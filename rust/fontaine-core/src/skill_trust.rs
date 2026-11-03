//! The user's record of skills whose `allowed-tools` they chose to honour. It lives in
//! `<config dir>/skill-trust.toml`, never in a settings file and never in a repository, and each
//! entry is pinned to a digest of the skill file, so a changed file is asked about again.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::ConfigError;
use crate::extensions::Scope;
use crate::owner_only;

pub const TRUST_FILE: &str = "skill-trust.toml";

pub fn trust_path(config_dir: &Path) -> PathBuf {
    config_dir.join(TRUST_FILE)
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustFile {
    #[serde(default)]
    pub skill: Vec<TrustRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustRecord {
    /// The composed skill name, as `/skills` shows it.
    pub name: String,
    /// `sha256:<hex>` of the skill file's bytes, frontmatter and body.
    pub digest: String,
    /// The trusted version's declared `allowed-tools`, kept so a later change can be shown as a
    /// diff without keeping the old file.
    #[serde(default)]
    pub tools: Vec<String>,
    /// The trusted version's `!` command lines.
    #[serde(default)]
    pub commands: Vec<String>,
}

/// Whether a skill's recorded trust still matches the file that is loaded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrustState {
    Untrusted,
    /// The user trusted an earlier version of the file.
    Stale,
    Trusted,
}

/// The parsed record a session reads; read-only for the session's lifetime.
#[derive(Clone, Debug, Default)]
pub struct TrustedSkills {
    records: Vec<TrustRecord>,
}

impl TrustedSkills {
    pub fn from_file(file: TrustFile) -> TrustedSkills {
        TrustedSkills {
            records: file.skill,
        }
    }

    pub fn record(&self, name: &str) -> Option<&TrustRecord> {
        self.records.iter().find(|record| record.name == name)
    }

    pub fn records(&self) -> &[TrustRecord] {
        &self.records
    }

    /// A project skill is never trusted, whatever the record says.
    pub fn state(&self, name: &str, scope: Scope, digest: &str) -> TrustState {
        if scope == Scope::Project {
            return TrustState::Untrusted;
        }
        match self.record(name) {
            None => TrustState::Untrusted,
            Some(record) if record.digest == digest => TrustState::Trusted,
            Some(_) => TrustState::Stale,
        }
    }
}

/// The `!` command lines a skill's body asks to have run, as `!<command>`.
pub fn command_lines(body: &str) -> Vec<String> {
    crate::skills::parts(body)
        .into_iter()
        .filter_map(|part| match part {
            crate::skills::Part::Command(command) => Some(format!("!{command}")),
            crate::skills::Part::Text(_) => None,
        })
        .collect()
}

/// A skill's declared `allowed-tools` split into specific rules, which a trusted skill lends as
/// allows, and wildcard shell rules, which only reach the guardian as context.
pub fn split_tools(skill: &crate::extensions::Skill) -> (Vec<String>, Vec<String>) {
    let (wildcard, specific): (Vec<_>, Vec<_>) = skill
        .declared_tools
        .iter()
        .partition(|rule| crate::skills::is_wildcard_shell(rule));
    let text = |rules: Vec<&crate::permission_rules::Rule>| {
        rules.into_iter().map(ToString::to_string).collect()
    };
    (text(specific), text(wildcard))
}

/// What trusting `skill` as it is now would record.
pub fn record_of(skill: &crate::extensions::Skill) -> TrustRecord {
    TrustRecord {
        name: skill.name.clone(),
        digest: skill.digest.clone(),
        tools: skill
            .declared_tools
            .iter()
            .map(ToString::to_string)
            .collect(),
        commands: command_lines(&skill.body),
    }
}

/// The lines that differ between `before` and `after`: `- x` for one that went away and `+ x`
/// for one that is new. Order within each side is kept.
pub fn diff_lines(before: &[String], after: &[String]) -> Vec<String> {
    before
        .iter()
        .filter(|line| !after.contains(line))
        .map(|line| format!("- {line}"))
        .chain(
            after
                .iter()
                .filter(|line| !before.contains(line))
                .map(|line| format!("+ {line}")),
        )
        .collect()
}

pub fn digest_of(bytes: &[u8]) -> String {
    format!("sha256:{}", crate::checkpoint::hex(&Sha256::digest(bytes)))
}

/// Schema check for `skill-trust.toml` text, used by the docs check.
pub fn validate_skill_trust_toml(text: &str) -> Result<(), String> {
    toml::from_str::<TrustFile>(text)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn read_text(path: &Path) -> Result<Option<String>, io::Error> {
    match crate::file_lock::read_shared(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn parse(text: &str) -> Result<TrustFile, String> {
    toml::from_str(text)
        .map_err(|e: toml::de::Error| crate::config::bare_toml_message(text, e.span(), e.message()))
}

/// The record a session starts with. A missing file is an empty record; an unreadable or corrupt
/// one loads nothing and comes back with a one-line warning.
pub fn load(config_dir: &Path) -> (TrustedSkills, Option<String>) {
    let path = trust_path(config_dir);
    let outcome = match read_text(&path) {
        Ok(None) => return (TrustedSkills::default(), None),
        Ok(Some(text)) => parse(&text),
        Err(e) => Err(fontaine_tools::error_chain::chain(&e)),
    };
    match outcome {
        Ok(file) => (TrustedSkills::from_file(file), None),
        Err(cause) => (
            TrustedSkills::default(),
            Some(format!(
                "{} cannot be used ({cause}): no skill is trusted until it is fixed or removed",
                path.display()
            )),
        ),
    }
}

/// Reads the record for a change. Unlike [`load`], a corrupt file is an error, so a change never
/// overwrites it. The caller holds [`crate::file_lock::lock`] on [`trust_path`].
fn read_for_update(path: &Path) -> Result<TrustFile, ConfigError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(TrustFile::default()),
        Err(source) => {
            return Err(ConfigError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    parse(&text).map_err(|message| ConfigError::Parse {
        path: path.to_path_buf(),
        message,
    })
}

fn write(path: &Path, file: &TrustFile) -> Result<(), ConfigError> {
    let text = toml::to_string_pretty(file).map_err(|e| ConfigError::Parse {
        path: path.to_path_buf(),
        message: format!("cannot encode the record: {e}"),
    })?;
    owner_only::write_atomic(path, text.as_bytes()).map_err(|source| ConfigError::Write {
        path: path.to_path_buf(),
        source,
    })
}

fn locked(path: &Path) -> Result<crate::file_lock::FileLock, ConfigError> {
    crate::file_lock::lock(path).map_err(|source| ConfigError::Write {
        path: path.with_extension("lock"),
        source,
    })
}

/// Records `records`, replacing an earlier entry of the same name.
pub fn trust(config_dir: &Path, records: Vec<TrustRecord>) -> Result<(), ConfigError> {
    let path = trust_path(config_dir);
    let _lock = locked(&path)?;
    let mut file = read_for_update(&path)?;
    for record in records {
        file.skill.retain(|held| held.name != record.name);
        file.skill.push(record);
    }
    write(&path, &file)
}

/// Removes the entries named in `names`; the names that had none are returned.
pub fn untrust(config_dir: &Path, names: &[String]) -> Result<Vec<String>, ConfigError> {
    let path = trust_path(config_dir);
    let _lock = locked(&path)?;
    let mut file = read_for_update(&path)?;
    let mut missing = Vec::new();
    for name in names {
        let before = file.skill.len();
        file.skill.retain(|held| &held.name != name);
        if file.skill.len() == before {
            missing.push(name.clone());
        }
    }
    if missing.len() < names.len() {
        write(&path, &file)?;
    }
    Ok(missing)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(name: &str, digest: &str) -> TrustRecord {
        TrustRecord {
            name: name.into(),
            digest: digest.into(),
            tools: vec!["Bash(git *)".into()],
            commands: vec!["!git status".into()],
        }
    }

    #[test]
    fn a_record_round_trips_with_its_tools_and_commands() {
        let dir = tempfile::tempdir().unwrap();
        trust(dir.path(), vec![record("a:b", "sha256:1")]).unwrap();
        let (view, warning) = load(dir.path());
        assert!(warning.is_none());
        assert_eq!(view.record("a:b"), Some(&record("a:b", "sha256:1")));
        trust(dir.path(), vec![record("a:b", "sha256:2")]).unwrap();
        let (view, _) = load(dir.path());
        assert_eq!(view.records().len(), 1);
        assert_eq!(view.record("a:b").unwrap().digest, "sha256:2");
    }

    #[test]
    fn a_corrupt_file_loads_nothing_warns_and_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(trust_path(dir.path()), "[[skill]]\nname = 3\n").unwrap();
        let (view, warning) = load(dir.path());
        assert!(view.records().is_empty());
        assert!(warning.unwrap().contains("no skill is trusted"));
        assert!(trust(dir.path(), vec![record("a", "sha256:1")]).is_err());
        assert!(untrust(dir.path(), &["a".into()]).is_err());
        assert_eq!(
            std::fs::read_to_string(trust_path(dir.path())).unwrap(),
            "[[skill]]\nname = 3\n"
        );
    }

    #[test]
    fn an_unknown_key_is_rejected() {
        assert!(
            validate_skill_trust_toml("[[skill]]\nname = 'a'\ndigest = 'd'\nextra = 1\n").is_err()
        );
        assert!(validate_skill_trust_toml("[[skill]]\nname = 'a'\ndigest = 'd'\n").is_ok());
    }

    #[test]
    fn trust_state_follows_the_digest_and_never_trusts_a_project_skill() {
        let view = TrustedSkills::from_file(TrustFile {
            skill: vec![record("s", "sha256:1")],
        });
        assert_eq!(
            view.state("s", Scope::Plugin, "sha256:1"),
            TrustState::Trusted
        );
        assert_eq!(
            view.state("s", Scope::User, "sha256:1"),
            TrustState::Trusted
        );
        assert_eq!(view.state("s", Scope::User, "sha256:2"), TrustState::Stale);
        assert_eq!(
            view.state("t", Scope::User, "sha256:1"),
            TrustState::Untrusted
        );
        assert_eq!(
            view.state("s", Scope::Project, "sha256:1"),
            TrustState::Untrusted
        );
    }

    #[test]
    fn untrust_removes_entries_and_reports_the_unknown() {
        let dir = tempfile::tempdir().unwrap();
        trust(
            dir.path(),
            vec![record("a", "sha256:1"), record("b", "sha256:1")],
        )
        .unwrap();
        let missing = untrust(dir.path(), &["a".into(), "z".into()]).unwrap();
        assert_eq!(missing, ["z"]);
        let (view, _) = load(dir.path());
        assert!(view.record("a").is_none() && view.record("b").is_some());
    }

    #[test]
    fn command_lines_and_diffs_show_what_changed() {
        let before = command_lines("a !`git status` b\n```!\nls\n```\n");
        assert_eq!(before, ["!git status", "!ls"]);
        let after = command_lines("!`git status` !`rm -rf x`");
        assert_eq!(diff_lines(&before, &after), ["- !ls", "+ !rm -rf x"]);
        assert!(diff_lines(&after, &after).is_empty());
    }

    #[test]
    fn the_digest_is_a_prefixed_sha256() {
        assert_eq!(
            digest_of(b""),
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
