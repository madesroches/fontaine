//! The install receipt the installers write, and the decision whether this process is the binary
//! it describes.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::release_signature::Channel;

pub const RECEIPT_FILE: &str = "fontaine-receipt.json";

/// An install this process may replace itself from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallInfo {
    pub channel: Channel,
    pub version: String,
    pub binary_path: PathBuf,
    pub receipt_path: PathBuf,
}

#[derive(Debug)]
pub enum ReceiptError {
    Missing(PathBuf),
    Unreadable(PathBuf, std::io::Error),
    Malformed(&'static str),
    /// The receipt describes a different binary than the running one.
    OtherBinary {
        receipt: PathBuf,
        running: PathBuf,
        channel: Channel,
    },
    CurrentExe(std::io::Error),
}

impl std::fmt::Display for ReceiptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReceiptError::Missing(p) => write!(f, "there is no install receipt at {}", p.display()),
            ReceiptError::Unreadable(p, _) => {
                write!(f, "the install receipt {} cannot be read", p.display())
            }
            ReceiptError::Malformed(why) => write!(f, "the install receipt is unusable: {why}"),
            ReceiptError::OtherBinary {
                receipt, running, ..
            } => write!(
                f,
                "the install receipt describes {}, not the running {}",
                receipt.display(),
                running.display()
            ),
            ReceiptError::CurrentExe(_) => f.write_str("the running executable cannot be located"),
        }
    }
}

impl std::error::Error for ReceiptError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ReceiptError::Unreadable(_, e) | ReceiptError::CurrentExe(e) => Some(e),
            _ => None,
        }
    }
}

/// Where the installers put the receipt: `XDG_CONFIG_HOME` when set, else `~/.config` (Windows:
/// `%LOCALAPPDATA%`). This is not `dirs::config_dir`, which is the roaming profile on Windows.
pub fn receipt_path(env: &dyn Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    let non_empty = |name: &str| env(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    let base = non_empty("XDG_CONFIG_HOME").or_else(|| {
        if cfg!(windows) {
            non_empty("LOCALAPPDATA")
        } else {
            non_empty("HOME").map(|h| h.join(".config"))
        }
    })?;
    Some(base.join("fontaine").join(RECEIPT_FILE))
}

/// A dev build's version is `<base>-dev.<n>+<sha7>`; anything else is a release.
pub fn channel_of(version: &str) -> Channel {
    if version.contains("-dev.") {
        Channel::Dev
    } else {
        Channel::Release
    }
}

struct Parsed {
    binary: PathBuf,
    version: String,
}

fn parse(json: &str) -> Result<Parsed, ReceiptError> {
    let value: Value =
        serde_json::from_str(json).map_err(|_| ReceiptError::Malformed("not valid JSON"))?;
    let text = |key: &str| value.get(key).and_then(Value::as_str);
    let prefix = text("install_prefix").ok_or(ReceiptError::Malformed("no install_prefix"))?;
    if prefix.starts_with('~') || !Path::new(prefix).is_absolute() {
        return Err(ReceiptError::Malformed("install_prefix is not absolute"));
    }
    let name = value
        .get("binaries")
        .and_then(Value::as_array)
        .and_then(|b| b.first())
        .and_then(Value::as_str)
        .filter(|n| !n.is_empty())
        .ok_or(ReceiptError::Malformed("no binaries"))?;
    if name.contains(['/', '\\']) {
        return Err(ReceiptError::Malformed("binaries[0] is not a file name"));
    }
    let version = text("version").ok_or(ReceiptError::Malformed("no version"))?;
    let mut file = name.to_string();
    if cfg!(windows) && !file.ends_with(".exe") {
        file.push_str(".exe");
    }
    Ok(Parsed {
        binary: Path::new(prefix).join("bin").join(file),
        version: version.to_string(),
    })
}

/// The receipt at `path`, if it describes the executable `current_exe`.
pub fn locate(path: &Path, current_exe: &Path) -> Result<InstallInfo, ReceiptError> {
    let json = match std::fs::read_to_string(path) {
        Ok(json) => json,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ReceiptError::Missing(path.to_path_buf()));
        }
        Err(e) => return Err(ReceiptError::Unreadable(path.to_path_buf(), e)),
    };
    let parsed = parse(&json)?;
    let receipt_binary = parsed
        .binary
        .canonicalize()
        .map_err(|_| ReceiptError::OtherBinary {
            receipt: parsed.binary.clone(),
            running: current_exe.to_path_buf(),
            channel: channel_of(&parsed.version),
        })?;
    let running = current_exe
        .canonicalize()
        .map_err(ReceiptError::CurrentExe)?;
    if receipt_binary != running {
        return Err(ReceiptError::OtherBinary {
            receipt: receipt_binary,
            running,
            channel: channel_of(&parsed.version),
        });
    }
    Ok(InstallInfo {
        channel: channel_of(&parsed.version),
        version: parsed.version,
        binary_path: receipt_binary,
        receipt_path: path.to_path_buf(),
    })
}

/// Rewrites only `version`, atomically so a crash leaves the old receipt whole. The caller holds the update lock.
pub fn write_version(path: &Path, version: &str) -> Result<(), WriteError> {
    let json = std::fs::read_to_string(path).map_err(WriteError::Io)?;
    let mut value: Value = serde_json::from_str(&json).map_err(WriteError::Parse)?;
    let object = value.as_object_mut().ok_or(WriteError::NotAnObject)?;
    object.insert("version".to_string(), Value::String(version.to_string()));
    let mut text = serde_json::to_string_pretty(&value).map_err(WriteError::Parse)?;
    text.push('\n');
    fontaine_core::owner_only::write_atomic_keeping_mode(path, text.as_bytes())
        .map_err(WriteError::Io)
}

#[derive(Debug)]
pub enum WriteError {
    Io(std::io::Error),
    Parse(serde_json::Error),
    NotAnObject,
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WriteError::Io(_) => f.write_str("the install receipt cannot be rewritten"),
            WriteError::Parse(_) => f.write_str("the install receipt is not valid JSON"),
            WriteError::NotAnObject => f.write_str("the install receipt is not a JSON object"),
        }
    }
}

impl std::error::Error for WriteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            WriteError::Io(e) => Some(e),
            WriteError::Parse(e) => Some(e),
            WriteError::NotAnObject => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt(prefix: &str, binaries: &str, version: &str) -> String {
        format!(
            r#"{{"binaries":[{binaries}],"install_prefix":"{prefix}","install_layout":"flat","version":"{version}","provider":{{"source":"cargo-dist","version":"0.32.0"}}}}"#
        )
    }

    /// An install tree whose receipt names `bin/fontaine`; returns (root, receipt, binary).
    fn tree(version: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let name = if cfg!(windows) {
            "fontaine.exe"
        } else {
            "fontaine"
        };
        let binary = bin.join(name);
        std::fs::write(&binary, b"old").unwrap();
        let receipt_file = root.path().join(RECEIPT_FILE);
        let prefix = root.path().to_str().unwrap().replace('\\', "\\\\");
        std::fs::write(&receipt_file, receipt(&prefix, "\"fontaine\"", version)).unwrap();
        (root, receipt_file, binary)
    }

    #[test]
    fn channel_comes_from_the_version() {
        assert_eq!(channel_of("0.1.0-dev.42+abcdef1"), Channel::Dev);
        assert_eq!(channel_of("0.1.0"), Channel::Release);
    }

    #[test]
    fn matching_binary_is_self_updatable() {
        let (_root, receipt, binary) = tree("0.1.0-dev.7+abcdef1");
        let info = locate(&receipt, &binary).unwrap();
        assert_eq!(info.channel, Channel::Dev);
        assert_eq!(info.version, "0.1.0-dev.7+abcdef1");
    }

    #[test]
    fn release_receipt_is_release_channel() {
        let (_root, receipt, binary) = tree("1.2.3");
        assert_eq!(locate(&receipt, &binary).unwrap().channel, Channel::Release);
    }

    #[test]
    fn missing_receipt_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let err = locate(&dir.path().join(RECEIPT_FILE), Path::new("/x")).unwrap_err();
        assert!(matches!(err, ReceiptError::Missing(_)));
    }

    #[test]
    fn malformed_receipts_are_rejected() {
        for json in [
            "not json",
            "{}",
            r#"{"install_prefix":"/a","binaries":[],"version":"1"}"#,
            r#"{"install_prefix":"/a","binaries":["f"]}"#,
            r#"{"install_prefix":"relative","binaries":["f"],"version":"1"}"#,
            r#"{"install_prefix":"~/.cargo","binaries":["f"],"version":"1"}"#,
            r#"{"install_prefix":"/a","binaries":["../f"],"version":"1"}"#,
        ] {
            assert!(
                matches!(parse(json), Err(ReceiptError::Malformed(_))),
                "{json}"
            );
        }
    }

    #[test]
    fn exe_suffix_is_not_doubled_and_layout_is_ignored() {
        let prefix = if cfg!(windows) { r"C:\opt\x" } else { "/opt/x" };
        let json = format!(
            r#"{{"install_prefix":"{}","install_layout":"hierarchical","binaries":["fontaine.exe"],"version":"1.0.0"}}"#,
            prefix.replace('\\', "\\\\")
        );
        let binary = parse(&json).unwrap().binary;
        assert_eq!(binary.file_name().unwrap(), "fontaine.exe");
    }

    #[test]
    fn another_binary_is_not_self_updatable() {
        let (_root, receipt, _binary) = tree("1.0.0");
        let other = tempfile::NamedTempFile::new().unwrap();
        let err = locate(&receipt, other.path()).unwrap_err();
        assert!(matches!(err, ReceiptError::OtherBinary { .. }));
    }

    #[test]
    fn write_version_keeps_every_other_field() {
        let (_root, receipt, _binary) = tree("0.1.0-dev.1+aaaaaaa");
        write_version(&receipt, "0.1.0-dev.2+bbbbbbb").unwrap();
        let value: Value =
            serde_json::from_str(&std::fs::read_to_string(&receipt).unwrap()).unwrap();
        assert_eq!(value["version"], "0.1.0-dev.2+bbbbbbb");
        assert_eq!(value["install_layout"], "flat");
        assert_eq!(value["provider"]["source"], "cargo-dist");
    }

    #[test]
    fn receipt_path_prefers_xdg() {
        let env = |name: &str| match name {
            "XDG_CONFIG_HOME" => Some("/x".into()),
            "HOME" | "LOCALAPPDATA" => Some("/h".into()),
            _ => None,
        };
        assert_eq!(
            receipt_path(&env),
            Some(PathBuf::from("/x/fontaine").join(RECEIPT_FILE))
        );
    }
}
